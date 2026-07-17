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
use serde_json::{json, Map, Value};

use gml_config::Config;
use gml_llm::{Backend, ConnectorId, ConnectorRegistry, ModelBinding};
use gml_orchestrator::{ClientFactory, CompactionThresholds, Session};

pub mod character_store;
pub mod chat_search;
pub mod visual_assets;
pub mod world_store;
pub use character_store::{
    CharacterBaseRef, CharacterStore, CHARACTER_ASSETS_DIR, CHARACTER_FORMAT,
};
pub use chat_search::{
    ChatSearchHit, ChatSearchPage, ChatSearchQuery, ChatSearchScope, ChatSearchSort,
};
pub use visual_assets::{DialogVisualAsset, DialogVisualAssets};
pub use world_store::{WorldStore, ASSETS_DIR as ASSETS_DIR_NAME};

/// `SCHEMA_VERSION = 1` — hard-checked on load (no migrations exist).
pub const SCHEMA_VERSION: i64 = 1;
/// `DEFAULT_CHAT_TITLE`.
pub const DEFAULT_CHAT_TITLE: &str = "Новый чат";
/// Number of completed player turns that remain available for exact rewind.
pub const MAX_REWIND_TURNS: usize = 10;

/// Errors raised by [`DialogStore`].
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("chat not found: {0}")]
    ChatNotFound(String),
    #[error("world not found: {0}")]
    WorldNotFound(String),
    #[error("character not found: {0}")]
    CharacterNotFound(String),
    #[error("unsupported schema version: {0}")]
    SchemaVersion(String),
    #[error("invalid payload: {0}")]
    Payload(String),
    #[error("turn {turn} is not available for rewind in chat {chat_id}")]
    TurnNotRewindable { chat_id: String, turn: i64 },
    #[error("chat {chat_id} changed while its history turn was running")]
    HistoryChanged { chat_id: String },
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
    /// Generated portraits and location art persisted with this history.
    pub visual_assets: DialogVisualAssets,
    /// Completed player turns with a retained pre-turn checkpoint, newest last.
    /// This is derived from `dialog_turn_checkpoints` and is not duplicated in
    /// the canonical session payload.
    pub rewindable_turns: Vec<i64>,
}

impl DialogRuntime {
    /// `_runtime_to_payload(runtime)` -> `{schema_version, turn_count, session,
    /// transcript}`.
    pub fn to_payload(&self) -> Value {
        let mut payload = json!({
            "schema_version": SCHEMA_VERSION,
            "turn_count": self.turn_count,
            "session": self.session.to_payload(),
            "transcript": Value::Array(self.transcript.clone()),
        });
        if !self.visual_assets.is_empty() {
            payload
                .as_object_mut()
                .expect("dialog payload is an object")
                .insert(
                    "visual_assets".to_string(),
                    serde_json::to_value(&self.visual_assets)
                        .expect("dialog visual assets are serializable"),
                );
        }
        payload
    }

    /// Serialize the payload exactly as `DialogStore.save` writes it:
    /// `json.dumps(..., ensure_ascii=False, separators=(",",":"))`.
    pub fn payload_json(&self) -> String {
        // serde_json default output is compact + non-ASCII-preserving, which
        // matches `separators=(",",":")` + `ensure_ascii=False` exactly.
        serde_json::to_string(&self.to_payload()).unwrap_or_default()
    }
}

/// Full pre-turn state captured before orchestration starts and persisted only
/// when that turn commits successfully. The snapshot deliberately includes the
/// connector-owned session/thread identities so an edited tail can reuse the
/// provider's unchanged prompt-cache prefix.
#[derive(Clone, Debug)]
pub struct TurnCheckpoint {
    pub turn: i64,
    pub request_id: String,
    pub player_text: String,
    snapshot_json: String,
}

impl TurnCheckpoint {
    pub fn capture(
        runtime: &DialogRuntime,
        turn: i64,
        request_id: impl Into<String>,
        player_text: impl Into<String>,
    ) -> Result<Self, StoreError> {
        if turn <= 0 || turn != runtime.turn_count.saturating_add(1) {
            return Err(StoreError::Other(format!(
                "checkpoint turn {turn} does not follow runtime turn {}",
                runtime.turn_count
            )));
        }
        Ok(Self {
            turn,
            request_id: request_id.into(),
            player_text: player_text.into(),
            snapshot_json: runtime.payload_json(),
        })
    }
}

/// History operation staged together with a replacement player turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryTurnKind {
    /// Replace the selected turn and discard its old tail only after success.
    Edit,
    /// Create a new chat from the selected pre-turn snapshot only after success.
    Branch { title: Option<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryTurnReceiptKind {
    Edit,
    Branch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryTurnReceipt {
    pub kind: HistoryTurnReceiptKind,
    pub source_turn: i64,
    pub destination_chat_id: String,
    pub player_text: String,
}

/// Owned runtime restored from a pre-turn checkpoint plus an opaque commit
/// token. Preparing this value never writes SQLite or changes the active chat.
pub struct PreparedHistoryTurn {
    pub runtime: DialogRuntime,
    pub commit: PreparedHistoryCommit,
}

/// Opaque proof tying a staged history turn to the canonical source bytes that
/// were observed before model execution. It is consumed by the atomic commit.
pub struct PreparedHistoryCommit {
    kind: PreparedHistoryKind,
    guest_id: String,
    source_chat_id: String,
    destination_chat_id: String,
    turn: i64,
    expected_source_payload: String,
    expected_checkpoint_snapshot: String,
}

impl PreparedHistoryCommit {
    pub fn destination_chat_id(&self) -> &str {
        &self.destination_chat_id
    }
}

enum PreparedHistoryKind {
    Edit,
    Branch { title: String },
}

/// `class DialogStore` — SQLite-backed, content-addressed dialog persistence.
pub struct DialogStore {
    db_path: String,
    /// Rebuilds the live GM/NPC backend (Python module-level `make_client`).
    /// Used by `from_payload` to recreate the live client lazily on load.
    client_factory: ClientFactory,
    /// Connector registry used by the new binding-aware path. `None` is kept
    /// only for compatibility with embedders/tests that still inject one
    /// legacy factory.
    connector_registry: Option<Arc<ConnectorRegistry>>,
    default_binding: ModelBinding,
    /// Serializes every read/repair/write of the shared active-chat pointer.
    /// This also makes first access atomic: concurrent requests cannot each
    /// observe an empty scope and create a different initial chat.
    active_state_lock: Mutex<()>,
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
        let sample = client_factory();
        let connector_id = ConnectorId::new(sample.connector_id())
            .or_else(|_| ConnectorId::new("mock"))
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let sample_model = sample.model();
        let configured_model = if !sample_model.trim().is_empty() {
            sample_model
        } else if !config.model.trim().is_empty() {
            config.model.clone()
        } else {
            "default".to_string()
        };
        let default_binding = ModelBinding::new(
            connector_id,
            if configured_model.trim().is_empty() {
                "default".to_string()
            } else {
                configured_model
            },
        )
        .map_err(|e| StoreError::Other(e.to_string()))?;
        let store = DialogStore {
            db_path,
            client_factory,
            connector_registry: None,
            default_binding,
            active_state_lock: Mutex::new(()),
            cache: Mutex::new(HashMap::new()),
            config,
        };
        store.init_db()?;
        Ok(store)
    }

    /// Construct a binding-aware store. Restored histories resolve their own
    /// connector before a backend is created; the process default is used only
    /// for brand-new or truly legacy unbound histories.
    pub fn with_connectors(
        db_path: impl Into<String>,
        connector_registry: Arc<ConnectorRegistry>,
        default_binding: ModelBinding,
        config: Arc<Config>,
    ) -> Result<Self, StoreError> {
        let db_path = abspath(&db_path.into());
        let default_client = connector_registry
            .create_backend(&default_binding)
            .map_err(|e| StoreError::Other(e.to_string()))?;
        let factory_registry = connector_registry.clone();
        let factory_binding = default_binding.clone();
        let client_factory: ClientFactory = Arc::new(move || {
            factory_registry
                .create_backend(&factory_binding)
                .expect("validated connector binding remains registered")
        });
        let store = DialogStore {
            db_path,
            client_factory,
            connector_registry: Some(connector_registry),
            default_binding,
            active_state_lock: Mutex::new(()),
            cache: Mutex::new(HashMap::new()),
            config,
        };
        drop(default_client);
        store.init_db()?;
        Ok(store)
    }

    pub fn default_binding(&self) -> &ModelBinding {
        &self.default_binding
    }

    pub fn connector_registry(&self) -> Option<Arc<ConnectorRegistry>> {
        self.connector_registry.clone()
    }

    /// Build one live backend and the same-connector factory used by its NPC
    /// and generator children.
    pub fn clients_for_binding(
        &self,
        binding: &ModelBinding,
    ) -> Result<(Arc<dyn Backend>, ClientFactory, ModelBinding), StoreError> {
        self.client_bundle(binding)
    }

    fn client_bundle(
        &self,
        requested: &ModelBinding,
    ) -> Result<(Arc<dyn Backend>, ClientFactory, ModelBinding), StoreError> {
        let binding = self.normalize_binding(requested)?;
        if let Some(registry) = &self.connector_registry {
            let client = registry
                .create_backend(&binding)
                .map_err(|error| StoreError::Other(error.to_string()))?;
            let factory_registry = registry.clone();
            let factory_binding = binding.clone();
            let factory: ClientFactory = Arc::new(move || {
                factory_registry
                    .create_backend(&factory_binding)
                    .expect("validated connector binding remains registered")
            });
            return Ok((client, factory, binding));
        }

        let legacy = self.client_factory.clone();
        let model = binding.model_id().to_string();
        let client = legacy();
        client.set_model(&model);
        let factory_model = model.clone();
        let factory: ClientFactory = Arc::new(move || {
            let client = legacy();
            client.set_model(&factory_model);
            client
        });
        Ok((client, factory, binding))
    }

    fn normalize_binding(&self, requested: &ModelBinding) -> Result<ModelBinding, StoreError> {
        let Some(registry) = &self.connector_registry else {
            return Ok(requested.clone());
        };
        if registry.connector(requested.connector_id()).is_some() {
            return Ok(requested.clone());
        }
        Err(StoreError::Other(format!(
            "connector is not registered: {}",
            requested.connector_id().as_str()
        )))
    }

    fn binding_for_payload(&self, session_payload: &Value) -> Result<ModelBinding, StoreError> {
        if let Some(canonical) = session_payload.get("model_binding") {
            let binding: ModelBinding = serde_json::from_value(canonical.clone())
                .map_err(|error| StoreError::Payload(format!("invalid model_binding: {error}")))?;
            return self.normalize_binding(&binding);
        }
        let parsed = gml_orchestrator::session_payload::model_binding_from_payload(
            session_payload,
            &self.default_binding,
        );
        let Some(registry) = &self.connector_registry else {
            return Ok(parsed);
        };
        if registry.connector(parsed.connector_id()).is_some() {
            return Ok(parsed);
        }

        // Canonical bindings are authoritative: a missing connector is a loud
        // configuration error. Only pre-binding payloads receive legacy name
        // migration.
        let legacy_backend = session_payload
            .get("client_backend")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let legacy_model = session_payload
            .get("client_model")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        let migrated = if legacy_model.is_empty() {
            self.default_binding.clone()
        } else if let Some(connector_id) = registry.resolve_legacy_backend(legacy_backend) {
            ModelBinding::new(connector_id, legacy_model)
                .map_err(|error| StoreError::Other(error.to_string()))?
        } else {
            self.default_binding.clone()
        };
        self.normalize_binding(&migrated)
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
            CREATE TABLE IF NOT EXISTS dialog_turn_checkpoints (
                guest_id TEXT NOT NULL,
                chat_id TEXT NOT NULL,
                turn_no INTEGER NOT NULL,
                request_id TEXT NOT NULL,
                player_text TEXT NOT NULL,
                snapshot TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (guest_id, chat_id, turn_no)
            );
            CREATE INDEX IF NOT EXISTS idx_dialog_turn_checkpoints_chat_turn
                ON dialog_turn_checkpoints(guest_id, chat_id, turn_no DESC);
            CREATE TABLE IF NOT EXISTS dialog_history_turn_receipts (
                guest_id TEXT NOT NULL,
                source_chat_id TEXT NOT NULL,
                request_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                source_turn INTEGER NOT NULL,
                destination_chat_id TEXT NOT NULL,
                player_text TEXT NOT NULL,
                created_at TEXT NOT NULL,
                PRIMARY KEY (guest_id, source_chat_id, request_id)
            );
            CREATE INDEX IF NOT EXISTS idx_dialog_history_turn_receipts_destination
                ON dialog_history_turn_receipts(guest_id, destination_chat_id);
            CREATE TABLE IF NOT EXISTS architect_chats (
                kind TEXT NOT NULL,
                package_id TEXT NOT NULL,
                state TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (kind, package_id)
            );
            "#,
        )?;
        chat_search::initialize(&con)?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // architect chats (world/story architect conversations)
    // ------------------------------------------------------------------
    //
    // The architect conversation is a WORKING DIALOG (like GM chats), not
    // package content — it lives here in SQLite, keyed by (kind, package_id),
    // never inside the portable world/story package. Worlds/stories are global,
    // so there is no guest scope.

    /// Read the architect conversation for a package (`kind` = "world"|"story").
    /// `Ok(None)` when none has been stored yet.
    pub fn get_architect_chat(
        &self,
        kind: &str,
        package_id: &str,
    ) -> Result<Option<Value>, StoreError> {
        let con = self.connect()?;
        let raw: Option<String> = con
            .query_row(
                "SELECT state FROM architect_chats WHERE kind = ?1 AND package_id = ?2",
                rusqlite::params![kind, package_id],
                |row| row.get(0),
            )
            .optional()?;
        match raw {
            None => Ok(None),
            Some(text) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|e| StoreError::Payload(format!("parse architect chat: {e}"))),
        }
    }

    /// Upsert the architect conversation for a package.
    pub fn set_architect_chat(
        &self,
        kind: &str,
        package_id: &str,
        state: &Value,
    ) -> Result<(), StoreError> {
        let body = serde_json::to_string(state)
            .map_err(|e| StoreError::Payload(format!("serialize architect chat: {e}")))?;
        let con = self.connect()?;
        con.execute(
            "INSERT INTO architect_chats (kind, package_id, state, updated_at)
             VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(kind, package_id)
             DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
            rusqlite::params![kind, package_id, body],
        )?;
        Ok(())
    }

    /// Delete the architect conversation for a package (no-op when absent) —
    /// called when the package itself is deleted.
    pub fn delete_architect_chat(&self, kind: &str, package_id: &str) -> Result<(), StoreError> {
        let con = self.connect()?;
        con.execute(
            "DELETE FROM architect_chats WHERE kind = ?1 AND package_id = ?2",
            rusqlite::params![kind, package_id],
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
        let _active_guard = self
            .active_state_lock
            .lock()
            .expect("active-state mutex poisoned");
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
        // The active-state lock is already held, so create without activating
        // through the public path and install the pointer ourselves.
        let chat_id = self.create_chat(guest_id, None, None, 0, None, None, false)?;
        let con = self.connect()?;
        set_active_chat(&con, guest_id, &chat_id)?;
        con.commit_implicit()?;
        Ok(chat_id)
    }

    fn ensure_active(&self, guest_id: &str) -> Result<(), StoreError> {
        self.get_active(guest_id).map(|_| ())
    }

    /// `list_chats(guest_id)` — with active-pointer self-heal.
    pub fn list_chats(&self, guest_id: &str) -> Result<Vec<Value>, StoreError> {
        let _active_guard = self
            .active_state_lock
            .lock()
            .expect("active-state mutex poisoned");
        let con = self.connect()?;
        let mut stmt = con.prepare(
            "SELECT chat_id, title, preview, turn_count, payload, created_at, updated_at
             FROM dialog_chats
             WHERE guest_id = ?1
             ORDER BY updated_at DESC, created_at DESC, chat_id DESC",
        )?;
        let rows: Vec<(String, String, String, i64, String, String, String)> = stmt
            .query_map([guest_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                    row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    row.get::<_, Option<String>>(6)?.unwrap_or_default(),
                ))
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);

        let mut active = active_chat_id(&con, guest_id)?;
        let chat_ids: std::collections::HashSet<&String> = rows.iter().map(|r| &r.0).collect();
        if !rows.is_empty()
            && active
                .as_ref()
                .map(|a| !chat_ids.contains(a))
                .unwrap_or(true)
        {
            active = Some(rows[0].0.clone());
            set_active_chat(&con, guest_id, rows[0].0.as_str())?;
        }
        con.commit_implicit()?;

        let active_id = active.unwrap_or_default();
        Ok(rows
            .into_iter()
            .map(|(id, title, preview, turn_count, payload, created_at, updated_at)| {
                let active = id == active_id;
                let meta = chat_search::metadata_from_payload(&payload);
                json!({
                    "id": id,
                    "title": if title.is_empty() { DEFAULT_CHAT_TITLE.to_string() } else { title },
                    "preview": preview,
                    "turn_count": turn_count,
                    "story_id": meta.story_id,
                    "story_title": meta.story_title,
                    "world_id": meta.world_id,
                    "world_title": meta.world_title,
                    "character_id": meta.character_id,
                    "character_name": meta.character_name,
                    "kind": meta.kind,
                    "created_at": created_at,
                    "updated_at": updated_at,
                    "active": active,
                })
            })
            .collect())
    }

    /// Search chat metadata and player-facing transcript text for one guest.
    ///
    /// The FTS index is derived from `dialog_chats`, guest-scoped, and excludes
    /// hidden reasoning/tool payloads by construction. HTTP handlers should run
    /// this synchronous SQLite method in `spawn_blocking`.
    pub fn search_chats(
        &self,
        guest_id: &str,
        query: &ChatSearchQuery,
    ) -> Result<ChatSearchPage, StoreError> {
        let con = self.connect()?;
        chat_search::search(&con, guest_id, query)
    }

    /// `active_chat_id(guest_id)` — with self-heal to latest.
    pub fn active_chat_id(&self, guest_id: &str) -> Result<Option<String>, StoreError> {
        let _active_guard = self
            .active_state_lock
            .lock()
            .expect("active-state mutex poisoned");
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
        self.create_chat_with_binding(
            guest_id, session, transcript, turn_count, title, preview, activate, None,
        )
    }

    /// Binding-aware chat creation. `binding` is accepted only at creation;
    /// existing histories never expose an operation that replaces it.
    #[allow(clippy::too_many_arguments)]
    pub fn create_chat_with_binding(
        &self,
        guest_id: &str,
        session: Option<Session>,
        transcript: Option<Vec<Value>>,
        turn_count: i64,
        title: Option<&str>,
        preview: Option<&str>,
        activate: bool,
        binding: Option<ModelBinding>,
    ) -> Result<String, StoreError> {
        let _active_guard = activate.then(|| {
            self.active_state_lock
                .lock()
                .expect("active-state mutex poisoned")
        });
        let chat_id = self.new_chat_id(guest_id)?;
        let binding = binding.unwrap_or_else(|| self.default_binding.clone());
        let mut session = match session {
            Some(mut session) => {
                if session.model_binding().connector_id() != binding.connector_id() {
                    return Err(StoreError::Other(
                        "session connector does not match requested chat connector".to_string(),
                    ));
                }
                if session.model_binding().model_id() != binding.model_id() {
                    session.set_model_for_all_clients(binding.model_id());
                }
                session
            }
            None => {
                let (client, factory, binding) = self.client_bundle(&binding)?;
                Session::new_bound(client, factory, binding)
            }
        };
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
            visual_assets: DialogVisualAssets::default(),
            rewindable_turns: Vec::new(),
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

    /// `save(runtime)` — upsert the row and refresh created_at/updated_at.
    ///
    /// This method intentionally does not touch the in-memory cache: callers may
    /// invoke it while holding a cached runtime through [`DialogStore::with_runtime`].
    /// Use [`DialogStore::save_owned`] when saving a fresh owned runtime that must
    /// replace a previously cached copy.
    pub fn save(&self, runtime: &mut DialogRuntime) -> Result<(), StoreError> {
        let con = self.connect()?;
        let tx = con.unchecked_transaction()?;
        self.save_in_transaction(&tx, runtime)?;
        runtime.rewindable_turns = checkpoint_turns(&tx, &runtime.guest_id, &runtime.chat_id)?;
        tx.commit()?;
        Ok(())
    }

    fn save_in_transaction(
        &self,
        tx: &rusqlite::Transaction<'_>,
        runtime: &mut DialogRuntime,
    ) -> Result<(), StoreError> {
        runtime.title = title_for_save(runtime);
        runtime.preview = derive_preview(runtime);
        runtime.turn_count = runtime.turn_count.max(0);
        let payload = runtime.payload_json();

        tx.execute(
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
        let saved: Option<(Option<String>, Option<String>)> = tx
            .query_row(
                "SELECT created_at, updated_at FROM dialog_chats
                 WHERE guest_id = ?1 AND chat_id = ?2",
                rusqlite::params![runtime.guest_id, runtime.chat_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((created, updated)) = saved {
            if let Some(c) = created {
                runtime.created_at = c;
            }
            if let Some(u) = updated {
                runtime.updated_at = u;
            }
        }
        let document = chat_search::ChatSearchDocument::from_payload(
            &runtime.guest_id,
            &runtime.chat_id,
            &runtime.title,
            &runtime.preview,
            runtime.turn_count,
            &payload,
            &runtime.created_at,
            &runtime.updated_at,
        );
        chat_search::upsert_document(tx, &document)?;
        Ok(())
    }

    /// Save an owned runtime and replace any cached copy with the saved value.
    ///
    /// The streamed `/turn` path mutates a runtime loaded outside `with_runtime`.
    /// Without this replacement, later `/state`, `/debug`, and `/transcript`
    /// reads keep serving the older cached runtime until process restart.
    pub fn save_owned(&self, mut runtime: DialogRuntime) -> Result<(), StoreError> {
        self.save(&mut runtime)?;
        self.cache_put(runtime);
        Ok(())
    }

    /// Atomically commit a completed turn together with its exact pre-turn
    /// checkpoint, then retain only the newest [`MAX_REWIND_TURNS`] turns.
    pub fn save_owned_with_checkpoint(
        &self,
        mut runtime: DialogRuntime,
        checkpoint: TurnCheckpoint,
    ) -> Result<(), StoreError> {
        if checkpoint.turn != runtime.turn_count {
            return Err(StoreError::Other(format!(
                "checkpoint turn {} does not match completed runtime turn {}",
                checkpoint.turn, runtime.turn_count
            )));
        }

        let con = self.connect()?;
        let tx = con.unchecked_transaction()?;
        self.save_in_transaction(&tx, &mut runtime)?;
        // Fail closed if a caller ever commits after an out-of-band restore:
        // no checkpoint may describe a tail beyond the canonical runtime.
        tx.execute(
            "DELETE FROM dialog_turn_checkpoints
             WHERE guest_id = ?1 AND chat_id = ?2 AND turn_no > ?3",
            rusqlite::params![runtime.guest_id, runtime.chat_id, runtime.turn_count],
        )?;
        upsert_turn_checkpoint(&tx, &runtime, &checkpoint)?;
        prune_turn_checkpoints(&tx, &runtime.guest_id, &runtime.chat_id)?;
        runtime.rewindable_turns = checkpoint_turns(&tx, &runtime.guest_id, &runtime.chat_id)?;
        tx.commit()?;
        self.cache_put(runtime);
        Ok(())
    }

    /// Load a chat into a fresh [`DialogRuntime`] (rebuilds the live client).
    /// Mirrors `_runtime_from_payload`; callers own the result.
    pub fn load_chat(&self, guest_id: &str, chat_id: &str) -> Result<DialogRuntime, StoreError> {
        let con = self.connect()?;
        // The sqlite row is a wide column tuple; a type alias would not improve
        // readability of this single decode site.
        #[allow(clippy::type_complexity)]
        let row: Option<(
            String,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<String>,
            Option<String>,
        )> = con
            .query_row(
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
        let mut runtime = self.runtime_from_payload(
            guest_id,
            chat_id,
            &payload,
            title.unwrap_or_default(),
            preview.unwrap_or_default(),
            created_at.unwrap_or_default(),
            updated_at.unwrap_or_default(),
            turn_count.unwrap_or(0),
        )?;
        runtime.rewindable_turns = checkpoint_turns(&con, guest_id, chat_id)?;
        Ok(runtime)
    }

    /// Resolve a previously committed staged edit/branch request. Receipts are
    /// written in the same transaction as the completed turn, so their presence
    /// is a durable idempotency proof even when the final SSE frame was lost.
    pub fn history_turn_receipt(
        &self,
        guest_id: &str,
        source_chat_id: &str,
        request_id: &str,
    ) -> Result<Option<HistoryTurnReceipt>, StoreError> {
        let con = self.connect()?;
        let row: Option<(String, i64, String, String)> = con
            .query_row(
                "SELECT kind, source_turn, destination_chat_id, player_text
                 FROM dialog_history_turn_receipts
                 WHERE guest_id = ?1 AND source_chat_id = ?2 AND request_id = ?3",
                rusqlite::params![guest_id, source_chat_id, request_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;
        row.map(|(kind, source_turn, destination_chat_id, player_text)| {
            let kind = match kind.as_str() {
                "edit" => HistoryTurnReceiptKind::Edit,
                "branch" => HistoryTurnReceiptKind::Branch,
                other => {
                    return Err(StoreError::Payload(format!(
                        "invalid history receipt kind: {other}"
                    )))
                }
            };
            Ok(HistoryTurnReceipt {
                kind,
                source_turn,
                destination_chat_id,
                player_text,
            })
        })
        .transpose()
    }

    /// Restore a retained pre-turn snapshot into an owned runtime without
    /// mutating the source chat. The returned commit token must be consumed by
    /// [`DialogStore::commit_prepared_history_turn`] after model success.
    pub fn prepare_history_turn(
        &self,
        guest_id: &str,
        source_chat_id: &str,
        turn: i64,
        kind: HistoryTurnKind,
    ) -> Result<PreparedHistoryTurn, StoreError> {
        if turn <= 0 {
            return Err(StoreError::TurnNotRewindable {
                chat_id: source_chat_id.to_string(),
                turn,
            });
        }

        let con = self.connect()?;
        #[allow(clippy::type_complexity)]
        let row: Option<(String, String, String, String, String, String, i64)> = con
            .query_row(
                "SELECT cp.snapshot, c.payload, c.title, c.preview,
                        c.created_at, c.updated_at, c.turn_count
                 FROM dialog_turn_checkpoints cp
                 JOIN dialog_chats c
                   ON c.guest_id = cp.guest_id AND c.chat_id = cp.chat_id
                 WHERE cp.guest_id = ?1 AND cp.chat_id = ?2 AND cp.turn_no = ?3",
                rusqlite::params![guest_id, source_chat_id, turn],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                        row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    ))
                },
            )
            .optional()?;
        let Some((
            snapshot,
            source_payload,
            source_title,
            source_preview,
            source_created_at,
            source_updated_at,
            source_turn_count,
        )) = row
        else {
            return if chat_exists(&con, guest_id, source_chat_id)? {
                Err(StoreError::TurnNotRewindable {
                    chat_id: source_chat_id.to_string(),
                    turn,
                })
            } else {
                Err(StoreError::ChatNotFound(source_chat_id.to_string()))
            };
        };

        let (destination_chat_id, title, preview, created_at, updated_at, prepared_kind) =
            match kind {
                HistoryTurnKind::Edit => (
                    source_chat_id.to_string(),
                    source_title,
                    source_preview,
                    source_created_at,
                    source_updated_at,
                    PreparedHistoryKind::Edit,
                ),
                HistoryTurnKind::Branch { title } => {
                    let destination_chat_id = new_chat_id(&con, guest_id)?;
                    let requested_title =
                        clean_metadata_text(title.as_deref().unwrap_or_default(), 80);
                    let branch_title = if requested_title.is_empty() {
                        clean_metadata_text(&format!("{source_title} — ветка"), 80)
                    } else {
                        requested_title
                    };
                    (
                        destination_chat_id,
                        branch_title.clone(),
                        String::new(),
                        String::new(),
                        String::new(),
                        PreparedHistoryKind::Branch {
                            title: branch_title,
                        },
                    )
                }
            };

        let mut runtime = self.runtime_from_payload(
            guest_id,
            &destination_chat_id,
            &snapshot,
            title,
            preview,
            created_at,
            updated_at,
            source_turn_count,
        )?;
        runtime.rewindable_turns = checkpoint_turns(&con, guest_id, source_chat_id)?
            .into_iter()
            .filter(|checkpoint_turn| *checkpoint_turn < turn)
            .collect();
        if matches!(prepared_kind, PreparedHistoryKind::Branch { .. }) {
            runtime.session.rotate_provider_identities_for_branch();
        }

        Ok(PreparedHistoryTurn {
            runtime,
            commit: PreparedHistoryCommit {
                kind: prepared_kind,
                guest_id: guest_id.to_string(),
                source_chat_id: source_chat_id.to_string(),
                destination_chat_id,
                turn,
                expected_source_payload: source_payload,
                expected_checkpoint_snapshot: snapshot,
            },
        })
    }

    /// Atomically publish a successful edit/branch turn. Until this transaction
    /// commits, the source chat and active-chat pointer remain byte-identical to
    /// the state observed by [`DialogStore::prepare_history_turn`].
    pub fn commit_prepared_history_turn(
        &self,
        mut runtime: DialogRuntime,
        checkpoint: TurnCheckpoint,
        prepared: PreparedHistoryCommit,
    ) -> Result<(), StoreError> {
        if checkpoint.turn != prepared.turn || checkpoint.turn != runtime.turn_count {
            return Err(StoreError::Other(format!(
                "checkpoint turn {} does not match prepared history turn {} and runtime turn {}",
                checkpoint.turn, prepared.turn, runtime.turn_count
            )));
        }
        if runtime.guest_id != prepared.guest_id
            || runtime.chat_id != prepared.destination_chat_id
            || matches!(prepared.kind, PreparedHistoryKind::Edit)
                && runtime.chat_id != prepared.source_chat_id
        {
            return Err(StoreError::Other(
                "prepared history destination does not match runtime".to_string(),
            ));
        }

        let _active_guard = self
            .active_state_lock
            .lock()
            .expect("active-state mutex poisoned");
        let con = self.connect()?;
        let tx = con.unchecked_transaction()?;
        let observed: Option<(String, String)> = tx
            .query_row(
                "SELECT c.payload, cp.snapshot
                 FROM dialog_chats c
                 JOIN dialog_turn_checkpoints cp
                   ON cp.guest_id = c.guest_id AND cp.chat_id = c.chat_id
                 WHERE c.guest_id = ?1 AND c.chat_id = ?2 AND cp.turn_no = ?3",
                rusqlite::params![runtime.guest_id, prepared.source_chat_id, prepared.turn],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((source_payload, checkpoint_snapshot)) = observed else {
            return Err(StoreError::HistoryChanged {
                chat_id: prepared.source_chat_id,
            });
        };
        if source_payload != prepared.expected_source_payload
            || checkpoint_snapshot != prepared.expected_checkpoint_snapshot
        {
            return Err(StoreError::HistoryChanged {
                chat_id: prepared.source_chat_id,
            });
        }

        let receipt_kind = match &prepared.kind {
            PreparedHistoryKind::Edit => "edit",
            PreparedHistoryKind::Branch { .. } => "branch",
        };

        match prepared.kind {
            PreparedHistoryKind::Edit => {
                self.save_in_transaction(&tx, &mut runtime)?;
                tx.execute(
                    "DELETE FROM dialog_turn_checkpoints
                     WHERE guest_id = ?1 AND chat_id = ?2 AND turn_no >= ?3",
                    rusqlite::params![runtime.guest_id, runtime.chat_id, prepared.turn],
                )?;
                tx.execute(
                    "DELETE FROM dialog_history_turn_receipts
                     WHERE guest_id = ?1 AND source_chat_id = ?2
                       AND destination_chat_id = ?2 AND source_turn >= ?3",
                    rusqlite::params![runtime.guest_id, runtime.chat_id, prepared.turn],
                )?;
            }
            PreparedHistoryKind::Branch { title } => {
                if chat_exists(&tx, &runtime.guest_id, &runtime.chat_id)? {
                    return Err(StoreError::HistoryChanged {
                        chat_id: prepared.source_chat_id,
                    });
                }
                let inherited_checkpoints = load_inherited_branch_checkpoints(
                    &tx,
                    &runtime.guest_id,
                    &prepared.source_chat_id,
                    prepared.turn,
                )?;
                self.save_in_transaction(&tx, &mut runtime)?;
                for inherited in inherited_checkpoints {
                    let mut checkpoint_runtime = self.runtime_from_payload(
                        &runtime.guest_id,
                        &runtime.chat_id,
                        &inherited.snapshot,
                        title.clone(),
                        String::new(),
                        String::new(),
                        String::new(),
                        prepared.turn.saturating_sub(1),
                    )?;
                    checkpoint_runtime
                        .session
                        .rotate_provider_identities_for_branch();
                    insert_inherited_checkpoint(
                        &tx,
                        &runtime.guest_id,
                        &runtime.chat_id,
                        inherited,
                        checkpoint_runtime.payload_json(),
                    )?;
                }
                set_active_chat(&tx, &runtime.guest_id, &runtime.chat_id)?;
            }
        }

        upsert_turn_checkpoint(&tx, &runtime, &checkpoint)?;
        tx.execute(
            "INSERT INTO dialog_history_turn_receipts (
                guest_id, source_chat_id, request_id, kind, source_turn,
                destination_chat_id, player_text, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))",
            rusqlite::params![
                &runtime.guest_id,
                &prepared.source_chat_id,
                &checkpoint.request_id,
                receipt_kind,
                prepared.turn,
                &runtime.chat_id,
                &checkpoint.player_text,
            ],
        )?;
        prune_turn_checkpoints(&tx, &runtime.guest_id, &runtime.chat_id)?;
        runtime.rewindable_turns = checkpoint_turns(&tx, &runtime.guest_id, &runtime.chat_id)?;
        tx.commit()?;
        self.cache_put(runtime);
        Ok(())
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
        let sv = data
            .get("schema_version")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if sv != SCHEMA_VERSION {
            return Err(StoreError::SchemaVersion(
                data.get("schema_version")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "null".to_string()),
            ));
        }
        // Resolve the persisted binding before constructing a backend. This is
        // the invariant that prevents a process-level default from silently
        // changing the connector of an existing history after restart.
        let session_payload = data.get("session").unwrap_or(&Value::Null);
        let binding = self.binding_for_payload(session_payload)?;
        let (client, factory, binding) = self.client_bundle(&binding)?;
        let mut session = Session::from_payload_bound(session_payload, client, factory, binding)
            .map_err(StoreError::Payload)?;
        // Honor env-tuned compaction thresholds on the loaded session too.
        session.compaction = CompactionThresholds::from_config(&self.config);
        let transcript = match data.get("transcript") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let turn_count = data.get("turn_count").and_then(|v| v.as_i64()).unwrap_or(0);
        let visual_assets = data
            .get("visual_assets")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| StoreError::Payload(format!("invalid visual_assets: {error}")))?
            .unwrap_or_default();
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
            visual_assets,
            rewindable_turns: Vec::new(),
        })
    }

    /// Restore the exact state immediately before `turn`, discard the edited
    /// tail and every checkpoint belonging to that tail, and replace the cache
    /// only after the SQLite transaction commits.
    pub fn rewind_chat_to_turn(
        &self,
        guest_id: &str,
        chat_id: &str,
        turn: i64,
    ) -> Result<(), StoreError> {
        if turn <= 0 {
            return Err(StoreError::TurnNotRewindable {
                chat_id: chat_id.to_string(),
                turn,
            });
        }

        let con = self.connect()?;
        let tx = con.unchecked_transaction()?;
        let row: Option<(String, String, String, String, String, i64)> = tx
            .query_row(
                "SELECT cp.snapshot, c.title, c.preview, c.created_at, c.updated_at, c.turn_count
                 FROM dialog_turn_checkpoints cp
                 JOIN dialog_chats c
                   ON c.guest_id = cp.guest_id AND c.chat_id = cp.chat_id
                 WHERE cp.guest_id = ?1 AND cp.chat_id = ?2 AND cp.turn_no = ?3",
                rusqlite::params![guest_id, chat_id, turn],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                        row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                        row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                    ))
                },
            )
            .optional()?;
        let Some((snapshot, title, preview, created_at, updated_at, row_turn_count)) = row else {
            return if chat_exists(&tx, guest_id, chat_id)? {
                Err(StoreError::TurnNotRewindable {
                    chat_id: chat_id.to_string(),
                    turn,
                })
            } else {
                Err(StoreError::ChatNotFound(chat_id.to_string()))
            };
        };

        let mut runtime = self.runtime_from_payload(
            guest_id,
            chat_id,
            &snapshot,
            title,
            preview,
            created_at,
            updated_at,
            row_turn_count,
        )?;
        tx.execute(
            "DELETE FROM dialog_turn_checkpoints
             WHERE guest_id = ?1 AND chat_id = ?2 AND turn_no >= ?3",
            rusqlite::params![guest_id, chat_id, turn],
        )?;
        self.save_in_transaction(&tx, &mut runtime)?;
        runtime.rewindable_turns = checkpoint_turns(&tx, guest_id, chat_id)?;
        tx.commit()?;
        self.cache_put(runtime);
        Ok(())
    }

    /// Create and activate an independent chat from the state immediately
    /// before `turn`. Older retained checkpoints are copied so the new branch
    /// keeps its own rewind window. Every copied snapshot receives branch-owned
    /// provider identities so the source and branch can never share mutable
    /// conversation or prompt-cache scopes, including after a later rewind.
    pub fn branch_chat_from_turn(
        &self,
        guest_id: &str,
        source_chat_id: &str,
        turn: i64,
        title: Option<&str>,
    ) -> Result<String, StoreError> {
        if turn <= 0 {
            return Err(StoreError::TurnNotRewindable {
                chat_id: source_chat_id.to_string(),
                turn,
            });
        }
        let _active_guard = self
            .active_state_lock
            .lock()
            .expect("active-state mutex poisoned");
        let con = self.connect()?;
        let tx = con.unchecked_transaction()?;
        let row: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT cp.snapshot, c.title, c.turn_count
                 FROM dialog_turn_checkpoints cp
                 JOIN dialog_chats c
                   ON c.guest_id = cp.guest_id AND c.chat_id = cp.chat_id
                 WHERE cp.guest_id = ?1 AND cp.chat_id = ?2 AND cp.turn_no = ?3",
                rusqlite::params![guest_id, source_chat_id, turn],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                        row.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    ))
                },
            )
            .optional()?;
        let Some((snapshot, source_title, source_turn_count)) = row else {
            return if chat_exists(&tx, guest_id, source_chat_id)? {
                Err(StoreError::TurnNotRewindable {
                    chat_id: source_chat_id.to_string(),
                    turn,
                })
            } else {
                Err(StoreError::ChatNotFound(source_chat_id.to_string()))
            };
        };

        let chat_id = new_chat_id(&tx, guest_id)?;
        let requested_title = clean_metadata_text(title.unwrap_or_default(), 80);
        let branch_title = if requested_title.is_empty() {
            clean_metadata_text(&format!("{source_title} — ветка"), 80)
        } else {
            requested_title
        };
        let mut runtime = self.runtime_from_payload(
            guest_id,
            &chat_id,
            &snapshot,
            branch_title.clone(),
            String::new(),
            String::new(),
            String::new(),
            source_turn_count,
        )?;
        runtime.session.rotate_provider_identities_for_branch();

        let inherited_checkpoints =
            load_inherited_branch_checkpoints(&tx, guest_id, source_chat_id, turn)?;

        self.save_in_transaction(&tx, &mut runtime)?;
        for inherited in inherited_checkpoints {
            let mut checkpoint_runtime = self.runtime_from_payload(
                guest_id,
                &chat_id,
                &inherited.snapshot,
                branch_title.clone(),
                String::new(),
                String::new(),
                String::new(),
                source_turn_count,
            )?;
            checkpoint_runtime
                .session
                .rotate_provider_identities_for_branch();
            insert_inherited_checkpoint(
                &tx,
                guest_id,
                &chat_id,
                inherited,
                checkpoint_runtime.payload_json(),
            )?;
        }
        runtime.rewindable_turns = checkpoint_turns(&tx, guest_id, &chat_id)?;
        set_active_chat(&tx, guest_id, &chat_id)?;
        tx.commit()?;
        self.cache_put(runtime);
        Ok(chat_id)
    }

    /// `activate_chat(guest_id, chat_id)` — set the active pointer if the chat
    /// exists. Returns true on success.
    pub fn activate_chat(&self, guest_id: &str, chat_id: &str) -> Result<bool, StoreError> {
        let chat_id = chat_id.trim();
        if chat_id.is_empty() {
            return Ok(false);
        }
        let _active_guard = self
            .active_state_lock
            .lock()
            .expect("active-state mutex poisoned");
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
        let _active_guard = self
            .active_state_lock
            .lock()
            .expect("active-state mutex poisoned");
        // Collect embedding texts + world scope BEFORE the row is gone
        // (best-effort). The scope routes the purge to this chat's cache file
        // only (per-world, or global for `None`).
        let (embed_texts, world_id) = self.chat_embedding_scope(guest_id, &chat_id);

        let con = self.connect()?;
        let tx = con.unchecked_transaction()?;
        let removed = tx.execute(
            "DELETE FROM dialog_chats WHERE guest_id = ?1 AND chat_id = ?2",
            rusqlite::params![guest_id, chat_id],
        )?;
        if removed == 0 {
            return Ok(json!({"deleted": false, "reason": "chat not found"}));
        }
        tx.execute(
            "DELETE FROM dialog_turn_checkpoints WHERE guest_id = ?1 AND chat_id = ?2",
            rusqlite::params![guest_id, chat_id],
        )?;
        tx.execute(
            "DELETE FROM dialog_history_turn_receipts
             WHERE guest_id = ?1
               AND (source_chat_id = ?2 OR destination_chat_id = ?2)",
            rusqlite::params![guest_id, chat_id],
        )?;
        chat_search::delete_document(&tx, guest_id, &chat_id)?;
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
        let purged = purge_embeddings(&embed_texts, &self.config, world_id.as_deref());

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
        let _active_guard = self
            .active_state_lock
            .lock()
            .expect("active-state mutex poisoned");
        let con = self.connect()?;
        let tx = con.unchecked_transaction()?;
        #[allow(clippy::type_complexity)]
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
        for (src_guest, chat_id, title, preview, turn_count, payload, created_at, updated_at) in
            rows
        {
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
            let document = chat_search::ChatSearchDocument::from_payload(
                &target,
                &target_chat_id,
                &title,
                &preview,
                turn_count,
                &payload,
                &created_at,
                &updated_at,
            );
            chat_search::upsert_document(&tx, &document)?;
            tx.execute(
                "INSERT INTO dialog_turn_checkpoints (
                    guest_id, chat_id, turn_no, request_id, player_text, snapshot, created_at
                 )
                 SELECT ?3, ?4, turn_no, request_id, player_text, snapshot, created_at
                 FROM dialog_turn_checkpoints
                 WHERE guest_id = ?1 AND chat_id = ?2",
                rusqlite::params![src_guest, chat_id, target, target_chat_id],
            )?;
            tx.execute(
                "DELETE FROM dialog_turn_checkpoints WHERE guest_id = ?1 AND chat_id = ?2",
                rusqlite::params![src_guest, chat_id],
            )?;
            chat_search::delete_document(&tx, &src_guest, &chat_id)?;
            tx.execute(
                "DELETE FROM dialog_chats WHERE guest_id = ?1 AND chat_id = ?2",
                rusqlite::params![src_guest, chat_id],
            )?;
            moved += 1;
        }
        // Chat ids may be remapped on collision while merging scopes. Drop
        // source-scope idempotency receipts instead of retaining stale targets.
        tx.execute(
            "DELETE FROM dialog_history_turn_receipts WHERE guest_id <> ?1",
            [&target],
        )?;
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

    /// `_chat_embedding_texts(guest_id, chat_id)` plus the chat's world scope —
    /// never raises. Returns `(embedding_texts, world_id)` where `world_id` is
    /// the session's `world_ref.id` (`None` for built-in/procedural chats). The
    /// scope tells [`delete_chat`] which cache file to purge — the SAME `World`
    /// that produced the texts, so both come from a single load (no drift).
    fn chat_embedding_scope(&self, guest_id: &str, chat_id: &str) -> (Vec<String>, Option<String>) {
        match self.load_chat(guest_id, chat_id) {
            Ok(mut rt) => {
                let world_id = rt
                    .session
                    .world
                    .world_ref
                    .as_ref()
                    .map(|r| r.id.clone())
                    .filter(|id| !id.trim().is_empty());
                let docs = rt.session.world.retrieval_documents("player");
                let texts = docs
                    .iter()
                    .map(|d| d.contextual_text())
                    .filter(|t| !t.trim().is_empty())
                    .collect();
                (texts, world_id)
            }
            Err(_) => (Vec::new(), None),
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

    /// Return whether `chat_id` belongs to the supplied persistence scope.
    /// This is intentionally cheaper than rebuilding a live [`DialogRuntime`]
    /// and is used to validate explicit chat ids at HTTP request boundaries.
    pub fn chat_exists(&self, guest_id: &str, chat_id: &str) -> Result<bool, StoreError> {
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

struct InheritedCheckpoint {
    turn: i64,
    request_id: String,
    player_text: String,
    snapshot: String,
    created_at: String,
}

fn load_inherited_branch_checkpoints(
    con: &Connection,
    guest_id: &str,
    source_chat_id: &str,
    before_turn: i64,
) -> Result<Vec<InheritedCheckpoint>, StoreError> {
    let mut statement = con.prepare(
        "SELECT turn_no, request_id, player_text, snapshot, created_at
         FROM dialog_turn_checkpoints
         WHERE guest_id = ?1 AND chat_id = ?2 AND turn_no < ?3
         ORDER BY turn_no ASC",
    )?;
    let rows = statement.query_map(
        rusqlite::params![guest_id, source_chat_id, before_turn],
        |row| {
            Ok(InheritedCheckpoint {
                turn: row.get(0)?,
                request_id: row.get(1)?,
                player_text: row.get(2)?,
                snapshot: row.get(3)?,
                created_at: row.get(4)?,
            })
        },
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn insert_inherited_checkpoint(
    con: &Connection,
    guest_id: &str,
    chat_id: &str,
    inherited: InheritedCheckpoint,
    snapshot: String,
) -> Result<(), StoreError> {
    con.execute(
        "INSERT INTO dialog_turn_checkpoints (
            guest_id, chat_id, turn_no, request_id, player_text, snapshot, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            guest_id,
            chat_id,
            inherited.turn,
            inherited.request_id,
            inherited.player_text,
            snapshot,
            inherited.created_at,
        ],
    )?;
    Ok(())
}

fn upsert_turn_checkpoint(
    con: &Connection,
    runtime: &DialogRuntime,
    checkpoint: &TurnCheckpoint,
) -> Result<(), StoreError> {
    con.execute(
        "INSERT INTO dialog_turn_checkpoints (
            guest_id, chat_id, turn_no, request_id, player_text, snapshot, created_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))
         ON CONFLICT(guest_id, chat_id, turn_no) DO UPDATE SET
            request_id = excluded.request_id,
            player_text = excluded.player_text,
            snapshot = excluded.snapshot,
            created_at = excluded.created_at",
        rusqlite::params![
            &runtime.guest_id,
            &runtime.chat_id,
            checkpoint.turn,
            &checkpoint.request_id,
            &checkpoint.player_text,
            &checkpoint.snapshot_json,
        ],
    )?;
    Ok(())
}

fn prune_turn_checkpoints(
    con: &Connection,
    guest_id: &str,
    chat_id: &str,
) -> Result<(), StoreError> {
    con.execute(
        "DELETE FROM dialog_turn_checkpoints
         WHERE guest_id = ?1 AND chat_id = ?2
           AND turn_no NOT IN (
               SELECT turn_no FROM dialog_turn_checkpoints
               WHERE guest_id = ?1 AND chat_id = ?2
               ORDER BY turn_no DESC
               LIMIT ?3
           )",
        rusqlite::params![guest_id, chat_id, MAX_REWIND_TURNS as i64],
    )?;
    Ok(())
}

fn checkpoint_turns(
    con: &Connection,
    guest_id: &str,
    chat_id: &str,
) -> Result<Vec<i64>, StoreError> {
    let mut stmt = con.prepare(
        "SELECT turn_no FROM dialog_turn_checkpoints
         WHERE guest_id = ?1 AND chat_id = ?2
         ORDER BY turn_no ASC",
    )?;
    let turns = stmt
        .query_map(rusqlite::params![guest_id, chat_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(turns)
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
    Err(StoreError::Other(
        "could not allocate unique chat id".to_string(),
    ))
}

pub(crate) fn normalize_world_payload(payload: Value) -> Value {
    let mut object = match payload {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let title = clean_metadata_text(
        object
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        100,
    );
    if !title.is_empty() {
        object.insert("title".to_string(), Value::String(title));
    }
    Value::Object(object)
}

pub(crate) fn merge_world_payload(base: Value, patch: Value) -> Value {
    let mut base = match base {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    if let Value::Object(patch) = patch {
        for (key, value) in patch {
            if !value.is_null() {
                base.insert(key, value);
            }
        }
    }
    Value::Object(base)
}

pub(crate) fn world_row_response(
    world_id: &str,
    title: &str,
    preview: &str,
    payload: &str,
    created_at: &str,
    updated_at: &str,
) -> Value {
    let payload_value: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let mut out = Map::new();
    if let Value::Object(map) = payload_value {
        for (key, value) in map {
            out.insert(key, value);
        }
    }
    out.insert("id".to_string(), json!(world_id));
    out.insert("kind".to_string(), json!("world"));
    out.insert(
        "title".to_string(),
        json!(if title.is_empty() {
            "Новый мир"
        } else {
            title
        }),
    );
    out.insert("preview".to_string(), json!(preview));
    out.insert("created_at".to_string(), json!(created_at));
    out.insert("updated_at".to_string(), json!(updated_at));
    Value::Object(out)
}

pub(crate) fn world_title_from_payload(payload: &Value) -> String {
    let title = payload
        .get("title")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("world_lore")
                .and_then(|lore| lore.get("name"))
                .and_then(Value::as_str)
        })
        .unwrap_or_default();
    let title = clean_metadata_text(title, 100);
    if title.is_empty() {
        "Новый мир".to_string()
    } else {
        title
    }
}

pub(crate) fn world_preview_from_payload(payload: &Value, title: &str) -> String {
    for text in [
        payload.get("public_premise").and_then(Value::as_str),
        payload
            .get("world_lore")
            .and_then(|lore| lore.get("public_premise"))
            .and_then(Value::as_str),
        payload.get("world_size").and_then(Value::as_str),
        payload.get("genre").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    {
        let preview = clean_metadata_text(text, 180);
        if !preview.is_empty() {
            return preview;
        }
    }
    if let Some(messages) = payload.get("architect_messages").and_then(Value::as_array) {
        for message in messages.iter().rev() {
            let role = message.get("role").and_then(Value::as_str).unwrap_or("");
            if role != "user" {
                continue;
            }
            let preview = clean_metadata_text(
                message
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                180,
            );
            if !preview.is_empty() {
                return preview;
            }
        }
    }
    clean_metadata_text(title, 180)
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
        let kind = event
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let agent = event
            .get("agent")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
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

/// Best-effort embeddings purge (never raises), scoped to `world_id`'s cache
/// file (`None` -> global cache).
fn purge_embeddings(texts: &[String], config: &Config, world_id: Option<&str>) -> i64 {
    if texts.is_empty() {
        return 0;
    }
    gml_rag::purge_embeddings_for_texts(texts, config, world_id)
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
pub(crate) fn token_urlsafe(nbytes: usize) -> String {
    use base64::Engine;
    let mut buf = vec![0u8; nbytes];
    getrandom::getrandom(&mut buf).expect("OS entropy for chat id");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}
