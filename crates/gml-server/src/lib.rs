//! gml-server — the GM-Lab HTTP/SSE server (axum).
//!
//! Faithful port of `gm-lab/server.py` (PORT_PLAN §3 cross-platform, §5 HTTP/SSE
//! contract, §6 persistence). The existing React frontend (`web/src`) runs
//! UNCHANGED against this server: every URL / method / body / response and the
//! exact SSE frame format (`data: {json}\n\n`, terminal `data: {"kind":"done"}`)
//! matches `web/src/api.js` + the timeline/tts/devSettings stores.
//!
//! The crate is a LIBRARY exposing [`build_router`] + [`run_http`] /
//! [`run_https`] so both `gml-app` modes (Tauri loopback + headless `--server`)
//! call in. [`AppState`] owns the [`DialogStore`], the GM-client factory (wired
//! through [`gml_llm::make_client`] with a `gml-codex` hook), the shared
//! [`RuntimeSettings`] + [`Config`], the TTS HTTP client, and a per-chat
//! `tokio::sync::Mutex` (held across a streamed `/turn`, replacing Python's
//! per-runtime `RLock`).

pub mod openai_key;
pub mod payload;
pub mod sys_tokens;
pub mod tls;

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path as AxPath, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use bytes::Bytes;
use serde_json::{json, Map, Value};
use tokio::sync::Mutex;

use gml_audio::{cache_lookup, cache_store, compress_audio, tts_format, tts_synth, Sidecar};
use gml_config::{Config, RuntimeSettings};
use gml_llm::Backend;
use gml_orchestrator::{run_turn_into, CompactionThresholds, Session};
use gml_persistence::{DialogRuntime, DialogStore};
use gml_world::{World, WorldLore, WorldSpec};

/// Reserved `story_id` that routes campaign creation through the living-world
/// canon generator (`World::from_worldgen`) instead of the static story
/// catalog. Optional `seed`/`genre`/`tone`/`scale` body fields tune the
/// [`gml_world::WorldSpec`]; everything else is derived from the canon.
pub const PROCEDURAL_STORY_ID: &str = "procedural";

/// `CHAT_SCOPE_ID` — the single shared chat scope (`GM_CHAT_SCOPE_ID`, default
/// `"shared"`). All chats live under this guest id.
pub fn chat_scope_id() -> String {
    match std::env::var("GM_CHAT_SCOPE_ID") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => "shared".to_string(),
    }
}

/// A synchronous factory that builds a fresh GM/NPC [`Backend`] (the Rust
/// stand-in for Python's module-level `make_client`). The server builds this
/// once (selecting codex / llamacpp / openai / mock by config, wiring the
/// `gml-codex` hook) and shares it into the [`DialogStore`] and every
/// [`Session`].
pub type MakeClient = Arc<dyn Fn() -> Arc<dyn Backend> + Send + Sync>;

/// Shared application state, cloned into every handler.
#[derive(Clone)]
pub struct AppState {
    /// SQLite-backed dialog persistence (the shared chat scope).
    pub store: Arc<DialogStore>,
    /// Builds fresh GM/NPC clients (codex/llamacpp/openai/mock by config).
    pub make_client: MakeClient,
    /// Immutable startup config.
    pub config: Arc<Config>,
    /// Dynamic, atomic-persisted UI settings.
    pub settings: Arc<RuntimeSettings>,
    /// HTTP client for the TTS sidecar proxy.
    pub http: reqwest::Client,
    /// Unified inference sidecar manager (RAG embeddings + rerank + optional TTS).
    pub sidecar: Option<Arc<Sidecar>>,
    /// Per-chat async locks — held across a streamed `/turn` (Python RLock).
    /// The outer map guard is a plain `std::sync::Mutex` (held briefly to
    /// get-or-create the per-chat lock); the per-chat locks are `tokio::Mutex`
    /// (held `.await`-ed across a streamed turn).
    pub locks: Arc<std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Resolved path to the built SPA `index.html` (`web/dist/index.html`).
    pub index_html: Arc<Option<std::path::PathBuf>>,
}

impl AppState {
    /// Get-or-create the per-chat lock for `chat_id`. The outer map mutex is
    /// held only for the brief get-or-create.
    fn chat_lock(&self, chat_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().expect("locks mutex poisoned");
        locks
            .entry(chat_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

// =========================================================================
// router
// =========================================================================

/// `build_router(state)` — every route from `server.py` + the §5 contract.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // GET
        .route("/", get(get_index))
        .route("/state", get(get_state))
        .route("/debug", get(get_debug))
        .route("/models", get(get_models))
        .route("/settings", get(get_settings).post(post_settings))
        .route("/transcript", get(get_transcript))
        .route("/stories", get(get_stories))
        .route("/chats", get(get_chats).post(post_create_chat))
        .route("/worlds", get(get_worlds).post(post_create_world))
        .route("/world-architect/chat", post(post_world_architect_chat))
        .route("/export", get(get_export))
        .route("/codex/status", get(get_codex_status))
        .route("/sidecar/status", get(get_sidecar_status))
        .route("/images/generate", post(post_generate_image))
        .route("/image-files/{run_id}/{filename}", get(get_generated_image))
        .route("/images/{run_id}/{filename}", get(get_generated_image))
        // POST
        .route("/chats/{id}/activate", post(post_activate_chat))
        .route("/chats/{id}/delete", post(post_delete_chat))
        .route("/worlds/{id}", post(post_update_world))
        .route("/worlds/{id}/delete", post(post_delete_world))
        .route("/model", post(post_model))
        .route("/cmd", post(post_cmd))
        .route("/turn", post(post_turn))
        .route("/transcribe", post(post_transcribe))
        .route("/tts", post(post_tts))
        .route("/codex/login", post(post_codex_login))
        .route("/codex/logout", post(post_codex_logout))
        .route("/debug/roll", post(post_debug_roll))
        .route("/debug/fact", post(post_debug_fact))
        .route("/debug/fact_delete", post(post_debug_fact_delete))
        .route("/debug/player", post(post_debug_player))
        .route("/debug/npc", post(post_debug_npc))
        .route("/debug/story", post(post_debug_story))
        .route("/debug/scene", post(post_debug_scene))
        .route("/debug/state_record", post(post_debug_state_record))
        .route("/debug/rumor", post(post_debug_rumor))
        // dev token counter (OpenAI /v1/responses/input_tokens) + key storage
        .route(
            "/debug/openai_key",
            get(get_openai_key).post(post_openai_key),
        )
        .route("/debug/openai_key/delete", post(post_openai_key_delete))
        .route("/debug/tokenize", post(post_debug_tokenize))
        // /index* -> index.html; everything else -> 404 {error:"not found"}.
        .fallback(fallback_handler)
        .with_state(state)
}

// =========================================================================
// helpers
// =========================================================================

/// `self._json(obj, code)` — JSON response with `application/json; charset=utf-8`
/// and Python-identical compact-but-non-ASCII body (serde_json default).
fn json_response(code: StatusCode, value: &Value) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_default();
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = code;
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    resp
}

fn ok_json(value: &Value) -> Response {
    json_response(StatusCode::OK, value)
}

fn not_found() -> Response {
    json_response(StatusCode::NOT_FOUND, &json!({"error": "not found"}))
}

/// Parse a JSON request body into a `Map` (`self._body()` — `{}` on empty /
/// invalid, matching Python's tolerant parser).
fn parse_body(bytes: &Bytes) -> Map<String, Value> {
    if bytes.is_empty() {
        return Map::new();
    }
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// `_bool_from_body(value, default)`.
fn bool_from_body(value: Option<&Value>, default: bool) -> bool {
    match value {
        None | Some(Value::Null) => default,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !matches!(
            s.trim().to_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
        Some(other) => gml_orchestrator::truthy(other),
    }
}

fn body_str(map: &Map<String, Value>, key: &str) -> String {
    map.get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn body_cache_id(map: &Map<String, Value>, key: &str) -> Option<String> {
    let value = body_str(map, key);
    normalize_cache_id(&value)
}

fn normalize_cache_id(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let id: String = trimmed
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | ':' | '.'))
        .take(128)
        .collect();
    if id.is_empty() {
        None
    } else {
        Some(id)
    }
}

/// Build a [`WorldSpec`] for the procedural campaign route from optional body
/// fields. A blank/absent field falls back to the spec default; the seed
/// defaults to a fresh non-zero value so two procedural campaigns differ unless
/// the caller pins a seed.
fn worldspec_from_body(map: &Map<String, Value>) -> WorldSpec {
    let mut spec = WorldSpec::default();
    let seed = body_str(map, "seed");
    spec.seed = if seed.is_empty() {
        // A fresh dice seed gives a distinct, reproducible-by-value spec seed.
        World::new_dice_seed().to_string()
    } else {
        seed
    };
    let genre = body_str(map, "genre");
    if !genre.is_empty() {
        spec.genre = genre;
    }
    let tone = body_str(map, "tone");
    if !tone.is_empty() {
        spec.tone = tone;
    }
    let scale = body_str(map, "scale");
    if !scale.is_empty() {
        spec.scale = scale;
    }
    spec
}

fn required_world_lore_from_body(
    map: &Map<String, Value>,
    spec: &WorldSpec,
) -> Result<WorldLore, String> {
    let Some(raw) = map.get("world_lore") else {
        return Err("world_lore is required for procedural worlds".to_string());
    };
    if raw.is_null() {
        return Err("world_lore is required for procedural worlds".to_string());
    }
    if !raw.is_object() {
        return Err("world_lore must be an object".to_string());
    }
    let mut lore: WorldLore =
        serde_json::from_value(raw.clone()).map_err(|e| format!("invalid world_lore: {e}"))?;
    lore.normalize_for_worldgen(&spec.seed, &spec.genre, &spec.tone, &spec.scale);
    if lore.is_empty() {
        Err("world_lore must not be empty".to_string())
    } else {
        Ok(lore)
    }
}

fn reusable_world_payload_from_body(map: &Map<String, Value>) -> Result<Value, String> {
    let draft = bool_from_body(map.get("draft"), false)
        || body_str(map, "status").eq_ignore_ascii_case("draft");
    world_payload_from_body(map, draft)
}

fn world_payload_from_body(map: &Map<String, Value>, draft_mode: bool) -> Result<Value, String> {
    for key in [
        "activate",
        "seed",
        "story_id",
        "story_title",
        "story_brief",
        "storyBrief",
        "public_intro",
        "publicIntro",
        "scale",
    ] {
        if map.contains_key(key) {
            return Err(format!(
                "{key} belongs to story creation, not world creation"
            ));
        }
    }

    let title = body_str_any(map, &["title"]);
    let genre = body_str_any(map, &["genre"]);
    let tone = body_str_any(map, &["tone"]);
    let world_size = body_str_any(map, &["world_size", "worldSize"]);
    let population = body_str_any(map, &["population"]);
    if !draft_mode && title.is_empty() {
        return Err("title is required".to_string());
    }
    if !draft_mode && genre.is_empty() {
        return Err("genre is required".to_string());
    }
    if !draft_mode && tone.is_empty() {
        return Err("tone is required".to_string());
    }
    if !draft_mode && world_size.is_empty() {
        return Err("world_size is required".to_string());
    }
    if !draft_mode && population.is_empty() {
        return Err("population is required".to_string());
    }

    let world_lore = map
        .get("world_lore")
        .or_else(|| map.get("worldLore"))
        .unwrap_or(&Value::Null);
    if !draft_mode && world_lore.is_null() {
        return Err("world_lore is required".to_string());
    }
    if !world_lore.is_null() && !world_lore.is_object() {
        return Err("world_lore must be an object".to_string());
    }
    if !draft_mode && !value_has_text(world_lore) {
        return Err("world_lore must not be empty".to_string());
    }

    let mut payload = Map::new();
    insert_string_field(&mut payload, "title", title);
    insert_string_field(&mut payload, "genre", genre);
    insert_string_field(&mut payload, "tone", tone);
    insert_string_field(&mut payload, "world_size", world_size);
    insert_string_field(&mut payload, "population", population);
    insert_string_field(
        &mut payload,
        "public_premise",
        body_str_any(map, &["public_premise", "publicPremise"]),
    );
    if world_lore.is_object() {
        payload.insert("world_lore".to_string(), world_lore.clone());
    }
    let status = body_str(map, "status");
    if status.eq_ignore_ascii_case("draft") || draft_mode {
        payload.insert("status".to_string(), Value::String("draft".to_string()));
    } else if status.eq_ignore_ascii_case("ready") {
        payload.insert("status".to_string(), Value::String("ready".to_string()));
    }
    insert_architect_persistence_fields(&mut payload, map);
    Ok(Value::Object(payload))
}

fn body_str_any(map: &Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_str))
        .unwrap_or("")
        .trim()
        .to_string()
}

fn insert_string_field(payload: &mut Map<String, Value>, key: &str, value: String) {
    if !value.trim().is_empty() {
        payload.insert(key.to_string(), Value::String(value.trim().to_string()));
    }
}

fn insert_architect_persistence_fields(payload: &mut Map<String, Value>, map: &Map<String, Value>) {
    if let Some(messages) = clean_architect_visible_messages(map.get("architect_messages")) {
        payload.insert("architect_messages".to_string(), Value::Array(messages));
    }
    if let Some(history) = clean_architect_model_history(map.get("architect_model_history")) {
        payload.insert("architect_model_history".to_string(), Value::Array(history));
    }
    for (key, source) in [
        ("architect_cache_session_id", "architect_cache_session_id"),
        ("architect_cache_thread_id", "architect_cache_thread_id"),
    ] {
        let value = body_str(map, source);
        if !value.is_empty() {
            payload.insert(key.to_string(), Value::String(value));
        }
    }
}

fn clean_architect_visible_messages(value: Option<&Value>) -> Option<Vec<Value>> {
    let array = value?.as_array()?;
    let mut out = Vec::new();
    for message in array {
        let Some(object) = message.as_object() else {
            continue;
        };
        let role = object
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match role {
            "user" | "assistant" | "think" => {
                let content = object
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim();
                if !content.is_empty() {
                    out.push(json!({"role": role, "content": content}));
                }
            }
            "tool" => {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim();
                if !name.is_empty() {
                    out.push(json!({
                        "role": "tool",
                        "name": name,
                        "args": object.get("args").cloned().unwrap_or_else(|| json!({})),
                    }));
                }
            }
            _ => {}
        }
    }
    Some(out)
}

fn clean_architect_model_history(value: Option<&Value>) -> Option<Vec<Value>> {
    let array = value?.as_array()?;
    let mut out = Vec::new();
    for message in array {
        let Some(object) = message.as_object() else {
            continue;
        };
        let role = object
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if role != "user" && role != "assistant" {
            continue;
        }
        let content = object
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim();
        if !content.is_empty() {
            out.push(json!({"role": role, "content": content}));
        }
    }
    Some(out)
}

fn value_has_text(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => items.iter().any(value_has_text),
        Value::Object(map) => map.values().any(value_has_text),
        _ => false,
    }
}

/// Resolve the active chat id, self-healing/creating as needed (`get_active`).
// The `Err` is an axum `Response` (the established error channel in this crate);
// boxing it would ripple through every handler for no real benefit.
#[allow(clippy::result_large_err)]
fn active_chat(state: &AppState) -> Result<String, Response> {
    let scope = chat_scope_id();
    state.store.get_active(&scope).map_err(|e| {
        json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": e.to_string()}),
        )
    })
}

/// Run `f` against the active chat runtime under its per-chat lock, on a
/// blocking thread (rusqlite + World are sync). Returns the `f` result.
async fn with_active<T, F>(state: &AppState, f: F) -> Result<T, Response>
where
    T: Send + 'static,
    F: FnOnce(&mut DialogRuntime) -> T + Send + 'static,
{
    let scope = chat_scope_id();
    let chat_id = active_chat(state)?;
    let lock = state.chat_lock(&chat_id);
    let _guard = lock.lock().await;
    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || store.with_runtime(&scope, &chat_id, f))
        .await
        .map_err(|e| {
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"ok": false, "error": format!("join error: {e}")}),
            )
        })?;
    match res {
        Ok(Some(v)) => Ok(v),
        Ok(None) => Err(json_response(
            StatusCode::NOT_FOUND,
            &json!({"ok": false, "error": "chat not found"}),
        )),
        Err(e) => Err(json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": e.to_string()}),
        )),
    }
}

/// `ensure_client(dialog)` — lazily attach a live client to the session.
fn ensure_client(runtime: &mut DialogRuntime, state: &AppState) {
    let session = &mut runtime.session;
    let cfg = &state.config;
    let matches = session.client_backend.is_empty() || session.client_backend == cfg.backend;
    let placeholder_client = cfg.backend != "mock"
        && (session.client.model() == "mock" || session.client_model == "mock");
    // Replace the client only if it is the default placeholder (model "mock"
    // with a non-mock backend) OR the backend changed. The default Session
    // always holds a live client, so for fidelity we re-key identity when the
    // backend mismatches (Python: client is None -> rebuild).
    if !matches || placeholder_client {
        if !matches {
            session.client_model = String::new();
        } else if session.client_model == "mock" {
            session.client_model.clear();
        }
        session.client_session_id = String::new();
        session.client_thread_id = String::new();
        session.npc_client_state.clear();
        session.client = (state.make_client)();
        session.client_backend = cfg.backend.clone();
    }
    let client = session.client.clone();
    client.set_session_identity(
        Some(session.client_session_id.as_str()),
        Some(session.client_thread_id.as_str()),
    );
    session.client_session_id = client.session_id();
    session.client_thread_id = client.thread_id();
    if !session.client_model.is_empty() {
        client.set_model(&session.client_model);
    } else {
        session.client_model = client.model();
    }
}

// =========================================================================
// GET handlers
// =========================================================================

async fn get_index(State(state): State<AppState>) -> Response {
    // Ensure an active chat exists (`self._dialog()` side-effect).
    let _ = active_chat(&state);
    serve_index(&state)
}

fn serve_index(state: &AppState) -> Response {
    let body = match state.index_html.as_ref() {
        Some(path) => std::fs::read(path).unwrap_or_else(|_| placeholder_html().into_bytes()),
        None => placeholder_html().into_bytes(),
    };
    let mut resp = Response::new(Body::from(body));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, must-revalidate"),
    );
    resp
}

fn placeholder_html() -> String {
    "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">\
<title>GM-Lab</title></head><body style=\"font-family:system-ui;padding:2rem\">\
<h1>GM-Lab</h1><p>Фронтенд не собран. Соберите его:</p>\
<pre>cd web &amp;&amp; npm install &amp;&amp; npm run build</pre>\
<p>После сборки <code>web/dist/index.html</code> появится и эта страница его отдаст.</p>\
</body></html>"
        .to_string()
}

async fn get_state(State(state): State<AppState>) -> Response {
    // Best-effort, once per process: sharpen the system-prompt token baseline via
    // OpenAI's input-token counter when a dev key is configured (else no-op).
    sys_tokens::ensure(&state.http);
    let cfg = state.config.clone();
    let settings = state.settings.clone();
    match with_active(&state, move |rt| payload::state(rt, &cfg, &settings)).await {
        Ok(v) => ok_json(&v),
        Err(resp) => resp,
    }
}

async fn get_debug(State(state): State<AppState>) -> Response {
    let cfg = state.config.clone();
    let settings = state.settings.clone();
    match with_active(&state, move |rt| payload::debug_data(rt, &cfg, &settings)).await {
        Ok(v) => ok_json(&v),
        Err(resp) => resp,
    }
}

async fn get_transcript(State(state): State<AppState>) -> Response {
    match with_active(&state, payload::replay_events).await {
        Ok(events) => ok_json(&json!({"events": events})),
        Err(resp) => resp,
    }
}

async fn get_stories() -> Response {
    // Surface the living-world generator as a selectable "story" so the UI can
    // offer a brief-less procedural campaign (locked decision #4).
    let mut stories = gml_stories::list_stories();
    let mut procedural = Map::new();
    procedural.insert("id".into(), json!(PROCEDURAL_STORY_ID));
    procedural.insert("title".into(), json!("Процедурный мир"));
    procedural.insert(
        "description".into(),
        json!("Сгенерированный живой мир: место, люди рядом и ближайший конфликт. Канон — источник истины."),
    );
    procedural.insert(
        "story_brief".into(),
        json!("Ты начинаешь в живом, сгенерированном мире: рядом уже есть место, люди и первый источник напряжения. Осмотрись, выбери, кому верить, и реши, за какую нитку потянуть первым."),
    );
    procedural.insert("procedural".into(), json!(true));
    stories.push(procedural);
    ok_json(&json!({
        "ok": true,
        "default_story_id": PROCEDURAL_STORY_ID,
        "stories": stories,
    }))
}

/// Forwards the architect agent loop's segments into the SSE channel so the chat
/// renders live like the main GM turn: `architect_delta` carries per-hop
/// content/thinking deltas (tagged with a `sid`), `architect_tool` surfaces each
/// tool call as it happens. The UI groups by `sid` into separate reasoning
/// spoilers and reply bubbles, interleaved with the tool cards.
struct ArchitectStreamSink {
    tx: tokio::sync::mpsc::UnboundedSender<Value>,
}

impl gml_agents::ArchitectStream for ArchitectStreamSink {
    fn delta(&mut self, channel: &str, text: &str, sid: &str) {
        if text.is_empty() {
            return;
        }
        let chan = if channel == gml_llm::channel::THINKING {
            "thinking"
        } else {
            "content"
        };
        let _ = self.tx.send(json!({
            "kind": "architect_delta",
            "data": { "channel": chan, "text": text, "sid": sid },
        }));
    }

    fn tool(&mut self, call: &Value, sid: &str) {
        let mut data = call.as_object().cloned().unwrap_or_default();
        data.insert("sid".to_string(), Value::String(sid.to_string()));
        let _ = self
            .tx
            .send(json!({ "kind": "architect_tool", "data": Value::Object(data) }));
    }
}

/// `POST /world-architect/chat` — Server-Sent Events. Streams the architect's
/// reply as it generates (`architect_delta`), surfaces each tool call
/// (`architect_tool`), then sends the full result (`architect_done`) carrying the
/// draft, usage, debug info and the persisted world. Terminates with `done`.
async fn post_world_architect_chat(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    let message = body_str(&data, "message");
    if message.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "message is required"}),
        );
    }
    let history = data
        .get("history")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let draft = data.get("draft").cloned().unwrap_or(Value::Null);
    let fallback_cache_id = body_cache_id(&data, "cache_id");
    let cache_session_id = body_cache_id(&data, "cache_session_id")
        .or_else(|| body_cache_id(&data, "architect_session_id"))
        .or_else(|| fallback_cache_id.clone());
    let cache_thread_id = body_cache_id(&data, "cache_thread_id")
        .or_else(|| body_cache_id(&data, "architect_thread_id"))
        .or(fallback_cache_id);
    let world_id = body_str(&data, "world_id");
    let world_id = if world_id.is_empty() {
        None
    } else {
        Some(world_id)
    };
    let visible_messages = visible_architect_messages_for_request(&data, &message);

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let app = state.clone();
    tokio::spawn(async move {
        let initial_payload = architect_world_payload(
            &draft,
            visible_messages.clone(),
            history.clone(),
            cache_session_id.as_deref(),
            cache_thread_id.as_deref(),
        );
        let (mut world, mut worlds) =
            match persist_world_payload(&app, world_id, initial_payload).await {
                Ok(saved) => saved,
                Err(_resp) => {
                    let _ = tx.send(json!({
                        "kind": "architect_error",
                        "data": "не удалось сохранить черновик мира",
                    }));
                    return;
                }
            };
        let persisted_world_id = world
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let client = (app.make_client)();
        client.set_session_identity(cache_session_id.as_deref(), cache_thread_id.as_deref());
        let mut sink = ArchitectStreamSink { tx: tx.clone() };
        let architect_options = gml_agents::WorldArchitectOptions {
            image_prompts: image_generation_enabled(&app),
        };
        match gml_agents::world_architect_turn_with_options(
            client.as_ref(),
            &history,
            &draft,
            &message,
            architect_options,
            &mut sink,
        )
        .await
        {
            Ok(output) => {
                // The agent loop already streamed each tool call live (architect_tool)
                // via the sink. Persist the ordered visible segments (think / reply /
                // tool) so reopening the world restores the interleaved view.
                let mut visible_after = visible_messages;
                visible_after.extend(output.visible_segments.clone());
                let mut model_history_after = history;
                model_history_after.push(output.user_msg.clone());
                model_history_after.push(output.assistant_history_msg.clone());
                let final_payload = architect_world_payload(
                    output.draft.as_ref().unwrap_or(&Value::Null),
                    visible_after,
                    model_history_after,
                    Some(client.session_id().as_str()),
                    Some(client.thread_id().as_str()),
                );
                match persist_world_payload(&app, Some(persisted_world_id.clone()), final_payload)
                    .await
                {
                    Ok((saved_world, saved_worlds)) => {
                        world = saved_world;
                        worlds = saved_worlds;
                    }
                    Err(_resp) => {
                        let _ = tx.send(json!({
                            "kind": "architect_error",
                            "data": "не удалось сохранить мир",
                            "world": world,
                            "worlds": worlds,
                            "world_id": persisted_world_id,
                        }));
                        return;
                    }
                }
                let _ = tx.send(json!({
                    "kind": "architect_done",
                    "data": {
                        "ok": true,
                        "reply": output.reply,
                        "draft": output.draft,
                        "user_message": output.user_msg,
                        "assistant_history_message": output.assistant_history_msg,
                        "assistant_message": output.assistant_msg,
                        "calls": output.calls,
                        "cache_session_id": client.session_id(),
                        "cache_thread_id": client.thread_id(),
                        "usage": architect_usage(&output.stats),
                        "stats": output.stats,
                        "thinking": output.thinking,
                        "request_messages": output.request_messages,
                        "world_id": persisted_world_id,
                        "world": world,
                        "worlds": worlds,
                    }
                }));
            }
            Err(e) => {
                let _ = tx.send(json!({
                    "kind": "architect_error",
                    "data": e.to_string(),
                    "world": world,
                    "worlds": worlds,
                    "world_id": persisted_world_id,
                }));
            }
        }
    });

    let stream = async_stream::stream! {
        let mut rx = rx;
        while let Some(ev) = rx.recv().await {
            let line = format!("data: {}\n\n", serde_json::to_string(&ev).unwrap_or_default());
            yield Ok::<Bytes, std::io::Error>(Bytes::from(line));
        }
        yield Ok(Bytes::from("data: {\"kind\": \"done\"}\n\n"));
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    let body = Body::from_stream(stream);
    (headers, body).into_response()
}

fn visible_architect_messages_for_request(map: &Map<String, Value>, message: &str) -> Vec<Value> {
    let mut visible = clean_architect_visible_messages(map.get("visible_messages"))
        .or_else(|| clean_architect_visible_messages(map.get("architect_messages")))
        .unwrap_or_default();
    let has_current_user = visible.iter().rev().any(|item| {
        item.get("role").and_then(Value::as_str) == Some("user")
            && item
                .get("content")
                .and_then(Value::as_str)
                .map(|content| content.trim() == message.trim())
                .unwrap_or(false)
    });
    if !has_current_user {
        visible.push(json!({"role": "user", "content": message.trim()}));
    }
    visible
}

fn architect_world_payload(
    draft: &Value,
    visible_messages: Vec<Value>,
    model_history: Vec<Value>,
    cache_session_id: Option<&str>,
    cache_thread_id: Option<&str>,
) -> Value {
    let mut payload = draft_payload_fields(draft);
    payload.insert("status".to_string(), json!("draft"));
    payload.insert(
        "architect_messages".to_string(),
        Value::Array(visible_messages),
    );
    payload.insert(
        "architect_model_history".to_string(),
        Value::Array(
            clean_architect_model_history(Some(&Value::Array(model_history))).unwrap_or_default(),
        ),
    );
    if let Some(cache_session_id) = cache_session_id.and_then(normalize_cache_id) {
        payload.insert(
            "architect_cache_session_id".to_string(),
            Value::String(cache_session_id),
        );
    }
    if let Some(cache_thread_id) = cache_thread_id.and_then(normalize_cache_id) {
        payload.insert(
            "architect_cache_thread_id".to_string(),
            Value::String(cache_thread_id),
        );
    }
    Value::Object(payload)
}

fn draft_payload_fields(draft: &Value) -> Map<String, Value> {
    let mut payload = Map::new();
    let Some(map) = draft.as_object() else {
        return payload;
    };
    insert_string_field(&mut payload, "title", body_str_any(map, &["title"]));
    insert_string_field(&mut payload, "genre", body_str_any(map, &["genre"]));
    insert_string_field(&mut payload, "tone", body_str_any(map, &["tone"]));
    insert_string_field(
        &mut payload,
        "world_size",
        body_str_any(map, &["world_size", "worldSize"]),
    );
    insert_string_field(
        &mut payload,
        "population",
        body_str_any(map, &["population"]),
    );
    insert_string_field(
        &mut payload,
        "public_premise",
        body_str_any(map, &["public_premise", "publicPremise"]),
    );
    if let Some(lore) = map.get("world_lore").or_else(|| map.get("worldLore")) {
        if lore.is_object() {
            payload.insert("world_lore".to_string(), lore.clone());
        }
    }
    payload
}

async fn persist_world_payload(
    state: &AppState,
    world_id: Option<String>,
    payload: Value,
) -> Result<(Value, Vec<Value>), Response> {
    let scope = chat_scope_id();
    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        let world = match world_id {
            Some(world_id) => store.update_world(&scope, &world_id, payload)?,
            None => store.create_world(&scope, payload)?,
        };
        let worlds = store.list_worlds(&scope)?;
        Ok::<(Value, Vec<Value>), gml_persistence::StoreError>((world, worlds))
    })
    .await;
    match res {
        Ok(Ok(saved)) => Ok(saved),
        Ok(Err(gml_persistence::StoreError::WorldNotFound(id))) => Err(json_response(
            StatusCode::NOT_FOUND,
            &json!({"ok": false, "error": format!("world not found: {id}")}),
        )),
        Ok(Err(e)) => Err(json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": e.to_string()}),
        )),
        Err(e) => Err(json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": format!("join error: {e}")}),
        )),
    }
}

/// Normalize a call's `_meta` stats into the `{in, out, cached, tokens}` shape
/// the architect token-usage readout consumes (mirrors the main chat usage).
fn architect_usage(stats: &serde_json::Map<String, Value>) -> Value {
    let n = |key: &str| stats.get(key).and_then(Value::as_i64).unwrap_or(0);
    let input = n("prompt_eval_count");
    let output = n("eval_count");
    let cached = n("cached_tokens");
    json!({ "in": input, "out": output, "cached": cached, "tokens": input + output })
}

async fn get_settings(State(state): State<AppState>) -> Response {
    let _ = active_chat(&state);
    ok_json(&json!({
        "ok": true,
        "settings": state.settings.get(),
        "settings_options": state.settings.options(),
    }))
}

async fn get_codex_status(State(state): State<AppState>) -> Response {
    let _ = active_chat(&state);
    ok_json(&Value::Object(gml_codex::auth_status()))
}

async fn get_sidecar_status(State(state): State<AppState>) -> Response {
    ok_json(&sidecar_status_payload(&state).await)
}

async fn sidecar_status_payload(state: &AppState) -> Value {
    let rag_enabled = state.config.rag_enabled;
    let reranker_enabled = state.config.rag_enabled && state.config.rag_rerank_enabled;
    let tts_enabled = state.settings.tts_enabled(None);
    let image_enabled = image_generation_enabled(state);
    let enabled = rag_enabled || tts_enabled || image_enabled;

    let Some(sidecar) = &state.sidecar else {
        return json!({
            "ok": true,
            "enabled": enabled,
            "state": if enabled { "unavailable" } else { "disabled" },
            "ready": false,
            "pid": Value::Null,
            "base_url": state.config.infer_base_url.clone(),
            "components": sidecar_components(None, state, tts_enabled, image_enabled),
            "error": if enabled { "sidecar manager is not attached" } else { "" },
        });
    };

    let snapshot = sidecar.snapshot();
    let health = sidecar.health_payload().await.ok();
    let health_ready = sidecar_health_ready(
        health.as_ref(),
        rag_enabled,
        reranker_enabled,
        tts_enabled,
        image_enabled,
    );
    let state_label = if health_ready {
        "ready".to_string()
    } else {
        snapshot.state.as_str().to_string()
    };
    let error = if health_ready {
        String::new()
    } else {
        snapshot.error.clone().unwrap_or_default()
    };

    json!({
        "ok": true,
        "enabled": enabled,
        "state": if enabled { state_label } else { "disabled".to_string() },
        "manager_state": snapshot.state.as_str(),
        "manager_ready": snapshot.ready,
        "ready": enabled && health_ready,
        "pid": snapshot.pid,
        "base_url": snapshot.base_url,
        "elapsed_ms": snapshot.started_elapsed.map(|d| d.as_millis()),
        "ready_timeout_ms": snapshot.ready_timeout.as_millis(),
        "components": sidecar_components(health.as_ref(), state, tts_enabled, image_enabled),
        "error": error,
    })
}

fn image_generation_enabled(state: &AppState) -> bool {
    state.config.image_enabled && state.settings.image_enabled(None)
}

fn bool_env(on: bool) -> String {
    if on { "1" } else { "0" }.to_string()
}

fn restart_sidecar_in_background(
    state: &AppState,
    tts_enabled: bool,
    image_enabled: bool,
    warm_image: bool,
) {
    let Some(sidecar) = state.sidecar.clone() else {
        return;
    };
    sidecar.set_env("TTS_ENABLED", bool_env(tts_enabled));
    sidecar.set_env("IMAGE_ENABLED", bool_env(image_enabled));
    let should_start = state.config.rag_enabled || tts_enabled || image_enabled;
    let http = state.http.clone();
    let base_url = state.config.infer_base_url.clone();
    let timeout =
        std::time::Duration::from_secs_f64((state.config.image_timeout_seconds + 10.0).max(1.0));
    tokio::spawn(async move {
        sidecar.shutdown().await;
        if !should_start {
            return;
        }
        if let Err(e) = sidecar.ensure_started(true).await {
            tracing::warn!("sidecar restart after settings update failed: {e}");
            return;
        }
        if !warm_image {
            return;
        }
        match http
            .post(format!("{base_url}/images/start"))
            .timeout(timeout)
            .send()
            .await
        {
            Ok(resp) if !resp.status().is_success() => {
                tracing::warn!("image ComfyUI warmup returned {}", resp.status());
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("image ComfyUI warmup request failed: {e}"),
        }
    });
}

fn sidecar_health_ready(
    health: Option<&Value>,
    rag_enabled: bool,
    reranker_enabled: bool,
    tts_enabled: bool,
    image_enabled: bool,
) -> bool {
    let Some(health) = health else {
        return false;
    };
    let mut any = false;
    for (enabled, key) in [
        (rag_enabled, "embedder"),
        (reranker_enabled, "reranker"),
        (tts_enabled, "tts"),
        (image_enabled, "image"),
    ] {
        if enabled {
            any = true;
            if !component_up(health, key) {
                return false;
            }
        }
    }
    any
}

fn component_up(health: &Value, key: &str) -> bool {
    health
        .get(key)
        .and_then(|v| v.get("up"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn component_field<'a>(health: Option<&'a Value>, key: &str, field: &str) -> Option<&'a Value> {
    health
        .and_then(|body| body.get(key))
        .and_then(|v| v.get(field))
}

fn sidecar_components(
    health: Option<&Value>,
    state: &AppState,
    tts_enabled: bool,
    image_enabled: bool,
) -> Value {
    json!({
        "embedder": {
            "enabled": state.config.rag_enabled,
            "up": health.map(|body| component_up(body, "embedder")).unwrap_or(false),
            "model": component_field(health, "embedder", "model")
                .and_then(Value::as_str)
                .unwrap_or(state.config.rag_embeddings_model.as_str()),
            "quant": component_field(health, "embedder", "quant")
                .and_then(Value::as_str)
                .unwrap_or(state.config.embedder_quant.as_str()),
            "dim": component_field(health, "embedder", "dim").cloned().unwrap_or(Value::Null),
        },
        "reranker": {
            "enabled": state.config.rag_enabled && state.config.rag_rerank_enabled,
            "up": health.map(|body| component_up(body, "reranker")).unwrap_or(false),
            "model": component_field(health, "reranker", "model")
                .and_then(Value::as_str)
                .unwrap_or(state.config.rag_rerank_model.as_str()),
            "quant": component_field(health, "reranker", "quant")
                .and_then(Value::as_str)
                .unwrap_or(state.config.reranker_quant.as_str()),
        },
        "tts": {
            "enabled": tts_enabled,
            "up": health.map(|body| component_up(body, "tts")).unwrap_or(false),
            "model": component_field(health, "tts", "model").cloned().unwrap_or(Value::Null),
            "voices": component_field(health, "tts", "voices").cloned().unwrap_or(Value::Array(vec![])),
        },
        "image": {
            "enabled": image_enabled,
            "up": health.map(|body| component_up(body, "image")).unwrap_or(false),
            "warm": component_field(health, "image", "warm").cloned().unwrap_or(Value::Bool(false)),
            "runtime_ready": component_field(health, "image", "runtime_ready").cloned().unwrap_or(Value::Bool(false)),
            "runtime_root": component_field(health, "image", "runtime_root").cloned().unwrap_or(Value::Null),
            "output_dir": component_field(health, "image", "output_dir").cloned().unwrap_or(Value::Null),
            "comfy_url": component_field(health, "image", "comfy_url").cloned().unwrap_or(Value::Null),
            "comfy_up": component_field(health, "image", "comfy_up").cloned().unwrap_or(Value::Bool(false)),
            "models": component_field(health, "image", "models").cloned().unwrap_or(Value::Array(vec![])),
            "error": component_field(health, "image", "error").cloned().unwrap_or(Value::String(String::new())),
        },
    })
}

// --- dev token counter (OpenAI /v1/responses/input_tokens) + key storage -----

/// `{ok: true, saved, hint}` merged from `openai_key::status()`.
fn openai_key_ok() -> Response {
    let mut out = Map::new();
    out.insert("ok".into(), Value::Bool(true));
    out.extend(openai_key::status());
    ok_json(&Value::Object(out))
}

/// `GET /debug/openai_key` — saved flag + masked hint (never the raw key).
async fn get_openai_key() -> Response {
    openai_key_ok()
}

/// `POST /debug/openai_key {key}` — store the key (server-side), return status.
async fn post_openai_key(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    openai_key::save_key(data.get("key").and_then(Value::as_str).unwrap_or(""));
    // A new/changed key can change the accurate baseline — drop it and re-warm.
    gml_orchestrator::compact::clear_sys_tokens_override();
    sys_tokens::reset();
    sys_tokens::ensure(&state.http);
    openai_key_ok()
}

/// `POST /debug/openai_key/delete`.
async fn post_openai_key_delete() -> Response {
    openai_key::delete_key();
    // No key → fall back to the chars/token estimate.
    gml_orchestrator::compact::clear_sys_tokens_override();
    sys_tokens::reset();
    ok_json(&json!({"ok": true, "saved": false, "hint": ""}))
}

/// `POST /debug/tokenize {text, model}` — proxy to OpenAI's free
/// `/v1/responses/input_tokens` (no model run; returns just the input-token
/// count). Faithful port of `server.py`'s handler.
async fn post_debug_tokenize(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    let text = data
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let model = data
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let key = openai_key::load_key();
    if key.is_empty() {
        return ok_json(&json!({"ok": false, "error": "Сначала сохрани OpenAI API-ключ."}));
    }
    if text.is_empty() {
        return ok_json(&json!({"ok": false, "error": "Пустой текст."}));
    }
    let model_req = if model.is_empty() {
        "gpt-4o-mini".to_string()
    } else {
        model.clone()
    };
    let resp = state
        .http
        .post("https://api.openai.com/v1/responses/input_tokens")
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .json(&json!({"model": model_req, "input": text}))
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await;
    let resp = match resp {
        Ok(r) => r,
        Err(e) => return ok_json(&json!({"ok": false, "error": e.to_string()})),
    };
    let st = resp.status();
    let raw = resp.text().await.unwrap_or_default();
    let body_val: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    if !st.is_success() {
        let msg = body_val
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let detail = if msg.is_empty() {
            raw.chars().take(200).collect::<String>()
        } else {
            msg.to_string()
        };
        return ok_json(&json!({
            "ok": false,
            "status": st.as_u16(),
            "error": format!("OpenAI {}: {}", st.as_u16(), detail),
        }));
    }
    let count = body_val.get("input_tokens").cloned().unwrap_or(Value::Null);
    ok_json(&json!({
        "ok": true,
        "count": count,
        "chars": text.chars().count(),
        "model": model,
    }))
}

async fn get_chats(State(state): State<AppState>) -> Response {
    let scope = chat_scope_id();
    // get_active ensures at least one chat exists.
    if let Err(resp) = active_chat(&state) {
        return resp;
    }
    let store = state.store.clone();
    let scope2 = scope.clone();
    let res = tokio::task::spawn_blocking(move || {
        let chats = store.list_chats(&scope2)?;
        let active = store.active_chat_id(&scope2)?;
        Ok::<(Vec<Value>, Option<String>), gml_persistence::StoreError>((chats, active))
    })
    .await;
    match res {
        Ok(Ok((chats, active))) => ok_json(&json!({
            "ok": true,
            "active_chat_id": active.unwrap_or_default(),
            "chats": chats,
        })),
        Ok(Err(e)) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": e.to_string()}),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": format!("join error: {e}")}),
        ),
    }
}

async fn get_worlds(State(state): State<AppState>) -> Response {
    let scope = chat_scope_id();
    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        let worlds = store.list_worlds(&scope)?;
        Ok::<Value, gml_persistence::StoreError>(json!({
            "ok": true,
            "worlds": worlds,
        }))
    })
    .await;
    join_json(res)
}

async fn post_create_world(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    let payload = match reusable_world_payload_from_body(&data) {
        Ok(payload) => payload,
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"ok": false, "error": error}),
            )
        }
    };

    let scope = chat_scope_id();
    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        let world = store.create_world(&scope, payload)?;
        let worlds = store.list_worlds(&scope)?;
        Ok::<Value, gml_persistence::StoreError>(json!({
            "ok": true,
            "world": world,
            "worlds": worlds,
        }))
    })
    .await;
    join_json(res)
}

async fn post_update_world(
    State(state): State<AppState>,
    AxPath(id): AxPath<String>,
    body: Bytes,
) -> Response {
    let world_id = urlencoding::decode(&id)
        .map(|c| c.into_owned())
        .unwrap_or(id);
    let data = parse_body(&body);
    let payload = match reusable_world_payload_from_body(&data) {
        Ok(payload) => payload,
        Err(error) => {
            return json_response(
                StatusCode::BAD_REQUEST,
                &json!({"ok": false, "error": error}),
            )
        }
    };

    let scope = chat_scope_id();
    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        let world = store.update_world(&scope, &world_id, payload)?;
        let worlds = store.list_worlds(&scope)?;
        Ok::<Value, gml_persistence::StoreError>(json!({
            "ok": true,
            "world": world,
            "worlds": worlds,
        }))
    })
    .await;
    match res {
        Ok(Ok(v)) => ok_json(&v),
        Ok(Err(gml_persistence::StoreError::WorldNotFound(id))) => json_response(
            StatusCode::NOT_FOUND,
            &json!({"ok": false, "error": format!("world not found: {id}")}),
        ),
        Ok(Err(e)) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": e.to_string()}),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": format!("join error: {e}")}),
        ),
    }
}

async fn get_models(State(state): State<AppState>) -> Response {
    let cfg = state.config.clone();
    let settings = state.settings.clone();
    // Build/ensure the client, then list models. Errors -> {ok:false, models:[]}.
    let scope = chat_scope_id();
    let chat_id = match active_chat(&state) {
        Ok(id) => id,
        Err(resp) => return resp,
    };
    let lock = state.chat_lock(&chat_id);
    let _guard = lock.lock().await;
    let store = state.store.clone();
    let make_client = state.make_client.clone();
    let app = state.clone();

    // ensure_client mutates the runtime; do it under the lock then snapshot the
    // live client (Arc) so list_models() can run async outside spawn_blocking.
    let client: Result<Arc<dyn Backend>, Response> = {
        let scope = scope.clone();
        let chat_id = chat_id.clone();
        let store = store.clone();
        tokio::task::spawn_blocking(move || {
            store.with_runtime(&scope, &chat_id, |rt| {
                ensure_client(rt, &app);
                let _ = make_client; // factory already wired via app
                rt.session.client.clone()
            })
        })
        .await
        .map_err(|e| {
            json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &json!({"ok": false, "error": format!("join error: {e}"), "models": []}),
            )
        })
        .and_then(|r| match r {
            Ok(Some(c)) => Ok(c),
            Ok(None) => Err(json_response(
                StatusCode::NOT_FOUND,
                &json!({"ok": false, "error": "chat not found", "models": []}),
            )),
            Err(e) => Err(json_response(
                StatusCode::BAD_REQUEST,
                &json!({"ok": false, "error": e.to_string(), "models": []}),
            )),
        })
    };
    let client = match client {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let models = list_models(client.as_ref(), &cfg).await;
    let current = {
        let m = client.model();
        if m.is_empty() {
            cfg.model.clone()
        } else {
            m
        }
    };
    ok_json(&json!({
        "ok": true,
        "model": current,
        "models": models,
        "settings": settings.get(),
        "settings_options": settings.options(),
    }))
}

async fn get_export(State(state): State<AppState>) -> Response {
    let cfg = state.config.clone();
    match with_active(&state, move |rt| payload::export_data(rt, &cfg)).await {
        Ok(v) => {
            // Python: json.dumps(..., ensure_ascii=False, indent=2, default=str).
            let body = serde_json::to_vec_pretty(&v).unwrap_or_default();
            let mut resp = Response::new(Body::from(body));
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            );
            resp.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=\"gm-lab-export.json\""),
            );
            resp
        }
        Err(resp) => resp,
    }
}

// =========================================================================
// chat lifecycle
// =========================================================================

async fn post_create_chat(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    let brief = body_str(&data, "brief");
    let story_id = body_str(&data, "story_id");
    let effective_story_id = if story_id.is_empty() {
        PROCEDURAL_STORY_ID.to_string()
    } else {
        story_id.clone()
    };
    let title = body_str(&data, "title");
    let activate = bool_from_body(data.get("activate"), true);

    let is_procedural = effective_story_id == PROCEDURAL_STORY_ID;
    if !story_id.is_empty() && !is_procedural && !gml_stories::story_ids().contains(&story_id) {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": format!("unknown story_id: {story_id}")}),
        );
    }

    let scope = chat_scope_id();
    let cfg = state.config.clone();
    let settings = state.settings.clone();
    let make_client = state.make_client.clone();
    let store = state.store.clone();

    // Build the session (brief -> seeded world via the model; story -> catalog).
    let model_hint = match store.active_chat_id(&scope) {
        Ok(Some(active_id)) => store
            .with_runtime(&scope, &active_id, |rt| model_hint_for_new_chat(rt, &cfg))
            .ok()
            .flatten()
            .unwrap_or_default(),
        _ => String::new(),
    };

    let session = if !brief.is_empty() {
        let client = (make_client)();
        if !model_hint.is_empty() {
            client.set_model(&model_hint);
        }
        match gml_agents::build_world_seed(client.as_ref(), &brief).await {
            Ok(seed) => {
                let world = World::from_seed(&seed);
                build_session(client, world, &make_client, &cfg, &model_hint)
            }
            Err(e) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"ok": false, "error": e.to_string()}),
                )
            }
        }
    } else if is_procedural {
        // Living-world canon path (locked decision #4): generate the canon and
        // derive the legacy-facing World from it. The resulting session is
        // canon-authoritative — its scene is rebuilt from the start place.
        let spec = worldspec_from_body(&data);
        let world_lore = match required_world_lore_from_body(&data, &spec) {
            Ok(v) => v,
            Err(error) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"ok": false, "error": error}),
                );
            }
        };
        let client = (make_client)();
        let mut world = World::from_worldgen_with_lore(&spec, world_lore);
        let story_title = body_str(&data, "story_title");
        if !story_title.is_empty() {
            world.set_story_title(&story_title);
        } else if !title.is_empty() {
            world.set_story_title(&title);
        } else if !world.world_canon.world_lore.name.is_empty() {
            let lore_name = world.world_canon.world_lore.name.clone();
            world.set_story_title(&lore_name);
        }
        let story_brief = body_str(&data, "story_brief");
        if !story_brief.is_empty() {
            world.set_story_brief(&story_brief);
        }
        let public_intro = body_str(&data, "public_intro");
        if !public_intro.is_empty() {
            world.set_public_intro(&public_intro);
        } else if !world.world_canon.world_lore.public_premise.is_empty() {
            let public_premise = world.world_canon.world_lore.public_premise.clone();
            world.set_public_intro(&public_premise);
        }
        story_session(client, world, &cfg, &model_hint)
    } else {
        let seed = match gml_stories::story_seed(&effective_story_id) {
            Ok(s) => s,
            Err(e) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"ok": false, "error": e.to_string()}),
                )
            }
        };
        let client = (make_client)();
        let world = World::from_seed(&seed);
        story_session(client, world, &cfg, &model_hint)
    };

    let derived_title = if !title.is_empty() {
        title.clone()
    } else if !brief.is_empty() {
        brief.clone()
    } else {
        session.world.story_title.clone()
    };

    let cfg2 = cfg.clone();
    let settings2 = settings.clone();
    let res = tokio::task::spawn_blocking(move || {
        let chat_id = store.create_chat(
            &scope,
            Some(session),
            None,
            0,
            Some(&derived_title),
            None,
            activate,
        )?;
        let active = store.active_chat_id(&scope)?.unwrap_or_default();
        let is_active = chat_id == active;
        let mut response = Map::new();
        response.insert("ok".to_string(), Value::Bool(true));
        response.insert("active_chat_id".to_string(), Value::String(active.clone()));
        store.with_runtime(&scope, &chat_id, |rt| {
            response.insert("chat".to_string(), payload::chat_response(rt, is_active));
            if is_active {
                response.insert("state".to_string(), payload::state(rt, &cfg2, &settings2));
                response.insert(
                    "transcript".to_string(),
                    json!({"events": payload::replay_events(rt)}),
                );
            }
        })?;
        Ok::<Value, gml_persistence::StoreError>(Value::Object(response))
    })
    .await;
    join_json(res)
}

async fn post_activate_chat(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Response {
    let chat_id = urlencoding::decode(&id)
        .map(|c| c.into_owned())
        .unwrap_or(id);
    let scope = chat_scope_id();
    let cfg = state.config.clone();
    let settings = state.settings.clone();
    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        if !store.activate_chat(&scope, &chat_id)? {
            return Ok(json!({"ok": false, "error": "chat not found"}));
        }
        let mut out = Map::new();
        store.with_runtime(&scope, &chat_id, |rt| {
            out.insert("ok".to_string(), Value::Bool(true));
            out.insert("chat".to_string(), payload::chat_response(rt, true));
            out.insert("state".to_string(), payload::state(rt, &cfg, &settings));
            out.insert(
                "transcript".to_string(),
                json!({"events": payload::replay_events(rt)}),
            );
        })?;
        Ok::<Value, gml_persistence::StoreError>(Value::Object(out))
    })
    .await;
    match res {
        Ok(Ok(v)) => {
            if v.get("ok").and_then(Value::as_bool) == Some(false) {
                json_response(StatusCode::NOT_FOUND, &v)
            } else {
                ok_json(&v)
            }
        }
        Ok(Err(e)) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": e.to_string()}),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": format!("join error: {e}")}),
        ),
    }
}

async fn post_delete_chat(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Response {
    let chat_id = urlencoding::decode(&id)
        .map(|c| c.into_owned())
        .unwrap_or(id);
    let scope = chat_scope_id();
    let cfg = state.config.clone();
    let settings = state.settings.clone();
    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        let result = store.delete_chat(&scope, &chat_id)?;
        if result.get("deleted").and_then(Value::as_bool) != Some(true) {
            let reason = result
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("chat not found")
                .to_string();
            return Ok(json!({"ok": false, "error": reason, "__status": 404}));
        }
        let active_id = store.get_active(&scope)?;
        let chats = store.list_chats(&scope)?;
        let active_chat_id = store.active_chat_id(&scope)?.unwrap_or_default();
        let embeddings_purged = result
            .get("embeddings_purged")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let mut out = Map::new();
        store.with_runtime(&scope, &active_id, |rt| {
            out.insert("ok".to_string(), Value::Bool(true));
            out.insert("deleted".to_string(), Value::Bool(true));
            out.insert("active_chat_id".to_string(), Value::String(active_chat_id));
            out.insert("chats".to_string(), Value::Array(chats));
            out.insert("chat".to_string(), payload::chat_response(rt, true));
            out.insert("state".to_string(), payload::state(rt, &cfg, &settings));
            out.insert(
                "transcript".to_string(),
                json!({"events": payload::replay_events(rt)}),
            );
            out.insert("embeddings_purged".to_string(), json!(embeddings_purged));
        })?;
        Ok::<Value, gml_persistence::StoreError>(Value::Object(out))
    })
    .await;
    match res {
        Ok(Ok(mut v)) => {
            let status = v.get("__status").and_then(Value::as_i64);
            if let Value::Object(ref mut m) = v {
                m.remove("__status");
            }
            if status == Some(404) {
                json_response(StatusCode::NOT_FOUND, &v)
            } else {
                ok_json(&v)
            }
        }
        Ok(Err(e)) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": e.to_string()}),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": format!("join error: {e}")}),
        ),
    }
}

async fn post_delete_world(State(state): State<AppState>, AxPath(id): AxPath<String>) -> Response {
    let world_id = urlencoding::decode(&id)
        .map(|c| c.into_owned())
        .unwrap_or(id);
    let scope = chat_scope_id();
    let store = state.store.clone();
    let res = tokio::task::spawn_blocking(move || {
        let result = store.delete_world(&scope, &world_id)?;
        if result.get("deleted").and_then(Value::as_bool) != Some(true) {
            let reason = result
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("world not found")
                .to_string();
            return Ok(json!({"ok": false, "error": reason, "__status": 404}));
        }
        let worlds = store.list_worlds(&scope)?;
        Ok::<Value, gml_persistence::StoreError>(json!({
            "ok": true,
            "deleted": true,
            "worlds": worlds,
        }))
    })
    .await;
    match res {
        Ok(Ok(mut v)) => {
            let status = v.get("__status").and_then(Value::as_i64);
            if let Value::Object(ref mut m) = v {
                m.remove("__status");
            }
            if status == Some(404) {
                json_response(StatusCode::NOT_FOUND, &v)
            } else {
                ok_json(&v)
            }
        }
        Ok(Err(e)) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": e.to_string()}),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": format!("join error: {e}")}),
        ),
    }
}

// =========================================================================
// model / settings / cmd
// =========================================================================

async fn post_model(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    let model = body_str(&data, "model");
    if model.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "model is required"}),
        );
    }
    let cfg = state.config.clone();
    let settings = state.settings.clone();
    let app = state.clone();
    let model2 = model.clone();
    match with_active(&state, move |rt| {
        ensure_client(rt, &app);
        let client = rt.session.client.clone();
        client.set_model(&model2);
        rt.session.set_model_for_all_clients(&model2);
        // reconcile_for_model best-effort (model meta lookup omitted for the
        // sync path — settings reconcile uses None which is a safe no-op clamp).
        let _ = settings.reconcile_for_model(None);
        rt.session.client_session_id = client.session_id();
        rt.session.client_thread_id = client.thread_id();
        let npc_ids: Vec<String> = rt.session.npc_clients.keys().cloned().collect();
        for npc_id in npc_ids {
            rt.session.remember_npc_client(&npc_id);
        }
        payload::state(rt, &cfg, &settings)
    })
    .await
    {
        Ok(state_payload) => {
            // Persist after the mutation.
            persist_active(&state).await;
            ok_json(&json!({"ok": true, "state": state_payload}))
        }
        Err(resp) => resp,
    }
}

async fn post_settings(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    // Python: runtime_settings.update(data["settings"] if "settings" in data else data).
    let update = match data.get("settings") {
        Some(Value::Object(m)) => Some(m.clone()),
        Some(_) => Some(Map::new()),
        None => Some(data.clone()),
    };
    let tts_was_enabled = state.settings.tts_enabled(None);
    let image_was_enabled = image_generation_enabled(&state);
    let settings_map = state.settings.update(update.as_ref());
    let tts_now_enabled = state.settings.tts_enabled(Some(&settings_map));
    let image_now_enabled =
        state.config.image_enabled && state.settings.image_enabled(Some(&settings_map));
    if tts_was_enabled != tts_now_enabled || image_was_enabled != image_now_enabled {
        restart_sidecar_in_background(
            &state,
            tts_now_enabled,
            image_now_enabled,
            !image_was_enabled && image_now_enabled,
        );
    }
    let cfg = state.config.clone();
    let settings = state.settings.clone();
    match with_active(&state, move |rt| payload::state(rt, &cfg, &settings)).await {
        Ok(state_payload) => {
            persist_active(&state).await;
            ok_json(&json!({
                "ok": true,
                "settings": settings_map,
                "settings_options": state.settings.options(),
                "state": state_payload,
            }))
        }
        Err(resp) => resp,
    }
}

async fn post_cmd(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    let cmd = body_str(&data, "cmd");
    let arg = body_str(&data, "arg");
    let cfg = state.config.clone();
    let settings = state.settings.clone();

    if cmd == "new" && !arg.is_empty() {
        // Create a new seeded chat (mirrors /chats with brief).
        let scope = chat_scope_id();
        let make_client = state.make_client.clone();
        let store = state.store.clone();
        let model_hint = match store.active_chat_id(&scope) {
            Ok(Some(active_id)) => store
                .with_runtime(&scope, &active_id, |rt| model_hint_for_new_chat(rt, &cfg))
                .ok()
                .flatten()
                .unwrap_or_default(),
            _ => String::new(),
        };
        let client = (make_client)();
        if !model_hint.is_empty() {
            client.set_model(&model_hint);
        }
        let session = match gml_agents::build_world_seed(client.as_ref(), &arg).await {
            Ok(seed) => {
                let world = World::from_seed(&seed);
                build_session(client, world, &make_client, &cfg, &model_hint)
            }
            Err(e) => {
                return json_response(
                    StatusCode::BAD_REQUEST,
                    &json!({"ok": false, "error": e.to_string()}),
                )
            }
        };
        let arg2 = arg.clone();
        let res = tokio::task::spawn_blocking(move || {
            let chat_id =
                store.create_chat(&scope, Some(session), None, 0, Some(&arg2), None, true)?;
            let mut out = Map::new();
            store.with_runtime(&scope, &chat_id, |rt| {
                out.insert("ok".to_string(), Value::Bool(true));
                out.insert("chat".to_string(), payload::chat_response(rt, true));
                out.insert("state".to_string(), payload::state(rt, &cfg, &settings));
            })?;
            Ok::<Value, gml_persistence::StoreError>(Value::Object(out))
        })
        .await;
        return join_json(res);
    }

    // reset / constraint / event — mutate the active chat in place.
    let app = state.clone();
    let cmd2 = cmd.clone();
    let arg2 = arg.clone();
    let result = with_active(&state, move |rt| -> Result<Value, (StatusCode, String)> {
        match cmd2.as_str() {
            "reset" => {
                let session = &mut rt.session;
                let matches = session.client_backend.is_empty()
                    || session.client_backend == app.config.backend;
                let (mut model, mut session_id, mut thread_id) =
                    (String::new(), String::new(), String::new());
                if matches {
                    model = if !session.client_model.is_empty() {
                        session.client_model.clone()
                    } else {
                        session.client.model()
                    };
                    session_id = if !session.client_session_id.is_empty() {
                        session.client_session_id.clone()
                    } else {
                        session.client.session_id()
                    };
                    thread_id = if !session.client_thread_id.is_empty() {
                        session.client_thread_id.clone()
                    } else {
                        session.client.thread_id()
                    };
                }
                let story_id = session.world.story_id.clone();
                if !gml_stories::story_ids().contains(&story_id) {
                    let label = if story_id.is_empty() {
                        "unknown".to_string()
                    } else {
                        story_id
                    };
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!("cannot reset non-catalog story: {label}"),
                    ));
                }
                let seed = gml_stories::story_seed(&story_id)
                    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
                let world = World::from_seed(&seed);
                let factory = session.npc_client_factory.clone();
                let mut new_session = Session::with_world((app.make_client)(), world, factory);
                new_session.compaction = CompactionThresholds::from_config(&app.config);
                new_session.client_backend = app.config.backend.clone();
                new_session.client_model = model;
                new_session.client_session_id = session_id;
                new_session.client_thread_id = thread_id;
                rt.session = new_session;
                rt.transcript.clear();
                rt.turn_count = 0;
            }
            "constraint" if !arg2.is_empty() => {
                rt.session.world.constraints.push(arg2.clone());
            }
            "event" if !arg2.is_empty() => {
                rt.session.world.hidden_events.push(arg2.clone());
            }
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("unknown or incomplete command: {cmd2}"),
                ));
            }
        }
        Ok(payload::state(rt, &app.config, &app.settings))
    })
    .await;

    match result {
        Ok(Ok(state_payload)) => {
            persist_active(&state).await;
            ok_json(&json!({"ok": true, "state": state_payload}))
        }
        Ok(Err((code, msg))) => json_response(code, &json!({"ok": false, "error": msg})),
        Err(resp) => resp,
    }
}

// =========================================================================
// /turn (SSE)
// =========================================================================

async fn post_turn(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    let text = body_str(&data, "text");

    let scope = chat_scope_id();
    let chat_id = match active_chat(&state) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    // The turn runs INCREMENTALLY: a tokio task drives run_turn_into, sending
    // events into a channel; the response body streams each as a `data: ...`
    // frame as it arrives, then appends the terminal `done` frame. The per-chat
    // lock is held for the whole streamed turn (Python RLock).
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<gml_types::Event>();
    let lock = state.chat_lock(&chat_id);
    let store = state.store.clone();
    let app = state.clone();
    let scope2 = scope.clone();
    let chat_id2 = chat_id.clone();

    tokio::spawn(async move {
        let _guard = lock.lock().await;
        // Take the runtime out of the cache, run the turn against an owned
        // Session (run_turn_into needs &mut + async), then write back + persist.
        let loaded = {
            let store = store.clone();
            let scope = scope2.clone();
            let chat_id = chat_id2.clone();
            tokio::task::spawn_blocking(move || store.load_chat(&scope, &chat_id))
                .await
                .ok()
                .and_then(|r| r.ok())
        };
        let mut rt = match loaded {
            Some(rt) => rt,
            None => {
                let _ = tx.send(gml_types::Event::new(
                    "error",
                    Some("ГМ".to_string()),
                    Value::String("chat not found".to_string()),
                    None,
                ));
                return;
            }
        };

        ensure_client_owned(&mut rt, &app);
        rt.turn_count += 1;
        let turn_no = rt.turn_count;

        // Forward events from the orchestrator into the SSE channel AND collect
        // them for transcript append (non-error events go to the transcript as
        // server.py does; the orchestrator never yields the terminal `done`).
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<gml_types::Event>();
        let settings = app.settings.clone();
        let text2 = text.clone();

        // We must both stream and append-to-transcript each event. Drive the
        // turn, draining ev_rx into the client channel + transcript.
        let mut session = std::mem::replace(&mut rt.session, placeholder_session(&app));
        let turn_handle = tokio::spawn(async move {
            run_turn_into(&mut session, &settings, &text2, ev_tx).await;
            session
        });

        while let Some(event) = ev_rx.recv().await {
            // Append non-delta? server.py appends EVERY event yielded by run_turn
            // (including deltas) to the transcript, then replay filters deltas.
            rt.transcript
                .push(json!({"turn": turn_no, "event": &event}));
            // Stream to the client (ignore send errors = client gone).
            if tx.send(event).is_err() {
                // client disconnected; keep draining so the turn finishes + saves.
            }
        }
        let session = turn_handle
            .await
            .unwrap_or_else(|_| placeholder_session(&app));
        rt.session = session;

        // Persist the dialog and replace the cached runtime used by /state,
        // /debug, and /transcript.
        let store = store.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let _ = store.save_owned(rt);
        })
        .await;
        // tx drops here -> the response stream sees end-of-events and appends
        // the terminal `done` frame.
    });

    // Build the streaming body: each event -> `data: {json}\n\n`, then `done`.
    let stream = async_stream::stream! {
        let mut rx = rx;
        while let Some(event) = rx.recv().await {
            let line = format!("data: {}\n\n", serde_json::to_string(&event).unwrap_or_default());
            yield Ok::<Bytes, std::io::Error>(Bytes::from(line));
        }
        yield Ok(Bytes::from("data: {\"kind\": \"done\"}\n\n"));
    };

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    let body = Body::from_stream(stream);
    (headers, body).into_response()
}

/// `ensure_client(dialog)` for an owned runtime (the `/turn` path operates on a
/// freshly loaded runtime rather than the cache).
fn ensure_client_owned(rt: &mut DialogRuntime, state: &AppState) {
    ensure_client(rt, state);
}

/// A throwaway session used while the real one is moved out for the async turn.
fn placeholder_session(state: &AppState) -> Session {
    let client = (state.make_client)();
    let world = World::from_worldgen(&WorldSpec::default());
    Session::with_world(client, world, state.make_client.clone())
}

// =========================================================================
// transcribe / tts / codex
// =========================================================================

async fn post_generate_image(State(state): State<AppState>, body: Bytes) -> Response {
    if !image_generation_enabled(&state) {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "error": "image generation disabled"}),
        );
    }
    let Some(sidecar) = &state.sidecar else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "error": "sidecar manager is not attached"}),
        );
    };
    if let Err(e) = sidecar.ensure_started(true).await {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "error": format!("image sidecar unavailable: {e}")}),
        );
    }

    let url = format!("{}/images/generate", state.config.infer_base_url);
    let timeout = std::time::Duration::from_secs_f64(state.config.image_timeout_seconds + 10.0);
    match state
        .http
        .post(url)
        .timeout(timeout)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(resp) => proxy_sidecar_response(resp, "application/json; charset=utf-8").await,
        Err(e) => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "error": format!("image sidecar request failed: {e}")}),
        ),
    }
}

async fn get_generated_image(
    State(state): State<AppState>,
    AxPath((run_id, filename)): AxPath<(String, String)>,
) -> Response {
    if !image_generation_enabled(&state) {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "error": "image generation disabled"}),
        );
    }
    let Some(sidecar) = &state.sidecar else {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "error": "sidecar manager is not attached"}),
        );
    };
    if let Err(e) = sidecar.ensure_started(true).await {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "error": format!("image sidecar unavailable: {e}")}),
        );
    }

    let url = format!(
        "{}/image-files/{}/{}",
        state.config.infer_base_url,
        urlencoding::encode(&run_id),
        urlencoding::encode(&filename)
    );
    match state
        .http
        .get(url)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
    {
        Ok(resp) => proxy_sidecar_response(resp, "image/png").await,
        Err(e) => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "error": format!("image fetch failed: {e}")}),
        ),
    }
}

async fn proxy_sidecar_response(resp: reqwest::Response, default_content_type: &str) -> Response {
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(default_content_type)
        .to_string();
    match resp.bytes().await {
        Ok(bytes) => {
            let mut out = Response::new(Body::from(bytes));
            *out.status_mut() = status;
            out.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&content_type)
                    .unwrap_or(HeaderValue::from_static("application/octet-stream")),
            );
            out
        }
        Err(e) => json_response(
            StatusCode::BAD_GATEWAY,
            &json!({"ok": false, "error": format!("sidecar response read failed: {e}")}),
        ),
    }
}

async fn post_transcribe(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if state.config.backend != "codex" {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "транскрипция доступна только при GM_BACKEND=codex"}),
        );
    }
    if body.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "пустое аудио"}),
        );
    }
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("audio/webm")
        .to_string();
    match gml_audio::transcribe(&state.config, &body, &content_type).await {
        Ok(text) => ok_json(&json!({"ok": true, "text": text})),
        Err(e) => {
            // status surfaced into the JSON exactly like server.py.
            let status = e.status().map(|s| json!(s)).unwrap_or(Value::Null);
            ok_json(&json!({"ok": false, "error": e.to_string(), "status": status}))
        }
    }
}

async fn post_tts(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    let text = body_str(&data, "text");
    if text.is_empty() {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "empty text"}),
        );
    }
    // Resolve the voice (explicit / role / npc gender), mirroring the handler.
    let explicit = data
        .get("voice")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let voice = if matches!(explicit.as_str(), "gm" | "male" | "female") {
        explicit
    } else {
        let role = data
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let npc_id = body_str(&data, "npc_id");
        if role == "gm" || npc_id.is_empty() {
            "gm".to_string()
        } else {
            // Look up the NPC's pronouns from the active session.
            let pronouns = npc_pronouns(&state, &npc_id).await;
            gml_audio::npc_voice(&pronouns).to_string()
        }
    };

    let dir = gml_audio::tts::cache_dir();
    let fmt = tts_format();

    // Disk-cache hit -> no sidecar.
    if let Some(clip) = cache_lookup(&dir, &voice, &text, fmt) {
        return tts_clip_response(clip.bytes, clip.content_type, &voice);
    }

    let stream = bool_from_body(data.get("stream"), false);
    if stream {
        // cache miss + streaming requested -> head-first PCM proxy, cache after.
        match gml_audio::tts::stream_open(&state.http, &text, &voice).await {
            Ok(pcm_stream) => {
                return pcm_stream_response(pcm_stream, voice, text, dir, fmt);
            }
            Err(e) => {
                return json_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    &json!({"ok": false, "error": format!("TTS-сервис недоступен: {e}")}),
                );
            }
        }
    }

    // cache miss, non-stream -> /speak synth, compress, cache, return.
    match tts_synth(&state.http, &text, &voice).await {
        Ok(wav) => {
            let clip = compress_audio(wav, fmt).await;
            cache_store(&dir, &voice, &text, &clip.bytes, clip.ext);
            tts_clip_response(clip.bytes, clip.content_type, &voice)
        }
        Err(e) => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            &json!({"ok": false, "error": format!("TTS-сервис недоступен: {e}")}),
        ),
    }
}

fn tts_clip_response(bytes: Vec<u8>, content_type: &str, voice: &str) -> Response {
    let mut resp = Response::new(Body::from(bytes));
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).unwrap_or(HeaderValue::from_static("audio/ogg")),
    );
    resp.headers_mut().insert(
        HeaderName::from_static("x-tts-voice"),
        HeaderValue::from_str(voice).unwrap_or(HeaderValue::from_static("gm")),
    );
    resp
}

/// Stream PCM head-first from the sidecar, accumulating for a post-stream cache.
fn pcm_stream_response(
    pcm_stream: gml_audio::tts::PcmStream,
    voice: String,
    text: String,
    dir: std::path::PathBuf,
    fmt: gml_audio::TtsFormat,
) -> Response {
    use futures::StreamExt;
    let sr = pcm_stream.sample_rate;
    let upstream = pcm_stream.response.bytes_stream();
    let voice_hdr = voice.clone();
    let body_stream = async_stream::stream! {
        let mut acc: Vec<u8> = Vec::new();
        let mut completed = true;
        let mut upstream = upstream;
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(bytes) => {
                    acc.extend_from_slice(&bytes);
                    yield Ok::<Bytes, std::io::Error>(bytes);
                }
                Err(_) => {
                    completed = false;
                    break;
                }
            }
        }
        if completed && !acc.is_empty() {
            gml_audio::tts::finalize_stream_cache(&dir, &voice, &text, &acc, sr, fmt).await;
        }
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("audio/pcm"));
    headers.insert(
        HeaderName::from_static("x-sample-rate"),
        HeaderValue::from_str(&sr.to_string()).unwrap_or(HeaderValue::from_static("24000")),
    );
    headers.insert(
        HeaderName::from_static("x-tts-voice"),
        HeaderValue::from_str(&voice_hdr).unwrap_or(HeaderValue::from_static("gm")),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (headers, Body::from_stream(body_stream)).into_response()
}

/// Look up an NPC's pronouns from the active session (for voice mapping).
async fn npc_pronouns(state: &AppState, npc_id: &str) -> String {
    let npc_id = npc_id.to_string();
    with_active(state, move |rt| {
        rt.session
            .world
            .npcs
            .get(&npc_id)
            .map(|n| n.pronouns.clone())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default()
}

async fn post_codex_login(State(state): State<AppState>) -> Response {
    if state.config.backend != "codex" {
        return json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": "GM_BACKEND is not codex"}),
        );
    }
    match gml_codex::run_oauth(&state.http, &state.config).await {
        Ok(_) => ok_json(&json!({"ok": true, "auth": gml_codex::auth_status()})),
        Err(e) => json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": e.to_string(), "auth": gml_codex::auth_status()}),
        ),
    }
}

async fn post_codex_logout(State(state): State<AppState>) -> Response {
    match gml_codex::revoke_credential(&state.http, &state.config).await {
        Ok(_) => ok_json(&json!({"ok": true, "auth": gml_codex::auth_status()})),
        Err(e) => json_response(
            StatusCode::BAD_REQUEST,
            &json!({"ok": false, "error": e.to_string(), "auth": gml_codex::auth_status()}),
        ),
    }
}

// =========================================================================
// debug mutators
// =========================================================================

/// Shared body for the debug mutators that return the fresh debug payload.
async fn debug_mutate<F>(state: &AppState, f: F) -> Response
where
    F: FnOnce(&mut DialogRuntime, &Config, &RuntimeSettings) -> Value + Send + 'static,
{
    let cfg = state.config.clone();
    let settings = state.settings.clone();
    match with_active(state, move |rt| f(rt, &cfg, &settings)).await {
        Ok(v) => {
            persist_active(state).await;
            // Some mutators (npc) signal failure via ok:false; surface 400.
            if v.get("ok").and_then(Value::as_bool) == Some(false) {
                json_response(StatusCode::BAD_REQUEST, &v)
            } else {
                ok_json(&v)
            }
        }
        Err(resp) => resp,
    }
}

async fn post_debug_roll(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    debug_mutate(&state, move |rt, cfg, settings| {
        let w = &mut rt.session.world;
        if data.contains_key("next") {
            w.forced_die_next = die_or_none(data.get("next"));
        }
        if data.contains_key("all") {
            w.forced_die_all = die_or_none(data.get("all"));
        }
        payload::debug_data(rt, cfg, settings)
    })
    .await
}

async fn post_debug_fact(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    debug_mutate(&state, move |rt, cfg, settings| {
        let text = data.get("text").and_then(Value::as_str).unwrap_or("");
        let kind = data
            .get("kind")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("public");
        rt.session.world.add_fact(text, kind);
        payload::debug_data(rt, cfg, settings)
    })
    .await
}

async fn post_debug_fact_delete(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    debug_mutate(&state, move |rt, cfg, settings| {
        let id = data.get("id").and_then(Value::as_str).unwrap_or("");
        rt.session.world.remove_fact(id);
        payload::debug_data(rt, cfg, settings)
    })
    .await
}

async fn post_debug_player(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    debug_mutate(&state, move |rt, cfg, settings| {
        let fields = data.get("fields").cloned().unwrap_or(Value::Null);
        let fields = if fields.is_object() {
            fields
        } else {
            Value::Object(Map::new())
        };
        let reason = data
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("debug edit");
        rt.session.world.update_player_character(&fields, reason);
        payload::debug_data(rt, cfg, settings)
    })
    .await
}

async fn post_debug_npc(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    debug_mutate(&state, move |rt, cfg, settings| {
        let npc_id = data
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if rt
            .session
            .apply_debug_edit(&npc_id, &Value::Object(data.clone()))
        {
            payload::debug_data(rt, cfg, settings)
        } else {
            json!({"ok": false, "error": format!("no such npc: {npc_id}")})
        }
    })
    .await
}

async fn post_debug_story(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    debug_mutate(&state, move |rt, cfg, settings| {
        let w = &mut rt.session.world;
        if let Some(v) = data.get("title").and_then(Value::as_str) {
            w.set_story_title(v);
        }
        if let Some(v) = data.get("story_brief").and_then(Value::as_str) {
            w.set_story_brief(v);
        }
        if let Some(v) = data.get("public_intro").and_then(Value::as_str) {
            w.set_public_intro(v);
        }
        if let Some(v) = data.get("hidden_truth").and_then(Value::as_str) {
            w.set_hidden_truth(v);
        }
        if let Some(v) = data.get("hidden_events") {
            w.set_hidden_events(v);
        }
        payload::debug_data(rt, cfg, settings)
    })
    .await
}

async fn post_debug_scene(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    debug_mutate(&state, move |rt, cfg, settings| {
        let patch = match data.get("patch") {
            Some(Value::Object(_)) => data.get("patch").unwrap().clone(),
            _ => Value::Object(data.clone()),
        };
        rt.session.world.patch_scene(&patch);
        payload::debug_data(rt, cfg, settings)
    })
    .await
}

async fn post_debug_state_record(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    debug_mutate(&state, move |rt, cfg, settings| {
        let null = Value::Null;
        rt.session.world.apply_state_memory_record_batch(
            data.get("add").unwrap_or(&null),
            data.get("update").unwrap_or(&null),
            data.get("delete").unwrap_or(&null),
            data.get("hard_delete")
                .map(gml_orchestrator::truthy)
                .unwrap_or(false),
        );
        payload::debug_data(rt, cfg, settings)
    })
    .await
}

async fn post_debug_rumor(State(state): State<AppState>, body: Bytes) -> Response {
    let data = parse_body(&body);
    debug_mutate(&state, move |rt, cfg, settings| {
        let w = &mut rt.session.world;
        let action = data
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        match action.as_str() {
            "add" => {
                w.add_debug_rumor(
                    data.get("speaker").and_then(Value::as_str).unwrap_or(""),
                    data.get("text").and_then(Value::as_str).unwrap_or(""),
                    gml_orchestrator::session::RUMORS_CAP,
                );
            }
            "delete" => {
                w.remove_rumor(data.get("seq").unwrap_or(&Value::Null));
            }
            "confirm" => {
                let confirmed = data
                    .get("confirmed")
                    .map(gml_orchestrator::truthy)
                    .unwrap_or(true);
                w.set_rumor_confirmed(data.get("seq").unwrap_or(&Value::Null), confirmed);
            }
            _ => {}
        }
        payload::debug_data(rt, cfg, settings)
    })
    .await
}

// =========================================================================
// fallback (unknown route -> 404 / index for /index*)
// =========================================================================

async fn fallback_handler(
    State(state): State<AppState>,
    req: axum::http::Request<Body>,
) -> Response {
    let path = req.uri().path();
    if req.method() == axum::http::Method::GET && path.starts_with("/index") {
        let _ = active_chat(&state);
        return serve_index(&state);
    }
    not_found()
}

// =========================================================================
// small helpers
// =========================================================================

fn die_or_none(value: Option<&Value>) -> Option<i64> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.is_empty() => None,
        Some(v) => {
            let n = v
                .as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()));
            n.map(|x| x.max(1))
        }
    }
}

/// `_model_hint_for_new_chat(dialog)`.
fn model_hint_for_new_chat(rt: &DialogRuntime, cfg: &Config) -> String {
    let session = &rt.session;
    let matches = session.client_backend.is_empty() || session.client_backend == cfg.backend;
    if !matches {
        return String::new();
    }
    if !session.client_model.is_empty() {
        return session.client_model.clone();
    }
    session.client.model()
}

/// Build a seeded session (`_seeded_session`).
fn build_session(
    client: Arc<dyn Backend>,
    world: World,
    make_client: &MakeClient,
    cfg: &Config,
    model_hint: &str,
) -> Session {
    let mut session = Session::with_world(client.clone(), world, make_client.clone());
    session.compaction = CompactionThresholds::from_config(cfg);
    session.client_backend = cfg.backend.clone();
    let m = client.model();
    session.client_model = if !m.is_empty() {
        m
    } else {
        model_hint.to_string()
    };
    session.client_session_id = client.session_id();
    session.client_thread_id = client.thread_id();
    session
}

/// Build a story session (`_story_session`) — no live client work yet.
fn story_session(
    client: Arc<dyn Backend>,
    world: World,
    cfg: &Config,
    model_hint: &str,
) -> Session {
    let factory: gml_orchestrator::ClientFactory = Arc::new({
        let _ = client; // story session uses the default factory for NPCs
        || -> Arc<dyn Backend> { Arc::new(gml_llm::MockClient::new()) }
    });
    let mut session = Session::with_world(client_placeholder(), world, factory);
    session.compaction = CompactionThresholds::from_config(cfg);
    session.client_backend = cfg.backend.clone();
    session.client_model = model_hint.to_string();
    session
}

/// A placeholder client for a story session (Python passes `client=None`; we
/// keep a mock until `ensure_client` builds the real one on first use).
fn client_placeholder() -> Arc<dyn Backend> {
    Arc::new(gml_llm::MockClient::new())
}

/// `_list_models(client)`.
async fn list_models(client: &dyn Backend, cfg: &Config) -> Vec<Value> {
    let models = client.list_models().await;
    if !models.is_empty() {
        return models;
    }
    let model = {
        let m = client.model();
        if m.is_empty() {
            if cfg.model.is_empty() {
                "default".to_string()
            } else {
                cfg.model.clone()
            }
        } else {
            m
        }
    };
    vec![json!({"id": model, "name": model, "supported": true})]
}

/// Persist the active chat (DialogStore.save) after a mutating handler.
async fn persist_active(state: &AppState) {
    let scope = chat_scope_id();
    let chat_id = match state.store.active_chat_id(&scope) {
        Ok(Some(id)) => id,
        _ => return,
    };
    let store = state.store.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let _ = store.with_runtime(&scope, &chat_id, |rt| {
            let _ = store.save(rt);
        });
    })
    .await;
}

/// Collapse a `spawn_blocking` of a `Result<Value, StoreError>` into a Response.
fn join_json(
    res: Result<Result<Value, gml_persistence::StoreError>, tokio::task::JoinError>,
) -> Response {
    match res {
        Ok(Ok(v)) => ok_json(&v),
        Ok(Err(e)) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": e.to_string()}),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &json!({"ok": false, "error": format!("join error: {e}")}),
        ),
    }
}

// =========================================================================
// run_http / run_https
// =========================================================================

/// `run_http(addr, router)` — serve plain HTTP (Tauri loopback / `--server`
/// without TLS). Binds and serves until the process is killed.
pub async fn run_http(addr: std::net::SocketAddr, router: Router) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
}

/// `run_https(addr, router, cert_dir)` — serve the dual HTTP/HTTPS LAN listener
/// with the 1-byte TLS sniff (`0x16` -> TLS handshake; else 308-redirect to
/// https). Ports `tls_cert.py` + the `_SniffingHTTPSServer` peek logic.
pub async fn run_https(
    addr: std::net::SocketAddr,
    router: Router,
    cert_dir: &std::path::Path,
) -> std::io::Result<()> {
    use tokio_rustls::TlsAcceptor;

    let (cert_path, key_path) =
        tls::ensure_self_signed(cert_dir).map_err(|e| std::io::Error::other(e.to_string()))?;
    let certs = load_certs(&cert_path)?;
    let key = load_key(&key_path)?;
    let mut tls_config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls_config));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let make_service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let acceptor = acceptor.clone();
        let mut make_service = make_service.clone();
        tokio::spawn(async move {
            // Peek the first byte: 0x16 = TLS ClientHello.
            let mut first = [0u8; 1];
            let n = match stream.peek(&mut first).await {
                Ok(n) => n,
                Err(_) => return,
            };
            if n == 1 && first[0] == 0x16 {
                if let Ok(tls_stream) = acceptor.accept(stream).await {
                    serve_conn(tls_stream, &mut make_service, peer).await;
                }
            } else {
                // Plaintext on the TLS port -> 308 redirect to https. We read the
                // request to grab the Host header, then write a minimal 308.
                let _ = first; // consumed below by reading the request line.
                redirect_plaintext_to_https(stream).await;
            }
        });
    }
}

async fn serve_conn<S>(
    stream: S,
    make_service: &mut axum::extract::connect_info::IntoMakeServiceWithConnectInfo<
        Router,
        std::net::SocketAddr,
    >,
    peer: std::net::SocketAddr,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use tower::Service;

    let tower_service = match make_service.call(peer).await {
        Ok(s) => s,
        Err(_) => return,
    };
    let io = TokioIo::new(stream);
    let hyper_service = service_fn(move |req: axum::http::Request<hyper::body::Incoming>| {
        let mut svc = tower_service.clone();
        async move {
            let req = req.map(Body::new);
            svc.call(req).await
        }
    });
    let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
        .serve_connection_with_upgrades(io, hyper_service)
        .await;
}

/// Read just enough of a plaintext request to find the Host, then 308-redirect
/// to `https://host{path}`.
async fn redirect_plaintext_to_https<S>(mut stream: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.unwrap_or(0);
    let text = String::from_utf8_lossy(&buf[..n]);
    let mut path = "/".to_string();
    let mut host = String::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            // "GET /path HTTP/1.1"
            let mut parts = line.split_whitespace();
            let _method = parts.next();
            if let Some(p) = parts.next() {
                path = p.to_string();
            }
        } else if let Some(h) = line
            .strip_prefix("Host:")
            .or_else(|| line.strip_prefix("host:"))
        {
            host = h.trim().to_string();
        }
    }
    if host.is_empty() {
        return;
    }
    let location = format!("https://{host}{path}");
    let response = format!(
        "HTTP/1.1 308 Permanent Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

fn load_certs(
    path: &std::path::Path,
) -> std::io::Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    let pem = std::fs::read(path)?;
    let mut reader = std::io::BufReader::new(&pem[..]);
    rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>()
}

fn load_key(path: &std::path::Path) -> std::io::Result<rustls::pki_types::PrivateKeyDer<'static>> {
    let pem = std::fs::read(path)?;
    let mut reader = std::io::BufReader::new(&pem[..]);
    rustls_pemfile::private_key(&mut reader)?.ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "no private key in PEM")
    })
}
