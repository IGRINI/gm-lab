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
//!     event sequence and ends with `data: {"kind": "done"}\n\n`
//!   - unknown route -> 404 {error:"not found"}

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Map, Value};
use tower::ServiceExt; // oneshot

use gml_config::{Config, RuntimeSettings};
use gml_llm::{
    Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput, MockClient,
};
use gml_persistence::{CharacterStore, DialogStore, WorldStore};
use gml_server::{build_router, AppState};

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
    state.make_client = Arc::new(move || {
        Arc::new(IdentitySpyBackend {
            state: spy_factory.clone(),
        }) as Arc<dyn Backend>
    });
    spy
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

    let make_client: gml_server::MakeClient =
        Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>);
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
        make_client,
        config: cfg,
        settings,
        http: reqwest::Client::new(),
        sidecar: None,
        locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
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

    let make_client: gml_server::MakeClient =
        Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>);
    let factory: gml_orchestrator::ClientFactory =
        Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>);

    let db_path = tmp.path().join("dialogs.sqlite3");
    let store = Arc::new(
        DialogStore::new(db_path.to_string_lossy().to_string(), factory, cfg.clone())
            .expect("open temp dialog store"),
    );

    let world_store = Arc::new(
        WorldStore::new(tmp.path().join("library")).expect("open temp world store"),
    );
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
        make_client,
        config: cfg,
        settings,
        http: reqwest::Client::new(),
        sidecar: None,
        locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        index_html: Arc::new(None),
    }
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
    let resp = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/turn")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&serde_json::json!({ "text": text })).unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
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
    assert!(!got["npcs"].as_array().unwrap().is_empty());
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
    assert!(!got["npcs"].as_array().unwrap().is_empty());
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
    assert!(
        text.ends_with("data: {\"kind\": \"done\"}\n\n"),
        "stream must end with the done frame"
    );
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
async fn turn_replaces_cached_runtime_before_state_reads() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    let (status, body) = get(&state, "/state").await;
    assert_eq!(status, StatusCode::OK);
    let before: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(before["run_usage"]["turns"], 0);

    let (status, sse) = post_turn_text(&state, "Я осматриваю зал трактира.").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        sse.ends_with("data: {\"kind\": \"done\"}\n\n"),
        "turn must complete with done frame"
    );

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
async fn transcribe_is_400_when_backend_not_codex() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
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
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let got: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(got["ok"], false);
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

    // Pin a seed so the generated world is deterministic.
    let (status, body) = post(
        &state,
        "/chats",
        serde_json::json!({
            "story_id": "procedural",
            "seed": "12345",
            "activate": true,
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
    assert!(
        text.ends_with("data: {\"kind\": \"done\"}\n\n"),
        "procedural turn must complete with a done frame"
    );
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
    let (status, body) = post(
        &state,
        "/chats",
        serde_json::json!({
            "story_id": "procedural",
            "seed": "world-manager-seed",
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
    let visible_messages = json!([
        {"role": "assistant", "content": "Опиши мир."},
        {"role": "user", "content": "Хочу фентезийный иссекай с богами и клятвами."}
    ]);
    let (status, body) = post(
        &state,
        "/world-architect/chat",
        json!({
            "message": "Хочу фентезийный иссекай с богами и клятвами.",
            "history": [],
            "draft": {"genre": "fantasy isekai"},
            "visible_messages": visible_messages,
            "cache_session_id": "world-architect:test-session",
            "cache_thread_id": "world-architect:test-thread"
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
    assert_eq!(
        got["world"]["architect_messages"][1]["content"],
        "Хочу фентезийный иссекай с богами и клятвами."
    );
    // The turn is now interleaved like the main chat: reasoning (think) → draft
    // tool call → reasoning → chat reply. Index 2 is the hop-1 reasoning, index 3
    // the draft tool, the last entry the model's chat reply.
    assert_eq!(got["world"]["architect_messages"][2]["role"], "think");
    assert_eq!(
        got["world"]["architect_messages"][3]["name"],
        "draft_world_bible"
    );
    let visible = got["world"]["architect_messages"].as_array().unwrap();
    // [intro assistant, user, think, tool, think, assistant reply] = 6 segments.
    assert_eq!(visible.len(), 6);
    assert_eq!(visible.last().unwrap()["role"], "assistant");
    assert!(visible.last().unwrap()["content"]
        .as_str()
        .unwrap_or("")
        .contains("Порог Второго Неба"));
    // Model history keeps the user turn and the final assistant reply.
    assert_eq!(
        got["world"]["architect_model_history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        got["world"]["architect_cache_session_id"],
        "world-architect:test-session"
    );
    assert_eq!(got["worlds"].as_array().unwrap().len(), 1);

    let (status, body) = get(&state, "/worlds").await;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(listed["worlds"][0]["id"], json!(world_id));
    assert_eq!(
        listed["worlds"][0]["architect_model_history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
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
            "history": [],
            "draft": {"genre": "fantasy"},
            "visible_messages": [{"role": "user", "content": "Хочу мир клятв."}]
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
    assert_eq!(
        updated["world"]["architect_messages"][0]["content"],
        "Хочу мир клятв."
    );
    // The agent loop ends with a chat reply, so model history keeps the user turn
    // and the final assistant reply; the /worlds update preserves both.
    assert_eq!(
        updated["world"]["architect_model_history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
async fn world_architect_chat_restores_cache_identity_and_returns_model_history() {
    let tmp = tempfile::tempdir().unwrap();
    let mut state = mock_state(&tmp);
    let spy = install_identity_spy(&mut state);

    let prior_user = gml_agents::world_architect_user_message(
        &json!({"title": "Первый черновик"}),
        "Собери основу мира.",
    );
    let (status, body) = post(
        &state,
        "/world-architect/chat",
        json!({
            "message": "Добавь религии.",
            "history": [
                prior_user,
                {"role": "assistant", "content": "Собрал первый черновик."}
            ],
            "draft": {"title": "Второй черновик"},
            "cache_session_id": "world-architect:test-session",
            "cache_thread_id": "world-architect:test-thread"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let got = architect_result(&body);
    assert_eq!(got["ok"], true);
    assert_eq!(got["cache_session_id"], "world-architect:test-session");
    assert_eq!(got["cache_thread_id"], "world-architect:test-thread");
    assert_eq!(got["user_message"]["role"], "user");
    assert!(got["user_message"]["content"]
        .as_str()
        .unwrap()
        .contains("Current Draft JSON"));
    assert_eq!(got["assistant_history_message"]["role"], "assistant");
    assert_eq!(
        got["assistant_history_message"]["content"],
        "Ответ архитектора"
    );

    let spy = spy.lock().expect("identity spy lock");
    assert_eq!(spy.session_id, "world-architect:test-session");
    assert_eq!(spy.thread_id, "world-architect:test-thread");
    let sent = spy.messages.last().unwrap().as_array().unwrap();
    assert_eq!(sent[0]["role"], "system");
    assert!(sent[1]["content"]
        .as_str()
        .unwrap()
        .contains("Первый черновик"));
    assert!(sent.last().unwrap()["content"]
        .as_str()
        .unwrap()
        .contains("Второй черновик"));
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

#[tokio::test]
async fn create_procedural_chat_accepts_world_lore_from_architect() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(
        &state,
        "/chats",
        serde_json::json!({
            "story_id": "procedural",
            "seed": "architect-lore-server",
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
    assert_eq!(status, StatusCode::BAD_REQUEST, "dangling world_id must fail");
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
        json!({"story_id": story_id, "activate": true}),
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
                assert_eq!(w.world_ref.as_ref().map(|r| r.id.as_str()), Some(world_id.as_str()));
                assert_eq!(w.story_ref.as_ref().map(|r| r.id.as_str()), Some(story_id.as_str()));
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
    assert!(got["error"].as_str().unwrap_or_default().contains("nope-world"));

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
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
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
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&exported));
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
    let (status, body) =
        post_bytes(&state, "/library/import", "application/zip", b"not a zip".to_vec()).await;
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
    assert!(got["error"].as_str().unwrap_or_default().contains("lore is empty"));
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

    let (status, body) =
        post_bytes(&state, "/library/import", "application/zip", crafted).await;
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
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(&story_manifest).unwrap()).unwrap();
    assert_eq!(
        manifest["world_ref"]["id"], json!(imported_world_id),
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
    assert_eq!(status, StatusCode::OK, "create story: {}", String::from_utf8_lossy(&body));
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
    assert_eq!(status, StatusCode::OK, "update world: {}", String::from_utf8_lossy(&ubody));
    let updated: Value = serde_json::from_slice(&ubody).unwrap();
    assert_eq!(updated["ok"], true, "world updated: {updated}");
    // (The world-row response does not surface `version`; the live version is
    // asserted below via the drift warning's `live_version`.)

    // Launch the story -> drift: authored v2, live v3.
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "activate": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "launch story: {}", String::from_utf8_lossy(&body));
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
        .with_runtime("shared", launched["active_chat_id"].as_str().unwrap(), |rt| {
            let w = &rt.session.world;
            assert_eq!(w.world_ref_authored_version, Some(2), "authored pin recorded");
            assert_eq!(
                w.world_ref.as_ref().map(|r| r.version),
                Some(3),
                "world_ref stamps the LIVE version"
            );
            assert_eq!(w.world_ref.as_ref().map(|r| r.id.as_str()), Some(world_id.as_str()));
        })
        .unwrap();
}

/// No drift (authored == live): the launch response carries NO `warnings` key.
#[tokio::test]
async fn launch_story_without_drift_emits_no_warnings_key() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let world_id = create_saved_world(&state, "Город Железных Снов").await;

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
    assert_eq!(status, StatusCode::OK, "create story: {}", String::from_utf8_lossy(&body));
    let created: Value = serde_json::from_slice(&body).unwrap();
    let story_id = created["story"]["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "activate": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "launch story: {}", String::from_utf8_lossy(&body));
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
        .with_runtime("shared", launched["active_chat_id"].as_str().unwrap(), |rt| {
            assert_eq!(rt.session.world.world_ref_authored_version, Some(2));
        })
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

    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": story_id, "activate": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "launch unpinned story: {}", String::from_utf8_lossy(&body));
    let launched: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(launched["ok"], true);
    assert!(
        launched.get("warnings").is_none(),
        "unpinned world_ref -> no warning, got {launched}"
    );

    // No pin recorded; but the world_ref still stamps the live version.
    state
        .store
        .with_runtime("shared", launched["active_chat_id"].as_str().unwrap(), |rt| {
            let w = &rt.session.world;
            assert_eq!(w.world_ref_authored_version, None, "unpinned -> no pin");
            assert_eq!(w.world_ref.as_ref().map(|r| r.id.as_str()), Some(world_id.as_str()));
        })
        .unwrap();
}

// =========================================================================
// K1 characters (docs/CHARACTERS_AND_STORY_TZ.md §К1.1–К1.4)
// =========================================================================

/// Create a character via `POST /characters` and return its id.
async fn create_character_via_api(state: &AppState, title: &str, payload: Value) -> String {
    let body = if payload.is_null() {
        json!({ "title": title })
    } else {
        json!({ "title": title, "payload": payload })
    };
    let (status, resp) = post(state, "/characters", body).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "create character: {}",
        String::from_utf8_lossy(&resp)
    );
    let v: Value = serde_json::from_slice(&resp).unwrap();
    v["character"]["id"].as_str().unwrap().to_string()
}

/// CRUD over the HTTP surface: create (default hero), list, update metadata
/// (version bump + rename), delete.
#[tokio::test]
async fn character_crud_over_http() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // Create with the DEFAULT hero payload (no payload in the body).
    let id = create_character_via_api(&state, "Мой герой", Value::Null).await;

    // Listed, version 1, default hero name in the payload.
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
        json!("Искатель"),
        "default hero name from PlayerCharacter::default()"
    );

    // Update metadata (rename) -> version 2.
    let (status, body) =
        post(&state, &format!("/characters/{id}"), json!({"title": "Переименован"})).await;
    assert_eq!(status, StatusCode::OK, "update: {}", String::from_utf8_lossy(&body));
    let updated: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(updated["character"]["version"], json!(2));
    assert_eq!(updated["character"]["title"], json!("Переименован"));

    // Empty-title create is a 400.
    let (status, _b) = post(&state, "/characters", json!({"title": "   "})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Update of a missing id is a 400.
    let (status, _b) = post(&state, "/characters/nope", json!({"title": "x"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Delete -> gone from the list.
    let (status, body) = post(&state, &format!("/characters/{id}/delete"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "delete: {}", String::from_utf8_lossy(&body));
    let deleted: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(deleted["deleted"], json!(true));
    let (status, body) = get(&state, "/characters").await;
    assert_eq!(status, StatusCode::OK);
    let listed: Value = serde_json::from_slice(&body).unwrap();
    assert!(listed["characters"].as_array().unwrap().is_empty());
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
        zip.write_all(&serde_json::to_vec(&manifest).unwrap()).unwrap();
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
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&exported));
    let names = zip_entry_names(&exported);
    assert!(names.iter().any(|n| n == "character.json"), "names={names:?}");
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
    assert_eq!(found["payload"]["player_character"]["card_revision"], json!(3));

    // Re-import WITHOUT overwrite -> 409 collision (same manifest id).
    let (status, _b) =
        post_bytes(&dst, "/library/import", "application/zip", exported.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "collision without overwrite -> 409");
    // WITH overwrite=1 -> OK.
    let (status, _b) =
        post_bytes(&dst, "/library/import?overwrite=1", "application/zip", exported).await;
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
    assert_eq!(status, StatusCode::OK, "create authored: {}", String::from_utf8_lossy(&body));
    let authored: Value = serde_json::from_slice(&body).unwrap();
    let authored_story_id = authored["story"]["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": authored_story_id, "character_id": char_id, "activate": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "launch authored+char: {}", String::from_utf8_lossy(&body));
    let launched: Value = serde_json::from_slice(&body).unwrap();
    // The override warning is present (story carried its own PC).
    let warnings = launched["warnings"].as_array().expect("warnings present");
    assert!(
        warnings.iter().any(|w| w["code"] == json!("story_pc_override")),
        "authored-with-PC + character_id must warn story_pc_override: {warnings:?}"
    );
    // The PC was OVERLAID from the package (full replace, card_revision verbatim)
    // and char_ref stamped.
    state
        .store
        .with_runtime("shared", launched["active_chat_id"].as_str().unwrap(), |rt| {
            let w = &rt.session.world;
            assert_eq!(w.player_character.name, "Кассандра", "package PC overlaid");
            assert_eq!(w.player_character.card_revision, 7, "card_revision travels verbatim");
            assert_eq!(
                w.char_ref.as_ref().map(|r| r.id.as_str()),
                Some(char_id.as_str()),
                "char_ref stamped"
            );
        })
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
    assert_eq!(status, StatusCode::OK, "create procedural: {}", String::from_utf8_lossy(&body));
    let proc: Value = serde_json::from_slice(&body).unwrap();
    let proc_story_id = proc["story"]["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": proc_story_id, "character_id": char_id, "activate": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "launch procedural+char: {}", String::from_utf8_lossy(&body));
    let launched: Value = serde_json::from_slice(&body).unwrap();
    let has_override = launched
        .get("warnings")
        .and_then(Value::as_array)
        .map(|ws| ws.iter().any(|w| w["code"] == json!("story_pc_override")))
        .unwrap_or(false);
    assert!(!has_override, "procedural story must NOT warn story_pc_override: {launched}");
    // Overlay still applies + char_ref set.
    state
        .store
        .with_runtime("shared", launched["active_chat_id"].as_str().unwrap(), |rt| {
            let w = &rt.session.world;
            assert_eq!(w.player_character.name, "Кассандра");
            assert_eq!(w.char_ref.as_ref().map(|r| r.id.as_str()), Some(char_id.as_str()));
        })
        .unwrap();

    // ---- Case C: no character_id -> no warn, no char_ref ----
    let (status, body) = post(
        &state,
        "/chats",
        json!({"story_id": proc_story_id, "activate": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "launch no-char: {}", String::from_utf8_lossy(&body));
    let launched: Value = serde_json::from_slice(&body).unwrap();
    let has_override = launched
        .get("warnings")
        .and_then(Value::as_array)
        .map(|ws| ws.iter().any(|w| w["code"] == json!("story_pc_override")))
        .unwrap_or(false);
    assert!(!has_override, "no character_id -> no story_pc_override");
    state
        .store
        .with_runtime("shared", launched["active_chat_id"].as_str().unwrap(), |rt| {
            assert!(rt.session.world.char_ref.is_none(), "no char_ref without character_id");
        })
        .unwrap();

    // A supplied-but-unknown character_id is a 400 (no-fallback).
    let (status, _b) = post(
        &state,
        "/chats",
        json!({"story_id": proc_story_id, "character_id": "ghost", "activate": true}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown character_id -> 400");
}

/// `POST /chats/{chat_id}/save-character`: create-new (no id) then update
/// (with id, snapshot + version bump), and missing-id -> 400.
#[tokio::test]
async fn save_character_new_update_and_missing_id() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // A procedural chat gives us a default hero to save. The procedural launch
    // requires world_lore, so use inline lore.
    let (status, body) = post(
        &state,
        "/chats",
        json!({
            "story_id": "procedural",
            "seed": "save-char-test",
            "activate": true,
            "world_lore": {
                "name": "Тестовый мир",
                "public_premise": "Мир для теста сохранения героя.",
                "world_laws": ["закон один"],
                "location_rules": ["правило одно"]
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "create chat: {}", String::from_utf8_lossy(&body));
    let launched: Value = serde_json::from_slice(&body).unwrap();
    let chat_id = launched["active_chat_id"].as_str().unwrap().to_string();

    // Save-back WITHOUT an id -> creates a NEW character (title = hero name).
    let (status, body) = post(&state, &format!("/chats/{chat_id}/save-character"), json!({})).await;
    assert_eq!(status, StatusCode::OK, "save new: {}", String::from_utf8_lossy(&body));
    let saved: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(saved["ok"], true);
    let new_id = saved["character"]["id"].as_str().unwrap().to_string();
    assert_eq!(saved["character"]["version"], json!(1));
    // title = the default hero name.
    assert_eq!(saved["character"]["title"], json!("Искатель"));

    // Save-back WITH the id -> snapshot the existing character (version bump).
    let (status, body) = post(
        &state,
        &format!("/chats/{chat_id}/save-character"),
        json!({"character_id": new_id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "save update: {}", String::from_utf8_lossy(&body));
    let updated: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(updated["character"]["id"], json!(new_id));
    assert_eq!(updated["character"]["version"], json!(2), "snapshot bumps version");

    // Save-back with an UNKNOWN id -> 400.
    let (status, _b) = post(
        &state,
        &format!("/chats/{chat_id}/save-character"),
        json!({"character_id": "ghost"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "unknown id -> 400");
}
