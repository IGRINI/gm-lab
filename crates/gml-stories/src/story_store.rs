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
//!   "world_embedded": true,
//!   "title": "...",
//!   "description": "...",
//!   "seed": { ...the EXACT legacy catalog seed Value... }
//! }
//! ```
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
}

impl std::fmt::Display for StoryStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoryStoreError::Io(msg) => write!(f, "{msg}"),
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

    /// `story_metadata(story_id) -> {id, title, description, story_brief}`.
    ///
    /// Returns [`UnknownStory`] for an id absent from the library. The map
    /// preserves the public catalog key order: `id`, `title`, `description`,
    /// `story_brief`.
    pub fn story_metadata(&self, story_id: &str) -> Result<Map<String, Value>, UnknownStory> {
        let env = self
            .find(story_id)
            .ok_or_else(|| UnknownStory(story_id.to_string()))?;
        Ok(env.metadata())
    }

    /// `list_stories() -> list[{id, title, description, story_brief}]` —
    /// discovery order. Each element has the same shape as [`Self::story_metadata`].
    pub fn list_stories(&self) -> Vec<Map<String, Value>> {
        self.stories.iter().map(|s| s.metadata()).collect()
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
        let env = StoryEnvelope {
            id: id.clone(),
            version: 1,
            kind: kind.to_string(),
            world_embedded: false,
            world_ref: Some(world_ref),
            title: title.to_string(),
            description: description.trim().to_string(),
            seed: plot,
        };
        self.write_envelope(&env)?;
        let meta = env.metadata();
        self.stories.push(env);
        Ok(meta)
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
}

impl StoryEnvelope {
    /// Build an envelope from a parsed `story.json`, using `id` (the folder name)
    /// as the authoritative id.
    fn from_value(id: &str, value: Value) -> Result<Self, StoryStoreError> {
        let obj = value
            .as_object()
            .ok_or_else(|| StoryStoreError::Io(format!("story {id}: story.json is not an object")))?;
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
        Ok(StoryEnvelope {
            id: id.to_string(),
            version,
            kind,
            world_embedded,
            world_ref,
            title,
            description,
            seed,
        })
    }

    /// `{id, title, description, story_brief}` — story_brief is derived from the
    /// seed (`story_brief` if non-empty, else `public_intro`), matching the
    /// legacy free function exactly.
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
        meta
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

    /// The on-disk `story.json` value (envelope + seed). A `world_ref` is
    /// emitted only when the story is bound to a world package, so the
    /// self-contained built-ins keep their exact byte shape.
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
        m.insert("seed".into(), self.seed.clone());
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

/// Read a string field from `seed[key]`, defaulting to `""`.
fn seed_str(seed: &Value, key: &str) -> String {
    seed.as_object()
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
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
        });
    }
    Ok(out)
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
            store.plot(&id).unwrap().get("hidden_truth").and_then(Value::as_str),
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
