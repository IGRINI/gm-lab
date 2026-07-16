//! HTTP/SSE contract tests for `gml-server`, driven via `tower::ServiceExt`
//! `oneshot` against [`build_router`] with a `MockClient` backend + a temp DB.
//!
//! Validates the wire contract the React frontend (`web/src`) depends on:
//!   - GET /settings == tests/reference/server/settings.json (shape + values)
//!   - GET /stories  == tests/reference/server/stories.json (exact)
//!   - GET /state    has the same top-level keys as state.json
//!   - GET /debug    has the documented `{ok, meta, ...}` shape
//!   - GET /chats    -> {ok, active_chat_id, chats[]}
//!   - POST /turn (SSE) yields `data: {json}\n\n` frames parseable as the turn
//!     event sequence and ends with a structured `done` frame
//!   - unknown route -> 404 {error:"not found"}

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};
use tower::ServiceExt; // oneshot

use gml_config::{Config, RuntimeSettings};
use gml_llm::{
    Backend, BackendError, ChatOutput, ChatStreamOutput, ConnectorCapability, ConnectorDescriptor,
    ConnectorError, ConnectorId, ConnectorRegistry, DeltaSink, JsonStreamOutput, ModelBinding,
    ModelConnector, ModelDescriptor,
};
use gml_mock::MockClient;
use gml_persistence::{CharacterStore, DialogStore, TurnCheckpoint, WorldStore};
use gml_server::{build_router, AppState, TurnRegistry};

#[derive(Default)]
struct IdentitySpyState {
    session_id: String,
    thread_id: String,
    messages: Vec<Value>,
    tools: Vec<Value>,
}

struct IdentitySpyBackend {
    state: Arc<std::sync::Mutex<IdentitySpyState>>,
}

#[async_trait::async_trait]
impl Backend for IdentitySpyBackend {
    fn model(&self) -> String {
        "identity-spy".to_string()
    }

    fn set_model(&self, _model: &str) {}

    fn set_session_identity(&self, session_id: Option<&str>, thread_id: Option<&str>) {
        let mut state = self.state.lock().expect("identity spy lock");
        if let Some(session_id) = session_id {
            if !session_id.trim().is_empty() {
                state.session_id = session_id.trim().to_string();
            }
        }
        if let Some(thread_id) = thread_id {
            if !thread_id.trim().is_empty() {
                state.thread_id = thread_id.trim().to_string();
            }
        }
    }

    fn session_id(&self) -> String {
        self.state
            .lock()
            .expect("identity spy lock")
            .session_id
            .clone()
    }

    fn thread_id(&self) -> String {
        self.state
            .lock()
            .expect("identity spy lock")
            .thread_id
            .clone()
    }

    async fn list_models(&self) -> Vec<Value> {
        vec![json!({"id": "identity-spy", "name": "identity-spy", "supported": true})]
    }

    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        _think: Option<bool>,
        _reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        let mut state = self.state.lock().expect("identity spy lock");
        state.messages.push(messages.clone());
        if let Some(tools) = tools {
            state.tools.push(tools.clone());
        }
        Ok(ChatOutput {
            thinking: String::new(),
            content: "Ответ архитектора".to_string(),
            calls: Vec::new(),
            assistant_msg: json!({"role": "assistant", "content": "Ответ архитектора"}),
        })
    }

    async fn chat_json(
        &self,
        _messages: &Value,
        _schema: &Value,
        _think: Option<bool>,
        _reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        Ok(Map::new())
    }

    async fn summarize(
        &self,
        _text: &str,
        _proper_nouns: &[String],
    ) -> Result<String, BackendError> {
        Ok(String::new())
    }

    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        _think: Option<bool>,
        _reasoning_role: &str,
        _sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        // Mirror `chat`: the world-architect handler now drives `chat_stream`, so
        // the spy must record the sent messages and return the same canned reply.
        let mut state = self.state.lock().expect("identity spy lock");
        state.messages.push(messages.clone());
        if let Some(tools) = tools {
            state.tools.push(tools.clone());
        }
        Ok(ChatStreamOutput {
            thinking: String::new(),
            content: "Ответ архитектора".to_string(),
            calls: Vec::new(),
            assistant_msg: json!({"role": "assistant", "content": "Ответ архитектора"}),
            stats: Map::new(),
        })
    }

    async fn chat_json_stream(
        &self,
        _messages: &Value,
        _schema: &Value,
        _think: Option<bool>,
        _reasoning_role: &str,
        _sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        Ok(JsonStreamOutput {
            data: Map::new(),
            stats: Map::new(),
        })
    }
}

fn install_identity_spy(state: &mut AppState) -> Arc<std::sync::Mutex<IdentitySpyState>> {
    let spy = Arc::new(std::sync::Mutex::new(IdentitySpyState::default()));
    let spy_factory = spy.clone();
    let factory: gml_orchestrator::ClientFactory = Arc::new(move || {
        Arc::new(IdentitySpyBackend {
            state: spy_factory.clone(),
        }) as Arc<dyn Backend>
    });
    state.store = Arc::new(
        DialogStore::new(
            state.store.db_path().to_string(),
            factory,
            state.config.clone(),
        )
        .expect("reopen dialog store with identity spy"),
    );
    spy
}

struct FailFirstTurnBackend {
    inner: MockClient,
    stream_calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Backend for FailFirstTurnBackend {
    fn model(&self) -> String {
        self.inner.model()
    }

    fn set_model(&self, model: &str) {
        self.inner.set_model(model);
    }

    async fn list_models(&self) -> Vec<Value> {
        self.inner.list_models().await
    }

    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        self.inner
            .chat(messages, tools, think, reasoning_role)
            .await
    }

    async fn chat_json(
        &self,
        messages: &Value,
        schema: &Value,
        think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        self.inner
            .chat_json(messages, schema, think, reasoning_role)
            .await
    }

    async fn summarize(&self, text: &str, proper_nouns: &[String]) -> Result<String, BackendError> {
        self.inner.summarize(text, proper_nouns).await
    }

    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        let call = self.stream_calls.fetch_add(1, Ordering::SeqCst);
        if call == 0 {
            return Err(BackendError::new("injected retryable model failure"));
        }
        self.inner
            .chat_stream(messages, tools, think, reasoning_role, sink)
            .await
    }

    async fn chat_json_stream(
        &self,
        messages: &Value,
        schema: &Value,
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        self.inner
            .chat_json_stream(messages, schema, think, reasoning_role, sink)
            .await
    }
}

fn fail_first_turn_state(tmp: &tempfile::TempDir) -> (AppState, Arc<AtomicUsize>) {
    let mut state = mock_state(tmp);
    let stream_calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = stream_calls.clone();
    let factory: gml_orchestrator::ClientFactory = Arc::new(move || {
        Arc::new(FailFirstTurnBackend {
            inner: MockClient::new(),
            stream_calls: factory_calls.clone(),
        }) as Arc<dyn Backend>
    });
    state.store = Arc::new(
        DialogStore::new(
            state.store.db_path().to_string(),
            factory,
            state.config.clone(),
        )
        .expect("reopen dialog store with fail-first backend"),
    );
    (state, stream_calls)
}

struct PendingTurnBackend {
    inner: MockClient,
    started: Arc<tokio::sync::Notify>,
    cancelled: Arc<tokio::sync::Notify>,
}

struct PendingCallGuard(Arc<tokio::sync::Notify>);

impl Drop for PendingCallGuard {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}

#[async_trait::async_trait]
impl Backend for PendingTurnBackend {
    fn model(&self) -> String {
        self.inner.model()
    }

    fn set_model(&self, model: &str) {
        self.inner.set_model(model);
    }

    async fn list_models(&self) -> Vec<Value> {
        self.inner.list_models().await
    }

    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        self.inner
            .chat(messages, tools, think, reasoning_role)
            .await
    }

    async fn chat_json(
        &self,
        messages: &Value,
        schema: &Value,
        think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        self.inner
            .chat_json(messages, schema, think, reasoning_role)
            .await
    }

    async fn summarize(&self, text: &str, proper_nouns: &[String]) -> Result<String, BackendError> {
        self.inner.summarize(text, proper_nouns).await
    }

    async fn chat_stream(
        &self,
        _messages: &Value,
        _tools: Option<&Value>,
        _think: Option<bool>,
        _reasoning_role: &str,
        _sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        let _cancelled = PendingCallGuard(self.cancelled.clone());
        self.started.notify_one();
        std::future::pending().await
    }

    async fn chat_json_stream(
        &self,
        messages: &Value,
        schema: &Value,
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        self.inner
            .chat_json_stream(messages, schema, think, reasoning_role, sink)
            .await
    }
}

fn pending_turn_state(
    tmp: &tempfile::TempDir,
) -> (AppState, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
    let mut state = mock_state(tmp);
    let started = Arc::new(tokio::sync::Notify::new());
    let cancelled = Arc::new(tokio::sync::Notify::new());
    let factory_started = started.clone();
    let factory_cancelled = cancelled.clone();
    let factory: gml_orchestrator::ClientFactory = Arc::new(move || {
        Arc::new(PendingTurnBackend {
            inner: MockClient::new(),
            started: factory_started.clone(),
            cancelled: factory_cancelled.clone(),
        }) as Arc<dyn Backend>
    });
    state.store = Arc::new(
        DialogStore::new(
            state.store.db_path().to_string(),
            factory,
            state.config.clone(),
        )
        .expect("reopen dialog store with pending backend"),
    );
    (state, started, cancelled)
}

fn seed_completed_turn_checkpoint(state: &AppState, text: &str) -> String {
    let chat_id = state.store.get_active("shared").expect("active chat");
    let mut runtime = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load runtime to seed checkpoint");
    assert_eq!(runtime.turn_count, 0);
    let checkpoint = TurnCheckpoint::capture(&runtime, 1, "seeded-turn", text)
        .expect("capture seeded checkpoint");
    runtime.turn_count = 1;
    runtime.session.turn = 1;
    runtime.session.last_player_action = text.to_string();
    runtime
        .session
        .gm_messages
        .push(json!({"role": "user", "content": text}));
    runtime.transcript.push(json!({
        "turn": 1,
        "request_id": "seeded-turn",
        "event": {"kind": "player", "agent": "Игрок", "data": text, "sid": null}
    }));
    runtime.transcript.push(json!({
        "turn": 1,
        "event": {"kind": "gm", "agent": "ГМ", "data": "seeded answer", "sid": null}
    }));
    state
        .store
        .save_owned_with_checkpoint(runtime, checkpoint)
        .expect("save seeded checkpoint");
    chat_id
}

/// Reproduce the three-row failure shape persisted by the pre-staging server:
/// the model failed before any tool call, but the turn prelude and player world
/// event were already durable.
async fn seed_legacy_failed_turn(state: &AppState, text: &str) -> String {
    let chat_id = state.store.get_active("shared").expect("active chat");
    let mut runtime = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load runtime to seed legacy failure");
    assert!(runtime.transcript.is_empty());

    let events =
        gml_orchestrator::run_turn(&mut runtime.session, state.settings.as_ref(), text).await;
    assert_eq!(
        events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        vec!["player", "error", "meta_total"],
        "the fail-first backend must produce the recoverable legacy tail"
    );
    assert!(events[1]
        .data
        .as_str()
        .is_some_and(|message| message.starts_with("Ошибка вызова модели:")));

    runtime.turn_count = runtime.session.turn;
    runtime.session.record_public("player", "speech", text, "");
    runtime.transcript = events
        .into_iter()
        .map(|event| json!({"turn": runtime.turn_count, "event": event}))
        .collect();
    state
        .store
        .save_owned(runtime)
        .expect("persist legacy failed turn");
    chat_id
}

fn draft_world_bible_properties(tools: &Value) -> &Map<String, Value> {
    let draft_tool = tools
        .as_array()
        .expect("tools array")
        .iter()
        .find(|tool| tool["function"]["name"] == "draft_world_bible")
        .expect("draft_world_bible tool");
    draft_tool["function"]["parameters"]["properties"]
        .as_object()
        .expect("draft_world_bible properties")
}

/// Build an [`AppState`] with the mock backend, a fresh temp DB, and an
/// explicit sidecar (`infer_base_url`) override — used by the world-image
/// ingestion tests so the server fetches from a local stub HTTP server.
fn mock_state_with_infer_url(tmp: &tempfile::TempDir, infer_base_url: &str) -> AppState {
    std::env::set_var("GM_BACKEND", "mock");
    std::env::set_var("GM_CHAT_SCOPE_ID", "shared");
    std::env::set_var("GM_IMAGE_ENABLED", "1");

    let mut cfg = Config::from_env();
    cfg.backend = "mock".to_string();
    cfg.infer_base_url = infer_base_url.trim_end_matches('/').to_string();
    let cfg = Arc::new(cfg);

    let settings_path = tmp.path().join("settings.json");
    let settings = Arc::new(RuntimeSettings::new(&cfg, settings_path));

    let factory: gml_orchestrator::ClientFactory =
        Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>);

    let db_path = tmp.path().join("dialogs.sqlite3");
    let store = Arc::new(
        DialogStore::new(db_path.to_string_lossy().to_string(), factory, cfg.clone())
            .expect("open temp dialog store"),
    );
    let world_store =
        Arc::new(WorldStore::new(tmp.path().join("library")).expect("open temp world store"));
    let story_store = Arc::new(std::sync::Mutex::new(
        gml_stories::StoryStore::new(tmp.path().join("library")).expect("open temp story store"),
    ));
    let character_store = Arc::new(std::sync::Mutex::new(
        CharacterStore::new(tmp.path().join("library")).expect("open temp character store"),
    ));

    AppState {
        store,
        world_store,
        story_store,
        character_store,
        config: cfg,
        settings,
        http: reqwest::Client::new(),
        sidecar: None,
        locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        turn_registry: Arc::new(TurnRegistry::default()),
        index_html: Arc::new(None),
    }
}

/// Build an [`AppState`] with the mock backend and a fresh temp DB.
fn mock_state(tmp: &tempfile::TempDir) -> AppState {
    // The server reads `GM_BACKEND` (via Config) and `GM_CHAT_SCOPE_ID`; pin both.
    std::env::set_var("GM_BACKEND", "mock");
    std::env::set_var("GM_CHAT_SCOPE_ID", "shared");
    std::env::set_var("GM_IMAGE_ENABLED", "1");

    let mut cfg = Config::from_env();
    cfg.backend = "mock".to_string();
    let cfg = Arc::new(cfg);

    let settings_path = tmp.path().join("settings.json");
    let settings = Arc::new(RuntimeSettings::new(&cfg, settings_path));

    let factory: gml_orchestrator::ClientFactory =
        Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>);

    let db_path = tmp.path().join("dialogs.sqlite3");
    let store = Arc::new(
        DialogStore::new(db_path.to_string_lossy().to_string(), factory, cfg.clone())
            .expect("open temp dialog store"),
    );

    let world_store =
        Arc::new(WorldStore::new(tmp.path().join("library")).expect("open temp world store"));
    let story_store = Arc::new(std::sync::Mutex::new(
        gml_stories::StoryStore::new(tmp.path().join("library")).expect("open temp story store"),
    ));
    let character_store = Arc::new(std::sync::Mutex::new(
        CharacterStore::new(tmp.path().join("library")).expect("open temp character store"),
    ));

    AppState {
        store,
        world_store,
        story_store,
        character_store,
        config: cfg,
        settings,
        http: reqwest::Client::new(),
        sidecar: None,
        locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        turn_registry: Arc::new(TurnRegistry::default()),
        index_html: Arc::new(None),
    }
}

fn connector_state(tmp: &tempfile::TempDir) -> AppState {
    let mut state = mock_state(tmp);
    let registry = Arc::new(ConnectorRegistry::new());
    registry
        .register(Arc::new(SpeechMockConnector))
        .expect("register mock connector");
    let binding = ModelBinding::new(ConnectorId::new("mock").unwrap(), "mock").unwrap();
    state.store = Arc::new(
        DialogStore::with_connectors(
            state.store.db_path().to_string(),
            registry,
            binding,
            state.config.clone(),
        )
        .expect("reopen dialog store with connectors"),
    );
    state
}

struct SpeechMockConnector;

#[async_trait::async_trait]
impl ModelConnector for SpeechMockConnector {
    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor::new(ConnectorId::new("mock").unwrap(), "Mock")
            .unwrap()
            .with_capability(ConnectorCapability::SpeechToText)
    }

    fn default_model_id(&self) -> String {
        "mock".to_string()
    }

    async fn transcribe(
        &self,
        audio: &[u8],
        content_type: &str,
        language: Option<&str>,
    ) -> Result<String, ConnectorError> {
        assert_eq!(audio, [1, 2, 3]);
        assert_eq!(content_type, "audio/webm");
        assert_eq!(language, None);
        Ok("проверка распознавания".to_string())
    }

    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError> {
        Ok(vec![ModelDescriptor::new("mock", "Mock")?])
    }

    fn create_backend(&self, model_id: &str) -> Arc<dyn Backend> {
        let backend = Arc::new(MockClient::new());
        backend.set_model(model_id);
        backend
    }
}

struct SlowMockConnector {
    id: &'static str,
    model: &'static str,
}

#[async_trait::async_trait]
impl ModelConnector for SlowMockConnector {
    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor::new(ConnectorId::new(self.id).unwrap(), self.id).unwrap()
    }

    fn default_model_id(&self) -> String {
        self.model.to_string()
    }

    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError> {
        // Widen the overlap in the regression test. The production architect
        // lock must still make the two complete turns execute sequentially.
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        Ok(vec![ModelDescriptor::new(self.model, self.model)?])
    }

    fn create_backend(&self, model_id: &str) -> Arc<dyn Backend> {
        let backend = Arc::new(MockClient::new());
        backend.set_model(model_id);
        backend
    }
}

fn competing_connector_state(tmp: &tempfile::TempDir) -> AppState {
    let mut state = mock_state(tmp);
    let registry = Arc::new(ConnectorRegistry::new());
    registry
        .register(Arc::new(SlowMockConnector {
            id: "connector-a",
            model: "model-a",
        }))
        .unwrap();
    registry
        .register(Arc::new(SlowMockConnector {
            id: "connector-b",
            model: "model-b",
        }))
        .unwrap();
    let binding = ModelBinding::new(ConnectorId::new("connector-a").unwrap(), "model-a").unwrap();
    state.store = Arc::new(
        DialogStore::with_connectors(
            state.store.db_path().to_string(),
            registry,
            binding,
            state.config.clone(),
        )
        .expect("reopen dialog store with competing connectors"),
    );
    state
}

async fn get(state: &AppState, path: &str) -> (StatusCode, Vec<u8>) {
    let resp = build_router(state.clone())
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

async fn post(state: &AppState, path: &str, body: Value) -> (StatusCode, Vec<u8>) {
    let resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec())
}

async fn post_turn_text(state: &AppState, text: &str) -> (StatusCode, String) {
    post_turn_request(state, text, None).await
}

async fn post_turn_request(
    state: &AppState,
    text: &str,
    request_id: Option<&str>,
) -> (StatusCode, String) {
    let mut payload = json!({"text": text});
    if let Some(request_id) = request_id {
        payload["request_id"] = Value::String(request_id.to_string());
    }
    let resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/turn")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn post_legacy_turn_request(
    state: &AppState,
    text: &str,
    request_id: &str,
) -> (StatusCode, String) {
    let payload = json!({
        "text": text,
        "request_id": request_id,
        "legacy_resume": true,
    });
    let resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/turn")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&payload).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn sse_payloads(text: &str) -> Vec<Value> {
    text.split("\n\n")
        .filter_map(|frame| frame.trim().strip_prefix("data: "))
        .map(|payload| {
            serde_json::from_str(payload)
                .unwrap_or_else(|error| panic!("invalid SSE JSON ({error}): {payload:?}"))
        })
        .collect()
}

fn assert_successful_done(text: &str) -> Value {
    let done = sse_payloads(text)
        .into_iter()
        .last()
        .expect("SSE stream must not be empty");
    assert_eq!(done["kind"], "done", "last SSE frame must be done");
    assert_eq!(done["ok"], true, "turn must commit successfully: {done}");
    assert_eq!(done["retryable"], false);
    assert_eq!(done["replayed"], false);
    assert!(
        done["request_id"]
            .as_str()
            .is_some_and(|request_id| !request_id.is_empty()),
        "done must carry the effective request id: {done}"
    );
    done
}

fn reference(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("reference")
        .join("server")
        .join(name);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap()
}

fn architect_lore_json() -> Value {
    serde_json::json!({
        "name": "Порог Второго Неба",
        "public_premise": "Мир держится на клятвах, духах мест и долгах между призванными чужаками и местными домами.",
        "dogmas": ["имя и клятва имеют юридическую и мистическую силу"],
        "world_laws": ["магия требует имени, цены или признанного права"],
        "regions": ["Семь земель под Осколочной Луной"],
        "religions": ["культ дорожных духов"],
        "gods": ["Старшие Духи Порогов"],
        "location_rules": ["каждая новая локация должна иметь связь с долгом, властью, дорогой или духом места"]
    })
}

#[tokio::test]
async fn get_settings_matches_reference() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = get(&state, "/settings").await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    let want = reference("settings.json");
    // Compare parsed JSON (order-independent; frontend JSON.parses each body).
    assert_eq!(got["ok"], want["ok"]);
    assert_eq!(got["settings"], want["settings"], "settings shape+values");
    assert_eq!(
        got["settings_options"], want["settings_options"],
        "settings_options shape+values"
    );
}

#[tokio::test]
async fn get_stories_matches_reference_exactly() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = get(&state, "/stories").await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    let want = reference("stories.json");
    assert_eq!(got, want, "/stories must match exactly");
}

#[tokio::test]
async fn get_state_has_reference_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = get(&state, "/state").await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    let want = reference("state.json");
    let want_keys: Vec<&String> = want.as_object().unwrap().keys().collect();
    let got_obj = got.as_object().expect("state is object");
    for key in &want_keys {
        assert!(got_obj.contains_key(*key), "/state missing key `{key}`");
    }
    // Spot-check deterministic values.
    assert_eq!(got["backend"], "mock");
    assert_eq!(got["story_id"], "procedural");
    assert_eq!(got["story_title"], "Процедурный мир");
    // Procedural worlds now start with a zero-actor canon (worldgen Layer 6
    // removed): the roster is present but empty until NPCs are generated lazily.
    assert!(got["npcs"].is_array());
    // context_usage / settings sub-objects present.
    assert!(got["context_usage"].is_object());
    assert!(got["settings"].is_object());
}

#[tokio::test]
async fn get_debug_has_ok_and_meta_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = get(&state, "/debug").await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    for key in [
        "meta", "runtime", "story", "scene", "facts", "npcs", "memory",
    ] {
        assert!(got.get(key).is_some(), "/debug missing key `{key}`");
    }
    assert_eq!(got["meta"]["backend"], "mock");
    // Empty procedural roster at bootstrap (worldgen Layer 6 removed); present as array.
    assert!(got["npcs"].is_array());
}

#[tokio::test]
async fn debug_state_record_route_uses_memory_backed_export() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/debug/state_record",
        serde_json::json!({
            "add": [{
                "kind": "fact",
                "text": "DEBUG_STATE_MEMORY_SENTINEL хранится в canon memory.",
                "scope": "public"
            }]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    let row = got["state_records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["text"]
                .as_str()
                .map(|text| text.contains("DEBUG_STATE_MEMORY_SENTINEL"))
                .unwrap_or(false)
        })
        .expect("debug state record row");
    let record_id = row["record_id"].as_str().unwrap().to_string();
    assert_eq!(row["kind"], "fact");
    assert_eq!(row["scope"], "public");
    assert!(!row["memory_id"].as_str().unwrap_or("").is_empty());

    let (status, body) = post(
        &state,
        "/debug/state_record",
        serde_json::json!({"delete": [record_id]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    let row = got["state_records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["text"]
                .as_str()
                .map(|text| text.contains("DEBUG_STATE_MEMORY_SENTINEL"))
                .unwrap_or(false)
        })
        .expect("archived debug state record row");
    assert_eq!(row["active"], false);
}

#[tokio::test]
async fn get_chats_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = get(&state, "/chats").await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert!(got["chats"].is_array());
    // get_active creates one default chat on first access.
    assert!(!got["chats"].as_array().unwrap().is_empty());
    assert_eq!(got["chats"][0]["story_id"], "procedural");
    assert_eq!(got["chats"][0]["kind"], "world");
    assert!(got["active_chat_id"]
        .as_str()
        .map(|s| !s.is_empty())
        .unwrap_or(false));
}

#[tokio::test]
async fn connector_catalog_and_chat_binding_contract() {
    let tmp = tempfile::tempdir().unwrap();
    let state = connector_state(&tmp);

    let (status, body) = get(&state, "/connectors").await;
    assert_eq!(status, StatusCode::OK);
    let catalog: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(catalog["connectors"][0]["id"], "mock");
    assert_eq!(catalog["connectors"][0]["default_model_id"], "mock");
    assert_eq!(catalog["connectors"][0]["auth"]["state"], "not_required");

    let (status, body) = get(&state, "/connectors/mock/models").await;
    assert_eq!(status, StatusCode::OK);
    let models: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(models["models"][0]["id"], "mock");

    let (status, _) = get(&state, "/chats").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = post(
        &state,
        "/model",
        json!({"connector_id": "xai", "model_id": "grok"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let error: Value = serde_json::from_slice(&body).unwrap();
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("connector is fixed"));
}

#[tokio::test]
async fn concurrent_first_architect_turns_cannot_bind_two_connectors() {
    let tmp = tempfile::tempdir().unwrap();
    let state = competing_connector_state(&tmp);
    let world_id = create_saved_world(&state, "Locked Architect World").await;

    let first = post(
        &state,
        "/world-architect/chat",
        json!({
            "world_id": world_id,
            "message": "Первый параллельный ход",
            "connector_id": "connector-a",
            "model_id": "model-a",
        }),
    );
    let second = post(
        &state,
        "/world-architect/chat",
        json!({
            "world_id": world_id,
            "message": "Второй параллельный ход",
            "connector_id": "connector-b",
            "model_id": "model-b",
        }),
    );
    let ((first_status, first_body), (second_status, second_body)) = tokio::join!(first, second);
    assert_eq!(first_status, StatusCode::OK);
    assert_eq!(second_status, StatusCode::OK);

    let results = [
        architect_result(&first_body),
        architect_result(&second_body),
    ];
    assert_eq!(
        results.iter().filter(|result| result["ok"] == true).count(),
        1,
        "exactly one connector may win the first history binding: {results:?}"
    );
    let rejected = results
        .iter()
        .find(|result| result["ok"] != true)
        .expect("one turn must be rejected");
    assert!(rejected["data"]
        .as_str()
        .unwrap_or_default()
        .contains("connector is fixed"));

    let persisted = state
        .store
        .get_architect_chat("world", &world_id)
        .expect("read architect chat")
        .expect("architect chat exists");
    let connector = persisted["model_binding"]["connector_id"].as_str().unwrap();
    assert!(connector == "connector-a" || connector == "connector-b");
    let user_messages = persisted["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .count();
    assert_eq!(user_messages, 1, "rejected turn must not enter history");
}

#[tokio::test]
async fn search_returns_unified_library_items() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    state
        .world_store
        .create_world(json!({
            "title": "Cobalt Atlas",
            "public_premise": "Skyships cross a chain of quiet islands.",
        }))
        .expect("create searchable world");

    let (status, body) = get(&state, "/search?scope=library&q=skyships").await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["total"], 1);
    assert_eq!(got["has_more"], false);
    assert_eq!(got["items"][0]["type"], "world");
    assert_eq!(got["items"][0]["title"], "Cobalt Atlas");
    assert_eq!(got["items"][0]["matched_fields"], json!(["world"]));
    assert!(got["items"][0]["world_id"].as_str().is_some());
}

#[tokio::test]
async fn search_finds_only_player_facing_chat_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let chat_id = state
        .store
        .create_chat("shared", None, None, 0, Some("Quiet Camp"), None, true)
        .expect("create chat");
    state
        .store
        .with_runtime("shared", &chat_id, |runtime| {
            runtime.turn_count = 1;
            runtime.transcript.push(json!({
                "turn": 1,
                "event": {
                    "kind": "player",
                    "agent": "Игрок",
                    "data": "amber signal by the old bridge",
                    "sid": null,
                },
            }));
            runtime.transcript.push(json!({
                "turn": 1,
                "event": {
                    "kind": "gm_thinking",
                    "agent": "ГМ",
                    "data": "private-thought-sentinel",
                    "sid": "hidden",
                },
            }));
            state.store.save(runtime).expect("save indexed chat");
        })
        .expect("mutate chat")
        .expect("chat exists");

    let (status, body) = get(
        &state,
        "/search?scope=chats&field=messages&q=amber%20signal",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["total"], 1);
    assert_eq!(got["items"][0]["type"], "chat");
    assert_eq!(got["items"][0]["id"], chat_id);
    assert_eq!(got["items"][0]["turn_count"], 1);
    assert_eq!(got["items"][0]["matched_fields"], json!(["messages"]));
    assert!(got["items"][0]["snippet"]
        .as_str()
        .unwrap_or_default()
        .contains("amber signal"));

    let (status, body) = get(
        &state,
        "/search?scope=chats&field=messages&q=private-thought-sentinel",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["total"], 0, "GM thinking must not enter message search");
}

#[tokio::test]
async fn search_rejects_an_oversized_query() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let query = "a".repeat(161);
    let (status, body) = get(&state, &format!("/search?q={query}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"].as_str().unwrap_or_default().contains("160"));
}

#[tokio::test]
async fn worlds_route_is_separate_from_chats() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    let (status, body) = get(&state, "/worlds").await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["worlds"], json!([]));
    assert_eq!(
        state.store.active_chat_id("shared").unwrap(),
        None,
        "reading worlds must not create an active chat"
    );

    let (status, body) = post(
        &state,
        "/worlds",
        json!({
            "title": "Порог Второго Неба",
            "genre": "fantasy isekai",
            "tone": "tense hopeful",
            "world_size": "Континент с несколькими королевствами",
            "population": "Десятки миллионов жителей",
            "public_premise": "Клятвы и долги имеют силу закона и магии.",
            "world_lore": {
                "name": "Порог Второго Неба",
                "public_premise": "Клятвы и долги имеют силу закона и магии.",
                "world_laws": ["магия требует имени, цены или признанного права"]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["world"]["kind"], "world");
    assert_eq!(got["world"]["title"], "Порог Второго Неба");
    assert_eq!(
        got["world"]["world_size"],
        "Континент с несколькими королевствами"
    );
    assert!(
        got.get("chat").is_none(),
        "world create must not return chat payload"
    );
    assert!(
        got.get("state").is_none(),
        "world create must not start a session"
    );
    assert!(got.get("transcript").is_none());
    assert_eq!(
        state.store.active_chat_id("shared").unwrap(),
        None,
        "creating a world must not create an active chat"
    );

    let world_id = got["world"]["id"].as_str().unwrap();
    let (status, body) = post(&state, &format!("/worlds/{world_id}/delete"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["deleted"], true);
    assert_eq!(got["worlds"], json!([]));
}

#[tokio::test]
async fn create_world_rejects_story_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let base = json!({
        "title": "Порог Второго Неба",
        "genre": "fantasy isekai",
        "tone": "tense hopeful",
        "world_size": "Континент",
        "population": "Десятки миллионов",
        "world_lore": {"name": "Порог Второго Неба", "world_laws": ["клятвы имеют силу"]}
    });
    for legacy_key in [
        "activate",
        "seed",
        "story_id",
        "story_brief",
        "storyBrief",
        "public_intro",
        "publicIntro",
        "scale",
    ] {
        let mut body = base.clone();
        body.as_object_mut()
            .unwrap()
            .insert(legacy_key.to_string(), json!("legacy"));
        let (status, bytes) = post(&state, "/worlds", body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{legacy_key}");
        let got: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(got["ok"], false);
        assert!(got["error"].as_str().unwrap_or("").contains(legacy_key));
    }
}

#[tokio::test]
async fn sidecar_status_route_returns_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = get(&state, "/sidecar/status").await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert!(got["state"].is_string());
    assert!(got["ready"].is_boolean());
    assert!(got["components"].is_object());
}

#[tokio::test]
async fn unknown_route_is_404_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = get(&state, "/no/such/route").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got, serde_json::json!({"error": "not found"}));
}

#[tokio::test]
async fn settings_response_content_type_is_json() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .uri("/settings")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(ct.contains("application/json"), "got content-type: {ct}");
}

#[tokio::test]
async fn turn_sse_streams_frames_and_ends_with_done() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // POST /turn returns an SSE stream of `data: {json}\n\n` frames.
    let resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/turn")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "text": "Я осматриваю зал трактира и прислушиваюсь к разговорам."
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    // Headers the frontend / proxies depend on.
    let headers = resp.headers().clone();
    assert!(headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .contains("text/event-stream"));
    assert_eq!(
        headers.get("cache-control").and_then(|v| v.to_str().ok()),
        Some("no-cache")
    );
    assert_eq!(
        headers
            .get("x-accel-buffering")
            .and_then(|v| v.to_str().ok()),
        Some("no")
    );

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();

    // Every frame is `data: {json}\n\n`. Parse each `data:` payload as JSON.
    assert_successful_done(&text);
    let mut kinds: Vec<String> = Vec::new();
    let mut done_seen = false;
    for frame in text.split("\n\n") {
        let frame = frame.trim();
        if frame.is_empty() {
            continue;
        }
        let payload = frame
            .strip_prefix("data: ")
            .unwrap_or_else(|| panic!("frame missing `data: ` prefix: {frame:?}"));
        let ev: Value = serde_json::from_str(payload)
            .unwrap_or_else(|e| panic!("frame is not JSON ({e}): {payload:?}"));
        let kind = ev
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if kind == "done" {
            done_seen = true;
            continue;
        }
        // Non-done frames must carry the {kind, agent, data, sid} envelope.
        assert!(ev.get("kind").is_some(), "event missing kind: {ev}");
        assert!(
            ev.as_object().unwrap().contains_key("agent"),
            "event missing agent: {ev}"
        );
        assert!(
            ev.as_object().unwrap().contains_key("data"),
            "event missing data: {ev}"
        );
        assert!(
            ev.as_object().unwrap().contains_key("sid"),
            "event missing sid: {ev}"
        );
        kinds.push(kind);
    }
    assert!(done_seen, "no done frame");
    // The turn always opens with the player echo and closes with meta_total.
    assert_eq!(kinds.first().map(String::as_str), Some("player"));
    assert_eq!(kinds.last().map(String::as_str), Some("meta_total"));
}

#[tokio::test]
async fn edit_and_branch_use_exact_pre_turn_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let chat_id = state.store.get_active("shared").expect("active chat");

    let (_, first_stream) = post_turn_text(&state, "Первый ход").await;
    let first_done = assert_successful_done(&first_stream);
    assert_eq!(first_done["turn"], 1);
    assert_eq!(first_done["rewindable"], true);
    let after_first = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load after first")
        .payload_json();

    let (_, second_stream) = post_turn_text(&state, "Второй ход").await;
    let second_done = assert_successful_done(&second_stream);
    assert_eq!(second_done["turn"], 2);
    assert_eq!(second_done["rewindable"], true);

    let (status, body) = get(&state, "/transcript").await;
    assert_eq!(status, StatusCode::OK);
    let transcript: Value = serde_json::from_slice(&body).unwrap();
    let players = transcript["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|event| event["kind"] == "player")
        .collect::<Vec<_>>();
    assert_eq!(players.len(), 2);
    assert_eq!(players[0]["turn"], 1);
    assert_eq!(players[0]["rewindable"], true);
    assert!(players[0]["message_id"].as_str().is_some());

    let (status, body) = post(
        &state,
        "/turn",
        json!({
            "chat_id": chat_id,
            "text": "Новый второй ход",
            "request_id": "staged-edit-turn",
            "history": {"kind": "edit", "turn": 2},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let replacement_stream = String::from_utf8(body).unwrap();
    let replacement_done = assert_successful_done(&replacement_stream);
    assert_eq!(replacement_done["chat_id"], chat_id);
    assert_eq!(replacement_done["turn"], 2);
    let source_after_replacement = state
        .store
        .load_chat("shared", &chat_id)
        .unwrap()
        .payload_json();
    assert_ne!(source_after_replacement, after_first);

    let branch_request_id = "staged-branch-turn";
    let branch_body = json!({
        "chat_id": chat_id,
        "text": "Альтернативный второй ход",
        "request_id": branch_request_id,
        "history": {"kind": "branch", "turn": 2, "title": "Другая ветвь"},
    });
    let (status, body) = post(&state, "/turn", branch_body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let branch_stream = String::from_utf8(body).unwrap();
    let branched = assert_successful_done(&branch_stream);
    let branch_id = branched["chat_id"].as_str().unwrap();
    assert_ne!(branch_id, chat_id);
    assert_eq!(branched["turn"], 2);
    let branch = state.store.load_chat("shared", branch_id).unwrap();
    assert_eq!(branch.turn_count, 2);
    assert_eq!(branch.rewindable_turns, vec![1, 2]);
    assert_eq!(
        state
            .store
            .load_chat("shared", &chat_id)
            .unwrap()
            .payload_json(),
        source_after_replacement,
        "branch must not mutate its source"
    );

    // Losing the terminal SSE receipt and retrying the same logical branch is
    // idempotent: the durable source->destination receipt replays the existing
    // branch instead of creating another chat.
    let (status, body) = post(&state, "/turn", branch_body).await;
    assert_eq!(status, StatusCode::OK);
    let replayed = sse_payloads(&String::from_utf8(body).unwrap())
        .into_iter()
        .last()
        .expect("replayed branch terminal receipt");
    assert_eq!(replayed["kind"], "done");
    assert_eq!(replayed["ok"], true);
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["chat_id"], branch_id);

    // The same durable receipt also resolves a late Stop after the in-flight
    // control has gone away: the client receives the committed branch bundle.
    let (status, body) = post(
        &state,
        &format!("/turn/{branch_request_id}/cancel"),
        json!({"chat_id": chat_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cancel: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(cancel["status"], "committed");
    assert_eq!(cancel["chat_id"], branch_id);
    assert_eq!(cancel["source_chat_id"], chat_id);

    let source_before_invalid = state
        .store
        .load_chat("shared", &chat_id)
        .unwrap()
        .payload_json();
    let (status, body) = post(
        &state,
        "/turn",
        json!({
            "chat_id": chat_id,
            "text": "Недоступная правка",
            "request_id": "invalid-staged-edit",
            "history": {"kind": "edit", "turn": 99},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let frames = sse_payloads(&String::from_utf8(body).unwrap());
    let done = frames.iter().find(|frame| frame["kind"] == "done").unwrap();
    assert_eq!(done["ok"], false);
    assert_eq!(done["retryable"], false);
    assert_eq!(
        state
            .store
            .load_chat("shared", &chat_id)
            .unwrap()
            .payload_json(),
        source_before_invalid
    );
}

#[tokio::test]
async fn staged_edit_model_failure_keeps_source_exact_and_can_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, stream_calls) = fail_first_turn_state(&tmp);
    let chat_id = seed_completed_turn_checkpoint(&state, "Исходный первый ход");
    let source_before = state.store.load_chat("shared", &chat_id).unwrap();
    let source_payload_before = source_before.payload_json();
    let source_metadata_before = (
        source_before.title.clone(),
        source_before.preview.clone(),
        source_before.created_at.clone(),
        source_before.updated_at.clone(),
    );
    let chats_before = state.store.list_chats("shared").unwrap();
    let request = json!({
        "chat_id": chat_id,
        "text": "Исправленный первый ход",
        "request_id": "failed-staged-edit",
        "history": {"kind": "edit", "turn": 1},
    });

    let (status, body) = post(&state, "/turn", request.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let failed = sse_payloads(&String::from_utf8(body).unwrap());
    let done = failed.iter().find(|event| event["kind"] == "done").unwrap();
    assert_eq!(done["ok"], false);
    assert_eq!(done["retryable"], true);
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);

    let source_after_failure = state.store.load_chat("shared", &chat_id).unwrap();
    assert_eq!(source_after_failure.payload_json(), source_payload_before);
    assert_eq!(
        (
            source_after_failure.title,
            source_after_failure.preview,
            source_after_failure.created_at,
            source_after_failure.updated_at,
        ),
        source_metadata_before
    );
    assert_eq!(state.store.list_chats("shared").unwrap(), chats_before);
    assert!(state
        .store
        .history_turn_receipt("shared", &chat_id, "failed-staged-edit")
        .unwrap()
        .is_none());

    let (status, body) = post(&state, "/turn", request).await;
    assert_eq!(status, StatusCode::OK);
    let retried = assert_successful_done(&String::from_utf8(body).unwrap());
    assert_eq!(retried["chat_id"], chat_id);
    assert_eq!(retried["turn"], 1);
    assert!(stream_calls.load(Ordering::SeqCst) >= 2);
    assert_ne!(
        state
            .store
            .load_chat("shared", &chat_id)
            .unwrap()
            .payload_json(),
        source_payload_before
    );
}

#[tokio::test]
async fn dropping_turn_stream_cancels_model_call_and_keeps_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, started, cancelled) = pending_turn_state(&tmp);
    let chat_id = state.store.get_active("shared").expect("active chat");
    let before = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load pre-turn runtime");

    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/turn")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "text": "Я останавливаю незавершённый ход.",
                        "request_id": "cancel-turn-contract",
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body();
    let first_frame = tokio::time::timeout(std::time::Duration::from_secs(2), body.frame())
        .await
        .expect("player frame timed out")
        .expect("turn body ended before player frame")
        .expect("turn body frame failed");
    let first_text = std::str::from_utf8(first_frame.data_ref().expect("SSE data frame"))
        .expect("SSE frame must be UTF-8");
    assert!(first_text.contains("\"kind\":\"player\""));

    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .expect("model call did not start");
    drop(body);
    tokio::time::timeout(std::time::Duration::from_secs(2), cancelled.notified())
        .await
        .expect("dropping SSE did not cancel the model call");

    let after = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load runtime after cancellation");
    assert_eq!(after.turn_count, before.turn_count);
    assert_eq!(after.transcript, before.transcript);
    assert_eq!(after.payload_json(), before.payload_json());
}

#[tokio::test]
async fn explicit_turn_cancel_aborts_model_and_returns_canonical_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, started, cancelled) = pending_turn_state(&tmp);
    let chat_id = state.store.get_active("shared").expect("active chat");
    let before = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load pre-turn runtime");
    let request_id = "explicit-cancel-turn-contract";

    let response = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/turn")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&json!({
                        "chat_id": chat_id,
                        "text": "Я явно останавливаю незавершённый ход.",
                        "request_id": request_id,
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut turn_body = response.into_body();
    let first_frame = tokio::time::timeout(std::time::Duration::from_secs(2), turn_body.frame())
        .await
        .expect("player frame timed out")
        .expect("turn body ended before player frame")
        .expect("turn body frame failed");
    assert!(
        std::str::from_utf8(first_frame.data_ref().expect("SSE data frame"))
            .unwrap()
            .contains("\"kind\":\"player\"")
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), started.notified())
        .await
        .expect("model call did not start");

    let (status, cancel_body) = post(
        &state,
        &format!("/turn/{request_id}/cancel"),
        json!({"chat_id": chat_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cancel: Value = serde_json::from_slice(&cancel_body).unwrap();
    assert_eq!(cancel["ok"], true);
    assert_eq!(cancel["status"], "cancelled");
    assert_eq!(cancel["committed"], false);
    assert_eq!(cancel["request_id"], request_id);
    assert_eq!(cancel["chat_id"], chat_id);
    assert_eq!(cancel["chat"]["turn_count"], before.turn_count);
    assert_eq!(cancel["transcript"]["events"], json!([]));

    tokio::time::timeout(std::time::Duration::from_secs(2), cancelled.notified())
        .await
        .expect("cancel endpoint did not abort the model call");
    let remaining = tokio::time::timeout(std::time::Duration::from_secs(2), turn_body.collect())
        .await
        .expect("cancelled turn stream did not finish")
        .unwrap()
        .to_bytes();
    let terminal = sse_payloads(std::str::from_utf8(&remaining).unwrap())
        .into_iter()
        .find(|event| event["kind"] == "done")
        .expect("cancelled terminal receipt");
    assert_eq!(terminal["ok"], false);
    assert_eq!(terminal["cancelled"], true);
    assert_eq!(terminal["retryable"], false);

    let after = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load runtime after explicit cancellation");
    assert_eq!(after.turn_count, before.turn_count);
    assert_eq!(after.transcript, before.transcript);
    assert_eq!(after.payload_json(), before.payload_json());
}

#[tokio::test]
async fn explicit_turn_chat_id_cannot_cross_persistence_scope() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let foreign_chat_id = state
        .store
        .create_chat("another-scope", None, None, 0, None, None, false)
        .unwrap();

    let (status, body) = post(
        &state,
        "/turn",
        json!({
            "chat_id": foreign_chat_id,
            "text": "Этот ход не должен попасть в чужую историю",
            "request_id": "foreign-scope-turn",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let error: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(error["ok"], false);
    assert_eq!(error["code"], "chat_not_found");
    let foreign = state
        .store
        .load_chat("another-scope", &foreign_chat_id)
        .unwrap();
    assert_eq!(foreign.turn_count, 0);
    assert!(foreign.transcript.is_empty());
}

#[tokio::test]
async fn turn_failure_rolls_back_and_successful_request_id_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, stream_calls) = fail_first_turn_state(&tmp);
    let chat_id = state.store.get_active("shared").expect("active chat");
    let before = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load pre-turn runtime");
    let before_payload = before.payload_json();
    let before_turn_count = before.turn_count;
    let (status, before_transcript_body) = get(&state, "/transcript").await;
    assert_eq!(status, StatusCode::OK);
    let before_transcript: Value = serde_json::from_slice(&before_transcript_body).unwrap();
    let request_id = "turn-idempotency-contract";
    let text = "Я осматриваю зал трактира.";

    let (status, failed_sse) = post_turn_request(&state, text, Some(request_id)).await;
    assert_eq!(status, StatusCode::OK);
    let failed_frames = sse_payloads(&failed_sse);
    let failed_done = failed_frames.last().expect("failed done frame");
    assert_eq!(failed_done["kind"], "done");
    assert_eq!(failed_done["ok"], false);
    assert_eq!(failed_done["retryable"], true);
    assert_eq!(failed_done["replayed"], false);
    assert_eq!(failed_done["request_id"], request_id);
    assert!(
        failed_frames.iter().any(|event| {
            event["kind"] == "error"
                && event["data"]
                    .as_str()
                    .is_some_and(|message| message.contains("injected retryable model failure"))
        }),
        "terminal model error must be streamed"
    );
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);

    let after_failure = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load runtime after failed attempt");
    assert_eq!(after_failure.payload_json(), before_payload);
    assert_eq!(after_failure.turn_count, before_turn_count);
    let (status, after_failure_transcript_body) = get(&state, "/transcript").await;
    assert_eq!(status, StatusCode::OK);
    let after_failure_transcript: Value =
        serde_json::from_slice(&after_failure_transcript_body).unwrap();
    assert_eq!(
        after_failure_transcript, before_transcript,
        "failed staged events must not leak through the cached transcript"
    );

    let (status, successful_sse) = post_turn_request(&state, text, Some(request_id)).await;
    assert_eq!(status, StatusCode::OK);
    let successful_done = assert_successful_done(&successful_sse);
    assert_eq!(successful_done["request_id"], request_id);
    let calls_after_success = stream_calls.load(Ordering::SeqCst);
    assert!(calls_after_success > 1, "retry must execute the model");

    let after_success = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load committed runtime");
    assert_eq!(after_success.turn_count, before_turn_count + 1);
    assert!(after_success
        .transcript
        .iter()
        .any(|row| { row.get("request_id").and_then(Value::as_str) == Some(request_id) }));
    let matching_player_events = after_success
        .transcript
        .iter()
        .filter(|row| {
            row.get("request_id").and_then(Value::as_str) == Some(request_id)
                && row["event"]["kind"] == "player"
                && row["event"]["data"] == text
        })
        .count();
    assert_eq!(
        matching_player_events, 1,
        "one logical request must persist exactly one matching player event"
    );
    let committed_payload = after_success.payload_json();

    let (status, replayed_sse) = post_turn_request(&state, text, Some(request_id)).await;
    assert_eq!(status, StatusCode::OK);
    let replayed_frames = sse_payloads(&replayed_sse);
    assert_eq!(
        replayed_frames.len(),
        1,
        "replay must not restream the turn"
    );
    let replayed_done = &replayed_frames[0];
    assert_eq!(replayed_done["kind"], "done");
    assert_eq!(replayed_done["ok"], true);
    assert_eq!(replayed_done["retryable"], false);
    assert_eq!(replayed_done["replayed"], true);
    assert_eq!(replayed_done["request_id"], request_id);
    assert_eq!(stream_calls.load(Ordering::SeqCst), calls_after_success);

    let after_replay = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load runtime after replay");
    assert_eq!(after_replay.payload_json(), committed_payload);

    let (status, conflict_sse) =
        post_turn_request(&state, "Другое действие", Some(request_id)).await;
    assert_eq!(status, StatusCode::OK);
    let conflict_frames = sse_payloads(&conflict_sse);
    let conflict_done = conflict_frames.last().expect("conflict done frame");
    assert_eq!(conflict_done["ok"], false);
    assert_eq!(conflict_done["retryable"], false);
    assert_eq!(conflict_done["replayed"], false);
    assert_eq!(stream_calls.load(Ordering::SeqCst), calls_after_success);
}

#[tokio::test]
async fn legacy_model_failure_resumes_same_turn_without_durable_duplicates() {
    let tmp = tempfile::tempdir().unwrap();
    let (state, stream_calls) = fail_first_turn_state(&tmp);
    let text = "Я осматриваю зал трактира.";
    let request_id = "legacy-resume-contract";
    let chat_id = seed_legacy_failed_turn(&state, text).await;

    let before = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load seeded legacy runtime");
    assert_eq!(before.turn_count, 1);
    assert_eq!(before.session.turn, 1);
    assert_eq!(
        before
            .session
            .gm_messages
            .iter()
            .filter(|message| **message == gml_agents::gm_user_message(text))
            .count(),
        1
    );
    let player_event = before
        .session
        .events
        .iter()
        .find(|event| event.turn == 1 && event.actor == "player" && event.speech == text)
        .expect("seeded player world event");
    let player_source_id = format!("world_event_{}", player_event.seq);
    let player_memory_count_before = before
        .session
        .world
        .world_canon
        .memory
        .units
        .values()
        .filter(|unit| unit.source_event_ids.contains(&player_source_id))
        .count();
    assert!(player_memory_count_before > 0);
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);

    let (status, resumed_sse) = post_legacy_turn_request(&state, text, request_id).await;
    assert_eq!(status, StatusCode::OK);
    let resumed_frames = sse_payloads(&resumed_sse);
    let resumed_done = resumed_frames.last().expect("resumed done frame");
    assert_eq!(resumed_done["kind"], "done");
    assert_eq!(
        resumed_done["ok"], true,
        "resume must commit: {resumed_done}"
    );
    assert_eq!(resumed_done["retryable"], false);
    assert_eq!(resumed_done["replayed"], false);
    assert_eq!(resumed_done["request_id"], request_id);
    assert!(
        resumed_frames.iter().all(|event| event["kind"] != "player"),
        "resume must not stream a second player echo: {resumed_frames:?}"
    );
    let calls_after_resume = stream_calls.load(Ordering::SeqCst);
    assert!(calls_after_resume > 1, "resume must call the model");

    let after = state
        .store
        .load_chat("shared", &chat_id)
        .expect("load resumed runtime");
    assert_eq!(after.turn_count, 1, "resume must not create a new turn");
    assert_eq!(after.session.turn, 1, "session turn stays unchanged");
    assert_eq!(
        after.session.run_usage.get("turns").and_then(Value::as_i64),
        Some(1),
        "the empty failed attempt is replaced by the successful usage"
    );
    assert_eq!(
        after
            .session
            .gm_messages
            .iter()
            .filter(|message| **message == gml_agents::gm_user_message(text))
            .count(),
        1,
        "resume must reuse the existing GM user action"
    );
    assert_eq!(
        after
            .session
            .events
            .iter()
            .filter(|event| { event.turn == 1 && event.actor == "player" && event.speech == text })
            .count(),
        1,
        "resume must reuse the existing player world event"
    );
    assert_eq!(
        after
            .session
            .world
            .world_canon
            .memory
            .units
            .values()
            .filter(|unit| unit.source_event_ids.contains(&player_source_id))
            .count(),
        player_memory_count_before,
        "resume must not duplicate player-event memories"
    );
    let player_rows: Vec<&Value> = after
        .transcript
        .iter()
        .filter(|row| row["event"]["kind"] == "player" && row["event"]["data"] == text)
        .collect();
    assert_eq!(player_rows.len(), 1, "one durable player transcript row");
    assert_eq!(player_rows[0]["request_id"], request_id);
    assert!(after.transcript.iter().all(|row| {
        row["event"]["kind"] != "error"
            || !row["event"]["data"]
                .as_str()
                .is_some_and(|message| message.starts_with("Ошибка вызова модели:"))
    }));

    let committed_payload = after.payload_json();
    let (status, replayed_sse) = post_legacy_turn_request(&state, text, request_id).await;
    assert_eq!(status, StatusCode::OK);
    let replayed_frames = sse_payloads(&replayed_sse);
    assert_eq!(replayed_frames.len(), 1, "replay must not restream events");
    assert_eq!(replayed_frames[0]["kind"], "done");
    assert_eq!(replayed_frames[0]["ok"], true);
    assert_eq!(replayed_frames[0]["replayed"], true);
    assert_eq!(replayed_frames[0]["request_id"], request_id);
    assert_eq!(stream_calls.load(Ordering::SeqCst), calls_after_resume);
    assert_eq!(
        state
            .store
            .load_chat("shared", &chat_id)
            .expect("load after replay")
            .payload_json(),
        committed_payload
    );
}

#[tokio::test]
async fn legacy_resume_rejects_unsafe_saved_shapes_without_calling_model() {
    for shape in [
        "extra_current_turn_row",
        "request_id_on_error_row",
        "nonzero_meta",
        "mismatched_run_usage",
        "wrong_gm_tail",
        "duplicate_player_world_event",
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let (state, stream_calls) = fail_first_turn_state(&tmp);
        let text = "Я осматриваю зал трактира.";
        let chat_id = seed_legacy_failed_turn(&state, text).await;
        let mut runtime = state
            .store
            .load_chat("shared", &chat_id)
            .expect("load legacy runtime for unsafe mutation");

        match shape {
            "extra_current_turn_row" => {
                let insert_at = runtime.transcript.len() - 2;
                runtime.transcript.insert(
                    insert_at,
                    json!({
                        "turn": runtime.turn_count,
                        "event": {
                            "kind": "delta",
                            "agent": "ГМ",
                            "data": {"text": "partial"},
                            "sid": null,
                        }
                    }),
                );
            }
            "request_id_on_error_row" => {
                runtime.transcript[1]["request_id"] = json!("old-receipt");
            }
            "nonzero_meta" => {
                runtime
                    .transcript
                    .last_mut()
                    .and_then(|row| row.get_mut("event"))
                    .and_then(|event| event.get_mut("data"))
                    .and_then(Value::as_object_mut)
                    .expect("meta_total data")["tokens"] = json!(1);
            }
            "mismatched_run_usage" => {
                runtime.transcript[2]["event"]["data"]["run"]["turns"] = json!(2);
            }
            "wrong_gm_tail" => runtime
                .session
                .gm_messages
                .push(json!({"role": "system", "content": "extra"})),
            "duplicate_player_world_event" => {
                let event = runtime
                    .session
                    .events
                    .iter()
                    .find(|event| event.actor == "player" && event.speech == text)
                    .expect("seeded player event")
                    .clone();
                runtime.session.events.push(event);
            }
            _ => unreachable!(),
        }

        state
            .store
            .save_owned(runtime)
            .expect("persist unsafe legacy shape");
        let unsafe_payload = state
            .store
            .load_chat("shared", &chat_id)
            .expect("load persisted unsafe shape")
            .payload_json();
        let calls_before = stream_calls.load(Ordering::SeqCst);

        let (status, rejected_sse) =
            post_legacy_turn_request(&state, text, &format!("unsafe-{shape}")).await;
        assert_eq!(status, StatusCode::OK, "shape: {shape}");
        let frames = sse_payloads(&rejected_sse);
        let done = frames.last().expect("rejected done frame");
        assert_eq!(done["kind"], "done", "shape: {shape}");
        assert_eq!(done["ok"], false, "shape: {shape}");
        assert_eq!(done["retryable"], false, "shape: {shape}");
        assert_eq!(done["replayed"], false, "shape: {shape}");
        assert_eq!(
            stream_calls.load(Ordering::SeqCst),
            calls_before,
            "unsafe shape must be rejected before model execution: {shape}"
        );
        assert_eq!(
            state
                .store
                .load_chat("shared", &chat_id)
                .expect("load after unsafe rejection")
                .payload_json(),
            unsafe_payload,
            "rejection must leave the saved runtime untouched: {shape}"
        );
    }
}

#[tokio::test]
async fn turn_rejects_non_boolean_legacy_resume_before_streaming() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/turn",
        json!({
            "text": "Я осматриваюсь.",
            "request_id": "bad-legacy-resume-type",
            "legacy_resume": "true",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let response: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"], "legacy_resume must be a boolean");
}

#[tokio::test]
async fn turn_replaces_cached_runtime_before_state_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    let (status, body) = get(&state, "/state").await;
    assert_eq!(status, StatusCode::OK);
    let before: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(before["run_usage"]["turns"], 0);

    let (status, sse) = post_turn_text(&state, "Я осматриваю зал трактира.").await;
    assert_eq!(status, StatusCode::OK);
    assert_successful_done(&sse);

    let (status, body) = get(&state, "/state").await;
    assert_eq!(status, StatusCode::OK);
    let after: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        after["run_usage"]["turns"], 1,
        "/state must read the just-saved streamed turn, not the stale pre-turn cache"
    );

    let (status, body) = get(&state, "/debug").await;
    assert_eq!(status, StatusCode::OK);
    let debug: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        debug["meta"]["turns"], 1,
        "/debug uses the same cached runtime and must be fresh too"
    );
}

#[tokio::test]
async fn transcribe_uses_the_active_chat_connector() {
    let tmp = tempfile::tempdir().unwrap();
    let state = connector_state(&tmp);
    let resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/transcribe")
                .header("content-type", "audio/webm")
                .body(Body::from(vec![1u8, 2, 3]))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let got: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["text"], "проверка распознавания");
    assert_eq!(got.get("connector_id"), None);
}

#[tokio::test]
async fn create_chat_without_story_requires_world_lore() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(&state, "/chats", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"]
        .as_str()
        .unwrap_or("")
        .contains("world_lore is required"));
}

/// Design §8: a bare procedural launch with world_lore but NO character is a 400
/// `protagonist_required` — no default hero is ever seeded. (world_lore is
/// present so the world_lore precedence 400 is NOT what fires here.)
#[tokio::test]
async fn procedural_chat_without_character_requires_protagonist() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/chats",
        serde_json::json!({
            "story_id": "procedural",
            "seed": "no-protagonist",
            "activate": true,
            "world_lore": architect_lore_json()
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "procedural no-char must 400: {}",
        String::from_utf8_lossy(&body)
    );
    let rejected: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["code"], json!("protagonist_required"));
}

/// Design §8: an authored story whose plot carries NO `player_character`, launched
/// without a `character_id`, is a 400 `protagonist_required`. With a character
/// package it launches fine.
#[tokio::test]
async fn authored_story_without_pc_requires_protagonist() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Мир без героя").await;

    // An authored story whose plot has no player_character.
    let (status, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "authored",
            "world_id": world_id,
            "title": "История без протагониста",
            "plot": {"story_brief": "Пролог без своего героя."}
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create authored: {}",
        String::from_utf8_lossy(&body)
    );
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    // Launch WITHOUT a character_id -> 400 protagonist_required.
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "authored no-PC launch must 400: {}",
        String::from_utf8_lossy(&body)
    );
    let rejected: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["code"], json!("protagonist_required"));

    // With a character package it launches fine.
    let char_id = create_test_character(&state).await;
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "character_id": char_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "authored + character launches: {}",
        String::from_utf8_lossy(&body)
    );
}

/// Review finding: a PROCEDURAL-kind story whose plot carries a protagonist is
/// exempt from the gate — so that protagonist must actually be seeded into the
/// generated world (previously the worldgen default hero leaked silently).
#[tokio::test]
async fn procedural_story_with_plot_pc_seeds_it_without_character() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Мир с плотовым героем").await;

    let (status, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "procedural",
            "world_id": world_id,
            "title": "Процедурная с героем",
            "plot": {
                "story_brief": "Процедурный пролог со своим героем.",
                "player_character": {"name": "Икс Плотовой", "class_role": "следопыт"}
            }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create: {}",
        String::from_utf8_lossy(&body)
    );
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    // No character_id: the gate is exempt because the plot carries a PC — and
    // that PC (not the worldgen default hero) must be the one playing.
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "plot-PC procedural launch must pass the gate: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        got["state"]["player_character"]["name"],
        json!("Икс Плотовой"),
        "the plot protagonist must be seeded, not the default hero: {}",
        got["state"]["player_character"]
    );
}

#[tokio::test]
async fn create_chat_with_story_id_returns_state() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/chats",
        serde_json::json!({"story_id": "frozen-harbor"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert!(got["chat"].is_object());
    assert!(got["state"].is_object());
    assert_eq!(got["state"]["story_id"], "frozen-harbor");
}

#[tokio::test]
async fn create_procedural_chat_is_canon_authoritative_and_turns() {
    // Locked decision #4: a procedural campaign is built from the living-world
    // canon plus an explicit architect-authored world_lore, and is fully playable
    // through a normal turn.
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let char_id = create_test_character(&state).await;

    // Pin a seed so the generated world is deterministic.
    let (status, body) = post(
        &state,
        "/chats",
        serde_json::json!({
            "story_id": "procedural",
            "seed": "12345",
            "activate": true,
            "character_id": char_id,
            "world_lore": architect_lore_json()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "procedural create should succeed");
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert!(got["chat"].is_object());
    assert!(
        got["state"].is_object(),
        "active procedural chat returns state"
    );
    assert_eq!(
        got["state"]["story_id"], "procedural",
        "story_id reflects the procedural route"
    );
    // The canon-derived scene must be non-empty (a real start place).
    let scene_title = got["state"]["scene"]["title"].as_str().unwrap_or("");
    assert!(
        !scene_title.is_empty(),
        "procedural start scene must have a canon-derived title, got: {got}"
    );

    // The session must be playable: POST /turn streams a normal turn.
    let resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/turn")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "text": "Я осматриваюсь и иду осмотреть окрестности."
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    assert_successful_done(&text);
    // The turn opened with the player echo (the scene was real enough to run).
    let mut kinds: Vec<String> = Vec::new();
    for frame in text.split("\n\n") {
        let frame = frame.trim();
        if let Some(payload) = frame.strip_prefix("data: ") {
            if let Ok(ev) = serde_json::from_str::<Value>(payload) {
                if let Some(k) = ev.get("kind").and_then(Value::as_str) {
                    kinds.push(k.to_string());
                }
            }
        }
    }
    assert_eq!(
        kinds.first().map(String::as_str),
        Some("player"),
        "turn must open with the player echo; kinds={kinds:?}"
    );
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("done"),
        "turn must end with done"
    );
}

#[tokio::test]
async fn create_procedural_chat_applies_world_manager_story_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let char_id = create_test_character(&state).await;
    let (status, body) = post(
        &state,
        "/chats",
        serde_json::json!({
            "story_id": "procedural",
            "seed": "world-manager-seed",
            "character_id": char_id,
            "genre": "postapocalyptic machine world",
            "tone": "bleak",
            "scale": "outpost",
            "title": "Пепельный Узел",
            "story_brief": "Ты приходишь к форпосту у старого машинного узла.",
            "public_intro": "Выжившие спорят за воду, энергию и право подходить к закрытому узлу.",
            "world_lore": architect_lore_json()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["chat"]["title"], "Пепельный Узел");
    assert_eq!(got["state"]["story_title"], "Пепельный Узел");
    assert_eq!(
        got["state"]["story_brief"]["text"],
        "Ты приходишь к форпосту у старого машинного узла."
    );

    let (status, body) = get(&state, "/debug").await;
    assert_eq!(status, StatusCode::OK);
    let debug: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(debug["story"]["title"], "Пепельный Узел");
    assert_eq!(
        debug["story"]["public_intro"],
        "Выжившие спорят за воду, энергию и право подходить к закрытому узлу."
    );
}

/// Parse the architect SSE stream and return the `architect_done` payload (or the
/// last `architect_error` frame). The handler streams `data: {json}\n\n` frames.
fn architect_result(body: &[u8]) -> Value {
    let text = String::from_utf8_lossy(body);
    let mut error = Value::Null;
    for frame in text.split("\n\n") {
        let Some(json) = frame.trim().strip_prefix("data: ") else {
            continue;
        };
        let Ok(ev) = serde_json::from_str::<Value>(json) else {
            continue;
        };
        match ev.get("kind").and_then(Value::as_str) {
            Some("architect_done") => return ev.get("data").cloned().unwrap_or(Value::Null),
            Some("architect_error") => error = ev,
            _ => {}
        }
    }
    error
}

#[tokio::test]
async fn world_architect_chat_returns_structured_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/world-architect/chat",
        serde_json::json!({
            "message": "Хочу фентезийный иссекай с богами и клятвами.",
            "history": [],
            "draft": {"genre": "fantasy isekai"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got = architect_result(&body);
    assert_eq!(got["ok"], true);
    // Agent loop: the model drafts the bible (hop 1, tool call) then finishes with
    // a chat reply (hop 2). The reply is the model's own text, no canned fallback.
    assert!(got["reply"]
        .as_str()
        .unwrap_or("")
        .contains("Порог Второго Неба"));
    assert_eq!(got["draft"]["title"], "Порог Второго Неба");
    assert!(got["draft"]["scale"].is_null());
    assert!(got["draft"]["story_brief"].is_null());
    assert!(got["draft"]["public_intro"].is_null());
    assert_eq!(
        got["draft"]["world_size"],
        "Континент с несколькими королевствами, духами дорог и дальними землями за картой."
    );
    assert!(got["draft"]["population"]
        .as_str()
        .unwrap_or("")
        .contains("Десятки миллионов"));
    assert!(got["draft"]["public_premise"]
        .as_str()
        .unwrap_or("")
        .contains("Имя, клятва и долг"));
    assert_eq!(
        got["draft"]["world_lore"]["gods"][0],
        "Старшие Духи Порогов"
    );
    assert_eq!(got["calls"][0]["name"], "draft_world_bible");
    // Usage is summed across both hops (per-hop token counts add up like the main
    // chat's per-turn total): 2 × {in: 760, out: 120}.
    assert_eq!(got["usage"]["in"], 1520);
    assert_eq!(got["usage"]["out"], 240);
    assert_eq!(got["usage"]["tokens"], 1760);
    assert_eq!(got["stats"]["eval_count"], 240);
    assert!(got["request_messages"].is_array());
}

#[tokio::test]
async fn world_architect_chat_creates_world_and_persists_history() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/world-architect/chat",
        json!({
            "message": "Хочу фентезийный иссекай с богами и клятвами.",
            "draft": {"genre": "fantasy isekai"}
        }),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let got = architect_result(&body);
    assert_eq!(got["ok"], true);
    let world_id = got["world"]["id"].as_str().expect("world id");
    assert_eq!(got["world_id"], json!(world_id));
    assert_eq!(got["world"]["status"], json!("draft"));
    assert_eq!(got["world"]["title"], json!("Порог Второго Неба"));
    // The world/chat split: the CONTENT response never carries the chat.
    assert!(got["world"].get("architect_messages").is_none());
    assert!(got["world"].get("architect_model_history").is_none());
    assert_eq!(got["worlds"].as_array().unwrap().len(), 1);

    // The conversation is server-side now: GET /worlds/{id}/architect restores
    // the interleaved view — user, hop-1 reasoning, draft tool, reasoning, reply.
    let (status, body) = get(&state, &format!("/worlds/{world_id}/architect")).await;
    assert_eq!(status, StatusCode::OK);
    let chat: Value = serde_json::from_slice(&body).unwrap();
    let visible = chat["architect"]["messages"].as_array().unwrap();
    assert_eq!(visible.len(), 5);
    assert_eq!(visible[0]["role"], "user");
    assert_eq!(
        visible[0]["content"],
        "Хочу фентезийный иссекай с богами и клятвами."
    );
    assert_eq!(visible[1]["role"], "think");
    assert_eq!(visible[2]["name"], "draft_world_bible");
    assert_eq!(visible.last().unwrap()["role"], "assistant");
    assert!(visible.last().unwrap()["content"]
        .as_str()
        .unwrap_or("")
        .contains("Порог Второго Неба"));

    // The /worlds list stays chat-free.
    let (status, body) = get(&state, "/worlds").await;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(listed["worlds"][0]["id"], json!(world_id));
    assert!(listed["worlds"][0].get("architect_model_history").is_none());
    assert!(listed["worlds"][0].get("architect_messages").is_none());
}

#[tokio::test]
async fn update_world_marks_ready_and_preserves_architect_history() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/world-architect/chat",
        json!({
            "message": "Хочу мир клятв.",
            "draft": {"genre": "fantasy"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let created = architect_result(&body);
    let world_id = created["world"]["id"].as_str().expect("world id");

    let (status, body) = post(
        &state,
        &format!("/worlds/{world_id}"),
        json!({
            "status": "ready",
            "title": "Порог Второго Неба",
            "genre": "fantasy isekai",
            "tone": "tense hopeful",
            "world_size": "Континент с несколькими королевствами",
            "population": "Десятки миллионов жителей",
            "public_premise": "Клятвы и долги имеют силу закона и магии.",
            "world_lore": {
                "name": "Порог Второго Неба",
                "world_laws": ["магия требует имени, цены или признанного права"]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let updated: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(updated["ok"], true);
    assert_eq!(updated["world"]["id"], json!(world_id));
    assert_eq!(updated["world"]["status"], json!("ready"));
    // Content responses never carry the chat...
    assert!(updated["world"].get("architect_messages").is_none());
    // ...and the ready save leaves the server-side conversation intact.
    let (status, body) = get(&state, &format!("/worlds/{world_id}/architect")).await;
    assert_eq!(status, StatusCode::OK);
    let chat: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        chat["architect"]["messages"][0]["content"],
        "Хочу мир клятв."
    );
}

#[tokio::test]
async fn world_architect_chat_restores_cache_identity_and_returns_model_history() {
    let tmp = tempfile::tempdir().unwrap();
    let mut state = mock_state(&tmp);
    let spy = install_identity_spy(&mut state);

    // Turn 1 creates the world; the turn's cache identity is persisted
    // server-side (architect.json), not round-tripped through the client.
    let (status, body) = post(
        &state,
        "/world-architect/chat",
        json!({
            "message": "Собери основу мира.",
            "draft": {"title": "Первый черновик"}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got = architect_result(&body);
    assert_eq!(got["ok"], true);
    let world_id = got["world"]["id"].as_str().expect("world id").to_string();

    // The mock client has no identity of its own; seed the persisted chat (in
    // the dialogs SQLite — its real home) with known cache ids, keeping the
    // turn-1 conversation, then verify the next turn restores them.
    let stored = state
        .store
        .get_architect_chat("world", &world_id)
        .expect("read chat")
        .expect("chat persisted to the DB by turn 1");
    let mut seeded = stored.as_object().cloned().unwrap();
    seeded.insert(
        "cache_session_id".into(),
        json!("world-architect:test-session"),
    );
    seeded.insert(
        "cache_thread_id".into(),
        json!("world-architect:test-thread"),
    );
    state
        .store
        .set_architect_chat("world", &world_id, &Value::Object(seeded))
        .expect("seed cache ids");
    let session_1 = "world-architect:test-session".to_string();
    let thread_1 = "world-architect:test-thread".to_string();

    // Turn 2 against the SAME world: the server restores the stored identity
    // into the fresh client, and the model history it replays carries the
    // turn-1 user message as PLAIN TEXT (no draft snapshot — the token fix).
    let (status, body) = post(
        &state,
        "/world-architect/chat",
        json!({
            "message": "Добавь религии.",
            "world_id": world_id
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got2 = architect_result(&body);
    assert_eq!(got2["ok"], true);
    assert_eq!(got2["assistant_history_message"]["role"], "assistant");
    assert_eq!(
        got2["assistant_history_message"]["content"],
        "Ответ архитектора"
    );

    let spy = spy.lock().expect("identity spy lock");
    assert_eq!(spy.session_id, session_1);
    assert_eq!(spy.thread_id, thread_1);
    let sent = spy.messages.last().unwrap().as_array().unwrap();
    assert_eq!(sent[0]["role"], "system");
    // History entry = the turn-1 user TEXT, verbatim, with no digest/draft.
    assert_eq!(sent[1]["role"], "user");
    assert_eq!(sent[1]["content"], "Собери основу мира.");
    // CACHE INVARIANT: the tail is the RAW user text — byte-equal to what the
    // server stores in history, so the whole prefix stays cacheable. State
    // never rides in messages (the model reads it via read_world_bible).
    assert_eq!(sent.last().unwrap()["content"], "Добавь религии.");
}

#[tokio::test]
async fn world_architect_chat_tools_follow_image_generation_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let mut state = mock_state(&tmp);
    let spy = install_identity_spy(&mut state);

    let (status, _body) = post(
        &state,
        "/world-architect/chat",
        json!({"message": "Собери мир с картой."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let tools = {
        let spy = spy.lock().expect("identity spy lock");
        spy.tools.last().cloned().expect("architect tools")
    };
    let props = draft_world_bible_properties(&tools);
    assert!(props.contains_key("world_image_prompt_en"));
    assert!(props.contains_key("world_map_prompt_en"));

    let tmp = tempfile::tempdir().unwrap();
    let mut state = mock_state(&tmp);
    let spy = install_identity_spy(&mut state);
    let (status, _body) = post(
        &state,
        "/settings",
        json!({"settings": {"image_enabled": false}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _body) = post(
        &state,
        "/world-architect/chat",
        json!({"message": "Собери мир без картинок."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let tools = {
        let spy = spy.lock().expect("identity spy lock");
        spy.tools.last().cloned().expect("architect tools")
    };
    let props = draft_world_bible_properties(&tools);
    assert!(!props.contains_key("world_image_prompt_en"));
    assert!(!props.contains_key("world_map_prompt_en"));
}

// =========================================================================
// С1 story architect (docs/CHARACTERS_AND_STORY_TZ.md §С1.1–С1.3)
// =========================================================================

#[tokio::test]
async fn story_architect_chat_creates_bound_story_and_persists_meta() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Порог Второго Неба").await;

    // First turn with NO story_id: the handler creates a world-bound authored
    // story BEFORE the model call, then folds the drafted plot in. The chat
    // state goes to the package's architect.json, never into story.json.
    let (status, body) = post(
        &state,
        "/story-architect/chat",
        json!({
            "message": "Сделай пролог в деревне у живой дороги.",
            "world_id": world_id
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got = architect_result(&body);
    assert_eq!(got["ok"], true, "architect_done: {got}");
    let story_id = got["story_id"].as_str().expect("story_id").to_string();
    assert_eq!(got["story"]["id"], json!(story_id));
    // The mock story architect drafts a plot: title + plot fields land in seed.
    assert!(got["reply"]
        .as_str()
        .unwrap_or("")
        .contains("Деревня у живой дороги"));
    assert_eq!(got["draft"]["title"], "Деревня у живой дороги");
    assert_eq!(got["story"]["kind"], "authored");
    assert_eq!(got["story"]["world_ref"]["id"], json!(world_id));
    assert_eq!(
        got["story"]["seed"]["hidden_truth"],
        "Староста скормил дороге собственного сына ради урожая."
    );
    // The story/chat split: the content row carries NO chat state at all.
    assert!(got["story"]["seed"].get("architect_messages").is_none());
    assert!(got["story"].get("architect_messages").is_none());
    assert!(got["story"].get("architect_cache_session_id").is_none());
    assert!(
        got["story"].get("meta").is_none(),
        "draft row is content-only — no nested meta"
    );
    // The conversation is restored via GET /stories/{id}/draft's architect block.
    let (status, body) = get(&state, &format!("/stories/{story_id}/draft")).await;
    assert_eq!(status, StatusCode::OK);
    let drafted: Value = serde_json::from_slice(&body).unwrap();
    let messages = drafted["architect"]["messages"].as_array().unwrap();
    assert_eq!(
        messages[0]["content"],
        "Сделай пролог в деревне у живой дороги."
    );
    assert!(messages.last().unwrap()["content"]
        .as_str()
        .unwrap_or("")
        .contains("Деревня у живой дороги"));
    assert!(drafted["story"].get("architect_messages").is_none());
    // The done payload mirrors the world one + {story_id, story, stories}.
    assert!(got["stories"].is_array());
    // The list refresher stays the MINIMAL player-facing catalog: no plot seed
    // (hidden_truth is GM-only) and no architect chat state leaks into it.
    for row in got["stories"].as_array().expect("stories array") {
        assert!(
            row.get("seed").is_none(),
            "catalog row must not leak the plot seed: {row}"
        );
        assert!(
            row.get("architect_messages").is_none(),
            "catalog row must not leak architect chat state: {row}"
        );
    }
    assert_eq!(got["calls"][0]["name"], "draft_story_plot");

    // Second turn WITH the story_id: version bumps further, plot is refined.
    let v1 = got["story"]["version"].as_u64().unwrap();
    let (status, body) = post(
        &state,
        "/story-architect/chat",
        json!({
            "message": "Усиль улики.",
            "history": [],
            "draft": got["draft"].clone(),
            "story_id": story_id,
            "world_id": world_id
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got2 = architect_result(&body);
    assert_eq!(got2["ok"], true);
    assert_eq!(got2["story_id"], json!(story_id));
    assert!(
        got2["story"]["version"].as_u64().unwrap() > v1,
        "version must bump per turn"
    );
}

#[tokio::test]
async fn story_architect_chat_requires_world_for_new_story() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    // No story_id and no world_id -> hard 400, nothing created.
    let (status, body) = post(
        &state,
        "/story-architect/chat",
        json!({"message": "Сделай историю."}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"].as_str().unwrap_or("").contains("world_id"));
}

#[tokio::test]
async fn story_architect_chat_rejects_unknown_world() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/story-architect/chat",
        json!({"message": "Сделай историю.", "world_id": "nope-world"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"].as_str().unwrap_or("").contains("nope-world"));
}

// =========================================================================
// character architect (mirror of the story architect; standalone hero)
// =========================================================================

#[tokio::test]
async fn character_architect_chat_creates_package_and_persists_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // First turn with NO character_id: the handler creates a .gmchar package
    // BEFORE the model call, then snapshots the drafted sheet. The chat state goes
    // to the dialogs SQLite (architect_chats), never into the package payload.
    let (status, body) = post(
        &state,
        "/character-architect/chat",
        json!({"message": "Сделай следопытку с луком."}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got = architect_result(&body);
    assert_eq!(got["ok"], true, "architect_done: {got}");
    let character_id = got["character_id"]
        .as_str()
        .expect("character_id")
        .to_string();
    assert_eq!(got["character"]["id"], json!(character_id));
    // The mock character architect drafts a flat sheet: name + stats land in the
    // package's payload.player_character.
    assert!(got["reply"].as_str().unwrap_or("").contains("Кара Вент"));
    assert_eq!(got["draft"]["name"], "Кара Вент");
    assert_eq!(
        got["character"]["payload"]["player_character"]["name"],
        "Кара Вент"
    );
    assert_eq!(
        got["character"]["payload"]["player_character"]["abilities"]["DEX"],
        16
    );
    assert_eq!(
        got["character"]["payload"]["player_character"]["spells"][0]["name"],
        "Отметка охотника"
    );
    // The package/chat split: the content payload carries NO chat state.
    assert!(got["character"]["payload"]
        .get("architect_messages")
        .is_none());
    assert!(got["character"].get("architect_messages").is_none());
    assert!(got["character"].get("architect_cache_session_id").is_none());
    assert!(got["characters"].is_array());
    assert_eq!(got["calls"][0]["name"], "draft_player_character");

    // The conversation is restored via GET /characters/{id}/architect — user,
    // hop-1 reasoning, draft tool, reasoning, reply.
    let (status, body) = get(&state, &format!("/characters/{character_id}/architect")).await;
    assert_eq!(status, StatusCode::OK);
    let chat: Value = serde_json::from_slice(&body).unwrap();
    let visible = chat["architect"]["messages"].as_array().unwrap();
    assert_eq!(visible[0]["role"], "user");
    assert_eq!(visible[0]["content"], "Сделай следопытку с луком.");
    assert!(visible
        .iter()
        .any(|m| m["name"] == "draft_player_character"));
    assert_eq!(visible.last().unwrap()["role"], "assistant");
    assert!(visible.last().unwrap()["content"]
        .as_str()
        .unwrap_or("")
        .contains("Кара Вент"));

    // The /characters list stays chat-free.
    let (status, body) = get(&state, "/characters").await;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&body).unwrap();
    let row = &listed["characters"][0];
    assert_eq!(row["id"], json!(character_id));
    assert!(row.get("architect_messages").is_none());
    assert!(row["payload"].get("architect_messages").is_none());

    // Second turn WITH the character_id: version bumps, sheet is re-snapshotted.
    let v1 = got["character"]["version"].as_u64().unwrap();
    let (status, body) = post(
        &state,
        "/character-architect/chat",
        json!({
            "message": "Подними уровень.",
            "draft": got["draft"].clone(),
            "character_id": character_id
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got2 = architect_result(&body);
    assert_eq!(got2["ok"], true);
    assert_eq!(got2["character_id"], json!(character_id));
    assert!(
        got2["character"]["version"].as_u64().unwrap() > v1,
        "version must bump per turn"
    );
}

#[tokio::test]
async fn character_architect_chat_requires_message() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(&state, "/character-architect/chat", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"].as_str().unwrap_or("").contains("message"));
}

#[tokio::test]
async fn get_character_architect_unknown_is_404() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = get(&state, "/characters/nope/architect").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"].as_str().unwrap_or("").contains("nope"));
}

#[tokio::test]
async fn save_protagonist_creates_character_from_story_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Мир для протагониста").await;

    // Author a story via the architect (the mock drafts a player_character "Мира").
    let (status, body) = post(
        &state,
        "/story-architect/chat",
        json!({"message": "Сделай пролог.", "world_id": world_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got = architect_result(&body);
    let story_id = got["story_id"].as_str().expect("story_id").to_string();
    assert_eq!(got["story"]["seed"]["player_character"]["name"], "Мира");

    // Save the suggested protagonist into a .gmchar package.
    let (status, body) = post(
        &state,
        &format!("/stories/{story_id}/save-protagonist"),
        json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "save-protagonist: {}",
        String::from_utf8_lossy(&body)
    );
    let saved: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(saved["ok"], true);
    // The loose authored PC is coerced through the canonical seam → full sheet.
    assert_eq!(
        saved["character"]["payload"]["player_character"]["name"],
        "Мира"
    );
    assert_eq!(saved["character"]["title"], "Мира");
    // The coercion fills the canonical stat fields even when the draft omitted them.
    assert!(saved["character"]["payload"]["player_character"]
        .get("abilities")
        .is_some());
    // The new package appears in the library.
    let (status, body) = get(&state, "/characters").await;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(listed["characters"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn save_protagonist_rejects_procedural_builtin_and_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Мир").await;

    // Procedural story -> 400 (draft_row rejects non-authored).
    let (_s, body) = post(
        &state,
        "/stories",
        json!({"kind": "procedural", "world_id": world_id, "title": "Процедурная"}),
    )
    .await;
    let created: Value = serde_json::from_slice(&body).unwrap();
    let proc_id = created["story"]["id"].as_str().unwrap().to_string();
    let (status, body) = post(
        &state,
        &format!("/stories/{proc_id}/save-protagonist"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert!(got["error"].as_str().unwrap_or("").contains("authored"));

    // Self-contained builtin -> 400.
    let (status, _body) = post(
        &state,
        "/stories/turnvale-murder/save-protagonist",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Unknown story -> 404.
    let (status, _body) = post(&state, "/stories/nope/save-protagonist", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // An authored story WITHOUT a protagonist -> 400 (nothing to save).
    let (_s, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "authored",
            "world_id": world_id,
            "title": "Без героя",
            "plot": {"story_brief": "старт"}
        }),
    )
    .await;
    let created: Value = serde_json::from_slice(&body).unwrap();
    let no_pc_id = created["story"]["id"].as_str().unwrap().to_string();
    let (status, body) = post(
        &state,
        &format!("/stories/{no_pc_id}/save-protagonist"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert!(got["error"].as_str().unwrap_or("").contains("protagonist"));
}

#[tokio::test]
async fn update_story_route_merges_and_bumps_version() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Мир историй").await;

    // Create an authored story to edit.
    let (_s, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "authored",
            "world_id": world_id,
            "title": "Черновая",
            "plot": {"story_brief": "старт", "hidden_truth": "тайна"}
        }),
    )
    .await;
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    // Plain update route: merge seed (add public_intro, drop hidden_truth) + set
    // meta; version bumps.
    let (status, body) = post(
        &state,
        &format!("/stories/{story_id}"),
        json!({
            "title": "Готовая",
            "seed": {"public_intro": "интро", "hidden_truth": null},
            "meta": {"architect_cache_session_id": "s:1"}
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "update: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["story"]["title"], "Готовая");
    assert_eq!(got["story"]["version"], 2);
    assert_eq!(got["story"]["seed"]["story_brief"], "старт");
    assert_eq!(got["story"]["seed"]["public_intro"], "интро");
    assert!(got["story"]["seed"].get("hidden_truth").is_none());
    assert_eq!(got["story"]["meta"]["architect_cache_session_id"], "s:1");
}

#[tokio::test]
async fn update_story_route_rejects_builtin_and_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // A self-contained builtin cannot be architect-edited.
    let (status, body) = post(&state, "/stories/turnvale-murder", json!({"title": "x"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"]
        .as_str()
        .unwrap_or("")
        .contains("self-contained"));

    // Unknown id -> 400.
    let (status, body) = post(&state, "/stories/nope", json!({"title": "x"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert!(got["error"].as_str().unwrap_or("").contains("not found"));

    // A world-bound PROCEDURAL story clears the builtin guard but is architect-
    // uneditable: its launch path ignores an authored seed, so folding a plot in
    // would be silent data loss. Both the plain update route and the architect
    // turn must reject it up front (before any model call).
    let world_id = create_saved_world(&state, "Мир для процедурной").await;
    let (_s, body) = post(
        &state,
        "/stories",
        json!({"kind": "procedural", "world_id": world_id, "title": "Процедурная"}),
    )
    .await;
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        &state,
        &format!("/stories/{story_id}"),
        json!({"title": "x"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "update route: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert!(got["error"].as_str().unwrap_or("").contains("authored"));

    let (status, body) = post(
        &state,
        "/story-architect/chat",
        json!({"story_id": story_id, "message": "Допиши сюжет."}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "architect route: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert!(got["error"].as_str().unwrap_or("").contains("authored"));
}

#[tokio::test]
async fn create_procedural_chat_accepts_world_lore_from_architect() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let char_id = create_test_character(&state).await;
    let (status, body) = post(
        &state,
        "/chats",
        serde_json::json!({
            "story_id": "procedural",
            "seed": "architect-lore-server",
            "character_id": char_id,
            "genre": "fantasy isekai",
            "tone": "tense",
            "scale": "region",
            "world_lore": {
                "name": "Город Железных Снов",
                "public_premise": "Люди живут в тени спящего машинного бога.",
                "religions": ["церковь Спящего Механизма"],
                "gods": ["Машинный Бог под городом"],
                "location_rules": ["новые места должны показывать связь с машинным культом"]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["state"]["story_title"], "Город Железных Снов");

    let (status, body) = get(&state, "/debug").await;
    assert_eq!(status, StatusCode::OK);
    let debug: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(debug["story"]["title"], "Город Железных Снов");
    assert_eq!(
        debug["story"]["public_intro"],
        "Люди живут в тени спящего машинного бога."
    );
}

#[tokio::test]
async fn settings_update_persists_and_reflects() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/settings",
        serde_json::json!({"settings": {"gm_suggest_options": true}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["settings"]["gm_suggest_options"], true);
    // /state reflects the new setting.
    let (_s, sbody) = get(&state, "/state").await;
    let st: Value = serde_json::from_slice(&sbody).unwrap();
    assert_eq!(st["settings"]["gm_suggest_options"], true);
}

// =========================================================================
// Phase 2 — world images live INSIDE the package, served independent of the
// image-generation flag and the sidecar lifecycle.
// =========================================================================

/// A small PNG-shaped byte payload the stub sidecar serves (a real PNG header
/// so `Content-Type: image/png` is plausible; the bytes only need to round-trip).
const STUB_PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR-stub-world-image";

/// Spin up a tiny local HTTP server that serves [`STUB_PNG`] at any
/// `/image-files/...` and `/images/...` path and 404s `/missing/...`. Returns
/// the base URL (e.g. `http://127.0.0.1:54321`); the server task runs detached
/// for the test's lifetime.
async fn spawn_stub_sidecar() -> String {
    use axum::routing::get as axget;
    async fn png() -> impl axum::response::IntoResponse {
        (
            [(axum::http::header::CONTENT_TYPE, "image/png")],
            STUB_PNG.to_vec(),
        )
    }
    async fn missing() -> axum::http::StatusCode {
        axum::http::StatusCode::NOT_FOUND
    }
    let app = axum::Router::new()
        .route("/image-files/{run}/{file}", axget(png))
        .route("/images/{run}/{file}", axget(png))
        .route("/missing/{run}/{file}", axget(missing));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub sidecar");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

/// GET that also returns the `Content-Type` header (for asset-route assertions).
async fn get_with_content_type(
    state: &AppState,
    path: &str,
) -> (StatusCode, Option<String>, Vec<u8>) {
    let resp = build_router(state.clone())
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let ctype = resp
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, ctype, bytes.to_vec())
}

#[tokio::test]
async fn create_world_ingests_sidecar_image_into_package() {
    let base = spawn_stub_sidecar().await;
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state_with_infer_url(&tmp, &base);

    let (status, body) = post(
        &state,
        "/worlds",
        json!({
            "title": "Мир с обложкой",
            "genre": "fantasy",
            "tone": "tense",
            "world_size": "Континент",
            "population": "Миллионы",
            "public_premise": "Образы важны.",
            "world_lore": {
                "name": "Мир с обложкой",
                "public_premise": "Образы важны.",
                "world_image_url": "/image-files/run-123/image_0.png"
            }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    let world_id = got["world"]["id"].as_str().unwrap().to_string();

    // RESPONSE field is the servable same-origin route.
    assert_eq!(
        got["world"]["world_lore"]["world_image_url"],
        json!(format!("/world-assets/{world_id}/world_image.png"))
    );

    // Bytes landed inside the package.
    let on_disk = tmp
        .path()
        .join("library")
        .join("worlds")
        .join(&world_id)
        .join("assets")
        .join("world_image.png");
    assert!(on_disk.is_file(), "asset written to package");
    assert_eq!(std::fs::read(&on_disk).unwrap(), STUB_PNG);

    // STORED manifest field is the package-relative path (portable).
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(
            tmp.path()
                .join("library")
                .join("worlds")
                .join(&world_id)
                .join("world.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["payload"]["world_lore"]["world_image_url"],
        json!("assets/world_image.png")
    );

    // The static route serves the bytes with image/png.
    let (status, ctype, asset_bytes) =
        get_with_content_type(&state, &format!("/world-assets/{world_id}/world_image.png")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ctype.as_deref(), Some("image/png"));
    assert_eq!(asset_bytes, STUB_PNG);

    // GET /worlds also rewrites to the servable route.
    let (_s, list_body) = get(&state, "/worlds").await;
    let list: Value = serde_json::from_slice(&list_body).unwrap();
    assert_eq!(
        list["worlds"][0]["world_lore"]["world_image_url"],
        json!(format!("/world-assets/{world_id}/world_image.png"))
    );
}

#[tokio::test]
async fn update_world_is_idempotent_for_already_ingested_image() {
    let base = spawn_stub_sidecar().await;
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state_with_infer_url(&tmp, &base);

    let (_s, body) = post(
        &state,
        "/worlds",
        json!({
            "title": "Мир",
            "genre": "fantasy",
            "tone": "tense",
            "world_size": "Континент",
            "population": "Миллионы",
            "public_premise": "Образы важны.",
            "world_lore": {"name": "Мир", "world_image_url": "/image-files/run-1/image_0.png"}
        }),
    )
    .await;
    let got: Value = serde_json::from_slice(&body).unwrap();
    let world_id = got["world"]["id"].as_str().unwrap().to_string();

    // Re-saving with the already-servable route must NOT re-fetch and must keep
    // the field as a servable route (the stored manifest re-derives relative).
    let (status, body2) = post(
        &state,
        &format!("/worlds/{world_id}"),
        json!({
            "title": "Мир",
            "genre": "fantasy",
            "tone": "tense",
            "world_size": "Континент",
            "population": "Миллионы",
            "public_premise": "Образы важны.",
            "world_lore": {
                "name": "Мир",
                "world_image_url": format!("/world-assets/{world_id}/world_image.png")
            }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {}",
        String::from_utf8_lossy(&body2)
    );
    let got2: Value = serde_json::from_slice(&body2).unwrap();
    assert_eq!(
        got2["world"]["world_lore"]["world_image_url"],
        json!(format!("/world-assets/{world_id}/world_image.png"))
    );
    // Manifest stays relative.
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(
            tmp.path()
                .join("library")
                .join("worlds")
                .join(&world_id)
                .join("world.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        manifest["payload"]["world_lore"]["world_image_url"],
        json!("assets/world_image.png")
    );
}

#[tokio::test]
async fn empty_image_field_is_valid_and_left_empty() {
    let base = spawn_stub_sidecar().await;
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state_with_infer_url(&tmp, &base);

    let (status, body) = post(
        &state,
        "/worlds",
        json!({
            "title": "Мир без картинки",
            "genre": "fantasy",
            "tone": "tense",
            "world_size": "Континент",
            "population": "Миллионы",
            "public_premise": "Без образов.",
            "world_lore": {"name": "Мир без картинки", "world_image_url": ""}
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    // Empty stays empty (a valid "no image" state), not an error, not a route.
    assert_eq!(got["world"]["world_lore"]["world_image_url"], json!(""));
    let world_id = got["world"]["id"].as_str().unwrap();
    let assets = tmp
        .path()
        .join("library")
        .join("worlds")
        .join(world_id)
        .join("assets");
    assert!(!assets.exists(), "no assets dir created for an empty image");
}

#[tokio::test]
async fn missing_sidecar_image_fails_save_and_writes_no_asset() {
    let base = spawn_stub_sidecar().await;
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state_with_infer_url(&tmp, &base);

    let (status, body) = post(
        &state,
        "/worlds",
        json!({
            "title": "Мир со сломанной картинкой",
            "genre": "fantasy",
            "tone": "tense",
            "world_size": "Континент",
            "population": "Миллионы",
            "public_premise": "Образ недоступен.",
            "world_lore": {
                "name": "Мир со сломанной картинкой",
                "world_image_url": "/missing/run-404/image_0.png"
            }
        }),
    )
    .await;
    // No-fallback: the save FAILS rather than dropping the reference or writing
    // a placeholder.
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(
        got["error"].as_str().unwrap_or("").contains("ingest"),
        "error mentions ingest: {got:?}"
    );

    // The pre-allocated package may exist, but NO image asset was written.
    let worlds_dir = tmp.path().join("library").join("worlds");
    if let Ok(entries) = std::fs::read_dir(&worlds_dir) {
        for entry in entries.flatten() {
            let asset = entry.path().join("assets").join("world_image.png");
            assert!(!asset.is_file(), "no placeholder asset written");
        }
    }
}

#[tokio::test]
async fn world_asset_route_works_when_image_generation_disabled() {
    let base = spawn_stub_sidecar().await;
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state_with_infer_url(&tmp, &base);

    let (status, body) = post(
        &state,
        "/worlds",
        json!({
            "title": "Мир",
            "genre": "fantasy",
            "tone": "tense",
            "world_size": "Континент",
            "population": "Миллионы",
            "public_premise": "Образы важны.",
            "world_lore": {"name": "Мир", "world_map_url": "/images/run-9/map_0.png"}
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    let world_id = got["world"]["id"].as_str().unwrap().to_string();
    assert_eq!(
        got["world"]["world_lore"]["world_map_url"],
        json!(format!("/world-assets/{world_id}/world_map.png"))
    );

    // Turn OFF image generation at runtime, then confirm the asset still serves.
    let (s, _b) = post(
        &state,
        "/settings",
        json!({"settings": {"image_enabled": false}}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let (status, ctype, asset_bytes) =
        get_with_content_type(&state, &format!("/world-assets/{world_id}/world_map.png")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "asset route must not be gated by image_enabled"
    );
    assert_eq!(ctype.as_deref(), Some("image/png"));
    assert_eq!(asset_bytes, STUB_PNG);
}

#[tokio::test]
async fn world_asset_route_rejects_bad_filename_and_404s_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // Bad filename (extension not allowed) -> 400.
    let (status, _c, _b) =
        get_with_content_type(&state, "/world-assets/abc123/world_image.txt").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Missing asset for a syntactically valid id/file -> 404.
    let (status, _c, _b) =
        get_with_content_type(&state, "/world-assets/abc123/world_image.png").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// =========================================================================
// Phase 4 — launch SAVED packages into the game (docs/MODS_PACKAGES_TZ.md):
// play a saved world procedurally, create stories bound to a world, and
// launch procedural / authored stories that compose the world + the plot.
// =========================================================================

/// Create a saved WORLD package via POST /worlds and return its id.
async fn create_saved_world(state: &AppState, title: &str) -> String {
    let (status, body) = post(
        state,
        "/worlds",
        json!({
            "title": title,
            "genre": "fantasy isekai",
            "tone": "tense hopeful",
            "world_size": "Континент",
            "population": "Миллионы",
            "public_premise": "Клятвы и долги имеют силу закона и магии.",
            "world_lore": {
                "name": title,
                "public_premise": "Клятвы и долги имеют силу закона и магии.",
                "world_laws": ["магия требует имени, цены или признанного права"],
                "location_rules": ["каждая локация связана с долгом, властью, дорогой или духом"]
            }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create world: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    got["world"]["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn play_saved_world_procedurally_records_world_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Порог Второго Неба").await;
    let char_id = create_test_character(&state).await;

    // POST /chats {world_id, story_id:"procedural"} -> procedural launch from the
    // SAVED world's lore (no inline world_lore supplied).
    let (status, body) = post(
        &state,
        "/chats",
        json!({
            "world_id": world_id,
            "story_id": "procedural",
            "seed": "play-saved-world",
            "activate": true,
            "character_id": char_id,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "play saved world: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    // The world content came from the saved package: the lore name became the
    // story title, and the public premise the public intro.
    assert_eq!(got["state"]["story_title"], "Порог Второго Неба");

    let (status, dbody) = get(&state, "/debug").await;
    assert_eq!(status, StatusCode::OK);
    let debug: Value = serde_json::from_slice(&dbody).unwrap();
    assert_eq!(debug["story"]["title"], "Порог Второго Неба");
    assert_eq!(debug["story"]["id"], "procedural");
    assert_eq!(
        debug["story"]["public_intro"],
        "Клятвы и долги имеют силу закона и магии."
    );

    // Provenance: the persisted session.world records world_ref with the right id.
    state
        .store
        .with_runtime("shared", got["active_chat_id"].as_str().unwrap(), |rt| {
            let world_ref = rt
                .session
                .world
                .world_ref
                .as_ref()
                .expect("world_ref recorded");
            assert_eq!(world_ref.id, world_id);
        })
        .unwrap();
}

#[tokio::test]
async fn play_saved_world_with_missing_world_id_errors_and_creates_no_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    let (status, body) = post(
        &state,
        "/chats",
        json!({
            "world_id": "does-not-exist",
            "story_id": "procedural",
            "activate": true,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "dangling world_id must fail"
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"]
        .as_str()
        .unwrap_or_default()
        .contains("does-not-exist"));
    // No chat was created by the failed request. Read the store DIRECTLY
    // (GET /chats would auto-create a default chat via get_active, which is
    // unrelated to our failed launch).
    let chats = state.store.list_chats("shared").unwrap();
    assert!(
        chats.is_empty(),
        "a failed launch must not persist a chat, got {chats:?}"
    );
}

#[tokio::test]
async fn create_and_launch_procedural_story_bound_to_world() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Город Железных Снов").await;
    let char_id = create_test_character(&state).await;

    // POST /stories {kind:"procedural", world_id, title} -> a story package bound
    // to the world.
    let (status, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "procedural",
            "world_id": world_id,
            "title": "Заводская смена",
            "description": "Короткий пролог в дымном цеху.",
            "plot": {"story_brief": "Ты выходишь на смену, когда машины начинают шептать."}
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create procedural story: {}",
        String::from_utf8_lossy(&body)
    );
    let created: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(created["ok"], true);
    let story_id = created["story"]["id"].as_str().unwrap().to_string();
    assert_eq!(created["story"]["title"], "Заводская смена");

    // It shows up in GET /stories.
    let (_s, sbody) = get(&state, "/stories").await;
    let stories: Value = serde_json::from_slice(&sbody).unwrap();
    assert!(stories["stories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == json!(story_id)));

    // Launch it: procedural world from the bound lore + the story title/brief.
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "character_id": char_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "launch procedural story: {}",
        String::from_utf8_lossy(&body)
    );
    let launched: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(launched["ok"], true);
    assert_eq!(launched["state"]["story_title"], "Заводская смена");

    let (_s, dbody) = get(&state, "/debug").await;
    let debug: Value = serde_json::from_slice(&dbody).unwrap();
    assert_eq!(debug["story"]["title"], "Заводская смена");
    assert_eq!(
        debug["story"]["brief"],
        "Ты выходишь на смену, когда машины начинают шептать."
    );
    // The public intro fell back to the bound world's premise.
    assert_eq!(
        debug["story"]["public_intro"],
        "Клятвы и долги имеют силу закона и магии."
    );

    // Provenance: both world_ref and story_ref are recorded.
    state
        .store
        .with_runtime(
            "shared",
            launched["active_chat_id"].as_str().unwrap(),
            |rt| {
                assert_eq!(
                    rt.session.world.world_ref.as_ref().map(|r| r.id.as_str()),
                    Some(world_id.as_str())
                );
                assert_eq!(
                    rt.session.world.story_ref.as_ref().map(|r| r.id.as_str()),
                    Some(story_id.as_str())
                );
            },
        )
        .unwrap();
}

#[tokio::test]
async fn create_and_launch_authored_story_composes_world_and_plot() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Порог Второго Неба").await;

    // POST /stories {kind:"authored", world_id, title, plot{...}}.
    let (status, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "authored",
            "world_id": world_id,
            "title": "Деревня у живой дороги",
            "plot": {
                "story_brief": "Ты пришёл в деревню, где дорога просыпается по ночам.",
                "public_intro": "Деревня живёт по правилам дороги.",
                "hidden_truth": "Староста скормил дороге собственного сына ради урожая.",
                "player_character": {"name": "Мира", "class_role": "странствующий писец"},
                "proper_nouns": ["Живая Дорога"],
                "npcs": [
                    {"id": "starosta", "name": "Старый Гедд", "role": "староста",
                     "persona": "Усталый человек, скрывающий вину."}
                ],
                "public_facts": [
                    {"id": "road_wakes", "text": "Дорога шевелится в полнолуние.", "kind": "public"}
                ],
                "scene": {
                    "id": "village_gate",
                    "title": "Ворота деревни",
                    "location_id": "village_gate",
                    "description": "Покосившиеся ворота у кромки живой дороги.",
                    "present_npcs": ["starosta"],
                    "tension": "Дорога вот-вот проснётся."
                },
                "time": 1080
            }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create authored story: {}",
        String::from_utf8_lossy(&body)
    );
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    // Launch it -> the World carries BOTH the world's lore-derived content AND
    // the authored plot.
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "launch authored story: {}",
        String::from_utf8_lossy(&body)
    );
    let launched: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(launched["ok"], true);
    assert_eq!(launched["state"]["story_title"], "Деревня у живой дороги");

    let (_s, dbody) = get(&state, "/debug").await;
    let debug: Value = serde_json::from_slice(&dbody).unwrap();
    // Authored plot present:
    assert_eq!(debug["story"]["title"], "Деревня у живой дороги");
    assert_eq!(
        debug["story"]["hidden_truth"],
        "Староста скормил дороге собственного сына ради урожая."
    );
    assert_eq!(debug["player_character"]["name"], "Мира");
    assert_eq!(debug["scene"]["title"], "Ворота деревни");
    assert_eq!(debug["time"]["time_of_day"], "18:00"); // 1080 minutes
                                                       // The authored NPC is in the roster and present in the authored scene.
    let npc_ids: Vec<&str> = debug["npcs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["id"].as_str().unwrap())
        .collect();
    assert!(npc_ids.contains(&"starosta"), "authored npc present");
    // World lore-derived content present: the bound world's proper nouns + its
    // generated canon survived the overlay (more than just the authored scene).
    state
        .store
        .with_runtime(
            "shared",
            launched["active_chat_id"].as_str().unwrap(),
            |rt| {
                let w = &rt.session.world;
                assert_eq!(
                    w.world_ref.as_ref().map(|r| r.id.as_str()),
                    Some(world_id.as_str())
                );
                assert_eq!(
                    w.story_ref.as_ref().map(|r| r.id.as_str()),
                    Some(story_id.as_str())
                );
                // The world bible flowed in via worldgen (lore name retained on
                // the canon's world_lore), and the authored scene was upserted on
                // top of the generated canon (canon has MORE than one place).
                assert_eq!(w.world_canon.world_lore.name, "Порог Второго Неба");
                assert!(
                    w.world_canon.places.len() >= 2,
                    "authored scene upserted into the generated world canon (got {} places: {:?})",
                    w.world_canon.places.len(),
                    w.world_canon.places.keys().collect::<Vec<_>>()
                );
                assert!(
                    w.world_canon.places.contains_key("village_gate"),
                    "places: {:?}",
                    w.world_canon.places.keys().collect::<Vec<_>>()
                );
            },
        )
        .unwrap();
}

#[tokio::test]
async fn create_story_with_missing_world_id_errors_and_writes_no_package() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    let (status, body) = post(
        &state,
        "/stories",
        json!({"kind": "authored", "world_id": "nope-world", "title": "Висящая история"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"]
        .as_str()
        .unwrap_or_default()
        .contains("nope-world"));

    // No story package written: GET /stories still has exactly the 3 builtins +
    // the procedural pseudo-entry.
    let (_s, sbody) = get(&state, "/stories").await;
    let stories: Value = serde_json::from_slice(&sbody).unwrap();
    assert_eq!(stories["stories"].as_array().unwrap().len(), 3 + 1);
}

#[tokio::test]
async fn delete_story_removes_the_package() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Мир для удаления").await;

    let (_s, body) = post(
        &state,
        "/stories",
        json!({"kind": "procedural", "world_id": world_id, "title": "Эфемерная история"}),
    )
    .await;
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    let (status, body) = post(&state, &format!("/stories/{story_id}/delete"), json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["deleted"], true);

    // Gone from the listing.
    let (_s, sbody) = get(&state, "/stories").await;
    let stories: Value = serde_json::from_slice(&sbody).unwrap();
    assert!(!stories["stories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == json!(story_id)));
}

// =========================================================================
// Phase-5 share UX: open library folder, export package zip, import zip.
// =========================================================================

/// POST raw bytes (e.g. a zip body) to `path` and return `(status, body)`.
async fn post_bytes(
    state: &AppState,
    path: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> (StatusCode, Vec<u8>) {
    let resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", content_type)
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, body.to_vec())
}

/// The set of file entry names inside a zip blob.
fn zip_entry_names(bytes: &[u8]) -> Vec<String> {
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).expect("valid zip");
    let mut names = Vec::new();
    for i in 0..zip.len() {
        let f = zip.by_index(i).unwrap();
        if !f.is_dir() {
            names.push(f.name().to_string());
        }
    }
    names
}

#[tokio::test]
async fn export_world_is_a_zip_with_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Экспортируемый мир").await;

    let (status, bytes) = get(&state, &format!("/worlds/{world_id}/export")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let names = zip_entry_names(&bytes);
    assert!(names.iter().any(|n| n == "world.json"), "names={names:?}");

    // Missing world -> 404.
    let (status, _b) = get(&state, "/worlds/does-not-exist/export").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn export_then_import_world_roundtrips_into_fresh_library() {
    let src_tmp = tempfile::tempdir().unwrap();
    let src = mock_state(&src_tmp);
    let world_id = create_saved_world(&src, "Переносимый мир").await;
    let (_s, exported) = get(&src, &format!("/worlds/{world_id}/export")).await;

    // Fresh, separate library.
    let dst_tmp = tempfile::tempdir().unwrap();
    let dst = mock_state(&dst_tmp);
    let (status, body) = post_bytes(&dst, "/library/import", "application/zip", exported).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["kind"], "world");
    let imported_id = got["id"].as_str().unwrap().to_string();

    // The world now appears in the destination library and round-trips.
    let (_s, wbody) = get(&dst, "/worlds").await;
    let worlds: Value = serde_json::from_slice(&wbody).unwrap();
    let found = worlds["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| w["id"] == json!(imported_id))
        .expect("imported world present");
    assert_eq!(found["title"], json!("Переносимый мир"));
}

#[tokio::test]
async fn export_story_baked_then_import_brings_world() {
    let src_tmp = tempfile::tempdir().unwrap();
    let src = mock_state(&src_tmp);
    let world_id = create_saved_world(&src, "Мир истории").await;
    let (_s, body) = post(
        &src,
        "/stories",
        json!({"kind": "procedural", "world_id": world_id, "title": "История с миром"}),
    )
    .await;
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    // Bake the world inside the story zip.
    let (status, exported) = get(&src, &format!("/stories/{story_id}/export?bake=1")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&exported)
    );
    let names = zip_entry_names(&exported);
    assert!(names.iter().any(|n| n == "story.json"), "names={names:?}");
    assert!(
        names.iter().any(|n| n == "world/world.json"),
        "names={names:?}"
    );

    // The embedded story.json copy has world_embedded=true.
    {
        let reader = std::io::Cursor::new(&exported);
        let mut zip = zip::ZipArchive::new(reader).unwrap();
        let mut f = zip.by_name("story.json").unwrap();
        let mut s = String::new();
        std::io::Read::read_to_string(&mut f, &mut s).unwrap();
        let manifest: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(manifest["world_embedded"], json!(true));
    }

    // Import into a fresh library: both story and world appear.
    let dst_tmp = tempfile::tempdir().unwrap();
    let dst = mock_state(&dst_tmp);
    let (status, body) = post_bytes(&dst, "/library/import", "application/zip", exported).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["kind"], "story");
    let imported_story = got["id"].as_str().unwrap().to_string();

    let (_s, sbody) = get(&dst, "/stories").await;
    let stories: Value = serde_json::from_slice(&sbody).unwrap();
    assert!(stories["stories"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["id"] == json!(imported_story)));

    // The baked world was imported too (it keeps its source id).
    let (_s, wbody) = get(&dst, "/worlds").await;
    let worlds: Value = serde_json::from_slice(&wbody).unwrap();
    assert!(worlds["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w["id"] == json!(world_id)));
}

#[tokio::test]
async fn export_story_bake_without_world_ref_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    // A built-in story has no world_ref (it embeds its world in the seed).
    let (_s, sbody) = get(&state, "/stories").await;
    let stories: Value = serde_json::from_slice(&sbody).unwrap();
    let builtin = stories["stories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] != json!("procedural"))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = get(&state, &format!("/stories/{builtin}/export?bake=1")).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
}

#[tokio::test]
async fn import_malformed_zip_errors_and_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (_s, before) = get(&state, "/worlds").await;
    let before_n = serde_json::from_slice::<Value>(&before).unwrap()["worlds"]
        .as_array()
        .unwrap()
        .len();

    // Garbage bytes -> bad zip.
    let (status, body) = post_bytes(
        &state,
        "/library/import",
        "application/zip",
        b"not a zip".to_vec(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "{}",
        String::from_utf8_lossy(&body)
    );

    // A valid zip with an unknown manifest format -> rejected.
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("world.json"), br#"{"format":"bogus/1"}"#).unwrap();
    let unknown = gml_server::share::zip_dir(src.path(), "").unwrap();
    let (status, body) = post_bytes(&state, "/library/import", "application/zip", unknown).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "{}",
        String::from_utf8_lossy(&body)
    );

    // Nothing was written.
    let (_s, after) = get(&state, "/worlds").await;
    let after_n = serde_json::from_slice::<Value>(&after).unwrap()["worlds"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(before_n, after_n);
}

#[tokio::test]
async fn import_collision_requires_overwrite() {
    let src_tmp = tempfile::tempdir().unwrap();
    let src = mock_state(&src_tmp);
    let world_id = create_saved_world(&src, "Коллизия").await;
    let (_s, exported) = get(&src, &format!("/worlds/{world_id}/export")).await;

    // First import into the destination succeeds.
    let dst_tmp = tempfile::tempdir().unwrap();
    let dst = mock_state(&dst_tmp);
    let (status, _b) =
        post_bytes(&dst, "/library/import", "application/zip", exported.clone()).await;
    assert_eq!(status, StatusCode::OK);

    // Re-importing the SAME id without overwrite -> 409.
    let (status, body) =
        post_bytes(&dst, "/library/import", "application/zip", exported.clone()).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "{}",
        String::from_utf8_lossy(&body)
    );

    // With overwrite=1 -> succeeds.
    let (status, body) = post_bytes(
        &dst,
        "/library/import?overwrite=1",
        "application/zip",
        exported,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
}

// =========================================================================
// CLEANUP/FIX pass — adversarial-review backend findings (F1-F5).
// =========================================================================

/// F1: a procedural launch carrying a PRESENT but EMPTY `world_lore` ({}) must
/// be rejected — the empty-lore guard runs BEFORE normalization, so no blank
/// fabricated world is ever launched and no chat is created.
#[tokio::test]
async fn procedural_launch_with_empty_world_lore_is_rejected_and_creates_no_chat() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    let (status, body) = post(
        &state,
        "/chats",
        json!({
            "story_id": "procedural",
            "seed": "empty-lore",
            "genre": "fantasy",
            "tone": "tense",
            "scale": "region",
            "world_lore": {}
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "empty world_lore must 400: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"]
        .as_str()
        .unwrap_or_default()
        .contains("world_lore must not be empty"));

    // No chat was created by the failed launch (read the store directly so we
    // don't trip get_active's auto-create).
    let chats = state.store.list_chats("shared").unwrap();
    assert!(
        chats.is_empty(),
        "a rejected empty-lore launch must not persist a chat, got {chats:?}"
    );
}

/// F1 (saved-world branch): a SAVED world whose stored `world_lore` is empty
/// must also be rejected on launch (the guard runs pre-normalization there too).
#[tokio::test]
async fn launch_saved_world_with_empty_lore_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    // Write a world package directly with an EMPTY world_lore object.
    let created = state
        .world_store
        .create_world(json!({"title": "Пустой мир", "world_lore": {}}))
        .unwrap();
    let world_id = created["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        &state,
        "/chats",
        json!({"world_id": world_id, "story_id": "procedural", "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "saved empty lore must 400: {}",
        String::from_utf8_lossy(&body)
    );
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], false);
    assert!(got["error"]
        .as_str()
        .unwrap_or_default()
        .contains("lore is empty"));
}

/// F2: a crafted `world_lore.world_image_url` (path traversal or an absolute
/// remote URL) is rejected at ingestion and never fetched. The stub sidecar
/// 404s `/missing/...`; an absolute remote URL points at an UNREACHABLE host,
/// so the only way the save could "succeed" with a fetch is if the loose parser
/// accepted it — which it must not. Either crafted ref yields a save failure
/// with an "unrecognized reference" message (no fetch attempted).
#[tokio::test]
async fn crafted_world_image_url_is_rejected_and_never_fetched() {
    let base = spawn_stub_sidecar().await;
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state_with_infer_url(&tmp, &base);

    for crafted in [
        "/image-files/../../secret",
        "http://169.254.169.254/image-files/run/secret.png",
        "//evil.example/image-files/run/secret.png",
    ] {
        let (status, body) = post(
            &state,
            "/worlds",
            json!({
                "title": "Мир с подделкой",
                "genre": "fantasy",
                "tone": "tense",
                "world_size": "Континент",
                "population": "Миллионы",
                "public_premise": "x",
                "world_lore": {"name": "Мир", "world_image_url": crafted}
            }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_GATEWAY,
            "crafted url {crafted:?} must be rejected: {}",
            String::from_utf8_lossy(&body)
        );
        let got: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(got["ok"], false);
        assert!(
            got["error"]
                .as_str()
                .unwrap_or_default()
                .contains("unrecognized reference"),
            "expected unrecognized-reference error for {crafted:?}, got {got:?}"
        );
    }

    // No world package leaked an image asset for any crafted ref.
    let worlds_dir = tmp.path().join("library").join("worlds");
    if let Ok(entries) = std::fs::read_dir(&worlds_dir) {
        for entry in entries.flatten() {
            let asset = entry.path().join("assets").join("world_image.png");
            assert!(!asset.is_file(), "no asset written for a crafted url");
        }
    }
}

/// F3: a CREATE whose image ingest fails (unreachable url) deletes the
/// just-allocated empty package — GET /worlds shows no new orphan world.
#[tokio::test]
async fn failed_create_ingest_leaves_no_orphan_world() {
    // Point the sidecar base at a dead loopback port: a RECOGNIZED
    // `/image-files/...` path will be fetched and the GET will FAIL (connection
    // refused) AFTER the empty package was allocated — exercising the rollback.
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state_with_infer_url(&tmp, "http://127.0.0.1:9");

    let (_s, before) = get(&state, "/worlds").await;
    let before_n = serde_json::from_slice::<Value>(&before).unwrap()["worlds"]
        .as_array()
        .unwrap()
        .len();

    let (status, body) = post(
        &state,
        "/worlds",
        json!({
            "title": "Мир-сирота",
            "genre": "fantasy",
            "tone": "tense",
            "world_size": "Континент",
            "population": "Миллионы",
            "public_premise": "x",
            "world_lore": {"name": "Мир", "world_image_url": "/image-files/run-404/gone.png"}
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "create with broken image must fail: {}",
        String::from_utf8_lossy(&body)
    );

    // The just-created empty package was rolled back: no new world in the list.
    let (_s, after) = get(&state, "/worlds").await;
    let after_n = serde_json::from_slice::<Value>(&after).unwrap()["worlds"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(
        before_n, after_n,
        "a failed create must leave the library untouched"
    );
    // And nothing on disk.
    let worlds_dir = tmp.path().join("library").join("worlds");
    let dir_count = std::fs::read_dir(&worlds_dir)
        .map(|it| it.flatten().filter(|e| e.path().is_dir()).count())
        .unwrap_or(0);
    assert_eq!(dir_count, 0, "no orphan world directory on disk");
}

/// Build a baked `.gmstory` zip where `story.json`'s `world_ref.id` (`OLD`)
/// DIFFERS from the baked world's manifest id (`BAKED`).
fn craft_baked_story_with_mismatched_ref() -> Vec<u8> {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("story.json", opts).unwrap();
        zip.write_all(
            br#"{"format":"gmlab.story/1","id":"crafted-story","world_embedded":true,"world_ref":{"id":"OLD-MISSING-WORLD","version":1}}"#,
        )
        .unwrap();
        zip.start_file("world/world.json", opts).unwrap();
        zip.write_all(
            r#"{"format":"gmlab.world/1","id":"BAKED-WORLD","title":"Запечённый мир"}"#.as_bytes(),
        )
        .unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// F4: importing a baked story whose `story.json world_ref.id` differs from the
/// baked world id rewrites the staged manifest to the ACTUAL imported world id,
/// and the referenced world exists after import (world swapped in first).
#[tokio::test]
async fn import_baked_story_rewrites_world_ref_to_imported_world() {
    let crafted = craft_baked_story_with_mismatched_ref();
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    let (status, body) = post_bytes(&state, "/library/import", "application/zip", crafted).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["kind"], "story");
    let story_id = got["id"].as_str().unwrap().to_string();

    // The baked world's manifest id is a safe segment, so it imports under that id.
    let imported_world_id = "BAKED-WORLD";

    // Read the imported story.json from disk: its world_ref.id was rewritten to
    // the actual imported world id (no longer the dangling "OLD-MISSING-WORLD").
    let story_manifest = tmp
        .path()
        .join("library")
        .join("stories")
        .join(&story_id)
        .join("story.json");
    let manifest: Value = serde_json::from_slice(&std::fs::read(&story_manifest).unwrap()).unwrap();
    assert_eq!(
        manifest["world_ref"]["id"],
        json!(imported_world_id),
        "world_ref.id must be rewritten to the imported world id, got {manifest:?}"
    );

    // The referenced world exists in the library.
    let (_s, wbody) = get(&state, "/worlds").await;
    let worlds: Value = serde_json::from_slice(&wbody).unwrap();
    assert!(
        worlds["worlds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["id"] == json!(imported_world_id)),
        "imported world must exist: {worlds:?}"
    );
}

/// Launch drift (warn but allow): a story pins the world's current version; the
/// world is then updated (bumping its version); launching the story yields ONE
/// `world_version_drift` warning with the right authored/live numbers, and the
/// persisted session records the authored pin plus the LIVE world_ref version.
///
/// (`POST /worlds` funnels create+update, so a freshly saved world is at v2; the
/// subsequent update bumps it to v3. The story pins v2 at bind time.)
#[tokio::test]
async fn launch_story_warns_on_world_version_drift() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    // create_saved_world (create+update) -> world version 2.
    let world_id = create_saved_world(&state, "Порог Второго Неба").await;
    let char_id = create_test_character(&state).await;

    // Bind a procedural story to the world; it pins the CURRENT version (v2).
    let (status, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "procedural",
            "world_id": world_id,
            "title": "Заводская смена",
            "plot": {"story_brief": "Ты выходишь на смену."}
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create story: {}",
        String::from_utf8_lossy(&body)
    );
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    // Update the world (POST /worlds/{id}) -> bumps version to v3.
    let (status, ubody) = post(
        &state,
        &format!("/worlds/{world_id}"),
        json!({
            "title": "Порог Второго Неба",
            "genre": "fantasy isekai",
            "tone": "tense hopeful",
            "world_size": "Континент",
            "population": "Миллионы",
            "public_premise": "Клятвы и долги имеют силу закона и магии.",
            "world_lore": {
                "name": "Порог Второго Неба",
                "public_premise": "Клятвы и долги имеют силу закона и магии.",
                "world_laws": ["магия требует имени, цены или признанного права"],
                "location_rules": ["каждая локация связана с долгом, властью, дорогой или духом"]
            }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "update world: {}",
        String::from_utf8_lossy(&ubody)
    );
    let updated: Value = serde_json::from_slice(&ubody).unwrap();
    assert_eq!(updated["ok"], true, "world updated: {updated}");
    // (The world-row response does not surface `version`; the live version is
    // asserted below via the drift warning's `live_version`.)

    // Launch the story -> drift: authored v2, live v3.
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "character_id": char_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "launch story: {}",
        String::from_utf8_lossy(&body)
    );
    let launched: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(launched["ok"], true);

    let warnings = launched["warnings"]
        .as_array()
        .expect("warnings array present on drift");
    assert_eq!(warnings.len(), 1, "exactly one warning: {warnings:?}");
    let w = &warnings[0];
    assert_eq!(w["code"], json!("world_version_drift"));
    assert_eq!(w["world_id"], json!(world_id));
    assert_eq!(w["authored_version"], json!(2));
    assert_eq!(w["live_version"], json!(3));
    assert!(
        w["message"].as_str().unwrap_or_default().contains("v2")
            && w["message"].as_str().unwrap_or_default().contains("v3"),
        "message names both versions: {w:?}"
    );

    // Persisted session: authored pin (v2) AND the live world_ref version (v3).
    state
        .store
        .with_runtime(
            "shared",
            launched["active_chat_id"].as_str().unwrap(),
            |rt| {
                let w = &rt.session.world;
                assert_eq!(
                    w.world_ref_authored_version,
                    Some(2),
                    "authored pin recorded"
                );
                assert_eq!(
                    w.world_ref.as_ref().map(|r| r.version),
                    Some(3),
                    "world_ref stamps the LIVE version"
                );
                assert_eq!(
                    w.world_ref.as_ref().map(|r| r.id.as_str()),
                    Some(world_id.as_str())
                );
            },
        )
        .unwrap();
}

/// No drift (authored == live): the launch response carries NO `warnings` key.
#[tokio::test]
async fn launch_story_without_drift_emits_no_warnings_key() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Город Железных Снов").await;
    let char_id = create_test_character(&state).await;

    // Bind a story (pins the world's current v2) and launch immediately — the
    // world has not moved, so authored == live and there is no drift.
    let (status, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "procedural",
            "world_id": world_id,
            "title": "Заводская смена",
            "plot": {"story_brief": "Ты выходишь на смену."}
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create story: {}",
        String::from_utf8_lossy(&body)
    );
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "character_id": char_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "launch story: {}",
        String::from_utf8_lossy(&body)
    );
    let launched: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(launched["ok"], true);
    assert!(
        launched.get("warnings").is_none(),
        "no drift -> no `warnings` key, got {launched}"
    );

    // The authored pin (the world's v2 at bind time) is still recorded even
    // without drift.
    state
        .store
        .with_runtime(
            "shared",
            launched["active_chat_id"].as_str().unwrap(),
            |rt| {
                assert_eq!(rt.session.world.world_ref_authored_version, Some(2));
            },
        )
        .unwrap();
}

/// A story whose `world_ref` omits `version` parses as unpinned (`0`): no
/// warning, and `world_ref_authored_version` stays `None`.
#[tokio::test]
async fn launch_unpinned_story_ref_records_no_pin_and_no_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Порог Второго Неба").await;

    // Hand-write a story.json with a `world_ref` that has an id but NO version
    // (parses as version 0 = unpinned). Written out-of-band under the temp
    // library, then a reload makes the story store see it.
    let story_id = "unpinned-story";
    let story_dir = tmp.path().join("library").join("stories").join(story_id);
    std::fs::create_dir_all(&story_dir).unwrap();
    let manifest = json!({
        "format": "gmlab.story/1",
        "id": story_id,
        "version": 1,
        "kind": "procedural",
        "world_ref": { "id": world_id },
        "world_embedded": false,
        "title": "Несвязанная версией история",
        "description": "Мир указан без версии.",
        "seed": { "story_brief": "Пролог без привязки к версии." }
    });
    std::fs::write(
        story_dir.join("story.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    state
        .story_store
        .lock()
        .expect("story store lock")
        .reload()
        .expect("reload story store");

    let char_id = create_test_character(&state).await;
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "character_id": char_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "launch unpinned story: {}",
        String::from_utf8_lossy(&body)
    );
    let launched: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(launched["ok"], true);
    assert!(
        launched.get("warnings").is_none(),
        "unpinned world_ref -> no warning, got {launched}"
    );

    // No pin recorded; but the world_ref still stamps the live version.
    state
        .store
        .with_runtime(
            "shared",
            launched["active_chat_id"].as_str().unwrap(),
            |rt| {
                let w = &rt.session.world;
                assert_eq!(w.world_ref_authored_version, None, "unpinned -> no pin");
                assert_eq!(
                    w.world_ref.as_ref().map(|r| r.id.as_str()),
                    Some(world_id.as_str())
                );
            },
        )
        .unwrap();
}

// =========================================================================
// K1 characters (docs/CHARACTERS_AND_STORY_TZ.md §К1.1–К1.4)
// =========================================================================

/// Create a character via `POST /characters` and return its id. `payload` is
/// always sent (design §8: no default hero — a package must carry a PC).
async fn create_character_via_api(state: &AppState, title: &str, payload: Value) -> String {
    let (status, resp) = post(
        state,
        "/characters",
        json!({ "title": title, "payload": payload }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create character: {}",
        String::from_utf8_lossy(&resp)
    );
    let v: Value = serde_json::from_slice(&resp).unwrap();
    v["character"]["id"].as_str().unwrap().to_string()
}

/// Create a minimal protagonist package and return its id — for the many
/// launches that now REQUIRE a character (design §8: no default hero is seeded,
/// so a procedural/PC-less launch without a `character_id` is a 400).
async fn create_test_character(state: &AppState) -> String {
    create_character_via_api(
        state,
        "Тест-Герой",
        json!({"player_character": {"name": "Тест-Герой"}}),
    )
    .await
}

/// CRUD over the HTTP surface: create (explicit payload — no default hero),
/// list, update metadata (version bump + rename), delete, and no-payload -> 400.
#[tokio::test]
async fn character_crud_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // Create with an EXPLICIT protagonist payload (design §8: no default hero).
    let id = create_character_via_api(
        &state,
        "Мой герой",
        json!({"player_character": {"name": "Мой герой"}}),
    )
    .await;

    // Listed, version 1, the supplied hero name in the payload.
    let (status, body) = get(&state, "/characters").await;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&body).unwrap();
    let chars = listed["characters"].as_array().unwrap();
    assert_eq!(chars.len(), 1);
    assert_eq!(chars[0]["id"], json!(id));
    assert_eq!(chars[0]["version"], json!(1));
    assert_eq!(chars[0]["title"], json!("Мой герой"));
    assert_eq!(
        chars[0]["payload"]["player_character"]["name"],
        json!("Мой герой"),
        "supplied hero name round-trips into the package payload"
    );

    // Update metadata (rename) -> version 2.
    let (status, body) = post(
        &state,
        &format!("/characters/{id}"),
        json!({"title": "Переименован"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "update: {}",
        String::from_utf8_lossy(&body)
    );
    let updated: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(updated["character"]["version"], json!(2));
    assert_eq!(updated["character"]["title"], json!("Переименован"));

    // Empty-title create is a 400.
    let (status, _b) = post(&state, "/characters", json!({"title": "   "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A create WITHOUT a payload is a 400 (no default hero is synthesized).
    let (status, body) = post(&state, "/characters", json!({"title": "Безгеройный"})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "no payload -> 400: {}",
        String::from_utf8_lossy(&body)
    );
    let no_pc: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(no_pc["ok"], false);
    assert!(no_pc["error"]
        .as_str()
        .unwrap_or("")
        .contains("payload is required"));

    // Update of a missing id is a 400.
    let (status, _b) = post(&state, "/characters/nope", json!({"title": "x"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Delete -> gone from the list.
    let (status, body) = post(&state, &format!("/characters/{id}/delete"), json!({})).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "delete: {}",
        String::from_utf8_lossy(&body)
    );
    let deleted: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(deleted["deleted"], json!(true));
    let (status, body) = get(&state, "/characters").await;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&body).unwrap();
    assert!(listed["characters"].as_array().unwrap().is_empty());
}

/// `POST /characters/{id}/draft` — the character studio's DIRECT manual save
/// (no architect chat, no SSE). Happy path (sheet FULL-replaced, version bumped,
/// title follows the hero name), a name-preserving edit bumps once without
/// retitling, a non-object / missing player_character -> 400, an unknown id ->
/// 404, the architect conversation is left byte-for-byte intact, and the export
/// stays clean (no chat leaks into the package).
#[tokio::test]
async fn character_draft_save_snapshots_sheet_and_follows_title() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // Create a character: version 1, title == hero name "Старое имя".
    let id = create_character_via_api(
        &state,
        "Старое имя",
        json!({"player_character": {"name": "Старое имя"}}),
    )
    .await;

    // Seed an architect conversation for this character in the dialogs DB — a
    // draft save must leave it byte-for-byte intact (it never touches the chat).
    let chat_before = json!({
        "messages": [{"role": "user", "content": "привет-архитектор"}],
        "model_history": [{"role": "user", "content": "привет-архитектор"}],
    });
    state
        .store
        .set_architect_chat("character", &id, &chat_before)
        .expect("seed architect chat");

    // ---- Happy path: POST the fully edited sheet (renamed hero + list edits) ----
    let (status, body) = post(
        &state,
        &format!("/characters/{id}/draft"),
        json!({"player_character": {
            "name": "Новое имя",
            "abilities": {"STR": 15},
            "spells": [{"name": "Огненный шар"}],
        }}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "draft save: {}",
        String::from_utf8_lossy(&body)
    );
    let saved: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(saved["ok"], true);
    // FULL REPLACE: the new name AND the edited lists land in the payload.
    assert_eq!(
        saved["character"]["payload"]["player_character"]["name"],
        json!("Новое имя")
    );
    assert_eq!(
        saved["character"]["payload"]["player_character"]["abilities"]["STR"],
        json!(15)
    );
    assert_eq!(
        saved["character"]["payload"]["player_character"]["spells"][0]["name"],
        json!("Огненный шар")
    );
    // Version 1 -> 3: snapshot bump (1->2) + retitle bump (2->3, name changed).
    assert_eq!(saved["character"]["version"], json!(3));
    assert_eq!(
        saved["character"]["title"],
        json!("Новое имя"),
        "title follows the hero name"
    );
    // No architect / SSE fields leak into the direct-save response.
    assert!(saved["character"].get("architect_messages").is_none());
    assert!(saved.get("reply").is_none());
    assert!(
        saved.get("characters").is_none(),
        "direct save returns just {{ok, character}}"
    );

    // ---- A name-PRESERVING edit bumps exactly once and does NOT retitle ----
    let (status, body) = post(
        &state,
        &format!("/characters/{id}/draft"),
        json!({"player_character": {"name": "Новое имя", "spells": []}}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "second draft save: {}",
        String::from_utf8_lossy(&body)
    );
    let saved2: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        saved2["character"]["version"],
        json!(4),
        "snapshot-only bump 3->4"
    );
    assert_eq!(
        saved2["character"]["title"],
        json!("Новое имя"),
        "title unchanged when name is"
    );
    assert_eq!(
        saved2["character"]["payload"]["player_character"]["spells"],
        json!([]),
        "list cleared to empty (full replace)"
    );

    // ---- The architect conversation is UNTOUCHED by the draft saves ----
    let (status, body) = get(&state, &format!("/characters/{id}/architect")).await;
    assert_eq!(status, StatusCode::OK);
    let arch: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(arch["architect"]["messages"], chat_before["messages"]);
    // The whole stored row is byte-for-byte identical.
    let stored = state
        .store
        .get_architect_chat("character", &id)
        .expect("read chat")
        .expect("chat still present");
    assert_eq!(
        stored, chat_before,
        "draft save must not rewrite the architect chat"
    );

    // ---- A non-object player_character -> 400 ----
    let (status, body) = post(
        &state,
        &format!("/characters/{id}/draft"),
        json!({"player_character": "не объект"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "non-object -> 400: {}",
        String::from_utf8_lossy(&body)
    );
    let err: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(err["ok"], false);
    assert!(err["error"]
        .as_str()
        .unwrap_or("")
        .contains("player_character must be an object"));
    // A MISSING player_character key is likewise a 400 (no silent no-op).
    let (status, _b) = post(&state, &format!("/characters/{id}/draft"), json!({})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "missing player_character -> 400"
    );

    // ---- An unknown id -> 404 ----
    let (status, body) = post(
        &state,
        "/characters/nope/draft",
        json!({"player_character": {"name": "Никто"}}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "unknown id -> 404: {}",
        String::from_utf8_lossy(&body)
    );

    // ---- The export stays CLEAN: character.json carries only the sheet ----
    let (status, exported) = get(&state, &format!("/characters/{id}/export")).await;
    assert_eq!(status, StatusCode::OK);
    let reader = std::io::Cursor::new(&exported);
    let mut zip = zip::ZipArchive::new(reader).unwrap();
    let mut f = zip.by_name("character.json").unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut f, &mut s).unwrap();
    let manifest: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(manifest["title"], json!("Новое имя"));
    assert_eq!(
        manifest["payload"]["player_character"]["name"],
        json!("Новое имя")
    );
    assert!(manifest.get("architect_messages").is_none());
    assert!(manifest["payload"].get("architect").is_none());
    assert!(
        !s.contains("привет-архитектор"),
        "no architect chat leaks into the export"
    );
}

/// Build a single-entry `character.json` zip with the given manifest value.
fn build_character_zip(manifest: Value) -> Vec<u8> {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut zip = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("character.json", options).unwrap();
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.finish().unwrap();
    }
    buf
}

/// Export -> import roundtrip into a fresh library; plus structural-validation
/// rejection (400) and id-collision (409).
#[tokio::test]
async fn character_export_import_roundtrip_reject_and_collision() {
    let src_tmp = tempfile::tempdir().unwrap();
    let src = mock_state(&src_tmp);
    let id = create_character_via_api(
        &src,
        "Переносимый герой",
        json!({"player_character": {"name": "Ариан", "card_revision": 3}}),
    )
    .await;

    // Export is a .gmchar.zip carrying character.json.
    let (status, exported) = get(&src, &format!("/characters/{id}/export")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&exported)
    );
    let names = zip_entry_names(&exported);
    assert!(
        names.iter().any(|n| n == "character.json"),
        "names={names:?}"
    );
    // Missing character -> 404.
    let (status, _b) = get(&src, "/characters/does-not-exist/export").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Import into a FRESH library.
    let dst_tmp = tempfile::tempdir().unwrap();
    let dst = mock_state(&dst_tmp);
    let (status, body) =
        post_bytes(&dst, "/library/import", "application/zip", exported.clone()).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["kind"], "character");
    let imported_id = got["id"].as_str().unwrap().to_string();

    // The character is live (reload happened) and the card_revision travelled.
    let (status, body) = get(&dst, "/characters").await;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&body).unwrap();
    let found = listed["characters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == json!(imported_id))
        .expect("imported character present");
    assert_eq!(
        found["payload"]["player_character"]["card_revision"],
        json!(3)
    );

    // Re-import WITHOUT overwrite -> 409 collision (same manifest id).
    let (status, _b) =
        post_bytes(&dst, "/library/import", "application/zip", exported.clone()).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "collision without overwrite -> 409"
    );
    // WITH overwrite=1 -> OK.
    let (status, _b) = post_bytes(
        &dst,
        "/library/import?overwrite=1",
        "application/zip",
        exported,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "overwrite import ok");

    // Structural-validation rejection: a character.json whose payload has no
    // player_character object -> 400, nothing lands.
    let bad = build_character_zip(json!({
        "format": "gmlab.character/1",
        "id": "badchar",
        "version": 1,
        "title": "Плохой",
        "payload": {"not_pc": {}}
    }));
    let (status, body) = post_bytes(&dst, "/library/import", "application/zip", bad).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "reject: {}",
        String::from_utf8_lossy(&body)
    );
}

/// Launch with a `character_id` overlays the PC and stamps `char_ref`. The
/// `story_pc_override` warning fires EXACTLY when the story carries its own
/// player_character: authored-with-PC story -> warn; procedural -> no warn;
/// no character_id -> no warn, no char_ref.
#[tokio::test]
async fn launch_with_character_overlays_pc_and_sets_char_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Мир Персонажей").await;

    // A character package with a distinctive hero name + card_revision.
    let char_id = create_character_via_api(
        &state,
        "Заглавный герой",
        json!({"player_character": {"name": "Кассандра", "card_revision": 7}}),
    )
    .await;

    // ---- Case A: authored story that CARRIES its own player_character ----
    let (status, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "authored",
            "world_id": world_id,
            "title": "История со своим героем",
            "plot": {
                "story_brief": "Пролог.",
                "player_character": {"name": "Авторский протагонист", "card_revision": 0}
            }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create authored: {}",
        String::from_utf8_lossy(&body)
    );
    let authored: Value = serde_json::from_slice(&body).unwrap();
    let authored_story_id = authored["story"]["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": authored_story_id, "character_id": char_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "launch authored+char: {}",
        String::from_utf8_lossy(&body)
    );
    let launched: Value = serde_json::from_slice(&body).unwrap();
    // The override warning is present (story carried its own PC).
    let warnings = launched["warnings"].as_array().expect("warnings present");
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == json!("story_pc_override")),
        "authored-with-PC + character_id must warn story_pc_override: {warnings:?}"
    );
    // The PC was OVERLAID from the package (full replace, card_revision verbatim)
    // and char_ref stamped.
    state
        .store
        .with_runtime(
            "shared",
            launched["active_chat_id"].as_str().unwrap(),
            |rt| {
                let w = &rt.session.world;
                assert_eq!(w.player_character.name, "Кассандра", "package PC overlaid");
                assert_eq!(
                    w.player_character.card_revision, 7,
                    "card_revision travels verbatim"
                );
                assert_eq!(
                    w.char_ref.as_ref().map(|r| r.id.as_str()),
                    Some(char_id.as_str()),
                    "char_ref stamped"
                );
            },
        )
        .unwrap();

    // ---- Case B: procedural story -> NO override warning ----
    let (status, body) = post(
        &state,
        "/stories",
        json!({
            "kind": "procedural",
            "world_id": world_id,
            "title": "Процедурная история",
            "plot": {"story_brief": "Смена."}
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create procedural: {}",
        String::from_utf8_lossy(&body)
    );
    let proc: Value = serde_json::from_slice(&body).unwrap();
    let proc_story_id = proc["story"]["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": proc_story_id, "character_id": char_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "launch procedural+char: {}",
        String::from_utf8_lossy(&body)
    );
    let launched: Value = serde_json::from_slice(&body).unwrap();
    let has_override = launched
        .get("warnings")
        .and_then(Value::as_array)
        .map(|ws| ws.iter().any(|w| w["code"] == json!("story_pc_override")))
        .unwrap_or(false);
    assert!(
        !has_override,
        "procedural story must NOT warn story_pc_override: {launched}"
    );
    // Overlay still applies + char_ref set.
    state
        .store
        .with_runtime(
            "shared",
            launched["active_chat_id"].as_str().unwrap(),
            |rt| {
                let w = &rt.session.world;
                assert_eq!(w.player_character.name, "Кассандра");
                assert_eq!(
                    w.char_ref.as_ref().map(|r| r.id.as_str()),
                    Some(char_id.as_str())
                );
            },
        )
        .unwrap();
    // §К1.5: the active chat's `/state` surfaces `char_ref {id, version}` so the
    // player-facing "save hero" control can offer "update the source".
    let (status, body) = get(&state, "/state").await;
    assert_eq!(status, StatusCode::OK);
    let st: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        st["char_ref"]["id"].as_str(),
        Some(char_id.as_str()),
        "/state must surface char_ref.id after a character launch: {st}"
    );
    assert!(
        st["char_ref"]["version"].is_u64(),
        "char_ref.version is a number"
    );

    // ---- Case C: no character_id on a PC-less procedural story -> 400 ----
    // Design §8: no default hero is ever seeded, so a launch with neither a
    // selected character nor an authored PC is rejected (protagonist_required).
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": proc_story_id, "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "no-char procedural launch must 400: {}",
        String::from_utf8_lossy(&body)
    );
    let rejected: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["code"], json!("protagonist_required"));

    // A supplied-but-unknown character_id is a 400 (no-fallback).
    let (status, _b) = post(
        &state,
        "/chats",
        json!({"story_id": proc_story_id, "character_id": "ghost", "activate": true}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown character_id -> 400"
    );
}

/// `POST /chats/{chat_id}/save-character`: create-new (no id) then update
/// (with id, snapshot + version bump), and missing-id -> 400.
#[tokio::test]
async fn save_character_new_update_and_missing_id() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // A procedural chat gives us a hero to save. No default hero is seeded
    // (design §8), so the launch carries an explicit character package; its PC
    // (name "Тест-Герой") is what save-back snapshots. The procedural launch
    // requires world_lore, so use inline lore.
    let char_id = create_test_character(&state).await;
    let (status, body) = post(
        &state,
        "/chats",
        json!({
            "story_id": "procedural",
            "seed": "save-char-test",
            "activate": true,
            "character_id": char_id,
            "world_lore": {
                "name": "Тестовый мир",
                "public_premise": "Мир для теста сохранения героя.",
                "world_laws": ["закон один"],
                "location_rules": ["правило одно"]
            }
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create chat: {}",
        String::from_utf8_lossy(&body)
    );
    let launched: Value = serde_json::from_slice(&body).unwrap();
    let chat_id = launched["active_chat_id"].as_str().unwrap().to_string();

    // Save-back WITHOUT an id -> creates a NEW character (title = hero name).
    let (status, body) = post(
        &state,
        &format!("/chats/{chat_id}/save-character"),
        json!({}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "save new: {}",
        String::from_utf8_lossy(&body)
    );
    let saved: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(saved["ok"], true);
    let new_id = saved["character"]["id"].as_str().unwrap().to_string();
    assert_eq!(saved["character"]["version"], json!(1));
    // title = the overlaid package hero name.
    assert_eq!(saved["character"]["title"], json!("Тест-Герой"));

    // Save-back WITH the id -> snapshot the existing character (version bump).
    let (status, body) = post(
        &state,
        &format!("/chats/{chat_id}/save-character"),
        json!({"character_id": new_id}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "save update: {}",
        String::from_utf8_lossy(&body)
    );
    let updated: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(updated["character"]["id"], json!(new_id));
    assert_eq!(
        updated["character"]["version"],
        json!(2),
        "snapshot bumps version"
    );

    // Save-back with an UNKNOWN id -> 400.
    let (status, _b) = post(
        &state,
        &format!("/chats/{chat_id}/save-character"),
        json!({"character_id": "ghost"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown id -> 400");
}
