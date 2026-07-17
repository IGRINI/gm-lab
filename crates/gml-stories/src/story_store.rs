//! Filesystem-backed story package store (Phase 3 of
//! `docs/MODS_PACKAGES_TZ.md`).
//!
//! Stories are portable packages on disk (the "mod" model). The filesystem is
//! the source of truth: at construction the store materializes the three
//! built-in stories as DEFAULT packages if they are missing, then scans the
//! `stories/` directory. After that, story data is read ONLY from the scanned
//! packages — the embedded `catalog.json` (see [`crate::CATALOG_JSON`]) is used
//! solely as the SOURCE for materializing the defaults on first run.
//!
//! Disk layout (per story):
//! ```text
//! <root>/stories/<story_id>/story.json
//! ```
//!
//! `story.json` envelope:
//! ```json
//! {
//!   "format": "gmlab.story/1",
//!   "id": "<story_id>",
//!   "version": 1,
//!   "kind": "authored",
//!   "world_ref": { "id": "...", "version": 0 },   // only when world-bound
//!   "world_embedded": true,
//!   "title": "...",
//!   "description": "...",
//!   "created_at": "...",  // only when set (blank string is not emitted)
//!   "updated_at": "...",  // only when set (blank string is not emitted)
//!   "seed": { ...the EXACT legacy catalog seed Value... },
//!   "meta": { ... }       // only when a NON-EMPTY object (see below)
//! }
//! ```
//!
//! `meta` is an opaque round-trip object (`§С1.1` of
//! `docs/CHARACTERS_AND_STORY_TZ.md`): it carries the story-architect chat state
//! (`architect_messages` / `architect_model_history` / `architect_cache_*`) that
//! MUST NOT live in `seed` (it would leak into worldgen and the python-byte gate)
//! nor at the top level (the fixed key whitelist in [`StoryEnvelope::from_value`]
//! drops unknown top keys). It is EMITTED ONLY when it is a non-empty object, and
//! `created_at`/`updated_at` are emitted ONLY when non-blank — so the three
//! built-in default packages keep their EXACT byte shape (nothing new is written
//! for them). `meta` is written LAST so an added key never shifts the position of
//! any existing key.
//!
//! The `seed` object is byte-shape-identical (key order preserved via serde_json
//! `preserve_order`) to the legacy `catalog.json` seed, so the game and the
//! python-byte-length invariant are unaffected.
//!
//! `world_ref` / non-embedded stories are Phase 4 — only the `format` field needs
//! to allow them here; world composition is not wired now.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::{UnknownStory, CATALOG_JSON, DEFAULT_STORY_ID};

/// A reference from a story package to the WORLD package it is bound to
/// (`docs/MODS_PACKAGES_TZ.md`). `version` is the world `version` the story was
/// authored against (`0` = unpinned / "any").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoryWorldRef {
    /// The referenced world package id.
    pub id: String,
    /// The world `version` the story targets (`0` = unpinned).
    pub version: u64,
}

/// On-disk envelope format tag for `story.json`.
pub const STORY_FORMAT: &str = "gmlab.story/1";
/// Legacy per-story architect-chat filename (pre-DB packages): read as a
/// fallback by `get_architect_state`, deleted by `purge_architect_artifacts`.
/// The conversation's real home is the dialogs SQLite.
const ARCHITECT_FILE: &str = "architect.json";
/// Legacy `meta` keys that carried the architect chat INSIDE `story.json`
/// before the split; read as a fallback and stripped on the next architect save.
const LEGACY_ARCHITECT_META_KEYS: [&str; 4] = [
    "architect_messages",
    "architect_model_history",
    "architect_cache_session_id",
    "architect_cache_thread_id",
];
/// Per-story manifest filename.
const STORY_FILE: &str = "story.json";
/// Sub-directory under the packages root that holds story packages.
const STORIES_DIR: &str = "stories";

/// Errors raised by [`StoryStore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoryStoreError {
    /// A story package on disk could not be read or parsed. Carries a
    /// human-readable reason. A malformed/unreadable package surfaces this — it
    /// is never silently replaced by a default.
    Io(String),
    /// [`StoryStore::update_story`] was asked to patch an id absent from the
    /// library. Distinct from [`StoryStoreError::Io`] so the server can map it to
    /// a 400 (`§С1.1`), mirroring `StoreError::CharacterNotFound`.
    StoryNotFound(String),
    /// A patch violated an `update_story` invariant (e.g. a blank title, a
    /// non-object seed/meta patch, or editing a story the architect may not
    /// touch — a self-contained builtin without a `world_ref`, `§С1.1`).
    Invalid(String),
}

impl std::fmt::Display for StoryStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoryStoreError::Io(msg) => write!(f, "{msg}"),
            StoryStoreError::StoryNotFound(id) => write!(f, "story not found: {id}"),
            StoryStoreError::Invalid(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for StoryStoreError {}

/// Filesystem-backed story package store.
///
/// The packages root (`<root>`) holds a `stories/` sub-directory; each story is a
/// folder named by its `story_id` containing a single `story.json`. Built-in
/// defaults are materialized atomically (temp file + `rename`) on first run.
pub struct StoryStore {
    root: PathBuf,
    /// Parsed, scanned story envelopes in catalog/discovery order.
    stories: Vec<StoryEnvelope>,
    write_lock: Mutex<()>,
}

impl StoryStore {
    /// Open a story package store rooted at `root`: ensure the three built-in
    /// default packages exist (materializing any that are missing from the
    /// embedded catalog), then scan `<root>/stories/`.
    ///
    /// A malformed/unreadable `story.json` aborts construction with
    /// [`StoryStoreError::Io`] rather than being silently dropped or replaced.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoryStoreError> {
        let root = abspath(root.into());
        let mut store = StoryStore {
            root,
            stories: Vec::new(),
            write_lock: Mutex::new(()),
        };
        std::fs::create_dir_all(store.stories_dir())
            .map_err(|e| StoryStoreError::Io(format!("create stories dir: {e}")))?;
        store.ensure_defaults()?;
        store.stories = store.scan()?;
        Ok(store)
    }

    /// Default packages root: `GM_PACKAGES_DIR` override, else the app-data
    /// `library` directory (identical resolution to the world store).
    pub fn default_root() -> PathBuf {
        match std::env::var("GM_PACKAGES_DIR") {
            Ok(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
            _ => PathBuf::from(gml_config::config::default_library_dir()),
        }
    }

    /// The packages root directory (`<root>`).
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn stories_dir(&self) -> PathBuf {
        self.root.join(STORIES_DIR)
    }

    /// The package directory of one story (`<root>/stories/<id>`). Public for the
    /// Phase-5 share UX (zip export reads this whole directory; zip import writes
    /// into it). `story_id` MUST be an already-validated single path segment.
    pub fn story_dir(&self, story_id: &str) -> PathBuf {
        self.stories_dir().join(story_id)
    }

    fn story_file(&self, story_id: &str) -> PathBuf {
        self.story_dir(story_id).join(STORY_FILE)
    }

    /// Whether a story package exists on disk (its `story.json` is present).
    /// Phase-5 import uses this as the overwrite-collision check.
    pub fn story_exists(&self, story_id: &str) -> bool {
        self.story_file(story_id).is_file()
    }

    /// Re-scan `<root>/stories/` from disk, replacing the in-memory list.
    ///
    /// Unlike [`WorldStore`], a [`StoryStore`] caches its scanned package list in
    /// memory, so a story package written to disk out-of-band (the Phase-5 zip
    /// import drops a new `stories/<id>/` folder) is invisible until a rescan.
    /// The import handler calls this after extracting a story so the package is
    /// live without a process restart. A malformed package aborts the rescan with
    /// [`StoryStoreError::Io`] and leaves the previous list in place.
    pub fn reload(&mut self) -> Result<(), StoryStoreError> {
        let scanned = self.scan()?;
        self.stories = scanned;
        Ok(())
    }

    // ------------------------------------------------------------------
    // public API (mirrors the former gml-stories free functions 1:1)
    // ------------------------------------------------------------------

    /// `story_ids() -> set[str]` — the SORTED-UNIQUE set of story ids present in
    /// the library.
    pub fn story_ids(&self) -> BTreeSet<String> {
        self.stories.iter().map(|s| s.id.clone()).collect()
    }

    /// The catalog/discovery-order story id of the default story
    /// (`turnvale-murder`).
    pub fn default_story_id(&self) -> &'static str {
        DEFAULT_STORY_ID
    }

    /// `story_metadata(story_id) -> {id, title, description, story_brief, kind,
    /// world_ref?}` (see [`StoryEnvelope::metadata`]) — the PLAYER-facing catalog
    /// row (NO `seed`, NO `architect_*`).
    ///
    /// Returns [`UnknownStory`] for an id absent from the library. The leading
    /// keys keep the public catalog order (`id`, `title`, `description`,
    /// `story_brief`); `kind`/`world_ref` are the only additive keys.
    pub fn story_metadata(&self, story_id: &str) -> Result<Map<String, Value>, UnknownStory> {
        let env = self
            .find(story_id)
            .ok_or_else(|| UnknownStory(story_id.to_string()))?;
        Ok(env.metadata())
    }

    /// `list_stories() -> list[{id, title, description, story_brief, kind,
    /// world_ref?}]` — discovery order. Each element has the same PLAYER-facing
    /// shape as [`Self::story_metadata`] (NO `seed`, NO `architect_*`).
    pub fn list_stories(&self) -> Vec<Map<String, Value>> {
        self.stories.iter().map(|s| s.metadata()).collect()
    }

    /// Read the story's architect-chat state (`architect.json` in the package
    /// dir). Falls back to the LEGACY `meta.architect_*` keys for packages
    /// written before the story/chat split. `Ok(None)` when the story has no
    /// architect chat; `Err(StoryNotFound)` for an unknown id.
    pub fn get_architect_state(&self, story_id: &str) -> Result<Option<Value>, StoryStoreError> {
        let story_id = story_id.trim();
        let env = self
            .find(story_id)
            .ok_or_else(|| StoryStoreError::StoryNotFound(story_id.to_string()))?;
        let path = self.story_dir(story_id).join(ARCHITECT_FILE);
        match std::fs::read_to_string(&path) {
            Ok(raw) => {
                let value: Value = serde_json::from_str(&raw).map_err(|e| {
                    StoryStoreError::Io(format!("parse architect state {story_id}: {e}"))
                })?;
                Ok(Some(normalize_architect_state(value)))
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(legacy_architect_state(&env.meta))
            }
            Err(e) => Err(StoryStoreError::Io(format!(
                "read architect state {story_id}: {e}"
            ))),
        }
    }

    /// Purge every architect-chat artifact from the story PACKAGE after the
    /// conversation has been persisted to its real home (the dialogs SQLite,
    /// `DialogStore::set_architect_chat`): deletes a stray `architect.json`
    /// and strips the legacy `meta.architect_*` keys. Content-invariant —
    /// `version`/`updated_at` preserved; the in-memory cache entry follows suit.
    pub fn purge_architect_artifacts(&mut self, story_id: &str) -> Result<(), StoryStoreError> {
        let story_id = story_id.trim().to_string();
        {
            let _guard = self.write_lock.lock().expect("story write lock poisoned");
            self.find(&story_id)
                .ok_or_else(|| StoryStoreError::StoryNotFound(story_id.clone()))?;
            let file = self.story_dir(&story_id).join(ARCHITECT_FILE);
            if file.is_file() {
                std::fs::remove_file(&file).map_err(|e| {
                    StoryStoreError::Io(format!("remove architect {story_id}: {e}"))
                })?;
            }
        }

        let needs_strip = self
            .find(&story_id)
            .map(|env| {
                LEGACY_ARCHITECT_META_KEYS
                    .iter()
                    .any(|k| env.meta.contains_key(*k))
            })
            .unwrap_or(false);
        if needs_strip {
            let stripped_env = {
                let env = self
                    .find(&story_id)
                    .ok_or_else(|| StoryStoreError::StoryNotFound(story_id.clone()))?;
                let mut stripped = env.clone();
                for key in LEGACY_ARCHITECT_META_KEYS {
                    stripped.meta.remove(key);
                }
                stripped
            };
            {
                let _guard = self.write_lock.lock().expect("story write lock poisoned");
                self.write_envelope(&stripped_env)?;
            }
            if let Some(slot) = self.stories.iter_mut().find(|s| s.id == story_id) {
                *slot = stripped_env;
            }
        }
        Ok(())
    }

    /// `draft_row(story_id) -> {id, version, title, description, kind, world_ref?,
    /// seed}` — the GM-scoped plot DRAFT row for the story architect
    /// (`GET /stories/{id}/draft`, `§С1.3`). Unlike [`Self::story_metadata`] this
    /// carries the full `seed` (the plot, incl. the GM's `hidden_truth`). The
    /// architect CHAT state is NOT part of the row — it lives in `architect.json`
    /// ([`Self::get_architect_state`]).
    ///
    /// Guards MIRROR `update_story` / `resolve_story_architect_world` so a caller
    /// gets the SAME rejections the architect turn would (mapped to 400 by the
    /// server): unknown id -> [`StoryStoreError::StoryNotFound`]; a self-contained
    /// builtin (no `world_ref`) or a non-`authored` (procedural) story ->
    /// [`StoryStoreError::Invalid`]. Only world-bound authored stories are draftable.
    pub fn draft_row(&self, story_id: &str) -> Result<Map<String, Value>, StoryStoreError> {
        let story_id = story_id.trim();
        let env = self
            .find(story_id)
            .ok_or_else(|| StoryStoreError::StoryNotFound(story_id.to_string()))?;
        if env.world_ref.is_none() {
            return Err(StoryStoreError::Invalid(format!(
                "story {story_id} is self-contained (no world_ref) and cannot be edited by the architect"
            )));
        }
        if env.kind != "authored" {
            return Err(StoryStoreError::Invalid(
                "update_story: only world-bound authored stories are editable".to_string(),
            ));
        }
        Ok(env.draft_row())
    }

    /// `story_seed(story_id) -> dict` — an OWNED deep clone of the story's seed
    /// with `id`/`title` overwritten from the envelope (so every session gets an
    /// isolated world). Returns [`UnknownStory`] for an unknown id.
    pub fn seed(&self, story_id: &str) -> Result<Value, UnknownStory> {
        let env = self
            .find(story_id)
            .ok_or_else(|| UnknownStory(story_id.to_string()))?;
        Ok(env.seed_value())
    }

    /// `default_story_seed() -> dict` — `seed(DEFAULT_STORY_ID)`.
    pub fn default_seed(&self) -> Value {
        self.seed(DEFAULT_STORY_ID)
            .expect("DEFAULT_STORY_ID must be present in the story library")
    }

    // ------------------------------------------------------------------
    // Phase-4 public surface: kind + world_ref + plot, and creation.
    // ------------------------------------------------------------------

    /// The story's `kind` — `"authored"` (rich hand-written plot) or
    /// `"procedural"` (generated from its bound world on launch). Returns
    /// [`UnknownStory`] for an unknown id.
    pub fn kind(&self, story_id: &str) -> Result<String, UnknownStory> {
        self.find(story_id)
            .map(|e| e.kind.clone())
            .ok_or_else(|| UnknownStory(story_id.to_string()))
    }

    /// The story's bound-world reference, if any. `Ok(None)` means the story is
    /// self-contained (the three built-ins embed their world). Returns
    /// [`UnknownStory`] for an unknown id.
    pub fn world_ref(&self, story_id: &str) -> Result<Option<StoryWorldRef>, UnknownStory> {
        self.find(story_id)
            .map(|e| e.world_ref.clone())
            .ok_or_else(|| UnknownStory(story_id.to_string()))
    }

    /// Whether the story's world is embedded in its own package (self-contained).
    pub fn world_embedded(&self, story_id: &str) -> Result<bool, UnknownStory> {
        self.find(story_id)
            .map(|e| e.world_embedded)
            .ok_or_else(|| UnknownStory(story_id.to_string()))
    }

    /// The story's authored PLOT overlay — an OWNED deep clone of the seed with
    /// `id`/`title` overwritten from the envelope (same shape as [`Self::seed`]).
    /// For Phase-4 stories bound to a world this is the plot to overlay onto the
    /// resolved world bible; for self-contained stories it is the full seed.
    pub fn plot(&self, story_id: &str) -> Result<Value, UnknownStory> {
        self.seed(story_id)
    }

    /// The story's package `version`. Returns [`UnknownStory`] for an unknown id.
    pub fn version(&self, story_id: &str) -> Result<u64, UnknownStory> {
        self.find(story_id)
            .map(|e| e.version)
            .ok_or_else(|| UnknownStory(story_id.to_string()))
    }

    /// Create a new story package bound to a world (`docs/MODS_PACKAGES_TZ.md`
    /// Phase 4). Allocates a fresh story id, writes `story.json` atomically, and
    /// returns the created story's metadata (`{id, title, description,
    /// story_brief}`) plus the surrounding launch fields the caller needs.
    ///
    /// * `kind` MUST be `"procedural"` or `"authored"`.
    /// * `world_ref` binds the story to a world package — the CALLER must have
    ///   already validated that the world exists (no-fallback rule); this method
    ///   only writes the package.
    /// * `plot` is the authored plot overlay (ignored content-wise for a
    ///   procedural story beyond title/brief, but stored verbatim).
    ///
    /// The store's in-memory list is updated so the new story is immediately
    /// discoverable via [`Self::list_stories`] / [`Self::seed`] without a rescan.
    pub fn create_bound_story(
        &mut self,
        title: &str,
        description: &str,
        kind: &str,
        world_ref: StoryWorldRef,
        plot: Value,
    ) -> Result<Map<String, Value>, StoryStoreError> {
        let kind = kind.trim();
        if kind != "procedural" && kind != "authored" {
            return Err(StoryStoreError::Io(format!(
                "story kind must be \"procedural\" or \"authored\", got {kind:?}"
            )));
        }
        if world_ref.id.trim().is_empty() {
            return Err(StoryStoreError::Io(
                "create_bound_story: world_ref.id is required".to_string(),
            ));
        }
        let plot = match plot {
            Value::Object(_) => plot,
            Value::Null => Value::Object(Map::new()),
            _ => {
                return Err(StoryStoreError::Io(
                    "create_bound_story: plot must be an object".to_string(),
                ))
            }
        };
        let title = title.trim();
        if title.is_empty() {
            return Err(StoryStoreError::Io(
                "create_bound_story: title is required".to_string(),
            ));
        }

        let _guard = self.write_lock.lock().expect("story write lock poisoned");
        let id = self.allocate_story_id()?;
        // A created (world-bound) story stamps both timestamps; the architect
        // (`update_story`) refreshes `updated_at` per turn. `meta` starts empty.
        let now = now_timestamp();
        let env = StoryEnvelope {
            id: id.clone(),
            version: 1,
            kind: kind.to_string(),
            world_embedded: false,
            world_ref: Some(world_ref),
            title: title.to_string(),
            description: description.trim().to_string(),
            seed: plot,
            meta: Map::new(),
            created_at: now.clone(),
            updated_at: now,
        };
        self.write_envelope(&env)?;
        let meta = env.metadata();
        self.stories.push(env);
        Ok(meta)
    }

    /// `§С1.1`: shallow-merge a `patch` into an existing WORLD-BOUND story and
    /// re-persist it (draft-first story-architect edit path). Returns the full
    /// updated envelope object (the on-disk `story.json` shape) so the caller can
    /// echo it back to the UI.
    ///
    /// Patchable keys (anything else is IGNORED — `kind`/`world_ref`/
    /// `world_embedded` are NEVER patchable, they are pinned at creation):
    /// * `title` — non-blank string (a blank/whitespace title is rejected).
    /// * `description` — string (blank allowed; it is metadata).
    /// * `seed` — object, shallow-merged INTO the existing plot seed with
    ///   null-drop (a `null` value DELETES that key; mirrors `merge_world_payload`).
    /// * `meta` — object, shallow-merged INTO the existing meta with null-drop
    ///   (this is where the architect chat state lives).
    ///
    /// Bumps `version` (`saturating_add(1)`), refreshes `updated_at`, writes
    /// atomically, and updates the in-memory cache. `created_at` is left as-is
    /// (NOT back-filled: an architect-created story already has one, and a
    /// hand-dropped package intentionally without one keeps its blank shape —
    /// re-stamping it would be a surprising side effect of an edit).
    ///
    /// HARD RULE (`§С1.1`): the story architect edits ONLY world-bound authored
    /// stories. A self-contained builtin (no `world_ref`) is rejected with
    /// [`StoryStoreError::Invalid`] — it must not be rewritten by this path.
    /// Unknown id -> [`StoryStoreError::StoryNotFound`].
    pub fn update_story(
        &mut self,
        story_id: &str,
        patch: Value,
    ) -> Result<Map<String, Value>, StoryStoreError> {
        let story_id = story_id.trim();
        let patch = match patch {
            Value::Object(m) => m,
            Value::Null => Map::new(),
            _ => {
                return Err(StoryStoreError::Invalid(
                    "update_story: patch must be an object".to_string(),
                ))
            }
        };

        let _guard = self.write_lock.lock().expect("story write lock poisoned");
        let idx = self
            .stories
            .iter()
            .position(|s| s.id == story_id)
            .ok_or_else(|| StoryStoreError::StoryNotFound(story_id.to_string()))?;

        // Architect edits are world-bound only; a self-contained builtin is not
        // architect-editable (it embeds its own world).
        if self.stories[idx].world_ref.is_none() {
            return Err(StoryStoreError::Invalid(format!(
                "story {story_id} is self-contained (no world_ref) and cannot be edited by the architect"
            )));
        }
        // The architect only authors AUTHORED plots. A world-bound PROCEDURAL
        // story clears the builtin guard above, but its launch path ignores the
        // authored seed — folding a plot in here would be silent data loss.
        if self.stories[idx].kind != "authored" {
            return Err(StoryStoreError::Invalid(
                "update_story: only world-bound authored stories are editable".to_string(),
            ));
        }

        // Work on a clone so a validation failure leaves the cache untouched.
        let mut env = StoryEnvelope {
            id: self.stories[idx].id.clone(),
            version: self.stories[idx].version,
            kind: self.stories[idx].kind.clone(),
            world_embedded: self.stories[idx].world_embedded,
            world_ref: self.stories[idx].world_ref.clone(),
            title: self.stories[idx].title.clone(),
            description: self.stories[idx].description.clone(),
            seed: self.stories[idx].seed.clone(),
            meta: self.stories[idx].meta.clone(),
            created_at: self.stories[idx].created_at.clone(),
            updated_at: self.stories[idx].updated_at.clone(),
        };

        // title: non-blank string, null-drop (a null/absent title keeps the old).
        if let Some(v) = patch.get("title") {
            if !v.is_null() {
                let t = v.as_str().unwrap_or_default().trim();
                if t.is_empty() {
                    return Err(StoryStoreError::Invalid(
                        "update_story: title must be non-empty".to_string(),
                    ));
                }
                env.title = t.to_string();
            }
        }
        // description: string, null-drop; blank is allowed (it is optional metadata).
        if let Some(v) = patch.get("description") {
            if !v.is_null() {
                env.description = v.as_str().unwrap_or_default().trim().to_string();
            }
        }
        // seed: shallow-merge the plot with null-drop.
        if let Some(v) = patch.get("seed") {
            match v {
                Value::Object(seed_patch) => {
                    let mut base = match &env.seed {
                        Value::Object(m) => m.clone(),
                        _ => Map::new(),
                    };
                    shallow_merge_null_drop(&mut base, seed_patch);
                    env.seed = Value::Object(base);
                }
                Value::Null => {}
                _ => {
                    return Err(StoryStoreError::Invalid(
                        "update_story: seed patch must be an object".to_string(),
                    ))
                }
            }
        }
        // meta: shallow-merge with null-drop (architect chat state).
        if let Some(v) = patch.get("meta") {
            match v {
                Value::Object(meta_patch) => {
                    shallow_merge_null_drop(&mut env.meta, meta_patch);
                }
                Value::Null => {}
                _ => {
                    return Err(StoryStoreError::Invalid(
                        "update_story: meta patch must be an object".to_string(),
                    ))
                }
            }
        }

        env.version = env.version.saturating_add(1);
        env.updated_at = now_timestamp();
        self.write_envelope(&env)?;
        let response = env.to_file_value().as_object().cloned().unwrap_or_default();
        self.stories[idx] = env;
        Ok(response)
    }

    /// Delete a story package directory. Returns `true` when a package was
    /// removed, `false` when no story with that id existed.
    pub fn delete_story(&mut self, story_id: &str) -> Result<bool, StoryStoreError> {
        let story_id = story_id.trim();
        if story_id.is_empty() {
            return Ok(false);
        }
        let _guard = self.write_lock.lock().expect("story write lock poisoned");
        let dir = self.story_dir(story_id);
        if !dir.join(STORY_FILE).is_file() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|e| StoryStoreError::Io(format!("delete story {story_id}: {e}")))?;
        self.stories.retain(|s| s.id != story_id);
        Ok(true)
    }

    /// Allocate a unique story id (a urlsafe token not already on disk).
    fn allocate_story_id(&self) -> Result<String, StoryStoreError> {
        for _ in 0..32 {
            let id = story_token();
            if !self.story_file(&id).exists() {
                return Ok(id);
            }
        }
        Err(StoryStoreError::Io(
            "could not allocate unique story id".to_string(),
        ))
    }

    // ------------------------------------------------------------------
    // internals
    // ------------------------------------------------------------------

    fn find(&self, story_id: &str) -> Option<&StoryEnvelope> {
        self.stories.iter().find(|s| s.id == story_id)
    }

    /// Materialize any missing built-in default story packages from the embedded
    /// catalog. Idempotent: only writes packages whose `story.json` is absent, so
    /// a user-edited default is never clobbered.
    fn ensure_defaults(&self) -> Result<(), StoryStoreError> {
        let _guard = self.write_lock.lock().expect("story write lock poisoned");
        let defaults = embedded_default_envelopes()?;
        for env in &defaults {
            if self.story_file(&env.id).is_file() {
                continue;
            }
            self.write_envelope(env)?;
        }
        Ok(())
    }

    /// Scan `<root>/stories/` for `story.json` packages, in directory order.
    /// A package whose `story.json` cannot be read or parsed aborts the scan with
    /// [`StoryStoreError::Io`] (no silent fallback to a default).
    fn scan(&self) -> Result<Vec<StoryEnvelope>, StoryStoreError> {
        let dir = self.stories_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoryStoreError::Io(format!("read stories dir: {e}"))),
        };
        let mut names: Vec<String> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| StoryStoreError::Io(format!("read stories dir: {e}")))?;
            if !entry.path().join(STORY_FILE).is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
        // Deterministic discovery order: built-in defaults first (in embedded
        // catalog order), then any user-added (drop-in) stories alphabetically.
        let builtin_order = builtin_id_order();
        names.sort_by(|a, b| {
            let ia = builtin_order.iter().position(|x| x == a);
            let ib = builtin_order.iter().position(|x| x == b);
            match (ia, ib) {
                (Some(ia), Some(ib)) => ia.cmp(&ib),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(b),
            }
        });
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            out.push(self.read_envelope(&name)?);
        }
        Ok(out)
    }

    fn read_envelope(&self, story_id: &str) -> Result<StoryEnvelope, StoryStoreError> {
        let path = self.story_file(story_id);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| StoryStoreError::Io(format!("read story {story_id}: {e}")))?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|e| StoryStoreError::Io(format!("parse story {story_id}: {e}")))?;
        StoryEnvelope::from_value(story_id, value)
    }

    /// Atomic write: serialize to a temp file in the story's directory, then
    /// `rename` over `story.json`.
    fn write_envelope(&self, env: &StoryEnvelope) -> Result<(), StoryStoreError> {
        let dir = self.story_dir(&env.id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| StoryStoreError::Io(format!("create story dir {}: {e}", env.id)))?;
        let body = serde_json::to_vec(&env.to_file_value())
            .map_err(|e| StoryStoreError::Io(format!("serialize story {}: {e}", env.id)))?;
        let final_path = dir.join(STORY_FILE);
        let tmp_path = dir.join(format!(".{STORY_FILE}.{}.tmp", unique_suffix()));
        std::fs::write(&tmp_path, &body)
            .map_err(|e| StoryStoreError::Io(format!("write temp story {}: {e}", env.id)))?;
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(StoryStoreError::Io(format!("rename story {}: {e}", env.id)));
        }
        Ok(())
    }
}

/// In-memory view of one `story.json`. `seed` is the exact legacy catalog seed
/// object; the surrounding fields are the package envelope.
#[derive(Clone)]
struct StoryEnvelope {
    id: String,
    version: u64,
    kind: String,
    world_embedded: bool,
    /// Phase-4: the world package this story is bound to (`None` for the
    /// self-contained built-ins, which embed their world in the seed).
    world_ref: Option<StoryWorldRef>,
    title: String,
    description: String,
    /// The raw seed object exactly as stored (key order preserved).
    ///
    /// For self-contained authored stories this is the full legacy catalog seed.
    /// For Phase-4 stories bound to a world via `world_ref`, it carries the
    /// authored PLOT overlay (`player_character`, `hidden_truth`, `scene`, …) —
    /// the world bible is resolved from `world_ref` at launch.
    seed: Value,
    /// `§С1.1`: opaque round-trip object carrying the story-architect chat state.
    /// Always an object in memory (empty when the story has no architect state);
    /// EMITTED to disk only when NON-EMPTY, so the built-in packages' bytes never
    /// change. Never merged into `seed`.
    meta: Map<String, Value>,
    /// `§С1.1`: package creation / last-update timestamps (SQLite-`datetime('now')`
    /// shape). Parsed with a blank default and EMITTED only when non-blank, so
    /// the built-ins (which carry neither) keep their exact byte shape.
    created_at: String,
    updated_at: String,
}

impl StoryEnvelope {
    /// Build an envelope from a parsed `story.json`, using `id` (the folder name)
    /// as the authoritative id.
    fn from_value(id: &str, value: Value) -> Result<Self, StoryStoreError> {
        let obj = value.as_object().ok_or_else(|| {
            StoryStoreError::Io(format!("story {id}: story.json is not an object"))
        })?;
        let world_ref = parse_world_ref(obj.get("world_ref"));
        // The seed (authored plot for Phase-4 stories) is REQUIRED for a
        // self-contained story (no world_ref). A story bound to a world may
        // legitimately carry a minimal/empty plot, so its `seed` defaults to an
        // empty object when absent.
        let seed = match obj.get("seed").cloned() {
            Some(seed) => {
                if !seed.is_object() {
                    return Err(StoryStoreError::Io(format!(
                        "story {id}: seed is not an object"
                    )));
                }
                seed
            }
            None if world_ref.is_some() => Value::Object(Map::new()),
            None => {
                return Err(StoryStoreError::Io(format!(
                    "story {id}: missing seed (a self-contained story must carry a seed)"
                )))
            }
        };
        let version = obj.get("version").and_then(Value::as_u64).unwrap_or(1);
        let kind = obj
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("authored")
            .to_string();
        let world_embedded = obj
            .get("world_embedded")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let title = obj
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let description = obj
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // `meta` is opaque and optional; a non-object (or absent) value reads as
        // the empty map so the in-memory shape is always an object.
        let meta = match obj.get("meta") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        // Timestamps parse-with-default: a missing/blank value stays "" and is not
        // re-emitted (keeps the built-in bytes identical).
        let created_at = obj
            .get("created_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let updated_at = obj
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        Ok(StoryEnvelope {
            id: id.to_string(),
            version,
            kind,
            world_embedded,
            world_ref,
            title,
            description,
            seed,
            meta,
            created_at,
            updated_at,
        })
    }

    /// `{id, title, description, story_brief, kind, world_ref?, has_pc, pc?}` —
    /// the PLAYER-facing catalog row (`GET /stories`). The leading four keys keep
    /// the legacy public catalog shape (`story_brief` is the seed's `story_brief`,
    /// else `public_intro`); `kind` (always), `world_ref` (when present), `has_pc`
    /// (always) and `pc` (when public material survives) are the ONLY additive
    /// keys — just enough for the front-end to gate the "✎ edit" affordance
    /// (`§С1.3`) and for the new-game wizard to present an authored story's own
    /// protagonist.
    ///
    /// `has_pc` is a NON-SECRET boolean (does the seed carry an authored
    /// protagonist?); `pc` is the protagonist reduced to [`PC_PUBLIC_FIELDS`] —
    /// the `hidden_truth`/mystery solution stays omitted with the rest of the
    /// seed, and the sheet's `gm_notes`/stat blocks never pass the whitelist.
    /// This row is DELIBERATELY minimal: it carries NO `seed` and NO
    /// `architect_*` chat state, because the catalog is loaded at app start
    /// for every player and the `seed` holds GM-only secrets (e.g. `hidden_truth`,
    /// the mystery solutions). The GM-scoped plot draft + chat state come from
    /// [`Self::draft_row`] via `GET /stories/{id}/draft` instead.
    fn metadata(&self) -> Map<String, Value> {
        let mut meta = Map::new();
        meta.insert("id".to_string(), Value::String(self.id.clone()));
        meta.insert("title".to_string(), Value::String(self.title.clone()));
        meta.insert(
            "description".to_string(),
            Value::String(self.description.clone()),
        );
        let brief = seed_str(&self.seed, "story_brief");
        let story_brief = if !brief.is_empty() {
            brief
        } else {
            seed_str(&self.seed, "public_intro")
        };
        meta.insert("story_brief".to_string(), Value::String(story_brief));
        meta.insert("kind".to_string(), Value::String(self.kind.clone()));
        if let Some(world_ref) = &self.world_ref {
            meta.insert(
                "world_ref".to_string(),
                json!({ "id": world_ref.id, "version": world_ref.version }),
            );
        }
        meta.insert("has_pc".to_string(), Value::Bool(seed_has_pc(&self.seed)));
        if let Some(pc) = seed_pc_public(&self.seed) {
            meta.insert("pc".to_string(), Value::Object(pc));
        }
        meta
    }

    /// `{id, version, title, description, kind, world_ref?, seed}` — the
    /// GM-scoped DRAFT row (`GET /stories/{id}/draft`, `§С1.3`). Unlike
    /// [`Self::metadata`] this DOES carry the full `seed` (the plot, incl. the
    /// GM's `hidden_truth`). It is NEVER emitted in the player-facing catalog.
    /// The architect CHAT state is NOT flattened here anymore — it lives in the
    /// package's `architect.json` (`StoryStore::get_architect_state`).
    ///
    /// `seed` is the plot with `id`/`title` overwritten from the envelope (same
    /// shape as [`Self::seed`] / `plot()`).
    fn draft_row(&self) -> Map<String, Value> {
        let mut row = Map::new();
        row.insert("id".to_string(), Value::String(self.id.clone()));
        row.insert("version".to_string(), Value::Number(self.version.into()));
        row.insert("title".to_string(), Value::String(self.title.clone()));
        row.insert(
            "description".to_string(),
            Value::String(self.description.clone()),
        );
        row.insert("kind".to_string(), Value::String(self.kind.clone()));
        if let Some(world_ref) = &self.world_ref {
            row.insert(
                "world_ref".to_string(),
                json!({ "id": world_ref.id, "version": world_ref.version }),
            );
        }
        // The plot draft (for the architect form): the seed with id/title
        // overwritten from the envelope, same shape as `seed()` / `plot()`.
        row.insert("seed".to_string(), self.seed_value());
        row
    }

    /// An OWNED deep clone of the seed with `id`/`title` overwritten from the
    /// envelope. Re-inserting existing keys keeps their position (IndexMap), so
    /// `id`/`title` stay the first two keys.
    fn seed_value(&self) -> Value {
        let mut seed = self.seed.clone();
        if let Value::Object(map) = &mut seed {
            map.insert("id".to_string(), Value::String(self.id.clone()));
            map.insert("title".to_string(), Value::String(self.title.clone()));
        }
        seed
    }

    /// The on-disk `story.json` value (envelope + seed). Trailing/optional keys
    /// (`world_ref`, `created_at`, `updated_at`, `meta`) are emitted ONLY when
    /// they carry content, so the self-contained built-ins keep their EXACT byte
    /// shape (`§С1.1` invariant, mirrored on the `package_ref_tests` pattern).
    /// `meta` is written LAST so an added key never shifts an existing one.
    fn to_file_value(&self) -> Value {
        let mut m = Map::new();
        m.insert("format".into(), json!(STORY_FORMAT));
        m.insert("id".into(), json!(self.id));
        m.insert("version".into(), json!(self.version));
        m.insert("kind".into(), json!(self.kind));
        if let Some(world_ref) = &self.world_ref {
            m.insert(
                "world_ref".into(),
                json!({ "id": world_ref.id, "version": world_ref.version }),
            );
        }
        m.insert("world_embedded".into(), json!(self.world_embedded));
        m.insert("title".into(), json!(self.title));
        m.insert("description".into(), json!(self.description));
        if !self.created_at.is_empty() {
            m.insert("created_at".into(), json!(self.created_at));
        }
        if !self.updated_at.is_empty() {
            m.insert("updated_at".into(), json!(self.updated_at));
        }
        m.insert("seed".into(), self.seed.clone());
        // meta LAST + only when non-empty — additive, byte-safe for builtins.
        if !self.meta.is_empty() {
            m.insert("meta".into(), Value::Object(self.meta.clone()));
        }
        Value::Object(m)
    }
}

/// Parse a `world_ref` object `{id, version}` into a [`StoryWorldRef`]. Returns
/// `None` when absent or when the id is missing/blank. `version` accepts an
/// integer (the on-disk form); a missing/non-integer version means unpinned
/// (`0`).
fn parse_world_ref(v: Option<&Value>) -> Option<StoryWorldRef> {
    let obj = v?.as_object()?;
    let id = obj.get("id").and_then(Value::as_str).unwrap_or_default();
    if id.trim().is_empty() {
        return None;
    }
    let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
    Some(StoryWorldRef {
        id: id.trim().to_string(),
        version,
    })
}

/// Shallow-merge `patch` into `base` with NULL-DROP semantics (mirrors
/// `merge_world_payload`): a `null` patch value DELETES that key from `base`;
/// any other value overwrites (or inserts). Only the top level is merged — a
/// nested object value fully replaces the base's value for that key.
fn shallow_merge_null_drop(base: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, value) in patch {
        if value.is_null() {
            base.remove(key);
        } else {
            base.insert(key.clone(), value.clone());
        }
    }
}

/// Read a string field from `seed[key]`, defaulting to `""`.
fn seed_str(seed: &Value, key: &str) -> String {
    seed.as_object()
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The seed's authored protagonist object under `player_character` (or the
/// legacy `player` alias). FIRST PRESENT key wins — no fall-through when it is
/// present but empty/non-object — mirroring the server's `story_plot_has_pc`
/// launch gate exactly, so `has_pc`, the `pc` summary and the gate can never
/// disagree on one seed.
fn seed_pc(seed: &Value) -> Option<&Map<String, Value>> {
    let obj = seed.as_object()?;
    let pc = obj.get("player_character").or_else(|| obj.get("player"))?;
    pc.as_object().filter(|m| !m.is_empty())
}

/// Whether the seed carries an authored protagonist — a non-empty
/// `player_character` (or legacy `player`) object, per [`seed_pc`] (the shared
/// launch-gate lookup). A NON-SECRET boolean, safe for the player-facing
/// catalog.
fn seed_has_pc(seed: &Value) -> bool {
    seed_pc(seed).is_some()
}

/// The PUBLIC protagonist fields allowed into the catalog row's `pc` object — a
/// WHITELIST (mirrors the character architect's base blocks): the catalog is
/// player-facing, so the sheet's `gm_notes` and ANY ad-hoc GM key the story
/// architect may fold into `player_character` stay private until deliberately
/// added here. Stat blocks / inventories are also out — the row presents the
/// hero, it does not replace the in-game sheet.
const PC_PUBLIC_FIELDS: [&str; 12] = [
    "name",
    "pronouns",
    "class_role",
    "level",
    "background",
    "age",
    "physical_type",
    "distinctive_features",
    "current_appearance",
    "personality",
    "values",
    "condition",
];

/// The seed's authored protagonist ([`seed_pc`] — the same lookup behind
/// [`seed_has_pc`]) reduced to [`PC_PUBLIC_FIELDS`]: blank strings dropped,
/// objects/arrays never pass. `None` when the seed has no PC or nothing public
/// survives — the catalog row then simply omits `pc`.
fn seed_pc_public(seed: &Value) -> Option<Map<String, Value>> {
    let pc = seed_pc(seed)?;
    let mut public = Map::new();
    for key in PC_PUBLIC_FIELDS {
        if let Some(v) = pc.get(key) {
            let keep = match v {
                Value::String(s) => !s.trim().is_empty(),
                Value::Number(_) | Value::Bool(_) => true,
                Value::Null | Value::Object(_) | Value::Array(_) => false,
            };
            if keep {
                public.insert(key.to_string(), v.clone());
            }
        }
    }
    if public.is_empty() {
        None
    } else {
        Some(public)
    }
}

/// The id order of the embedded built-in catalog. Defines the default story
/// discovery order so the `/stories` list stays stable regardless of how the
/// filesystem enumerates the package directories.
fn builtin_id_order() -> Vec<String> {
    embedded_default_envelopes()
        .map(|envs| envs.into_iter().map(|e| e.id).collect())
        .unwrap_or_default()
}

/// Parse the embedded `catalog.json` into default story envelopes. This is the
/// ONLY consumer of the embedded catalog — it exists solely to materialize the
/// built-in default packages on first run.
fn embedded_default_envelopes() -> Result<Vec<StoryEnvelope>, StoryStoreError> {
    let parsed: Value = serde_json::from_str(CATALOG_JSON)
        .map_err(|e| StoryStoreError::Io(format!("embedded catalog.json invalid: {e}")))?;
    let items = match parsed {
        Value::Array(items) => items,
        _ => {
            return Err(StoryStoreError::Io(
                "embedded catalog.json must be a JSON array".to_string(),
            ))
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for entry in items {
        let obj = entry.as_object().ok_or_else(|| {
            StoryStoreError::Io("embedded catalog entry must be an object".to_string())
        })?;
        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let title = obj
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let description = obj
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let seed = obj.get("seed").cloned().unwrap_or(Value::Null);
        out.push(StoryEnvelope {
            id,
            version: 1,
            kind: "authored".to_string(),
            world_embedded: true,
            world_ref: None,
            title,
            description,
            seed,
            // Built-ins carry NO meta and NO timestamps — to_file_value emits
            // neither, so materialized builtin bytes are byte-for-byte unchanged.
            meta: Map::new(),
            created_at: String::new(),
            updated_at: String::new(),
        });
    }
    Ok(out)
}

/// A UTC timestamp in SQLite `datetime('now')` shape (`"YYYY-MM-DD HH:MM:SS"`),
/// matching the world/character stores' timestamps. Computed from the wall clock
/// via a plain civil-date conversion (no extra dependency): gml-stories has no
/// rusqlite/chrono, and the value is a package annotation, not a byte-gated field.
/// Normalize an architect-state value to the canonical shape:
/// `{messages: [...], model_history: [...], cache_session_id?, cache_thread_id?}`.
/// Unknown keys are dropped; missing arrays become empty.
fn normalize_architect_state(state: Value) -> Value {
    let map = match state {
        Value::Object(m) => m,
        _ => Map::new(),
    };
    let arr = |key: &str| -> Value {
        map.get(key)
            .and_then(Value::as_array)
            .cloned()
            .map(Value::Array)
            .unwrap_or_else(|| Value::Array(Vec::new()))
    };
    let mut out = Map::new();
    out.insert("messages".into(), arr("messages"));
    out.insert("model_history".into(), arr("model_history"));
    for key in ["cache_session_id", "cache_thread_id"] {
        if let Some(id) = map.get(key).and_then(Value::as_str) {
            let id = id.trim();
            if !id.is_empty() {
                out.insert(key.into(), Value::String(id.to_string()));
            }
        }
    }
    Value::Object(out)
}

/// Extract the LEGACY `meta.architect_*` chat state (pre-split packages) as a
/// canonical architect-state value. `None` when the meta carries none.
fn legacy_architect_state(meta: &Map<String, Value>) -> Option<Value> {
    if !LEGACY_ARCHITECT_META_KEYS
        .iter()
        .any(|k| meta.contains_key(*k))
    {
        return None;
    }
    let mut state = Map::new();
    if let Some(v) = meta.get("architect_messages") {
        state.insert("messages".into(), v.clone());
    }
    if let Some(v) = meta.get("architect_model_history") {
        state.insert("model_history".into(), v.clone());
    }
    if let Some(v) = meta.get("architect_cache_session_id") {
        state.insert("cache_session_id".into(), v.clone());
    }
    if let Some(v) = meta.get("architect_cache_thread_id") {
        state.insert("cache_thread_id".into(), v.clone());
    }
    Some(normalize_architect_state(Value::Object(state)))
}

fn now_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let tod = (secs % 86_400) as i64;
    let (hour, minute, second) = (tod / 3600, (tod % 3600) / 60, tod % 60);
    // Civil date from a day count since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// A unique suffix for atomic temp files (process id + a monotonic counter).
fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}", std::process::id(), n)
}

/// A urlsafe, collision-resistant story id. Mixes the wall clock (nanos), the
/// process id, and a monotonic counter through a small avalanche hash, then
/// base36-encodes it — no extra crypto dependency, and `allocate_story_id`
/// re-rolls on the (astronomically unlikely) on-disk collision anyway.
fn story_token() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = nanos
        ^ (u64::from(std::process::id()).wrapping_mul(0x9E37_79B9_7F4A_7C15))
        ^ n.wrapping_mul(0xD1B5_4A32_D192_ED03);
    // splitmix64 finalizer for a good avalanche.
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    let mut s = String::with_capacity(13);
    s.push_str("story-");
    let mut v = x;
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if v == 0 {
        s.push('0');
    }
    while v > 0 {
        s.push(ALPHABET[(v % 36) as usize] as char);
        v /= 36;
    }
    s
}

/// Best-effort absolutization that does not require the path to exist yet.
fn abspath(path: PathBuf) -> PathBuf {
    if let Ok(canon) = std::fs::canonicalize(&path) {
        let s = canon.to_string_lossy();
        let stripped = s.strip_prefix(r"\\?\").unwrap_or(&s).to_string();
        return PathBuf::from(stripped);
    }
    if path.is_absolute() {
        return path;
    }
    if let Ok(cwd) = std::env::current_dir() {
        return cwd.join(path);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> (tempfile::TempDir, StoryStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = StoryStore::new(dir.path()).expect("open store");
        (dir, store)
    }

    #[test]
    fn builtins_expose_kind_and_no_world_ref() {
        let (_dir, store) = temp_store();
        for id in store.story_ids() {
            assert_eq!(store.kind(&id).unwrap(), "authored");
            assert_eq!(store.world_ref(&id).unwrap(), None);
            assert!(store.world_embedded(&id).unwrap());
        }
    }

    #[test]
    fn create_bound_story_persists_and_exposes_world_ref() {
        let (_dir, mut store) = temp_store();
        let before = store.story_ids().len();
        let meta = store
            .create_bound_story(
                "Связанная история",
                "Описание",
                "authored",
                StoryWorldRef {
                    id: "some-world".to_string(),
                    version: 3,
                },
                json!({"hidden_truth": "тайна", "story_brief": "кратко"}),
            )
            .expect("create");
        let id = meta.get("id").and_then(Value::as_str).unwrap().to_string();

        // It is immediately discoverable without a rescan.
        assert_eq!(store.story_ids().len(), before + 1);
        assert_eq!(store.kind(&id).unwrap(), "authored");
        assert_eq!(
            store.world_ref(&id).unwrap(),
            Some(StoryWorldRef {
                id: "some-world".to_string(),
                version: 3,
            })
        );
        assert!(!store.world_embedded(&id).unwrap());
        assert_eq!(
            store
                .plot(&id)
                .unwrap()
                .get("hidden_truth")
                .and_then(Value::as_str),
            Some("тайна")
        );

        // A fresh store over the SAME root re-scans the package from disk.
        let reopened = StoryStore::new(_dir.path()).expect("reopen");
        assert_eq!(
            reopened.world_ref(&id).unwrap(),
            Some(StoryWorldRef {
                id: "some-world".to_string(),
                version: 3,
            })
        );
    }

    /// The catalog row's `pc` is a strict PUBLIC whitelist of the seed's
    /// protagonist: presentation fields pass, `gm_notes` (GM-only) and the
    /// mechanical blocks (abilities/inventory) never do, and a PC-less story
    /// omits the key entirely.
    #[test]
    fn catalog_row_pc_is_a_public_whitelist() {
        let (_dir, mut store) = temp_store();
        let with_pc = store
            .create_bound_story(
                "С протагонистом",
                "",
                "authored",
                StoryWorldRef {
                    id: "w".to_string(),
                    version: 1,
                },
                json!({
                    "hidden_truth": "тайна",
                    "player_character": {
                        "name": "Дарра",
                        "class_role": "сыщица",
                        "level": 2,
                        "background": "вольная сыщица",
                        "current_appearance": "в промокшем дорожном плаще",
                        "gm_notes": "секрет мастера",
                        "abilities": {"STR": 9},
                        "inventory": ["кинжал"],
                        "condition": "   "
                    }
                }),
            )
            .expect("create with pc");
        let without_pc = store
            .create_bound_story(
                "Без протагониста",
                "",
                "authored",
                StoryWorldRef {
                    id: "w".to_string(),
                    version: 1,
                },
                json!({"story_brief": "кратко"}),
            )
            .expect("create without pc");

        let rows = store.list_stories();
        let row = |meta: &Map<String, Value>| {
            let id = meta.get("id").and_then(Value::as_str).unwrap();
            rows.iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(id))
                .cloned()
                .unwrap()
        };

        let with = row(&with_pc);
        assert_eq!(with.get("has_pc"), Some(&Value::Bool(true)));
        let pc = with
            .get("pc")
            .and_then(Value::as_object)
            .expect("pc object");
        assert_eq!(pc.get("name"), Some(&json!("Дарра")));
        assert_eq!(pc.get("class_role"), Some(&json!("сыщица")));
        assert_eq!(pc.get("level"), Some(&json!(2)));
        assert_eq!(
            pc.get("current_appearance"),
            Some(&json!("в промокшем дорожном плаще"))
        );
        // GM-only and mechanical fields never pass; blank strings are dropped.
        for hidden in ["gm_notes", "abilities", "inventory", "condition"] {
            assert!(pc.get(hidden).is_none(), "{hidden} must not leak");
        }

        let without = row(&without_pc);
        assert_eq!(without.get("has_pc"), Some(&Value::Bool(false)));
        assert!(
            without.get("pc").is_none(),
            "no pc key without a protagonist"
        );
    }

    /// `has_pc`, `pc` and the server's launch gate share ONE lookup: the first
    /// PRESENT of `player_character`/legacy `player` wins, with no fall-through
    /// when it is empty or non-object. The divergent both-keys shapes (reachable
    /// via update_story's shallow seed merge on a legacy package) must resolve
    /// identically everywhere — never "has_pc=true but no pc / launch 400s".
    #[test]
    fn pc_lookup_matches_launch_gate_on_divergent_seeds() {
        // Empty player_character shadows a populated legacy player: gate says
        // NO protagonist, so the catalog must too.
        let shadowed = json!({"player_character": {}, "player": {"name": "X"}});
        assert!(!seed_has_pc(&shadowed));
        assert!(seed_pc_public(&shadowed).is_none());

        // Non-object player_character shadows the legacy key the same way.
        let non_object = json!({"player_character": "Дарра", "player": {"name": "X"}});
        assert!(!seed_has_pc(&non_object));
        assert!(seed_pc_public(&non_object).is_none());

        // The legacy alias alone still counts.
        let legacy = json!({"player": {"name": "X"}});
        assert!(seed_has_pc(&legacy));
        assert_eq!(
            seed_pc_public(&legacy).and_then(|m| m.get("name").cloned()),
            Some(json!("X"))
        );
    }

    #[test]
    fn create_bound_story_rejects_bad_kind() {
        let (_dir, mut store) = temp_store();
        let err = store.create_bound_story(
            "t",
            "",
            "weird",
            StoryWorldRef {
                id: "w".to_string(),
                version: 1,
            },
            json!({}),
        );
        assert!(err.is_err());
    }

    #[test]
    fn deleting_a_builtin_then_reopening_resurrects_it() {
        // Documents + locks the actual behavior: a built-in default story that is
        // deleted from disk is RE-MATERIALIZED by `ensure_defaults` the next time
        // a StoryStore is opened over the same root (the embedded catalog is the
        // source for the three defaults, and ensure_defaults only writes packages
        // whose story.json is absent). This is intentional — the built-ins are
        // always available — not a bug.
        let dir = tempfile::tempdir().expect("tempdir");
        let builtin = DEFAULT_STORY_ID;

        {
            let mut store = StoryStore::new(dir.path()).expect("open store");
            assert!(store.story_ids().contains(builtin));
            assert!(store.delete_story(builtin).expect("delete builtin"));
            assert!(
                !store.story_ids().contains(builtin),
                "deleted builtin is gone from the live list"
            );
            assert!(
                !store.story_exists(builtin),
                "deleted builtin's package is gone from disk"
            );
        }

        // Reopen the SAME root: ensure_defaults resurrects the deleted builtin.
        let reopened = StoryStore::new(dir.path()).expect("reopen store");
        assert!(
            reopened.story_ids().contains(builtin),
            "ensure_defaults must resurrect a deleted builtin on reopen"
        );
        assert!(reopened.story_exists(builtin));
        // The full builtin set is back.
        assert_eq!(reopened.story_ids().len(), 3);
    }

    /// The materialized bytes of every built-in `story.json` must be UNCHANGED
    /// by the meta/timestamp additions: builtins carry no meta and no timestamps,
    /// so `to_file_value` emits neither. This locks the `§С1.1` byte invariant at
    /// the envelope level (the seed-length gate in `lib.rs` locks the seed body).
    #[test]
    fn builtin_story_json_bytes_have_no_meta_or_timestamps() {
        let (_dir, store) = temp_store();
        for id in store.story_ids() {
            let raw = std::fs::read_to_string(store.story_file(&id)).expect("read builtin");
            let value: Value = serde_json::from_str(&raw).expect("parse builtin");
            let obj = value.as_object().expect("object");
            assert!(!obj.contains_key("meta"), "{id}: builtin must emit no meta");
            assert!(
                !obj.contains_key("created_at"),
                "{id}: builtin must emit no created_at"
            );
            assert!(
                !obj.contains_key("updated_at"),
                "{id}: builtin must emit no updated_at"
            );
            assert!(
                !obj.contains_key("world_ref"),
                "{id}: builtin must emit no world_ref"
            );
        }
    }

    #[test]
    fn meta_and_timestamps_round_trip_through_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let id = {
            let mut store = StoryStore::new(dir.path()).expect("open store");
            let meta = store
                .create_bound_story(
                    "История",
                    "",
                    "authored",
                    StoryWorldRef {
                        id: "w".to_string(),
                        version: 1,
                    },
                    json!({}),
                )
                .expect("create");
            let id = meta.get("id").and_then(Value::as_str).unwrap().to_string();
            store
                .update_story(
                    &id,
                    json!({"meta": {"architect_cache_session_id": "story:sess"}}),
                )
                .expect("update meta");
            id
        };
        // Reopen over the same root: the meta + timestamps survive the disk round
        // trip (they are emitted because they now carry content).
        let reopened = StoryStore::new(dir.path()).expect("reopen");
        let raw =
            std::fs::read_to_string(reopened.story_file(&id)).expect("read created story.json");
        let obj: Value = serde_json::from_str(&raw).expect("parse");
        assert_eq!(
            obj["meta"]["architect_cache_session_id"], "story:sess",
            "meta persisted"
        );
        assert!(
            obj.get("created_at").and_then(Value::as_str).is_some(),
            "created_at persisted"
        );
        assert!(
            obj.get("updated_at").and_then(Value::as_str).is_some(),
            "updated_at persisted"
        );
        // meta is written LAST (after seed) so it never shifts an existing key.
        let keys: Vec<&String> = obj.as_object().unwrap().keys().collect();
        assert_eq!(keys.last().map(|s| s.as_str()), Some("meta"));
        assert!(keys.iter().position(|k| *k == "seed").unwrap() < keys.len() - 1);
    }

    #[test]
    fn update_story_merges_seed_and_meta_with_null_drop_and_bumps_version() {
        let (_dir, mut store) = temp_store();
        let created = store
            .create_bound_story(
                "Черновая",
                "исходное",
                "authored",
                StoryWorldRef {
                    id: "w".to_string(),
                    version: 2,
                },
                json!({"story_brief": "старт", "hidden_truth": "тайна"}),
            )
            .expect("create");
        let id = created
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert_eq!(store.version(&id).unwrap(), 1);

        // Patch: overwrite title, merge into seed (add public_intro, DROP
        // hidden_truth via null), and set a meta key.
        let updated = store
            .update_story(
                &id,
                json!({
                    "title": "Готовая",
                    "seed": {"public_intro": "интро", "hidden_truth": null},
                    "meta": {"architect_messages": [{"role": "user", "content": "привет"}]}
                }),
            )
            .expect("update");

        // Version bumped; title patched; the response is the full envelope shape.
        assert_eq!(updated["version"], 2);
        assert_eq!(store.version(&id).unwrap(), 2);
        assert_eq!(updated["title"], "Готовая");
        assert_eq!(updated["kind"], "authored");
        assert_eq!(updated["world_ref"]["id"], "w");
        assert_eq!(updated["world_ref"]["version"], 2);
        // seed shallow-merged: story_brief kept, public_intro added, hidden_truth
        // dropped by the explicit null.
        let seed = store.plot(&id).unwrap();
        assert_eq!(seed["story_brief"], "старт");
        assert_eq!(seed["public_intro"], "интро");
        assert!(seed.get("hidden_truth").is_none(), "null-drop removed key");
        // meta carried the architect state.
        assert_eq!(
            updated["meta"]["architect_messages"][0]["content"],
            "привет"
        );

        // Cache updated in place (no reload needed): a fresh read sees the patch.
        let meta = store.story_metadata(&id).unwrap();
        assert_eq!(meta.get("title").and_then(Value::as_str), Some("Готовая"));
    }

    #[test]
    fn update_story_unknown_id_errors() {
        let (_dir, mut store) = temp_store();
        let err = store
            .update_story("nope", json!({"title": "x"}))
            .unwrap_err();
        assert_eq!(err, StoryStoreError::StoryNotFound("nope".to_string()));
    }

    #[test]
    fn update_story_rejects_self_contained_builtin() {
        let (_dir, mut store) = temp_store();
        // The three built-ins are self-contained (no world_ref) — the architect
        // may not edit them.
        let err = store
            .update_story(DEFAULT_STORY_ID, json!({"title": "x"}))
            .unwrap_err();
        match err {
            StoryStoreError::Invalid(msg) => assert!(msg.contains("self-contained")),
            other => panic!("expected Invalid, got {other:?}"),
        }
        // The builtin is untouched (version still 1, title unchanged).
        assert_eq!(store.version(DEFAULT_STORY_ID).unwrap(), 1);
    }

    #[test]
    fn update_story_rejects_world_bound_procedural() {
        let (_dir, mut store) = temp_store();
        // A PROCEDURAL story clears the builtin (world_ref) guard, but its launch
        // path ignores an authored seed — the architect must not fold a plot in.
        let created = store
            .create_bound_story(
                "Процедурная",
                "",
                "procedural",
                StoryWorldRef {
                    id: "w".to_string(),
                    version: 1,
                },
                json!({}),
            )
            .expect("create");
        let id = created
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let err = store.update_story(&id, json!({"title": "x"})).unwrap_err();
        match err {
            StoryStoreError::Invalid(msg) => assert!(msg.contains("authored")),
            other => panic!("expected Invalid, got {other:?}"),
        }
        // The procedural story is untouched (version still 1).
        assert_eq!(store.version(&id).unwrap(), 1);
    }

    #[test]
    fn update_story_rejects_blank_title() {
        let (_dir, mut store) = temp_store();
        let created = store
            .create_bound_story(
                "Имя",
                "",
                "authored",
                StoryWorldRef {
                    id: "w".to_string(),
                    version: 1,
                },
                json!({}),
            )
            .expect("create");
        let id = created
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        let err = store
            .update_story(&id, json!({"title": "   "}))
            .unwrap_err();
        match err {
            StoryStoreError::Invalid(msg) => assert!(msg.contains("title")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn draft_row_carries_seed_and_architect_state_for_authored_bound_story() {
        let (_dir, mut store) = temp_store();
        let created = store
            .create_bound_story(
                "Черновая",
                "исходное",
                "authored",
                StoryWorldRef {
                    id: "w".to_string(),
                    version: 2,
                },
                json!({"story_brief": "старт", "hidden_truth": "тайна GM"}),
            )
            .expect("create");
        let id = created
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        // Fold in some architect chat state via a normal update (meta path).
        store
            .update_story(
                &id,
                json!({
                    "seed": {"public_intro": "интро"},
                    "meta": {
                        "architect_messages": [{"role": "user", "content": "привет"}],
                        "architect_model_history": [{"role": "user", "content": "привет"}],
                        "architect_cache_session_id": "story-architect:sess",
                        "architect_cache_thread_id": "story-architect:thread"
                    }
                }),
            )
            .expect("update");

        let row = store.draft_row(&id).expect("draft row");
        // GM-scoped: the full plot seed IS present (incl. the hidden_truth secret).
        assert_eq!(row.get("id").and_then(Value::as_str), Some(id.as_str()));
        assert_eq!(row.get("version").and_then(Value::as_u64), Some(2));
        assert_eq!(row.get("kind").and_then(Value::as_str), Some("authored"));
        assert_eq!(row["world_ref"]["id"], "w");
        let seed = row.get("seed").and_then(Value::as_object).expect("seed");
        assert_eq!(
            seed.get("story_brief").and_then(Value::as_str),
            Some("старт")
        );
        assert_eq!(
            seed.get("public_intro").and_then(Value::as_str),
            Some("интро")
        );
        assert_eq!(
            seed.get("hidden_truth").and_then(Value::as_str),
            Some("тайна GM")
        );
        // The story/chat split: the draft row is CONTENT-only — no architect_*
        // keys — while the legacy meta chat stays readable via the split API...
        assert!(row.get("architect_messages").is_none());
        assert!(row.get("architect_cache_session_id").is_none());
        let legacy = store
            .get_architect_state(&id)
            .expect("read architect state")
            .expect("legacy meta state present");
        assert_eq!(legacy["messages"][0]["content"], "привет");
        assert_eq!(legacy["cache_session_id"], "story-architect:sess");

        // ...and once the conversation moves to the dialogs DB, the package
        // artifacts are purged: a stray architect.json is deleted, the legacy
        // meta keys are stripped, and the content version is NOT bumped.
        let version_before = store.version(&id).expect("version");
        std::fs::write(store.story_dir(&id).join("architect.json"), b"{}")
            .expect("plant stray architect.json");
        store
            .purge_architect_artifacts(&id)
            .expect("purge artifacts");
        assert_eq!(
            store.version(&id).expect("version after purge"),
            version_before,
            "architect purge never bumps the content version"
        );
        assert!(!store.story_dir(&id).join("architect.json").is_file());
        let raw = std::fs::read_to_string(store.story_dir(&id).join("story.json"))
            .expect("read story.json");
        assert!(!raw.contains("architect_messages"));
        assert!(store
            .get_architect_state(&id)
            .expect("read after purge")
            .is_none());
    }

    #[test]
    fn draft_row_rejects_unknown_builtin_and_procedural() {
        let (_dir, mut store) = temp_store();
        // Unknown id -> StoryNotFound (server maps to 400/404).
        assert_eq!(
            store.draft_row("nope").unwrap_err(),
            StoryStoreError::StoryNotFound("nope".to_string())
        );
        // A self-contained builtin (no world_ref) -> Invalid.
        match store.draft_row(DEFAULT_STORY_ID).unwrap_err() {
            StoryStoreError::Invalid(msg) => assert!(msg.contains("self-contained")),
            other => panic!("expected Invalid, got {other:?}"),
        }
        // A world-bound PROCEDURAL story -> Invalid (only authored is draftable).
        let created = store
            .create_bound_story(
                "Процедурная",
                "",
                "procedural",
                StoryWorldRef {
                    id: "w".to_string(),
                    version: 1,
                },
                json!({}),
            )
            .expect("create");
        let id = created
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        match store.draft_row(&id).unwrap_err() {
            StoryStoreError::Invalid(msg) => assert!(msg.contains("authored")),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn catalog_row_never_leaks_seed_or_architect_state() {
        // The player-facing catalog (list_stories / story_metadata) MUST NOT carry
        // the plot seed (hidden_truth is GM-only) or the architect chat state, for
        // ANY story kind — builtin OR authored world-bound (§С1.3 GM-secret leak).
        let (_dir, mut store) = temp_store();
        let created = store
            .create_bound_story(
                "Связанная",
                "",
                "authored",
                StoryWorldRef {
                    id: "w".to_string(),
                    version: 1,
                },
                json!({"story_brief": "кратко", "hidden_truth": "секрет"}),
            )
            .expect("create");
        let id = created
            .get("id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        store
            .update_story(
                &id,
                json!({"meta": {"architect_messages": [{"role": "user", "content": "hi"}]}}),
            )
            .expect("update");

        for row in store.list_stories() {
            assert!(
                !row.contains_key("seed"),
                "catalog row leaks seed (hidden_truth is GM-only)"
            );
            assert!(
                !row.contains_key("architect_messages"),
                "catalog row leaks architect chat state"
            );
            assert!(
                !row.contains_key("architect_model_history"),
                "catalog row leaks architect model history"
            );
        }
        // And the single-row accessor matches the list shape.
        let meta = store.story_metadata(&id).expect("metadata");
        assert!(!meta.contains_key("seed"));
        assert!(!meta.contains_key("architect_messages"));
    }

    #[test]
    fn delete_story_removes_created_package_only() {
        let (_dir, mut store) = temp_store();
        let meta = store
            .create_bound_story(
                "Удаляемая",
                "",
                "procedural",
                StoryWorldRef {
                    id: "w".to_string(),
                    version: 1,
                },
                json!({}),
            )
            .expect("create");
        let id = meta.get("id").and_then(Value::as_str).unwrap().to_string();
        assert!(store.delete_story(&id).expect("delete"));
        assert!(!store.story_ids().contains(&id));
        // Built-ins survive.
        assert_eq!(store.story_ids().len(), 3);
        // Deleting a non-existent story is a no-op false.
        assert!(!store.delete_story("nope").expect("delete missing"));
    }
}
