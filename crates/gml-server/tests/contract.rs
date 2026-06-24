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
use serde_json::Value;
use tower::ServiceExt; // oneshot

use gml_config::{Config, RuntimeSettings};
use gml_llm::{Backend, MockClient};
use gml_persistence::DialogStore;
use gml_server::{build_router, AppState};

/// Build an [`AppState`] with the mock backend and a fresh temp DB.
fn mock_state(tmp: &tempfile::TempDir) -> AppState {
    // The server reads `GM_BACKEND` (via Config) and `GM_CHAT_SCOPE_ID`; pin both.
    std::env::set_var("GM_BACKEND", "mock");
    std::env::set_var("GM_CHAT_SCOPE_ID", "shared");

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

    AppState {
        store,
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
    assert!(got["active_chat_id"]
        .as_str()
        .map(|s| !s.is_empty())
        .unwrap_or(false));
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
async fn create_chat_without_story_uses_procedural_default() {
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);
    let (status, body) = post(&state, "/chats", serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let got: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(got["ok"], true);
    assert_eq!(got["state"]["story_id"], "procedural");
    assert!(got["state"]["scene"].is_object());
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
    // canon (World::from_worldgen) and is fully playable through a normal turn.
    let tmp = tempfile::tempdir().unwrap();
    let state = mock_state(&tmp);

    // Pin a seed so the generated world is deterministic.
    let (status, body) = post(
        &state,
        "/chats",
        serde_json::json!({"story_id": "procedural", "seed": "12345", "activate": true}),
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
