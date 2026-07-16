//! gml-persistence::character_store — filesystem-backed CHARACTER package store
//! (`docs/CHARACTERS_AND_STORY_TZ.md` §К1.1).
//!
//! Characters (player-character "hero" cards) are portable packages on disk, the
//! ORTHOGONAL package kind alongside worlds and stories: a hero is chosen when a
//! save is created, lives as a snapshot inside that save, and is explicitly
//! exported back to the library. The filesystem is the source of truth.
//!
//! Disk layout (per character):
//! ```text
//! <root>/characters/<id>/character.json
//! ```
//!
//! `character.json` envelope:
//! ```json
//! {
//!   "format": "gmlab.character/1",
//!   "id": "<id>",
//!   "version": 1,
//!   "title": "...",
//!   "preview": "...",
//!   "created_at": "...",
//!   "updated_at": "...",
//!   "world_ref": { "id": "...", "version": 1 },   // OPTIONAL base world
//!   "story_ref": { "id": "...", "version": 1 },   // OPTIONAL base story
//!   "payload": { "player_character": { ...canonical PC shape... }, ... }
//! }
//! ```
//!
//! HARD RULES (`§К1.1`):
//! - `payload` is an OPAQUE round-trip object (like `WorldEnvelope.payload`): the
//!   store NEVER interprets it beyond validating "is an object" (and that
//!   `payload.player_character` is an object on create/snapshot). The canonical
//!   PC serialization lives in the SERVER (which owns both the persistence and
//!   orchestrator deps) — that is the clean dependency seam.
//! - NO `ensure_defaults` and NO resurrection: a deleted character stays deleted.
//! - Both updates (`update_metadata`, `snapshot_character`) bump `version`
//!   (`saturating_add(1)`), refresh `updated_at`, and update the in-memory cache.
//! - Deep stat validation is NOT done here — that is lazy coercion at launch
//!   (`seed_player_character`), mirroring worlds.
//!
//! The store keeps an in-memory scan list (mirroring [`crate::StoryStore`]) so a
//! package written out-of-band (a zip import dropping a new `characters/<id>/`
//! folder) is picked up by [`CharacterStore::reload`] without a process restart.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{json, Map, Value};

use crate::{token_urlsafe, StoreError};

/// A pinned package reference recorded on a character (`world_ref` /
/// `story_ref`) — the world/story the hero was authored FOR. Same `{id, version}`
/// shape as the story store's `StoryWorldRef`; `version` is the referenced
/// package's version at character creation (`0` = unpinned). PROVENANCE ONLY:
/// like a save's `char_ref`, a base ref MAY dangle after the referenced package
/// is deleted — consumers must treat it as a hint, never a hard dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharacterBaseRef {
    /// The referenced package id.
    pub id: String,
    /// The referenced package `version` at character creation (`0` = unpinned).
    pub version: u64,
}

impl CharacterBaseRef {
    /// The `{id, version}` JSON object stored in `character.json` and returned
    /// by the `/characters` API.
    pub fn to_value(&self) -> Value {
        json!({ "id": self.id, "version": self.version })
    }

    /// Parse a `{id, version}` object; `None` for anything without a non-empty
    /// string id (missing/blank version degrades to `0` = unpinned).
    pub fn from_value(value: Option<&Value>) -> Option<CharacterBaseRef> {
        let obj = value?.as_object()?;
        let id = obj.get("id")?.as_str()?.trim();
        if id.is_empty() {
            return None;
        }
        let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
        Some(CharacterBaseRef {
            id: id.to_string(),
            version,
        })
    }
}

/// On-disk envelope format tag for `character.json`.
pub const CHARACTER_FORMAT: &str = "gmlab.character/1";
/// Per-character manifest filename.
const CHARACTER_FILE: &str = "character.json";
/// Sub-directory under the packages root that holds character packages.
const CHARACTERS_DIR: &str = "characters";

/// Filesystem-backed character package store.
///
/// The packages root (`<root>`) holds a `characters/` sub-directory; each
/// character is a folder named by its id containing a single `character.json`.
/// All writes are atomic (temp file + `rename`). The `Mutex` serializes mutating
/// operations. The in-memory `characters` list mirrors [`crate::StoryStore`] so
/// out-of-band imports become live after [`Self::reload`].
pub struct CharacterStore {
    root: PathBuf,
    /// Parsed, scanned character envelopes in discovery order.
    characters: Vec<CharacterEnvelope>,
    write_lock: Mutex<()>,
}

impl CharacterStore {
    /// Open (and create) a character package store rooted at `root`. The
    /// `<root>/characters` directory is created eagerly and scanned. Unlike the
    /// story store there are NO built-in defaults to materialize.
    ///
    /// A malformed/unreadable `character.json` aborts construction with
    /// [`StoreError`] rather than being silently dropped.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = abspath(root.into());
        let mut store = CharacterStore {
            root,
            characters: Vec::new(),
            write_lock: Mutex::new(()),
        };
        std::fs::create_dir_all(store.characters_dir())
            .map_err(|e| StoreError::Other(format!("create characters dir: {e}")))?;
        store.characters = store.scan()?;
        Ok(store)
    }

    /// Default packages root: `GM_PACKAGES_DIR` override, else the app-data
    /// `library` directory (identical resolution to the world/story stores).
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

    fn characters_dir(&self) -> PathBuf {
        self.root.join(CHARACTERS_DIR)
    }

    /// The package directory of one character (`<root>/characters/<id>`). Public
    /// for the share UX (zip export reads this whole directory; zip import writes
    /// into it). `id` MUST be an already-validated single path segment.
    pub fn character_dir(&self, id: &str) -> PathBuf {
        self.characters_dir().join(id)
    }

    fn character_file(&self, id: &str) -> PathBuf {
        self.character_dir(id).join(CHARACTER_FILE)
    }

    /// Whether a character package exists on disk (its `character.json` is
    /// present). Export/import use this as the no-fallback existence check.
    pub fn character_exists(&self, id: &str) -> bool {
        self.character_file(id).is_file()
    }

    /// Re-scan `<root>/characters/` from disk, replacing the in-memory list. The
    /// import handler calls this after extracting a character so the package is
    /// live without a process restart. A malformed package aborts the rescan and
    /// leaves the previous list in place.
    pub fn reload(&mut self) -> Result<(), StoreError> {
        let scanned = self.scan()?;
        self.characters = scanned;
        Ok(())
    }

    // ------------------------------------------------------------------
    // public API
    // ------------------------------------------------------------------

    /// List every character package, sorted `(updated_at DESC, created_at DESC,
    /// id DESC)`. Each entry is the flattened response object
    /// `{id, version, title, preview, created_at, updated_at, world_ref?,
    /// story_ref?, payload}` (the optional base refs, emitted when set).
    pub fn list_characters(&self) -> Vec<Value> {
        let mut envs: Vec<&CharacterEnvelope> = self.characters.iter().collect();
        envs.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| b.id.cmp(&a.id))
        });
        envs.iter().map(|e| e.to_response()).collect()
    }

    /// Read a single character by id. Returns `Err(CharacterNotFound)` when
    /// absent.
    pub fn get_character(&self, id: &str) -> Result<Value, StoreError> {
        let id = id.trim();
        self.find(id)
            .map(|e| e.to_response())
            .ok_or_else(|| StoreError::CharacterNotFound(id.to_string()))
    }

    /// Read a single character package's `version`. Returns
    /// `Err(CharacterNotFound)` when absent (the no-fallback existence check for a
    /// dangling reference).
    pub fn version(&self, id: &str) -> Result<u64, StoreError> {
        let id = id.trim();
        self.find(id)
            .map(|e| e.version)
            .ok_or_else(|| StoreError::CharacterNotFound(id.to_string()))
    }

    /// Create a new character package. Allocates a fresh id, writes
    /// `character.json` atomically, and updates the in-memory list so the new
    /// character is immediately discoverable without a rescan.
    ///
    /// `world_ref` / `story_ref` are the OPTIONAL base packages the hero is
    /// authored for (provenance, pinned at creation like a story's `world_ref` —
    /// never patchable afterwards; a standalone hero passes `None, None`).
    ///
    /// VALIDATION (`§К1.1`): `title` non-empty after trim; `payload` is an object
    /// and `payload.player_character` is an object. The `payload` is stored
    /// VERBATIM (opaque round-trip) — the store never interprets it further.
    pub fn create_character(
        &mut self,
        title: &str,
        payload: Value,
        world_ref: Option<CharacterBaseRef>,
        story_ref: Option<CharacterBaseRef>,
    ) -> Result<Value, StoreError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(StoreError::Payload(
                "create_character: title is required".to_string(),
            ));
        }
        validate_character_payload(&payload)?;

        let _guard = self
            .write_lock
            .lock()
            .expect("character write lock poisoned");
        let id = self.allocate_character_id()?;
        let now = now_timestamp()?;
        let env = CharacterEnvelope {
            id: id.clone(),
            version: 1,
            title: title.to_string(),
            created_at: now.clone(),
            updated_at: now,
            world_ref,
            story_ref,
            payload,
            preview_override: None,
        };
        self.write_envelope(&env)?;
        let response = env.to_response();
        self.characters.push(env);
        Ok(response)
    }

    /// The character's base-package references `(world_ref, story_ref)` — the
    /// world/story the hero was authored for, both optional. Returns
    /// `Err(CharacterNotFound)` for an unknown id.
    #[allow(clippy::type_complexity)]
    pub fn base_refs(
        &self,
        id: &str,
    ) -> Result<(Option<CharacterBaseRef>, Option<CharacterBaseRef>), StoreError> {
        let id = id.trim();
        self.find(id)
            .map(|e| (e.world_ref.clone(), e.story_ref.clone()))
            .ok_or_else(|| StoreError::CharacterNotFound(id.to_string()))
    }

    /// Shallow-merge `patch` into an existing character's TOP-LEVEL metadata
    /// (`title` / `preview`), null-drop (a `null` patch value is ignored so it
    /// cannot clear a field). Bumps `version`, refreshes `updated_at`, rewrites
    /// `character.json`, and updates the cache. Returns `Err(CharacterNotFound)`
    /// for an unknown id.
    ///
    /// NOTE: metadata only — `payload`/`player_character` are NOT touched here
    /// (that is [`Self::snapshot_character`]'s job). `preview` is stored in the
    /// envelope so a caller-supplied preview survives; `title` must stay
    /// non-empty (a blank/whitespace title patch is rejected).
    pub fn update_metadata(&mut self, id: &str, patch: Value) -> Result<Value, StoreError> {
        let id = id.trim().to_string();
        let _guard = self
            .write_lock
            .lock()
            .expect("character write lock poisoned");
        let idx = self
            .characters
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| StoreError::CharacterNotFound(id.clone()))?;

        let patch = match patch {
            Value::Object(m) => m,
            Value::Null => Map::new(),
            _ => {
                return Err(StoreError::Payload(
                    "update_metadata: patch must be an object".to_string(),
                ))
            }
        };

        let mut env = CharacterEnvelope {
            id: self.characters[idx].id.clone(),
            version: self.characters[idx].version,
            title: self.characters[idx].title.clone(),
            created_at: self.characters[idx].created_at.clone(),
            updated_at: self.characters[idx].updated_at.clone(),
            world_ref: self.characters[idx].world_ref.clone(),
            story_ref: self.characters[idx].story_ref.clone(),
            payload: self.characters[idx].payload.clone(),
            preview_override: self.characters[idx].preview_override.clone(),
        };

        // Shallow-merge title/preview with null-drop. A title patch must remain
        // non-empty after trim (no-fallback: a blank rename is an error).
        if let Some(v) = patch.get("title") {
            if !v.is_null() {
                let t = v.as_str().unwrap_or_default().trim();
                if t.is_empty() {
                    return Err(StoreError::Payload(
                        "update_metadata: title must be non-empty".to_string(),
                    ));
                }
                env.title = t.to_string();
            }
        }
        if let Some(v) = patch.get("preview") {
            if !v.is_null() {
                env.preview_override = Some(v.as_str().unwrap_or_default().to_string());
            }
        }

        env.version = env.version.saturating_add(1);
        env.updated_at = now_timestamp()?;
        self.write_envelope(&env)?;
        let response = env.to_response();
        self.characters[idx] = env;
        Ok(response)
    }

    /// FULLY REPLACE (`§К1.1`, NOT a merge) `payload.player_character` of an
    /// existing character with `pc` (the canonical PC object; its `card_revision`
    /// travels verbatim). Bumps `version`, refreshes `updated_at`, rewrites
    /// `character.json`, and updates the cache. Returns `Err(CharacterNotFound)`
    /// for an unknown id.
    ///
    /// VALIDATION: `pc` must be an object (the store never inspects its fields).
    pub fn snapshot_character(&mut self, id: &str, pc: Value) -> Result<Value, StoreError> {
        if !pc.is_object() {
            return Err(StoreError::Payload(
                "snapshot_character: player_character must be an object".to_string(),
            ));
        }
        let id = id.trim().to_string();
        let _guard = self
            .write_lock
            .lock()
            .expect("character write lock poisoned");
        let idx = self
            .characters
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| StoreError::CharacterNotFound(id.clone()))?;

        let existing = &self.characters[idx];
        // REPLACE the player_character key in the opaque payload object (other
        // payload keys, if any, are preserved). preserve_order keeps position.
        let mut payload = match existing.payload.clone() {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        payload.insert("player_character".to_string(), pc);

        let env = CharacterEnvelope {
            id: existing.id.clone(),
            version: existing.version.saturating_add(1),
            title: existing.title.clone(),
            created_at: existing.created_at.clone(),
            updated_at: now_timestamp()?,
            world_ref: existing.world_ref.clone(),
            story_ref: existing.story_ref.clone(),
            payload: Value::Object(payload),
            preview_override: existing.preview_override.clone(),
        };
        self.write_envelope(&env)?;
        let response = env.to_response();
        self.characters[idx] = env;
        Ok(response)
    }

    /// Delete a character package directory. Returns `true` when a package was
    /// removed, `false` when no character with that id existed. NEVER resurrected
    /// (no defaults): a deleted character stays deleted.
    pub fn delete_character(&mut self, id: &str) -> Result<bool, StoreError> {
        let id = id.trim();
        if id.is_empty() {
            return Ok(false);
        }
        let _guard = self
            .write_lock
            .lock()
            .expect("character write lock poisoned");
        let dir = self.character_dir(id);
        if !dir.join(CHARACTER_FILE).is_file() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|e| StoreError::Other(format!("delete character {id}: {e}")))?;
        self.characters.retain(|c| c.id != id);
        Ok(true)
    }

    // ------------------------------------------------------------------
    // internals
    // ------------------------------------------------------------------

    fn find(&self, id: &str) -> Option<&CharacterEnvelope> {
        self.characters.iter().find(|c| c.id == id)
    }

    fn allocate_character_id(&self) -> Result<String, StoreError> {
        for _ in 0..32 {
            let id = token_urlsafe(12);
            if !self.character_file(&id).exists() {
                return Ok(id);
            }
        }
        Err(StoreError::Other(
            "could not allocate unique character id".to_string(),
        ))
    }

    /// Scan `<root>/characters/` for `character.json` packages, sorted by id for
    /// a deterministic discovery order. A package whose `character.json` cannot be
    /// read or parsed aborts the scan (no silent fallback).
    fn scan(&self) -> Result<Vec<CharacterEnvelope>, StoreError> {
        let dir = self.characters_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoreError::Other(format!("read characters dir: {e}"))),
        };
        let mut names: Vec<String> = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|e| StoreError::Other(format!("read characters dir: {e}")))?;
            if !entry.path().join(CHARACTER_FILE).is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                names.push(name.to_string());
            }
        }
        names.sort();
        let mut out = Vec::with_capacity(names.len());
        for name in names {
            out.push(self.read_envelope(&name)?);
        }
        Ok(out)
    }

    fn read_envelope(&self, id: &str) -> Result<CharacterEnvelope, StoreError> {
        let path = self.character_file(id);
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| StoreError::Other(format!("read character {id}: {e}")))?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|e| StoreError::Payload(format!("parse character {id}: {e}")))?;
        // The folder name is the authoritative id (trust it over a stale `id`).
        Ok(CharacterEnvelope::from_value(id, value))
    }

    /// Atomic write: serialize to a temp file in the character's directory, then
    /// `rename` over `character.json`.
    fn write_envelope(&self, env: &CharacterEnvelope) -> Result<(), StoreError> {
        let dir = self.character_dir(&env.id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| StoreError::Other(format!("create character dir {}: {e}", env.id)))?;
        let body = serde_json::to_vec(&env.to_file_value())
            .map_err(|e| StoreError::Payload(format!("serialize character {}: {e}", env.id)))?;
        let final_path = dir.join(CHARACTER_FILE);
        let tmp_path = dir.join(format!(".{CHARACTER_FILE}.{}.tmp", token_urlsafe(6)));
        std::fs::write(&tmp_path, &body)
            .map_err(|e| StoreError::Other(format!("write temp character {}: {e}", env.id)))?;
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(StoreError::Other(format!(
                "rename character {}: {e}",
                env.id
            )));
        }
        Ok(())
    }
}

/// In-memory view of one `character.json`. `payload` is the exact opaque
/// round-trip object; the surrounding fields are the package envelope.
struct CharacterEnvelope {
    id: String,
    version: u64,
    title: String,
    created_at: String,
    updated_at: String,
    /// The base WORLD the hero was authored for (provenance, may dangle).
    world_ref: Option<CharacterBaseRef>,
    /// The base STORY the hero was authored for (provenance, may dangle).
    story_ref: Option<CharacterBaseRef>,
    /// The opaque round-trip payload (`{player_character: {...}, ...}`) exactly
    /// as stored. The store never interprets it beyond the create/snapshot
    /// object-shape validation.
    payload: Value,
    /// A caller-supplied preview override (from `update_metadata`); when `None`
    /// the preview is derived from the payload's player-character name.
    preview_override: Option<String>,
}

impl CharacterEnvelope {
    /// Build an envelope from a parsed `character.json`, using `id` (the folder
    /// name) as the authoritative id. Missing/blank metadata degrades gracefully.
    fn from_value(id: &str, value: Value) -> Self {
        let obj = value.as_object();
        let version = obj
            .and_then(|m| m.get("version"))
            .and_then(Value::as_u64)
            .unwrap_or(1);
        let title = obj
            .and_then(|m| m.get("title"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let created_at = obj
            .and_then(|m| m.get("created_at"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let updated_at = obj
            .and_then(|m| m.get("updated_at"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let world_ref = CharacterBaseRef::from_value(obj.and_then(|m| m.get("world_ref")));
        let story_ref = CharacterBaseRef::from_value(obj.and_then(|m| m.get("story_ref")));
        let payload = obj
            .and_then(|m| m.get("payload"))
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        let preview_override = obj
            .and_then(|m| m.get("preview"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());
        CharacterEnvelope {
            id: id.to_string(),
            version,
            title,
            created_at,
            updated_at,
            world_ref,
            story_ref,
            payload,
            preview_override,
        }
    }

    /// Derive the preview string: the caller-supplied override if set, else the
    /// player-character `name` from the opaque payload, else the title.
    fn preview(&self) -> String {
        if let Some(p) = &self.preview_override {
            if !p.trim().is_empty() {
                return p.clone();
            }
        }
        let name = self
            .payload
            .get("player_character")
            .and_then(|pc| pc.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        if !name.is_empty() {
            name
        } else {
            self.title.clone()
        }
    }

    /// The optional `world_ref`/`story_ref` keys shared by the file and response
    /// shapes — emitted ONLY when set, so a ref-less character serializes
    /// byte-identically to the pre-refs format.
    fn insert_base_refs(&self, map: &mut Map<String, Value>) {
        if let Some(world_ref) = &self.world_ref {
            map.insert("world_ref".to_string(), world_ref.to_value());
        }
        if let Some(story_ref) = &self.story_ref {
            map.insert("story_ref".to_string(), story_ref.to_value());
        }
    }

    /// The on-disk `character.json` value (envelope + payload).
    fn to_file_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("format".to_string(), json!(CHARACTER_FORMAT));
        map.insert("id".to_string(), json!(self.id));
        map.insert("version".to_string(), json!(self.version));
        map.insert("title".to_string(), json!(self.title));
        map.insert("preview".to_string(), json!(self.preview()));
        map.insert("created_at".to_string(), json!(self.created_at));
        map.insert("updated_at".to_string(), json!(self.updated_at));
        self.insert_base_refs(&mut map);
        map.insert("payload".to_string(), self.payload.clone());
        Value::Object(map)
    }

    /// The flattened response object the `/characters` API returns.
    fn to_response(&self) -> Value {
        let mut map = Map::new();
        map.insert("id".to_string(), json!(self.id));
        map.insert("version".to_string(), json!(self.version));
        map.insert("title".to_string(), json!(self.title));
        map.insert("preview".to_string(), json!(self.preview()));
        map.insert("created_at".to_string(), json!(self.created_at));
        map.insert("updated_at".to_string(), json!(self.updated_at));
        self.insert_base_refs(&mut map);
        map.insert("payload".to_string(), self.payload.clone());
        Value::Object(map)
    }
}

/// Validate a character package payload on create/import (`§К1.1`, `§К1.2`):
/// `payload` is an object and `payload.player_character` is an object. Deep stat
/// validation is deliberately NOT done here (lazy coercion at launch).
pub(crate) fn validate_character_payload(payload: &Value) -> Result<(), StoreError> {
    let obj = payload
        .as_object()
        .ok_or_else(|| StoreError::Payload("payload must be an object".to_string()))?;
    match obj.get("player_character") {
        Some(Value::Object(_)) => Ok(()),
        _ => Err(StoreError::Payload(
            "payload.player_character must be an object".to_string(),
        )),
    }
}

/// Produce a UTC timestamp byte-identical to SQLite's `datetime('now')`
/// (`"YYYY-MM-DD HH:MM:SS"`), matching the world/story stores' timestamps.
fn now_timestamp() -> Result<String, StoreError> {
    let con = rusqlite::Connection::open_in_memory()?;
    let ts: String = con.query_row("SELECT datetime('now')", [], |row| row.get(0))?;
    Ok(ts)
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

    fn temp_store() -> (tempfile::TempDir, CharacterStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CharacterStore::new(dir.path()).expect("open store");
        (dir, store)
    }

    fn pc_payload(name: &str, card_revision: i64) -> Value {
        json!({
            "player_character": {
                "name": name,
                "card_revision": card_revision,
            }
        })
    }

    #[test]
    fn create_get_list_and_reload() {
        let (dir, mut store) = temp_store();
        assert!(store.list_characters().is_empty());

        let created = store
            .create_character("Герой", pc_payload("Ариан", 2), None, None)
            .expect("create");
        let id = created["id"].as_str().unwrap().to_string();
        assert_eq!(created["version"], json!(1));
        assert_eq!(created["title"], json!("Герой"));
        assert_eq!(created["preview"], json!("Ариан"));
        assert_eq!(
            created["payload"]["player_character"]["card_revision"],
            json!(2)
        );

        assert_eq!(store.list_characters().len(), 1);
        let got = store.get_character(&id).expect("get");
        assert_eq!(got["id"], json!(id));

        // A fresh store over the SAME root re-scans the package from disk.
        let reopened = CharacterStore::new(dir.path()).expect("reopen");
        assert_eq!(
            reopened.get_character(&id).unwrap()["title"],
            json!("Герой")
        );
    }

    #[test]
    fn base_refs_round_trip_and_stay_optional() {
        let (dir, mut store) = temp_store();

        // A standalone hero: no refs in the response, none on disk.
        let plain = store
            .create_character("Одиночка", pc_payload("Бран", 0), None, None)
            .expect("create plain");
        assert!(plain.get("world_ref").is_none());
        assert!(plain.get("story_ref").is_none());
        let plain_id = plain["id"].as_str().unwrap().to_string();

        // A based hero: refs come back in the response…
        let based = store
            .create_character(
                "Основанный",
                pc_payload("Ариан", 1),
                Some(CharacterBaseRef {
                    id: "w1".into(),
                    version: 3,
                }),
                Some(CharacterBaseRef {
                    id: "s1".into(),
                    version: 7,
                }),
            )
            .expect("create based");
        assert_eq!(based["world_ref"], json!({"id": "w1", "version": 3}));
        assert_eq!(based["story_ref"], json!({"id": "s1", "version": 7}));
        let based_id = based["id"].as_str().unwrap().to_string();

        // …survive metadata + snapshot updates…
        let renamed = store
            .update_metadata(&based_id, json!({"title": "Новое имя"}))
            .expect("rename");
        assert_eq!(renamed["world_ref"], json!({"id": "w1", "version": 3}));
        let snapped = store
            .snapshot_character(&based_id, json!({"name": "Ариан II"}))
            .expect("snapshot");
        assert_eq!(snapped["story_ref"], json!({"id": "s1", "version": 7}));

        // …and a disk re-scan; the accessor mirrors them.
        let reopened = CharacterStore::new(dir.path()).expect("reopen");
        let (world_ref, story_ref) = reopened.base_refs(&based_id).expect("refs");
        assert_eq!(
            world_ref,
            Some(CharacterBaseRef {
                id: "w1".into(),
                version: 3
            })
        );
        assert_eq!(
            story_ref,
            Some(CharacterBaseRef {
                id: "s1".into(),
                version: 7
            })
        );
        assert_eq!(
            reopened.base_refs(&plain_id).expect("plain refs"),
            (None, None)
        );
        assert!(matches!(
            reopened.base_refs("nope"),
            Err(StoreError::CharacterNotFound(_))
        ));
    }

    #[test]
    fn create_rejects_bad_input() {
        let (_dir, mut store) = temp_store();
        assert!(store
            .create_character("  ", pc_payload("x", 0), None, None)
            .is_err());
        assert!(store
            .create_character(
                "t",
                json!({"player_character": "not-an-object"}),
                None,
                None
            )
            .is_err());
        assert!(store
            .create_character("t", json!("not-an-object"), None, None)
            .is_err());
        assert!(store.create_character("t", json!({}), None, None).is_err());
    }

    #[test]
    fn update_metadata_bumps_version_and_merges_null_drop() {
        let (_dir, mut store) = temp_store();
        let created = store
            .create_character("Старое имя", pc_payload("Ариан", 0), None, None)
            .expect("create");
        let id = created["id"].as_str().unwrap().to_string();

        // Rename + set preview.
        let updated = store
            .update_metadata(&id, json!({"title": "Новое имя", "preview": "Заметка"}))
            .expect("update");
        assert_eq!(updated["version"], json!(2));
        assert_eq!(updated["title"], json!("Новое имя"));
        assert_eq!(updated["preview"], json!("Заметка"));

        // A null title patch is dropped (title unchanged); version still bumps.
        let updated2 = store
            .update_metadata(&id, json!({"title": Value::Null}))
            .expect("update null-drop");
        assert_eq!(updated2["version"], json!(3));
        assert_eq!(updated2["title"], json!("Новое имя"));
        assert_eq!(updated2["preview"], json!("Заметка"));

        // Blank title is rejected.
        assert!(store.update_metadata(&id, json!({"title": "   "})).is_err());
        // Unknown id is CharacterNotFound.
        assert!(matches!(
            store.update_metadata("nope", json!({"title": "x"})),
            Err(StoreError::CharacterNotFound(_))
        ));
    }

    #[test]
    fn snapshot_replaces_player_character_not_merges() {
        let (_dir, mut store) = temp_store();
        // Seed a payload with EXTRA keys inside player_character + a sibling key.
        let created = store
            .create_character(
                "Герой",
                json!({
                    "player_character": {"name": "Ариан", "old_field": "keep-me?", "card_revision": 1},
                    "sibling": "kept",
                }),
                None,
                None,
            )
            .expect("create");
        let id = created["id"].as_str().unwrap().to_string();

        // Snapshot with a DIFFERENT PC: REPLACE, not merge — old_field must be
        // gone; card_revision travels verbatim; sibling payload key preserved.
        let snap = store
            .snapshot_character(&id, json!({"name": "Ариан II", "card_revision": 5}))
            .expect("snapshot");
        assert_eq!(snap["version"], json!(2));
        let pc = &snap["payload"]["player_character"];
        assert_eq!(pc["name"], json!("Ариан II"));
        assert_eq!(pc["card_revision"], json!(5));
        assert!(
            pc.get("old_field").is_none(),
            "snapshot must REPLACE, not merge: {pc}"
        );
        assert_eq!(snap["payload"]["sibling"], json!("kept"));

        // Non-object PC rejected; unknown id is CharacterNotFound.
        assert!(store.snapshot_character(&id, json!("x")).is_err());
        assert!(matches!(
            store.snapshot_character("nope", json!({"name": "z"})),
            Err(StoreError::CharacterNotFound(_))
        ));
    }

    #[test]
    fn delete_never_resurrects() {
        let dir = tempfile::tempdir().expect("tempdir");
        let id;
        {
            let mut store = CharacterStore::new(dir.path()).expect("open");
            let created = store
                .create_character("Герой", pc_payload("Ариан", 0), None, None)
                .expect("create");
            id = created["id"].as_str().unwrap().to_string();
            assert!(store.delete_character(&id).expect("delete"));
            assert!(!store.character_exists(&id));
            assert!(!store.delete_character(&id).expect("delete again"));
        }
        // Reopen: the deleted character stays deleted (no ensure_defaults).
        let reopened = CharacterStore::new(dir.path()).expect("reopen");
        assert!(reopened.list_characters().is_empty());
        assert!(matches!(
            reopened.get_character(&id),
            Err(StoreError::CharacterNotFound(_))
        ));
    }
}
