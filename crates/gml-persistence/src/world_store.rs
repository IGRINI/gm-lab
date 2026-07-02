//! gml-persistence::world_store — filesystem-backed world package store.
//!
//! Worlds are portable packages on disk (the "mod" model from
//! `docs/MODS_PACKAGES_TZ.md`, Phase 1). The filesystem is the source of truth;
//! the old SQLite `worlds` table is read exactly once for a one-time migration
//! and is never written to or read from on a live request again.
//!
//! Disk layout (per world):
//! ```text
//! <root>/worlds/<world_id>/world.json
//! ```
//!
//! `world.json` envelope:
//! ```json
//! {
//!   "format": "gmlab.world/1",
//!   "id": "<world_id>",
//!   "version": 1,
//!   "title": "...",
//!   "preview": "...",
//!   "created_at": "...",
//!   "updated_at": "...",
//!   "payload": { ...the EXACT existing world payload object... }
//! }
//! ```
//!
//! The `payload` object is byte-shape-identical to what the SQLite store wrote,
//! so the `/worlds` HTTP contract and the web frontend do not change. The
//! richer lore/assets/architect.json split is a LATER phase.
//!
//! Worlds are GLOBAL — there is no per-guest scoping (worlds are shared content,
//! not personal saves). Saves keep their guest scope in [`crate::DialogStore`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Map, Value};

use crate::{
    merge_world_payload, normalize_world_payload, token_urlsafe, world_preview_from_payload,
    world_row_response, world_title_from_payload, StoreError,
};

/// On-disk envelope format tag for `world.json`.
pub const WORLD_FORMAT: &str = "gmlab.world/1";
/// Per-world manifest filename.
const WORLD_FILE: &str = "world.json";
/// Sub-directory under the packages root that holds world packages.
const WORLDS_DIR: &str = "worlds";
/// Per-world sub-directory that holds generated/imported binary assets
/// (cover image, map, …). Image fields in `world.json` reference files here
/// by the package-relative path `assets/<file>`.
pub const ASSETS_DIR: &str = "assets";

/// Filesystem-backed world package store.
///
/// The packages root (`<root>`) holds a `worlds/` sub-directory; each world is a
/// folder named by its `world_id` containing a single `world.json`. All writes
/// are atomic (write a temp file in the same directory, then `rename`). The
/// `Mutex` serializes mutating operations so concurrent saves to the same root
/// cannot interleave the read-merge-write of an update.
pub struct WorldStore {
    root: PathBuf,
    write_lock: Mutex<()>,
}

impl WorldStore {
    /// Open (and create) a world package store rooted at `root`. The
    /// `<root>/worlds` directory is created eagerly.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = abspath(root.into());
        let store = WorldStore {
            root,
            write_lock: Mutex::new(()),
        };
        std::fs::create_dir_all(store.worlds_dir())
            .map_err(|e| StoreError::Other(format!("create worlds dir: {e}")))?;
        Ok(store)
    }

    /// Default packages root: `GM_PACKAGES_DIR` override, else the app-data
    /// `library` directory.
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

    fn worlds_dir(&self) -> PathBuf {
        self.root.join(WORLDS_DIR)
    }

    /// The package directory of one world (`<root>/worlds/<id>`). Public for the
    /// Phase-5 share UX (zip export reads this whole directory; zip import writes
    /// into it). `world_id` MUST be an already-validated single path segment.
    pub fn world_dir(&self, world_id: &str) -> PathBuf {
        self.worlds_dir().join(world_id)
    }

    /// Whether a world package exists on disk (its `world.json` is present).
    /// The Phase-5 export/import paths use this as the no-fallback existence
    /// check before zipping / before an overwrite-guarded import.
    pub fn world_exists(&self, world_id: &str) -> bool {
        self.world_file(world_id).is_file()
    }

    fn world_file(&self, world_id: &str) -> PathBuf {
        self.world_dir(world_id).join(WORLD_FILE)
    }

    /// The `assets/` directory for a world package (`<root>/worlds/<id>/assets`).
    pub fn assets_dir(&self, world_id: &str) -> PathBuf {
        self.world_dir(world_id).join(ASSETS_DIR)
    }

    /// Absolute on-disk path of one asset file inside a world package
    /// (`<root>/worlds/<id>/assets/<filename>`). `filename` is treated as a
    /// single path segment; callers must validate it (no separators / `..`).
    pub fn asset_path(&self, world_id: &str, filename: &str) -> PathBuf {
        self.assets_dir(world_id).join(filename)
    }

    /// Whether an asset file exists inside a world package.
    pub fn has_asset(&self, world_id: &str, filename: &str) -> bool {
        self.asset_path(world_id, filename).is_file()
    }

    /// Read an asset file's bytes. Returns `Ok(None)` when the file is absent.
    pub fn read_asset(&self, world_id: &str, filename: &str) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.asset_path(world_id, filename);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::Other(format!(
                "read asset {world_id}/{filename}: {e}"
            ))),
        }
    }

    /// Atomically write an asset file into a world package's `assets/`
    /// directory (created on demand). Writes a temp file in the same directory,
    /// then `rename`s it over the target so a reader never sees a partial file.
    /// `filename` MUST be a single, already-validated path segment.
    pub fn write_asset(
        &self,
        world_id: &str,
        filename: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        let dir = self.assets_dir(world_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| StoreError::Other(format!("create assets dir {world_id}: {e}")))?;
        let final_path = dir.join(filename);
        let tmp_path = dir.join(format!(".{filename}.{}.tmp", token_urlsafe(6)));
        std::fs::write(&tmp_path, bytes)
            .map_err(|e| StoreError::Other(format!("write temp asset {world_id}/{filename}: {e}")))?;
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(StoreError::Other(format!(
                "rename asset {world_id}/{filename}: {e}"
            )));
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // public API (mirrors the worlds methods the server needs)
    // ------------------------------------------------------------------

    /// List every world package, sorted `(updated_at DESC, created_at DESC,
    /// id DESC)` — the same ordering the SQLite store used. Each entry is the
    /// same flattened world object the API returned before (payload keys + the
    /// six injected `{id, kind, title, preview, created_at, updated_at}` keys).
    pub fn list_worlds(&self) -> Result<Vec<Value>, StoreError> {
        let mut envelopes = self.read_all_envelopes()?;
        // Sort newest-first; ties broken by id DESC (stable, deterministic).
        envelopes.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.created_at.cmp(&a.created_at))
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(envelopes.iter().map(|e| e.to_world_response()).collect())
    }

    /// Read a single world package's `version` (for Phase-4 `world_ref`
    /// provenance). Returns `Err(WorldNotFound)` when the world is absent — this
    /// is the no-fallback existence check for a dangling reference.
    pub fn world_version(&self, world_id: &str) -> Result<u64, StoreError> {
        let world_id = world_id.trim();
        if world_id.is_empty() {
            return Err(StoreError::WorldNotFound(world_id.to_string()));
        }
        let env = self
            .read_envelope(world_id)?
            .ok_or_else(|| StoreError::WorldNotFound(world_id.to_string()))?;
        Ok(env.version)
    }

    /// Read a single world by id. Returns `Err(WorldNotFound)` when absent.
    pub fn get_world(&self, world_id: &str) -> Result<Value, StoreError> {
        let world_id = world_id.trim();
        if world_id.is_empty() {
            return Err(StoreError::WorldNotFound(world_id.to_string()));
        }
        let env = self
            .read_envelope(world_id)?
            .ok_or_else(|| StoreError::WorldNotFound(world_id.to_string()))?;
        Ok(env.to_world_response())
    }

    /// Create a new world package. Allocates a unique `world_id`, normalizes the
    /// payload, derives title/preview, and writes `world.json` atomically.
    pub fn create_world(&self, payload: Value) -> Result<Value, StoreError> {
        let _guard = self.write_lock.lock().expect("world write lock poisoned");
        let world_id = self.allocate_world_id()?;
        let payload = normalize_world_payload(payload);
        let now = now_timestamp()?;
        let env = WorldEnvelope {
            id: world_id.clone(),
            version: 1,
            created_at: now.clone(),
            updated_at: now,
            payload,
        };
        self.write_envelope(&env)?;
        Ok(env.to_world_response())
    }

    /// Shallow-merge `patch` into an existing world's payload (exactly like the
    /// old SQLite `merge_world_payload`: non-null patch keys overwrite the base,
    /// `null` patch values are dropped so a later "ready" save preserves
    /// previously stored architect history/cache). Bumps `version`, refreshes
    /// `updated_at`, and rewrites `world.json` atomically.
    pub fn update_world(&self, world_id: &str, patch: Value) -> Result<Value, StoreError> {
        let world_id = world_id.trim();
        if world_id.is_empty() {
            return Err(StoreError::WorldNotFound(world_id.to_string()));
        }
        let _guard = self.write_lock.lock().expect("world write lock poisoned");
        let existing = self
            .read_envelope(world_id)?
            .ok_or_else(|| StoreError::WorldNotFound(world_id.to_string()))?;
        let merged = normalize_world_payload(merge_world_payload(existing.payload, patch));
        let env = WorldEnvelope {
            id: existing.id,
            version: existing.version.saturating_add(1),
            created_at: existing.created_at,
            updated_at: now_timestamp()?,
            payload: merged,
        };
        self.write_envelope(&env)?;
        Ok(env.to_world_response())
    }

    /// Delete a world package directory. Returns the same shape the SQLite store
    /// returned: `{deleted:true}` on success, `{deleted:false, reason:...}` for
    /// an empty id or a missing world (never touches chat state).
    pub fn delete_world(&self, world_id: &str) -> Result<Value, StoreError> {
        let world_id = world_id.trim();
        if world_id.is_empty() {
            return Ok(json!({"deleted": false, "reason": "world_id is required"}));
        }
        let _guard = self.write_lock.lock().expect("world write lock poisoned");
        let dir = self.world_dir(world_id);
        if !dir.join(WORLD_FILE).is_file() {
            return Ok(json!({"deleted": false, "reason": "world not found"}));
        }
        std::fs::remove_dir_all(&dir)
            .map_err(|e| StoreError::Other(format!("delete world {world_id}: {e}")))?;
        Ok(json!({"deleted": true}))
    }

    // ------------------------------------------------------------------
    // one-time migration from the legacy SQLite `worlds` table
    // ------------------------------------------------------------------

    /// One-time import of the legacy SQLite `worlds` table into filesystem
    /// packages. This is the ONLY place that reads the old table.
    ///
    /// Idempotent and safe to call on every startup:
    /// - no-op if any world package already exists (so a populated library is
    ///   never re-seeded);
    /// - no-op if the SQLite DB file or its `worlds` table is absent / empty;
    /// - preserves `world_id` (folder name), `payload`, `created_at`,
    ///   `updated_at`; `title`/`preview` are re-derived from the payload exactly
    ///   as before. Guest scoping is dropped (worlds become global); if two
    ///   guest rows share a `world_id`, the later-`updated_at` row wins.
    ///
    /// Returns the number of worlds imported (0 when skipped).
    pub fn migrate_from_sqlite(&self, db_path: &str) -> Result<usize, StoreError> {
        if self.has_any_world()? {
            return Ok(0);
        }
        if !Path::new(db_path).is_file() {
            return Ok(0);
        }
        let rows = read_legacy_world_rows(db_path)?;
        if rows.is_empty() {
            return Ok(0);
        }

        let _guard = self.write_lock.lock().expect("world write lock poisoned");
        let mut imported = 0usize;
        for row in rows {
            let payload = serde_json::from_str::<Value>(&row.payload)
                .unwrap_or(Value::Object(Map::new()));
            let payload = normalize_world_payload(payload);
            let env = WorldEnvelope {
                id: row.world_id.clone(),
                version: 1,
                created_at: row.created_at,
                updated_at: row.updated_at,
                payload,
            };
            self.write_envelope(&env)?;
            imported += 1;
        }
        Ok(imported)
    }

    // ------------------------------------------------------------------
    // internals
    // ------------------------------------------------------------------

    fn has_any_world(&self) -> Result<bool, StoreError> {
        let worlds = self.worlds_dir();
        let entries = match std::fs::read_dir(&worlds) {
            Ok(e) => e,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(StoreError::Other(format!("read worlds dir: {e}"))),
        };
        for entry in entries.flatten() {
            if entry.path().join(WORLD_FILE).is_file() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn allocate_world_id(&self) -> Result<String, StoreError> {
        for _ in 0..32 {
            let id = token_urlsafe(12);
            if !self.world_file(&id).exists() {
                return Ok(id);
            }
        }
        Err(StoreError::Other(
            "could not allocate unique world id".to_string(),
        ))
    }

    fn read_all_envelopes(&self) -> Result<Vec<WorldEnvelope>, StoreError> {
        let worlds = self.worlds_dir();
        let entries = match std::fs::read_dir(&worlds) {
            Ok(e) => e,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(StoreError::Other(format!("read worlds dir: {e}"))),
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.join(WORLD_FILE).is_file() {
                continue;
            }
            let dir_name = match entry.file_name().to_str() {
                Some(name) => name.to_string(),
                None => continue,
            };
            if let Some(env) = self.read_envelope(&dir_name)? {
                out.push(env);
            }
        }
        Ok(out)
    }

    fn read_envelope(&self, world_id: &str) -> Result<Option<WorldEnvelope>, StoreError> {
        let path = self.world_file(world_id);
        let raw = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                return Err(StoreError::Other(format!(
                    "read world {world_id}: {e}"
                )))
            }
        };
        let value: Value = serde_json::from_str(&raw)
            .map_err(|e| StoreError::Payload(format!("parse world {world_id}: {e}")))?;
        // The folder name is the authoritative id (a world is identified by its
        // directory). Trust it over a possibly-stale `id` field in the file.
        Ok(Some(WorldEnvelope::from_value(world_id, value)))
    }

    /// Atomic write: serialize to a temp file in the world's directory, then
    /// `rename` over `world.json`.
    fn write_envelope(&self, env: &WorldEnvelope) -> Result<(), StoreError> {
        let dir = self.world_dir(&env.id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| StoreError::Other(format!("create world dir {}: {e}", env.id)))?;
        let body = serde_json::to_vec(&env.to_file_value())
            .map_err(|e| StoreError::Payload(format!("serialize world {}: {e}", env.id)))?;
        let final_path = dir.join(WORLD_FILE);
        let tmp_path = dir.join(format!(".{WORLD_FILE}.{}.tmp", token_urlsafe(6)));
        std::fs::write(&tmp_path, &body)
            .map_err(|e| StoreError::Other(format!("write temp world {}: {e}", env.id)))?;
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(StoreError::Other(format!(
                "rename world {}: {e}",
                env.id
            )));
        }
        Ok(())
    }
}

/// In-memory view of one `world.json`. `payload` is the exact existing world
/// payload object; the surrounding envelope is metadata.
struct WorldEnvelope {
    id: String,
    version: u64,
    created_at: String,
    updated_at: String,
    payload: Value,
}

impl WorldEnvelope {
    /// Build an envelope from a parsed `world.json`, using `id` (the folder name)
    /// as the authoritative id. Missing/blank metadata degrades gracefully.
    fn from_value(id: &str, value: Value) -> Self {
        let obj = value.as_object();
        let version = obj
            .and_then(|m| m.get("version"))
            .and_then(Value::as_u64)
            .unwrap_or(1);
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
        let payload = obj
            .and_then(|m| m.get("payload"))
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        WorldEnvelope {
            id: id.to_string(),
            version,
            created_at,
            updated_at,
            payload,
        }
    }

    /// The on-disk `world.json` value (envelope + payload).
    fn to_file_value(&self) -> Value {
        let title = world_title_from_payload(&self.payload);
        let preview = world_preview_from_payload(&self.payload, &title);
        json!({
            "format": WORLD_FORMAT,
            "id": self.id,
            "version": self.version,
            "title": title,
            "preview": preview,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "payload": self.payload,
        })
    }

    /// The flattened world object the `/worlds` API returns — byte-shape
    /// identical to what the SQLite store produced via `world_row_response`.
    fn to_world_response(&self) -> Value {
        let title = world_title_from_payload(&self.payload);
        let preview = world_preview_from_payload(&self.payload, &title);
        let payload_json = serde_json::to_string(&self.payload).unwrap_or_default();
        world_row_response(
            &self.id,
            &title,
            &preview,
            &payload_json,
            &self.created_at,
            &self.updated_at,
        )
    }
}

/// A raw legacy `worlds` row read from SQLite (migration source only).
struct LegacyWorldRow {
    world_id: String,
    payload: String,
    created_at: String,
    updated_at: String,
}

/// Read every row of the legacy SQLite `worlds` table. Drops guest scoping:
/// when two guest rows share a `world_id`, the later-`updated_at` row wins.
/// Returns an empty vec if the table does not exist.
fn read_legacy_world_rows(db_path: &str) -> Result<Vec<LegacyWorldRow>, StoreError> {
    let con = Connection::open(db_path)?;
    let exists: Option<String> = con
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='worlds'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(Vec::new());
    }
    let mut stmt = con.prepare(
        "SELECT world_id, payload, created_at, updated_at
         FROM worlds
         ORDER BY updated_at ASC, created_at ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(LegacyWorldRow {
                world_id: row.get::<_, String>(0)?,
                payload: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                created_at: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                updated_at: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    // De-dup by world_id, keeping the last (latest updated_at, since ordered ASC).
    let mut by_id: std::collections::HashMap<String, LegacyWorldRow> =
        std::collections::HashMap::new();
    for row in rows {
        by_id.insert(row.world_id.clone(), row);
    }
    Ok(by_id.into_values().collect())
}

/// Produce a UTC timestamp byte-identical to SQLite's `datetime('now')`
/// (`"YYYY-MM-DD HH:MM:SS"`, seconds resolution). Uses an in-memory SQLite
/// connection so the format matches the legacy store exactly without adding a
/// time-formatting dependency.
fn now_timestamp() -> Result<String, StoreError> {
    let con = Connection::open_in_memory()?;
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
