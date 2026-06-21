//! gml-persistence — SQLite-backed dialog persistence keyed by chat scope.
//!
//! Faithful port of `gm-lab/dialog_store.py` (PORT_PLAN §6). Preserves the
//! schema (table/column/index/PK names), the on-disk JSON payload (field names,
//! key order, compact `(",",":")` separators, raw UTF-8), `SCHEMA_VERSION = 1`
//! hard-check, `datetime('now')` timestamps, the `(updated_at DESC, created_at
//! DESC, chat_id DESC)` ordering, and the per-connection PRAGMAs
//! (`busy_timeout=10000`, `journal_mode=WAL`, `synchronous=NORMAL`).
//!
//! One short-lived connection per op; multi-statement ops run in an explicit
//! transaction. The (de)serialization of `Session`/`World` lives upstream in
//! `gml-orchestrator` (`Session::to_payload` / `Session::from_payload`); this
//! crate wraps the top-level `{schema_version, turn_count, session, transcript}`
//! envelope and the DB lifecycle.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use gml_config::Config;
use gml_llm::Backend;
use gml_orchestrator::{ClientFactory, CompactionThresholds, Session};

/// `SCHEMA_VERSION = 1` — hard-checked on load (no migrations exist).
pub const SCHEMA_VERSION: i64 = 1;
/// `DEFAULT_CHAT_TITLE`.
pub const DEFAULT_CHAT_TITLE: &str = "Новый чат";

/// Errors raised by [`DialogStore`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("chat not found: {0}")]
    ChatNotFound(String),
    #[error("unsupported schema version: {0}")]
    SchemaVersion(String),
    #[error("invalid payload: {0}")]
    Payload(String),
    #[error("{0}")]
    Other(String),
}

/// `class DialogRuntime` — the in-memory state of one chat.
///
/// The Python dataclass carries a per-runtime `RLock`; here the per-chat lock
/// (held across a streamed turn in Python) is the caller's concern, so we omit
/// it from the value type and let the store's cache `Mutex` guard structural
/// access.
pub struct DialogRuntime {
    pub guest_id: String,
    pub chat_id: String,
    pub session: Session,
    pub transcript: Vec<Value>,
    pub turn_count: i64,
    pub title: String,
    pub preview: String,
    pub created_at: String,
    pub updated_at: String,
}

impl DialogRuntime {
    /// `_runtime_to_payload(runtime)` -> `{schema_version, turn_count, session,
    /// transcript}`.
    pub fn to_payload(&self) -> Value {
        json!({
            "schema_version": SCHEMA_VERSION,
            "turn_count": self.turn_count,
            "session": self.session.to_payload(),
            "transcript": Value::Array(self.transcript.clone()),
        })
    }

    /// Serialize the payload exactly as `DialogStore.save` writes it:
    /// `json.dumps(..., ensure_ascii=False, separators=(",",":"))`.
    pub fn payload_json(&self) -> String {
        // serde_json default output is compact + non-ASCII-preserving, which
        // matches `separators=(",",":")` + `ensure_ascii=False` exactly.
        serde_json::to_string(&self.to_payload()).unwrap_or_default()
    }
}

/// `class DialogStore` — SQLite-backed, content-addressed dialog persistence.
pub struct DialogStore {
    db_path: String,
    /// Rebuilds the live GM/NPC backend (Python module-level `make_client`).
    /// Used by `from_payload` to recreate the live client lazily on load.
    client_factory: ClientFactory,
    /// In-memory cache, updated after each DB write (mirrors Python `_cache`).
    cache: Mutex<HashMap<(String, String), DialogRuntime>>,
    /// Config carrier for the embeddings purge on delete (best-effort).
    config: Arc<Config>,
}

impl DialogStore {
    /// `DialogStore(db_path, client_factory)`.
    ///
    /// `client_factory` builds a fresh backend (GM + per-NPC clients share the
    /// same factory, as in Python where `make_client` builds both). `config`
    /// carries the RAG settings used to purge embeddings on delete.
    pub fn new(
        db_path: impl Into<String>,
        client_factory: ClientFactory,
        config: Arc<Config>,
    ) -> Result<Self, StoreError> {
        let db_path = abspath(&db_path.into());
        let store = DialogStore {
            db_path,
            client_factory,
            cache: Mutex::new(HashMap::new()),
            config,
        };
        store.init_db()?;
        Ok(store)
    }

    /// Default DB path: `GM_DIALOG_DB` override, else the app-data dir
    /// (`directories`) per PORT_PLAN §3.2.
    pub fn default_db_path() -> String {
        match std::env::var("GM_DIALOG_DB") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => gml_config::config::default_data_path("gm_lab_dialogs.sqlite3"),
        }
    }

    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    // ------------------------------------------------------------------
    // connection management
    // ------------------------------------------------------------------

    /// `_connect()` — short-lived connection with the three PRAGMAs.
    fn connect(&self) -> Result<Connection, StoreError> {
        let con = Connection::open(&self.db_path)?;
        con.busy_timeout(std::time::Duration::from_millis(10_000))?;
        con.pragma_update(None, "busy_timeout", 10_000)?;
        // journal_mode returns a row; use query form.
        con.pragma_update(None, "journal_mode", "WAL")?;
        con.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(con)
    }

    /// `_init_db()`.
    fn init_db(&self) -> Result<(), StoreError> {
        if let Some(parent) = std::path::Path::new(&self.db_path).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        let con = self.connect()?;
        con.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS dialog_chats (
                guest_id TEXT NOT NULL,
                chat_id TEXT NOT NULL,
                title TEXT NOT NULL,
                preview TEXT NOT NULL,
                turn_count INTEGER NOT NULL DEFAULT 0,
                payload TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (guest_id, chat_id)
            );
            CREATE INDEX IF NOT EXISTS idx_dialog_chats_guest_updated
                ON dialog_chats(guest_id, updated_at);
            CREATE TABLE IF NOT EXISTS guest_dialog_state (
                guest_id TEXT PRIMARY KEY,
                active_chat_id TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // public API
    // ------------------------------------------------------------------

    /// `get(guest_id, chat_id=None)`.
    pub fn get(&self, guest_id: &str, chat_id: Option<&str>) -> Result<(), StoreError> {
        // The cache stores DialogRuntime by value and is not Clone (Session
        // holds Arc<dyn Backend>); callers should use `with_runtime` / the
        // chat-row accessors instead of taking ownership. This method exists for
        // API parity and validates presence.
        match chat_id {
            None => self.ensure_active(guest_id),
            Some(id) => {
                if self.chat_exists(guest_id, id)? {
                    self.ensure_cached(guest_id, id)?;
                    Ok(())
                } else {
                    Err(StoreError::ChatNotFound(id.to_string()))
                }
            }
        }
    }

    /// `get_active(guest_id)` — resolve/self-heal the active chat, creating one
    /// if none exist. Ensures the resolved runtime is cached and returns its id.
    pub fn get_active(&self, guest_id: &str) -> Result<String, StoreError> {
        let con = self.connect()?;
        if let Some(active) = active_chat_id(&con, guest_id)? {
            if chat_exists(&con, guest_id, &active)? {
                drop(con);
                self.ensure_cached(guest_id, &active)?;
                return Ok(active);
            }
        }
        if let Some(latest) = latest_chat_id(&con, guest_id)? {
            set_active_chat(&con, guest_id, &latest)?;
            con.commit_implicit()?;
            drop(con);
            self.ensure_cached(guest_id, &latest)?;
            return Ok(latest);
        }
        drop(con);
        let rt = self.create_chat(guest_id, None, None, 0, None, None, true)?;
        Ok(rt)
    }

    fn ensure_active(&self, guest_id: &str) -> Result<(), StoreError> {
        self.get_active(guest_id).map(|_| ())
    }

    /// `list_chats(guest_id)` — with active-pointer self-heal.
    pub fn list_chats(&self, guest_id: &str) -> Result<Vec<Value>, StoreError> {
        let con = self.connect()?;
        let mut stmt = con.prepare(
            "SELECT chat_id, title, preview, turn_count, created_at, updated_at
             FROM dialog_chats
             WHERE guest_id = ?1
             ORDER BY updated_at DESC, created_at DESC, chat_id DESC",
        )?;
        let rows: Vec<(String, String, String, i64, String, String)> = stmt
            .query_map([guest_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                ))
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);

        let mut active = active_chat_id(&con, guest_id)?;
        let chat_ids: std::collections::HashSet<&String> = rows.iter().map(|r| &r.0).collect();
        if !rows.is_empty() && active.as_ref().map(|a| !chat_ids.contains(a)).unwrap_or(true) {
            active = Some(rows[0].0.clone());
            set_active_chat(&con, guest_id, rows[0].0.as_str())?;
        }
        con.commit_implicit()?;

        let active_id = active.unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|(id, title, preview, turn_count, created_at, updated_at)| {
                json!({
                    "id": id,
                    "title": if title.is_empty() { DEFAULT_CHAT_TITLE.to_string() } else { title },
                    "preview": preview,
                    "turn_count": turn_count,
                    "created_at": created_at,
                    "updated_at": updated_at,
                    "active": id == active_id,
                })
            })
            .collect())
    }

    /// `active_chat_id(guest_id)` — with self-heal to latest.
    pub fn active_chat_id(&self, guest_id: &str) -> Result<Option<String>, StoreError> {
        let con = self.connect()?;
        if let Some(active) = active_chat_id(&con, guest_id)? {
            if chat_exists(&con, guest_id, &active)? {
                return Ok(Some(active));
            }
        }
        let latest = latest_chat_id(&con, guest_id)?;
        if let Some(ref l) = latest {
            set_active_chat(&con, guest_id, l)?;
            con.commit_implicit()?;
        }
        Ok(latest)
    }

    /// `create_chat(guest_id, ...)`. Returns the new chat id (the runtime is
    /// stored in the cache and persisted).
    #[allow(clippy::too_many_arguments)]
    pub fn create_chat(
        &self,
        guest_id: &str,
        session: Option<Session>,
        transcript: Option<Vec<Value>>,
        turn_count: i64,
        title: Option<&str>,
        preview: Option<&str>,
        activate: bool,
    ) -> Result<String, StoreError> {
        let chat_id = self.new_chat_id(guest_id)?;
        let mut session = session.unwrap_or_else(|| Session::new((self.client_factory)()));
        // Honor env-tuned compaction thresholds (GM_HISTORY_TOKENS, NPC_HISTORY_TOKENS, ...).
        session.compaction = CompactionThresholds::from_config(&self.config);
        let mut runtime = DialogRuntime {
            guest_id: guest_id.to_string(),
            chat_id: chat_id.clone(),
            session,
            transcript: transcript.unwrap_or_default(),
            turn_count: turn_count.max(0),
            title: {
                let t = clean_metadata_text(title.unwrap_or(""), 80);
                if t.is_empty() {
                    DEFAULT_CHAT_TITLE.to_string()
                } else {
                    t
                }
            },
            preview: clean_metadata_text(preview.unwrap_or(""), 180),
            created_at: String::new(),
            updated_at: String::new(),
        };
        self.save(&mut runtime)?;
        if activate {
            let con = self.connect()?;
            set_active_chat(&con, guest_id, &chat_id)?;
            con.commit_implicit()?;
        }
        self.cache_put(runtime);
        Ok(chat_id)
    }

    /// `save(runtime)` — upsert the row, refresh created_at/updated_at, and
    /// update the in-memory cache.
    pub fn save(&self, runtime: &mut DialogRuntime) -> Result<(), StoreError> {
        runtime.title = title_for_save(runtime);
        runtime.preview = derive_preview(runtime);
        runtime.turn_count = runtime.turn_count.max(0);
        let payload = runtime.payload_json();

        let con = self.connect()?;
        con.execute(
            "INSERT INTO dialog_chats (
                guest_id, chat_id, title, preview, turn_count,
                payload, created_at, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'), datetime('now'))
            ON CONFLICT(guest_id, chat_id) DO UPDATE SET
                title = excluded.title,
                preview = excluded.preview,
                turn_count = excluded.turn_count,
                payload = excluded.payload,
                updated_at = datetime('now')",
            rusqlite::params![
                runtime.guest_id,
                runtime.chat_id,
                runtime.title,
                runtime.preview,
                runtime.turn_count,
                payload,
            ],
        )?;
        let saved: Option<(Option<String>, Option<String>)> = con
            .query_row(
                "SELECT created_at, updated_at FROM dialog_chats
                 WHERE guest_id = ?1 AND chat_id = ?2",
                rusqlite::params![runtime.guest_id, runtime.chat_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        con.commit_implicit()?;
        if let Some((created, updated)) = saved {
            if let Some(c) = created {
                runtime.created_at = c;
            }
            if let Some(u) = updated {
                runtime.updated_at = u;
            }
        }
        Ok(())
    }

    /// Load a chat into a fresh [`DialogRuntime`] (rebuilds the live client).
    /// Mirrors `_runtime_from_payload`; callers own the result.
    pub fn load_chat(
        &self,
        guest_id: &str,
        chat_id: &str,
    ) -> Result<DialogRuntime, StoreError> {
        let con = self.connect()?;
        let row: Option<(String, Option<String>, Option<String>, Option<i64>, Option<String>, Option<String>)> =
            con.query_row(
                "SELECT payload, title, preview, turn_count, created_at, updated_at
                 FROM dialog_chats
                 WHERE guest_id = ?1 AND chat_id = ?2",
                rusqlite::params![guest_id, chat_id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()?;
        let (payload, title, preview, turn_count, created_at, updated_at) =
            row.ok_or_else(|| StoreError::ChatNotFound(chat_id.to_string()))?;
        self.runtime_from_payload(
            guest_id,
            chat_id,
            &payload,
            title.unwrap_or_default(),
            preview.unwrap_or_default(),
            created_at.unwrap_or_default(),
            updated_at.unwrap_or_default(),
            turn_count.unwrap_or(0),
        )
    }

    /// `_runtime_from_payload(...)` — parse + schema check + rebuild.
    #[allow(clippy::too_many_arguments)]
    pub fn runtime_from_payload(
        &self,
        guest_id: &str,
        chat_id: &str,
        payload: &str,
        title: String,
        preview: String,
        created_at: String,
        updated_at: String,
        _row_turn_count: i64,
    ) -> Result<DialogRuntime, StoreError> {
        let data: Value = serde_json::from_str(payload)
            .map_err(|e| StoreError::Payload(format!("json parse: {e}")))?;
        let sv = data.get("schema_version").and_then(|v| v.as_i64()).unwrap_or(0);
        if sv != SCHEMA_VERSION {
            return Err(StoreError::SchemaVersion(
                data.get("schema_version")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string()),
            ));
        }
        // Rebuild the live client + NPC factory via the make_client factory.
        let client: Arc<dyn Backend> = (self.client_factory)();
        let factory: ClientFactory = self.client_factory.clone();
        let mut session = Session::from_payload(
            data.get("session").unwrap_or(&Value::Null),
            client,
            factory,
        )
        .map_err(StoreError::Payload)?;
        // Honor env-tuned compaction thresholds on the loaded session too.
        session.compaction = CompactionThresholds::from_config(&self.config);
        let transcript = match data.get("transcript") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let turn_count = data.get("turn_count").and_then(|v| v.as_i64()).unwrap_or(0);
        Ok(DialogRuntime {
            guest_id: guest_id.to_string(),
            chat_id: chat_id.to_string(),
            session,
            transcript,
            turn_count,
            title,
            preview,
            created_at,
            updated_at,
        })
    }

    /// `activate_chat(guest_id, chat_id)` — set the active pointer if the chat
    /// exists. Returns true on success.
    pub fn activate_chat(&self, guest_id: &str, chat_id: &str) -> Result<bool, StoreError> {
        let chat_id = chat_id.trim();
        if chat_id.is_empty() {
            return Ok(false);
        }
        let con = self.connect()?;
        if !chat_exists(&con, guest_id, chat_id)? {
            return Ok(false);
        }
        set_active_chat(&con, guest_id, chat_id)?;
        con.commit_implicit()?;
        self.ensure_cached(guest_id, chat_id)?;
        Ok(true)
    }

    /// `delete_chat(guest_id, chat_id)` — hard-delete, fix the active pointer,
    /// and best-effort purge the chat's world-text embeddings.
    pub fn delete_chat(&self, guest_id: &str, chat_id: &str) -> Result<Value, StoreError> {
        let chat_id = chat_id.trim().to_string();
        if chat_id.is_empty() {
            return Ok(json!({"deleted": false, "reason": "chat_id is required"}));
        }
        // Collect embedding texts BEFORE the row is gone (best-effort).
        let embed_texts = self.chat_embedding_texts(guest_id, &chat_id);

        let con = self.connect()?;
        let tx = con.unchecked_transaction()?;
        let removed = tx.execute(
            "DELETE FROM dialog_chats WHERE guest_id = ?1 AND chat_id = ?2",
            rusqlite::params![guest_id, chat_id],
        )?;
        if removed == 0 {
            return Ok(json!({"deleted": false, "reason": "chat not found"}));
        }
        let active = active_chat_id(&tx, guest_id)?;
        let mut new_active = active.clone();
        if active.as_deref().map(|a| a.is_empty()).unwrap_or(true)
            || active.as_deref() == Some(chat_id.as_str())
        {
            new_active = latest_chat_id(&tx, guest_id)?;
            match &new_active {
                Some(na) => set_active_chat(&tx, guest_id, na)?,
                None => {
                    tx.execute(
                        "DELETE FROM guest_dialog_state WHERE guest_id = ?1",
                        [guest_id],
                    )?;
                }
            }
        }
        tx.commit()?;

        self.cache_remove(guest_id, &chat_id);
        let purged = purge_embeddings(&embed_texts, &self.config);

        Ok(json!({
            "deleted": true,
            "active_chat_id": new_active.unwrap_or_default(),
            "embeddings_purged": purged,
        }))
    }

    /// `merge_all_chats_into_scope(target_guest_id)` — fold every other guest's
    /// chats into the target scope (kept for DB compatibility). Returns moved
    /// count; clears the whole cache.
    pub fn merge_all_chats_into_scope(&self, target_guest_id: &str) -> Result<i64, StoreError> {
        let target = target_guest_id.trim().to_string();
        if target.is_empty() {
            return Err(StoreError::Other("target_guest_id is required".to_string()));
        }
        let con = self.connect()?;
        let tx = con.unchecked_transaction()?;
        let rows: Vec<(String, String, String, String, i64, String, String, String)> = {
            let mut stmt = tx.prepare(
                "SELECT guest_id, chat_id, title, preview, turn_count, payload, created_at, updated_at
                 FROM dialog_chats
                 WHERE guest_id <> ?1
                 ORDER BY updated_at ASC, created_at ASC, chat_id ASC",
            )?;
            let collected: Vec<_> = stmt
                .query_map([&target], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                        r.get(5)?,
                        r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    ))
                })?
                .collect::<Result<_, _>>()?;
            collected
        };

        let mut moved = 0i64;
        for (src_guest, chat_id, title, preview, turn_count, payload, created_at, updated_at) in rows {
            let mut target_chat_id = chat_id.clone();
            if chat_exists(&tx, &target, &target_chat_id)? {
                target_chat_id = new_chat_id(&tx, &target)?;
            }
            tx.execute(
                "INSERT INTO dialog_chats (
                    guest_id, chat_id, title, preview, turn_count,
                    payload, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    target,
                    target_chat_id,
                    title,
                    preview,
                    turn_count.max(0),
                    payload,
                    created_at,
                    updated_at,
                ],
            )?;
            tx.execute(
                "DELETE FROM dialog_chats WHERE guest_id = ?1 AND chat_id = ?2",
                rusqlite::params![src_guest, chat_id],
            )?;
            moved += 1;
        }
        tx.execute(
            "DELETE FROM guest_dialog_state WHERE guest_id <> ?1",
            [&target],
        )?;
        if let Some(active) = latest_chat_id(&tx, &target)? {
            set_active_chat(&tx, &target, &active)?;
        }
        tx.commit()?;

        self.cache_clear();
        Ok(moved)
    }

    // ------------------------------------------------------------------
    // cache + helpers
    // ------------------------------------------------------------------

    /// `_chat_embedding_texts(guest_id, chat_id)` — never raises.
    fn chat_embedding_texts(&self, guest_id: &str, chat_id: &str) -> Vec<String> {
        match self.load_chat(guest_id, chat_id) {
            Ok(mut rt) => {
                let docs = rt.session.world.retrieval_documents("player");
                docs.iter()
                    .map(|d| d.contextual_text())
                    .filter(|t| !t.trim().is_empty())
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    /// Ensure the chat is loaded into the cache (no-op if already present or
    /// missing).
    fn ensure_cached(&self, guest_id: &str, chat_id: &str) -> Result<(), StoreError> {
        {
            let cache = self.cache.lock().expect("cache mutex poisoned");
            if cache.contains_key(&(guest_id.to_string(), chat_id.to_string())) {
                return Ok(());
            }
        }
        match self.load_chat(guest_id, chat_id) {
            Ok(rt) => {
                self.cache_put(rt);
                Ok(())
            }
            Err(StoreError::ChatNotFound(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn cache_put(&self, runtime: DialogRuntime) {
        let key = (runtime.guest_id.clone(), runtime.chat_id.clone());
        self.cache
            .lock()
            .expect("cache mutex poisoned")
            .insert(key, runtime);
    }

    fn cache_remove(&self, guest_id: &str, chat_id: &str) {
        self.cache
            .lock()
            .expect("cache mutex poisoned")
            .remove(&(guest_id.to_string(), chat_id.to_string()));
    }

    fn cache_clear(&self) {
        self.cache.lock().expect("cache mutex poisoned").clear();
    }

    /// Run a closure with a mutable reference to a cached runtime, loading it
    /// into the cache first if necessary. Returns `None` if the chat is missing.
    pub fn with_runtime<T>(
        &self,
        guest_id: &str,
        chat_id: &str,
        f: impl FnOnce(&mut DialogRuntime) -> T,
    ) -> Result<Option<T>, StoreError> {
        self.ensure_cached(guest_id, chat_id)?;
        let mut cache = self.cache.lock().expect("cache mutex poisoned");
        Ok(cache
            .get_mut(&(guest_id.to_string(), chat_id.to_string()))
            .map(f))
    }

    fn new_chat_id(&self, guest_id: &str) -> Result<String, StoreError> {
        let con = self.connect()?;
        new_chat_id(&con, guest_id)
    }

    fn chat_exists(&self, guest_id: &str, chat_id: &str) -> Result<bool, StoreError> {
        let con = self.connect()?;
        chat_exists(&con, guest_id, chat_id)
    }
}

// ======================================================================
// connection-level free functions (mirror the *_with_connection helpers)
// ======================================================================

/// Implicit "commit on clean exit of the context manager" — rusqlite
/// autocommits each statement by default, so a no-op here keeps the call sites
/// readable and matches Python's `con.commit()` on `_connection()` exit.
trait CommitImplicit {
    fn commit_implicit(&self) -> Result<(), StoreError>;
}
impl CommitImplicit for Connection {
    fn commit_implicit(&self) -> Result<(), StoreError> {
        Ok(())
    }
}

fn active_chat_id(con: &Connection, guest_id: &str) -> Result<Option<String>, StoreError> {
    let row: Option<Option<String>> = con
        .query_row(
            "SELECT active_chat_id FROM guest_dialog_state WHERE guest_id = ?1",
            [guest_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(row.flatten().filter(|s| !s.is_empty()))
}

fn latest_chat_id(con: &Connection, guest_id: &str) -> Result<Option<String>, StoreError> {
    let row: Option<String> = con
        .query_row(
            "SELECT chat_id FROM dialog_chats
             WHERE guest_id = ?1
             ORDER BY updated_at DESC, created_at DESC, chat_id DESC
             LIMIT 1",
            [guest_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(row.filter(|s| !s.is_empty()))
}

fn chat_exists(con: &Connection, guest_id: &str, chat_id: &str) -> Result<bool, StoreError> {
    let row: Option<i64> = con
        .query_row(
            "SELECT 1 FROM dialog_chats WHERE guest_id = ?1 AND chat_id = ?2 LIMIT 1",
            rusqlite::params![guest_id, chat_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(row.is_some())
}

fn set_active_chat(con: &Connection, guest_id: &str, chat_id: &str) -> Result<(), StoreError> {
    con.execute(
        "INSERT INTO guest_dialog_state (guest_id, active_chat_id, created_at, updated_at)
         VALUES (?1, ?2, datetime('now'), datetime('now'))
         ON CONFLICT(guest_id) DO UPDATE SET
             active_chat_id = excluded.active_chat_id,
             updated_at = datetime('now')",
        rusqlite::params![guest_id, chat_id],
    )?;
    Ok(())
}

fn new_chat_id(con: &Connection, guest_id: &str) -> Result<String, StoreError> {
    for _ in 0..32 {
        let chat_id = token_urlsafe(12);
        if !chat_exists(con, guest_id, &chat_id)? {
            return Ok(chat_id);
        }
    }
    Err(StoreError::Other("could not allocate unique chat id".to_string()))
}

// ======================================================================
// title / preview derivation (mirror _title_for_save / _derive_preview)
// ======================================================================

fn title_for_save(runtime: &DialogRuntime) -> String {
    let title = clean_metadata_text(&runtime.title, 80);
    if !title.is_empty() {
        return title;
    }
    let derived = derive_missing_title(runtime);
    if derived.is_empty() {
        DEFAULT_CHAT_TITLE.to_string()
    } else {
        derived
    }
}

fn derive_missing_title(runtime: &DialogRuntime) -> String {
    let scene_title = clean_metadata_text(&runtime.session.world.scene.title, 80);
    if !scene_title.is_empty() {
        return scene_title;
    }
    let first_player = first_player_event_text(&runtime.transcript);
    if !first_player.is_empty() {
        return clean_metadata_text(&first_player, 80);
    }
    String::new()
}

fn derive_preview(runtime: &DialogRuntime) -> String {
    let last_event = last_transcript_text(&runtime.transcript);
    if !last_event.is_empty() {
        return clean_metadata_text(&last_event, 180);
    }
    let last_action = clean_metadata_text(&runtime.session.last_player_action, 180);
    if !last_action.is_empty() {
        return last_action;
    }
    let scene_description = clean_metadata_text(&runtime.session.world.scene.description, 180);
    if !scene_description.is_empty() {
        return scene_description;
    }
    clean_metadata_text(&runtime.title, 180)
}

fn first_player_event_text(transcript: &[Value]) -> String {
    for row in transcript {
        let event = match row.get("event") {
            Some(Value::Object(_)) => row.get("event").unwrap(),
            _ => continue,
        };
        let kind = event.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        let agent = event.get("agent").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
        if kind == "player" || agent == "player" || agent == "игрок" {
            let text = event_text(event);
            if !text.is_empty() {
                return text;
            }
        }
    }
    String::new()
}

fn last_transcript_text(transcript: &[Value]) -> String {
    for row in transcript.iter().rev() {
        let event = match row.get("event") {
            Some(Value::Object(_)) => row.get("event").unwrap(),
            _ => continue,
        };
        let text = event_text(event);
        if !text.is_empty() {
            return text;
        }
    }
    String::new()
}

fn event_text(event: &Value) -> String {
    for key in ["data", "text", "speech", "action"] {
        if let Some(Value::String(value)) = event.get(key) {
            let text = clean_metadata_text(value, 160);
            if !text.is_empty() {
                return text;
            }
        }
    }
    String::new()
}

/// `_clean_metadata_text(value, limit)` — collapse whitespace; truncate with
/// "..." using **char** counts (Python `len`/slicing on `str`).
fn clean_metadata_text(value: &str, limit: usize) -> String {
    // " ".join(value.split()) — split on any whitespace run, join with single space.
    let text: String = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_len = text.chars().count();
    if char_len <= limit {
        return text;
    }
    let keep = limit.saturating_sub(3);
    let truncated: String = text.chars().take(keep).collect();
    format!("{}...", truncated.trim_end())
}

// ======================================================================
// misc
// ======================================================================

/// Best-effort embeddings purge (never raises).
fn purge_embeddings(texts: &[String], config: &Config) -> i64 {
    if texts.is_empty() {
        return 0;
    }
    gml_rag::purge_embeddings_for_texts(texts, config)
}

/// `os.path.abspath(db_path)`.
fn abspath(path: &str) -> String {
    match std::fs::canonicalize(path) {
        Ok(p) => strip_unc(p.to_string_lossy().into_owned()),
        Err(_) => {
            // File may not exist yet; join with cwd if relative.
            let pb = std::path::Path::new(path);
            if pb.is_absolute() {
                path.to_string()
            } else if let Ok(cwd) = std::env::current_dir() {
                cwd.join(pb).to_string_lossy().into_owned()
            } else {
                path.to_string()
            }
        }
    }
}

/// Windows `canonicalize` returns a `\\?\` UNC prefix that rusqlite + tooling
/// dislike; strip it for plain paths.
fn strip_unc(p: String) -> String {
    p.strip_prefix(r"\\?\").map(|s| s.to_string()).unwrap_or(p)
}

/// `secrets.token_urlsafe(nbytes)` — base64url(no padding) of `nbytes` random
/// OS-entropy bytes. Python's `token_urlsafe(12)` yields a 16-char id.
fn token_urlsafe(nbytes: usize) -> String {
    use base64::Engine;
    let mut buf = vec![0u8; nbytes];
    getrandom::getrandom(&mut buf).expect("OS entropy for chat id");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}
