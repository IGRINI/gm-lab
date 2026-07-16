//! Dispatch-level tests for the ADDITIVE canon GM tools (`move_player`,
//! `world_debug`, `generate_location`) wired in `turn.rs`.
//!
//! These drive the exact `run_tool` dispatch the turn loop uses (via
//! `run_tool_collect`), proving:
//!   - `move_player` commits a valid traversal through the validator-gated canon
//!     engine and reports the new current place;
//!   - a CONTRADICTORY `move_player` (unknown / not-here / hidden / blocked
//!     transition) is REJECTED and leaves the canon byte-for-byte unchanged — the
//!     §14 "the LLM cannot make a contradictory canon commit without the
//!     validator" acceptance point, proved at the tool-path level;
//!   - `world_debug` returns the canon + causal log and mutates nothing;
//!   - generated locations are drafted by a dedicated model context and committed
//!     into canon as places/memory.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use gml_llm::{
    Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
    SessionIdentity,
};
use gml_mock::MockClient;
use gml_orchestrator::{run_tool_collect, ClientFactory, Session};
use gml_stories::StoryStore;
use gml_world::{Npc, Place, Provenance, Transition, World};

fn factory() -> ClientFactory {
    Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>)
}

fn client() -> Arc<dyn Backend> {
    Arc::new(MockClient::new())
}

/// Default story seed from a HERMETIC store over a tempdir. There is no global
/// store; constructing a `StoryStore` materializes the builtins into the
/// throwaway directory, so these tests never touch the real user library.
fn default_story_seed() -> serde_json::Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = StoryStore::new(dir.path()).expect("open store");
    store.default_seed()
}

struct IdentityBackend {
    inner: MockClient,
    identity: SessionIdentity,
    scripted_json: Option<Map<String, Value>>,
    seen_json_messages: Option<Arc<Mutex<Vec<Value>>>>,
}

impl IdentityBackend {
    fn new(model: &str) -> Self {
        let inner = MockClient::new();
        inner.set_model(model);
        IdentityBackend {
            inner,
            identity: SessionIdentity::new(),
            scripted_json: None,
            seen_json_messages: None,
        }
    }

    fn with_scripted_json(model: &str, scripted_json: Map<String, Value>) -> Self {
        let inner = MockClient::new();
        inner.set_model(model);
        IdentityBackend {
            inner,
            identity: SessionIdentity::new(),
            scripted_json: Some(scripted_json),
            seen_json_messages: None,
        }
    }

    fn with_capture(model: &str, seen_json_messages: Arc<Mutex<Vec<Value>>>) -> Self {
        let inner = MockClient::new();
        inner.set_model(model);
        IdentityBackend {
            inner,
            identity: SessionIdentity::new(),
            scripted_json: None,
            seen_json_messages: Some(seen_json_messages),
        }
    }
}

#[async_trait]
impl Backend for IdentityBackend {
    fn model(&self) -> String {
        self.inner.model()
    }

    fn set_model(&self, model: &str) {
        self.inner.set_model(model);
    }

    fn set_session_identity(&self, session_id: Option<&str>, thread_id: Option<&str>) {
        self.identity.set(session_id, thread_id);
    }

    fn session_id(&self) -> String {
        self.identity.session_id()
    }

    fn thread_id(&self) -> String {
        self.identity.thread_id()
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
        if let Some(seen) = &self.seen_json_messages {
            seen.lock()
                .expect("seen_json_messages lock")
                .push(messages.clone());
        }
        if let Some(scripted_json) = &self.scripted_json {
            return Ok(scripted_json.clone());
        }
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

fn seeded_session() -> Session {
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
    Session::with_world(client(), world, factory())
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(fut)
}

/// The first visible, passable transition leaving the player's current canon
/// place — a genuinely valid `move_player` target.
fn a_valid_transition(session: &Session) -> String {
    let canon = &session.world.world_canon;
    let here = canon.player_place_id.clone();
    canon
        .exits_from(&here)
        .into_iter()
        .find(|t| t.visible && t.passable && t.blocked_by.is_empty())
        .map(|t| t.transition_id.clone())
        .expect("seeded start place must have at least one usable exit")
}

#[test]
fn move_player_commits_a_valid_traversal_through_the_canon() {
    let mut session = seeded_session();
    let start = session.world.world_canon.player_place_id.clone();
    let transition_id = a_valid_transition(&session);

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": transition_id, "reason": "иду через выход"}),
    ));

    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(
        payload["ok"],
        json!(true),
        "valid move must succeed: {payload}"
    );
    assert_eq!(payload["status"], json!("moved"));

    let new_place = session.world.world_canon.player_place_id.clone();
    assert_ne!(new_place, start, "player must have left the start place");
    assert_eq!(
        payload["place_id"].as_str().unwrap_or(""),
        new_place,
        "reported place must match canon player_place_id"
    );
    // At least the move_player event was committed to the canon log.
    assert!(payload["events"].as_i64().unwrap_or(0) >= 1);
}

#[test]
fn move_player_can_return_to_start_and_state_persists() {
    let mut session = seeded_session();
    let start = session.world.world_canon.player_place_id.clone();
    let out = a_valid_transition(&session);

    block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": out}),
    ));
    let arrived = session.world.world_canon.player_place_id.clone();
    assert_ne!(arrived, start);

    // Find the return edge back to start and take it.
    let back = session
        .world
        .world_canon
        .exits_from(&arrived)
        .into_iter()
        .find(|t| t.to_place == start && t.visible && t.passable)
        .map(|t| t.transition_id.clone())
        .expect("there must be a way back to the start place (TZ §7.4)");

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": back}),
    ));
    let payload: Value = serde_json::from_str(&result.full).unwrap();
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(
        session.world.world_canon.player_place_id, start,
        "player returned to the start place"
    );
}

#[test]
fn contradictory_move_player_is_rejected_and_canon_is_unchanged() {
    // §14: the LLM cannot make a contradictory canon commit without the validator.
    let mut session = seeded_session();
    let before = session.world.world_canon.clone();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": "transition_that_does_not_exist", "reason": "телепорт"}),
    ));

    // The tool result is a structured rejection: the model channel carries the
    // validator's code/message (`.full` is the human "(tool error: ...)" string).
    assert!(
        result.model.contains("code: unknown_transition"),
        "rejection code must come from the validator: {}",
        result.model
    );
    assert!(
        result.model.contains("ERROR"),
        "must be a structured tool error"
    );
    // An error event was emitted, never a scene update.
    assert!(
        events.iter().any(|e| e.kind == "error"),
        "a rejection must emit an error event"
    );
    assert!(
        !events.iter().any(|e| e.kind == "scene_update"),
        "a rejected move must NOT emit a scene update"
    );

    // The canon is byte-for-byte unchanged — the validator mutated nothing.
    assert_eq!(
        session.world.world_canon, before,
        "a rejected move_player must leave the canon completely unchanged"
    );
}

#[test]
fn move_player_missing_transition_id_is_a_clean_tool_error() {
    let mut session = seeded_session();
    let before = session.world.world_canon.clone();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"reason": "no id"}),
    ));
    assert!(
        result.model.contains("code: missing_transition_id"),
        "missing transition_id must be a clean tool error: {}",
        result.model
    );
    assert_eq!(session.world.world_canon, before, "no mutation on bad args");
}

#[test]
fn world_debug_returns_canon_and_causal_log_without_mutating() {
    let mut session = seeded_session();
    let before = session.world.world_canon.clone();

    let (_events, result) = block_on(run_tool_collect(&mut session, "world_debug", &json!({})));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true));
    assert!(
        payload.get("canon").is_some(),
        "full dump must include canon"
    );
    assert!(
        payload.get("causal_log").is_some(),
        "must include causal log"
    );

    // Read-only: nothing changed.
    assert_eq!(
        session.world.world_canon, before,
        "world_debug must not mutate canon"
    );
}

#[test]
fn world_debug_causal_log_only_omits_the_canon_dump() {
    let mut session = seeded_session();
    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "world_debug",
        &json!({"causal_log_only": true}),
    ));
    let payload: Value = serde_json::from_str(&result.full).unwrap();
    assert_eq!(payload["ok"], json!(true));
    assert!(
        payload.get("canon").is_none(),
        "causal_log_only must omit the canon dump"
    );
    assert!(payload.get("causal_log").is_some());
}

#[test]
fn generate_location_commits_place_memory_and_dedicated_client_state() {
    let mut session = seeded_session();
    let here = session.world.world_canon.player_place_id.clone();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_location",
        &json!({
            "purpose": "local_place",
            "request": "Сгенерируй маленький двор рядом с текущей сценой.",
            "parent_place_id": here,
            "commit": true,
        }),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["committed"], json!(true));
    assert_eq!(payload["generated"]["redacted"], json!(true));
    assert_eq!(payload["applied"]["redacted"], json!(true));
    let full = result.full.as_str();
    assert!(
        !full.contains("Караван ушёл не сам")
            && !full.contains("следы подков без гвоздей")
            && !full.contains("anti_repeat_key"),
        "UI/full payload must not expose hidden generator fields: {full}"
    );
    for event in events
        .iter()
        .filter(|event| event.kind == "world_state_update")
    {
        let event_text = event.data.to_string();
        assert!(
            !event_text.contains("Караван ушёл не сам")
                && !event_text.contains("следы подков без гвоздей")
                && !event_text.contains("anti_repeat_key"),
            "world_state_update must use the redacted public payload: {event_text}"
        );
    }
    let place_id = session
        .world
        .world_canon
        .places
        .values()
        .find(|place| place.name == "Дорожная остановка" && place.place_id != here)
        .map(|place| place.place_id.clone())
        .expect("generated place must be committed into canon");
    assert!(
        session.world.world_canon.memory.units.values().any(|m| {
            m.created_by == "location_generator"
                && m.place_ids.contains(&place_id)
                && !m.visibility_scopes.iter().any(|scope| scope == "player")
        }),
        "offscreen generated location must write scoped memory without player visibility"
    );
    assert!(
        session
            .world
            .world_canon
            .event_log
            .player_visible()
            .into_iter()
            .all(|event| event.kind != "generate_location" || event.place_id != place_id),
        "offscreen generated location event must not be globally player-visible"
    );
    assert!(
        !session.location_generator_client_state.model.is_empty(),
        "location generator keeps its own persisted client state"
    );
    assert!(
        !session.location_generator_anti_repeat.is_empty(),
        "anti-repeat keys are tracked across generator calls"
    );
}

#[test]
fn generate_location_enter_after_commit_moves_player_to_generated_place() {
    let mut session = seeded_session();
    let here = session.world.world_canon.player_place_id.clone();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_location",
        &json!({
            "purpose": "room",
            "request": "Игрок нашёл за лавкой скрытый двор и сразу вошёл внутрь.",
            "parent_place_id": here,
            "commit": true,
            "enter_after_commit": true,
        }),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["committed"], json!(true));
    assert_eq!(payload["applied"]["entered"], json!(true));

    let place_id = payload["applied"]["place_id"]
        .as_str()
        .expect("observed payload exposes generated place id")
        .to_string();
    assert_ne!(place_id, here, "generated place must be distinct");
    assert_eq!(
        session.world.world_canon.player_place_id, place_id,
        "enter_after_commit must move the canonical player location"
    );
    assert_eq!(
        session.world.scene.location_id, place_id,
        "live scene must be rebuilt from the generated current place"
    );
    assert_eq!(session.world.scene.title, "Дорожная остановка");
    assert_eq!(payload["generated"]["name"], json!("Дорожная остановка"));
    assert!(
        !result.full.contains("Караван ушёл не сам")
            && !result.full.contains("следы подков без гвоздей")
            && !result.full.contains("anti_repeat_key"),
        "observed payload may expose visible fields, not hidden generator fields: {}",
        result.full
    );
    assert!(
        session
            .world
            .world_canon
            .event_log
            .events
            .iter()
            .any(|event| {
                event.kind == "move_player"
                    && event
                        .effects
                        .iter()
                        .any(|effect| effect == &format!("player_at:{place_id}"))
            }),
        "enter_after_commit should commit a normal move_player event"
    );
    assert!(
        events.iter().any(|event| {
            event.kind == "scene_update"
                && event
                    .data
                    .get("location_id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| id == place_id)
        }),
        "tool stream should publish the generated current scene"
    );
    assert!(
        session
            .world
            .world_canon
            .memory
            .units
            .values()
            .any(|memory| {
                memory.created_by == "location_generator"
                    && memory.place_ids.contains(&place_id)
                    && memory
                        .visibility_scopes
                        .iter()
                        .any(|scope| scope == "player")
            }),
        "entered generated location should write player-visible scoped memory"
    );
}

#[test]
fn generate_location_skips_duplicate_generated_return_exit_to_parent() {
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
    let here = world.world_canon.player_place_id.clone();
    let scripted: Map<String, Value> = serde_json::from_value(json!({
        "name": "Сырой двор за рядами",
        "kind": "local_place",
        "visible_summary": "Сырой двор за тканевыми рядами.",
        "description": "Тесный рабочий двор за рядами, куда ведёт скрытый проход.",
        "hidden_summary": "",
        "features": ["мокрый брезент", "ящики", "кадка"],
        "sensory_details": ["пахнет сырой тканью"],
        "choices": ["вернуться назад"],
        "consequences": [],
        "hidden_clues": [],
        "knows_more": [],
        "transitions": [{
            "label": "Назад к рыночной площади",
            "destination_hint": format!("Возврат к [[loc:{here}|Рыночная площадь]]"),
            "kind": "back",
            "time_cost_minutes": 0,
            "risk": "none"
        }],
        "anti_repeat_key": "test-wet-cloth-yard",
        "memory_note": "Игрок нашёл сырой двор за рядами."
    }))
    .expect("scripted location is an object");
    let generator_factory: ClientFactory = Arc::new(move || {
        Arc::new(IdentityBackend::with_scripted_json(
            "return-duplicate-generator",
            scripted.clone(),
        )) as Arc<dyn Backend>
    });
    let mut session = Session::with_world(client(), world, generator_factory);

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_location",
        &json!({
            "purpose": "local_place",
            "request": "Игрок входит в скрытый двор за рядами ткани.",
            "parent_place_id": here,
            "commit": true,
            "enter_after_commit": true,
        }),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    let place_id = payload["applied"]["place_id"]
        .as_str()
        .expect("generated place id")
        .to_string();
    let exits = session.world.world_canon.exits_from(&place_id);
    let parent_edges = exits
        .iter()
        .filter(|transition| transition.to_place == here)
        .count();
    assert_eq!(
        parent_edges, 1,
        "the engine-created back edge is enough; generator return hooks must not duplicate it"
    );
    assert!(
        exits.iter().all(|transition| {
            transition.to_place == here
                || !transition.label.to_lowercase().contains("назад")
                || !transition.to_place.is_empty()
        }),
        "duplicate generated return exits must not become lazy unknown destinations: {exits:?}"
    );
}

#[test]
fn generate_location_history_survives_payload_and_reaches_next_generator_call() {
    let seen_messages: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let generator_factory: ClientFactory = {
        let seen_messages = seen_messages.clone();
        Arc::new(move || {
            Arc::new(IdentityBackend::with_capture(
                "history-generator",
                seen_messages.clone(),
            )) as Arc<dyn Backend>
        })
    };
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
    let mut session = Session::with_world(client(), world, generator_factory.clone());

    block_on(run_tool_collect(
        &mut session,
        "generate_location",
        &json!({
            "purpose": "local_place",
            "request": "Сгенерируй первый двор с приметными следами.",
            "commit": false,
        }),
    ));
    assert_eq!(
        session.location_generator_messages.len(),
        2,
        "one request/result exchange is stored"
    );

    let payload = session.to_payload();
    let mut restored = Session::from_payload(&payload, client(), generator_factory)
        .expect("session payload restores");
    assert_eq!(
        restored.location_generator_messages, session.location_generator_messages,
        "location generator history is persisted"
    );

    block_on(run_tool_collect(
        &mut restored,
        "generate_location",
        &json!({
            "purpose": "local_place",
            "request": "Сгенерируй второй двор, но не повторяй первый.",
            "commit": false,
        }),
    ));

    let calls = seen_messages.lock().expect("seen messages lock");
    assert!(
        calls.len() >= 2,
        "expected two generator chat_json calls, got {}",
        calls.len()
    );
    let second_call = serde_json::to_string(calls.last().expect("second call messages")).unwrap();
    assert!(
        second_call.contains("PREVIOUS LOCATION GENERATION REQUEST")
            && second_call.contains("PREVIOUS LOCATION GENERATION RESULT"),
        "second generator call must include persisted history: {second_call}"
    );
    assert!(
        second_call.contains("road-stop-abandoned-cart-tracks"),
        "anti-repeat/motif from the prior generated result must reach the next call: {second_call}"
    );
}

#[test]
fn generate_location_rejects_unknown_purpose_before_calling_generator() {
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
    let generator_factory: ClientFactory =
        Arc::new(|| Arc::new(IdentityBackend::new("unused-generator")) as Arc<dyn Backend>);
    let mut session = Session::with_world(client(), world, generator_factory);
    let before = session.world.world_canon.clone();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_location",
        &json!({
            "purpose": "unsupported_place_kind",
            "request": "Это не должно дойти до генератора.",
            "commit": true,
        }),
    ));

    assert!(
        result.model.contains("code: unsupported_generator_purpose"),
        "unknown purpose must be rejected before generation: {}",
        result.model
    );
    assert_eq!(
        session.world.world_canon, before,
        "bad generator purpose must not mutate canon"
    );
    assert!(
        session.location_generator_client_state.model.is_empty(),
        "generator client should not be created for rejected purpose"
    );
}

#[test]
fn generate_location_rejection_is_validator_gated_and_atomic() {
    let scripted: Map<String, Value> = serde_json::from_value(json!({
        "name": "Сломанный тестовый тупик",
        "kind": "local_place",
        "visible_summary": "Узкий проход с неверно заданным временем пути.",
        "description": "Короткий тестовый проход рядом с текущим местом.",
        "hidden_summary": "",
        "features": ["узкий проход"],
        "sensory_details": [],
        "choices": ["осмотреть проход"],
        "consequences": [],
        "hidden_clues": [],
        "knows_more": [],
        "transitions": [{
            "label": "В тестовый тупик",
            "destination_hint": "тестовый тупик",
            "kind": "path",
            "time_cost_minutes": -5,
            "risk": "test"
        }],
        "anti_repeat_key": "invalid-negative-time",
        "memory_note": "Тестовая локация не должна попасть в канон."
    }))
    .expect("scripted location is an object");
    let generator_factory: ClientFactory = Arc::new(move || {
        Arc::new(IdentityBackend::with_scripted_json(
            "bad-generator",
            scripted.clone(),
        )) as Arc<dyn Backend>
    });
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
    let mut session = Session::with_world(client(), world, generator_factory);
    let here = session.world.world_canon.player_place_id.clone();
    let before = session.world.world_canon.clone();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_location",
        &json!({
            "purpose": "local_place",
            "request": "Сгенерируй тестовый проход с невалидным временем.",
            "parent_place_id": here,
            "commit": true,
        }),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(false), "payload: {payload}");
    assert_eq!(payload["committed"], json!(false), "payload: {payload}");
    assert_eq!(payload["applied"]["status"], json!("rejected"));
    assert_eq!(payload["applied"]["code"], json!("negative_travel_time"));
    assert_eq!(
        session.world.world_canon, before,
        "rejected generation must not partially commit the generated place/event/transition"
    );
    assert!(
        events.iter().any(|event| event.kind == "error"),
        "rejected generation must emit an error event"
    );
    assert!(
        !events.iter().any(|event| event.kind == "scene_update"),
        "rejected generation must not refresh the scene"
    );
}

#[test]
fn set_model_for_all_clients_updates_location_generator_state_and_cache_identity() {
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260622);
    let generator_factory: ClientFactory =
        Arc::new(|| Arc::new(IdentityBackend::new("generator-model")) as Arc<dyn Backend>);
    let mut session = Session::with_world(
        Arc::new(IdentityBackend::new("gm-model")) as Arc<dyn Backend>,
        world,
        generator_factory,
    );
    let generator = session.ensure_location_generator_client();
    session.remember_location_generator_client();

    let session_id = generator.session_id();
    let thread_id = generator.thread_id();
    assert!(
        !session.location_generator_client_state.model.is_empty(),
        "location generator state is initialized"
    );
    assert_eq!(
        session.location_generator_client_state.session_id,
        session_id
    );
    assert_eq!(session.location_generator_client_state.thread_id, thread_id);

    session.set_model_for_all_clients("new-live-model");

    assert_eq!(generator.model(), "new-live-model");
    assert_eq!(
        session.location_generator_client_state.model, "new-live-model",
        "future restored location generator calls must not keep the old model"
    );

    session.location_generator_client = None;
    let restored = session.ensure_location_generator_client();
    assert_eq!(
        restored.session_id(),
        session_id,
        "location generator session id must survive restore for prompt-cache continuity"
    );
    assert_eq!(
        restored.thread_id(),
        thread_id,
        "location generator thread id must survive restore for prompt-cache continuity"
    );
}

#[test]
fn move_player_auto_generates_long_road_situation_content() {
    let mut session = seeded_session();
    let from = session.world.world_canon.player_place_id.clone();
    let destination = "far_watchtower".to_string();
    session.world.world_canon.insert_place(Place {
        place_id: destination.clone(),
        name: "Дальняя башня".to_string(),
        kind: "site".to_string(),
        provenance: Provenance::by("test", "road destination", 0),
        ..Default::default()
    });
    let transition_id = "test_long_road".to_string();
    session.world.world_canon.insert_transition(Transition {
        transition_id: transition_id.clone(),
        source_exit_id: transition_id.clone(),
        from_place: from,
        to_place: destination.clone(),
        destination_hint: "дальняя башня".to_string(),
        label: "По старой дороге".to_string(),
        kind: "road".to_string(),
        visible: true,
        passable: true,
        time_cost: 48 * 60,
        risk: "certain wild road: test-only guaranteed situation".to_string(),
        provenance: Provenance::by("test", "long road", 0),
        ..Default::default()
    });

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": transition_id, "reason": "долгий путь"}),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true));
    assert!(
        payload["generated_situation"]["applied"]["place_id"]
            .as_str()
            .is_some(),
        "road interruption must be filled by the location generator: {payload}"
    );
    let current = session.world.world_canon.player_place_id.clone();
    assert_ne!(
        current, destination,
        "the situation interrupts before arrival"
    );
    let place = session.world.world_canon.place(&current).unwrap();
    assert_eq!(place.name, "Дорожная остановка");
    assert!(
        session
            .world
            .world_canon
            .memory
            .units
            .values()
            .any(|m| m.created_by == "location_generator" && m.place_ids.contains(&current)),
        "generated road situation must write location memory"
    );
}

#[test]
fn canon_tools_are_available_in_the_canon_catalog() {
    // The historical base catalog remains separate; canon tools are appended by
    // the model-facing builder/catalog path.
    let static_tools = gml_agents::build_gm_tools();
    assert_eq!(static_tools.len(), 11, "base catalog remains separate");
    let static_names: Vec<String> = static_tools
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(!static_names.iter().any(|n| n == "move_player"));
    assert!(!static_names.iter().any(|n| n == "world_debug"));

    // The new tools live in the separate additive builder, appended at the end.
    // The core additive set is fixed; `long_rest` is a later deferred addition by
    // the canon builder, so tolerate (but do not require) its presence rather than
    // pinning an exact length that the parallel owner would break.
    let core = [
        "move_player",
        "world_debug",
        "generate_location",
        "take_item",
        "drop_item",
        "cast_spell",
        "generate_npc",
        "read_state",
    ];
    let canon_tools = gml_agents::build_canon_gm_tools();
    let canon_names: Vec<String> = canon_tools
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap_or("").to_string())
        .collect();
    // The core set leads the list, in order.
    assert_eq!(
        canon_names.iter().take(core.len()).collect::<Vec<_>>(),
        core.iter().collect::<Vec<_>>(),
        "core canon tools lead build_canon_gm_tools in order"
    );
    let const_names = gml_agents::CANON_GM_TOOL_NAMES.to_vec();
    assert_eq!(
        const_names.iter().take(core.len()).collect::<Vec<_>>(),
        core.iter().collect::<Vec<_>>(),
        "core canon tool names lead CANON_GM_TOOL_NAMES in order"
    );
    // Any trailing entry beyond the core must be a recognized later addition.
    for extra in canon_names.iter().skip(core.len()) {
        assert_eq!(extra, "long_rest", "unexpected extra canon tool: {extra}");
    }
}

// --- read_state dispatch (GM_CONTEXT_TZ §4) --------------------------------

#[test]
fn read_state_renders_requested_sections_from_live_world_without_mutating() {
    let mut session = seeded_session();
    let before = session.world.world_canon.clone();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "read_state",
        &json!({"sections": ["time", "scene", "player", "roster", "facts"]}),
    ));

    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true));
    // The GM-facing model channel carries the rendered blocks for each section.
    let text = payload["text"].as_str().unwrap_or("");
    for heading in [
        "## TIME",
        "## SCENE",
        "## PLAYER",
        "## ROSTER (full)",
        "## PUBLIC FACTS",
    ] {
        assert!(
            text.contains(heading),
            "missing section heading {heading}: {text}"
        );
    }
    assert!(
        result.model.contains("## TIME"),
        "model channel must carry the rendered state: {}",
        result.model
    );
    // Pure read: no canon mutation, and no events beyond the standard tool result.
    assert_eq!(
        session.world.world_canon, before,
        "read_state must not mutate canon"
    );
    assert!(
        !events
            .iter()
            .any(|e| e.kind == "scene_update" || e.kind == "error"),
        "read_state must emit no scene_update / error events"
    );
}

#[test]
fn read_state_roster_section_is_the_full_roster() {
    let mut session = seeded_session();
    let full = session.world.full_roster_context();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "read_state",
        &json!({"sections": ["roster"]}),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    let text = payload["text"].as_str().unwrap_or("");
    assert!(text.contains("## ROSTER (full)"));
    assert!(
        text.contains(full.trim()) || full == "(none)",
        "roster section must render the FULL roster"
    );
}

#[test]
fn read_state_empty_or_invalid_sections_error_and_list_the_valid_ones() {
    let mut session = seeded_session();
    let before = session.world.world_canon.clone();

    // Empty sections list.
    let (_e1, empty) = block_on(run_tool_collect(
        &mut session,
        "read_state",
        &json!({"sections": []}),
    ));
    assert!(
        empty.model.contains("time")
            && empty.model.contains("scene")
            && empty.model.contains("roster"),
        "empty read_state must list the valid sections: {}",
        empty.model
    );

    // Only-invalid section names.
    let (_e2, bad) = block_on(run_tool_collect(
        &mut session,
        "read_state",
        &json!({"sections": ["nonsense", "weather"]}),
    ));
    assert!(
        bad.model.contains("ERROR") || bad.full.contains("invalid_sections"),
        "invalid-only read_state must be a structured error: {} / {}",
        bad.model,
        bad.full
    );
    assert_eq!(
        session.world.world_canon, before,
        "an invalid read_state must not mutate canon"
    );
}

// --- generate_npc dispatch (NPC_GEN_DESIGN §6) -----------------------------

/// Serializes the env-mutating dedup tests in this binary: `npc_dedup_report`
/// reads process-global `GM_NPC_DEDUP_*` / `GM_RAG_RERANK_URL`, so two tests
/// setting them concurrently would race.
static DEDUP_ENV_LOCK: Mutex<()> = Mutex::new(());

/// The canned mock secret the generator returns for the "GM-Lab NPC generator"
/// marker — must NEVER surface in a tool result or SSE event.
const CANNED_SECRET: &str = "Прячет письмо пропавшего смотрителя под стойкой.";

/// A minimal inline HTTP stub for the `/rerank` endpoint returning a FIXED
/// relevance_score for index 0 — enough to drive the dedup gate deterministically
/// without a live sidecar (mirrors the `TcpListener` stub in gml-rag/tests).
struct RerankStub {
    url: String,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl RerankStub {
    fn start(score: f64) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind rerank stub");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{addr}/rerank");
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let _ = sock.set_nonblocking(false);
                        let _ = sock.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                        // Drain the WHOLE request (headers + body) before replying,
                        // else closing with unread bytes triggers a TCP RST the
                        // client sees as a send error. Read until a timeout/EOF.
                        let mut tmp = [0u8; 2048];
                        loop {
                            match sock.read(&mut tmp) {
                                Ok(0) => break,
                                Ok(_) => continue,
                                Err(_) => break,
                            }
                        }
                        let body = format!(
                            "{{\"results\":[{{\"index\":0,\"relevance_score\":{score}}}]}}"
                        );
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = sock.write_all(resp.as_bytes());
                        let _ = sock.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });
        RerankStub {
            url,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for RerankStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Happy path: the canned mock NPC commits a full canon card + actor + GM-only
/// memory, and the tool result / SSE are REDACTED (no secret / mechanics).
#[test]
fn generate_npc_commits_card_actor_and_redacts_secret() {
    let _guard = DEDUP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Disable dedup so the happy path is deterministic regardless of a sidecar.
    std::env::set_var("GM_NPC_DEDUP_ENABLED", "0");

    let mut session = seeded_session();
    let here = session.world.world_canon.player_place_id.clone();
    let roster_before = session.world.npcs.len();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({
            "request": "Игрок заговаривает с хозяином таверны, который явно что-то скрывает.",
            "role": "бармен",
            "present": true,
        }),
    ));
    std::env::remove_var("GM_NPC_DEDUP_ENABLED");

    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true), "payload: {payload}");
    assert_eq!(payload["committed"], json!(true));
    assert_eq!(payload["npc"]["name"], json!("Тихон Ржавый"));
    let npc_id = payload["npc"]["npc_id"]
        .as_str()
        .expect("committed npc id")
        .to_string();

    // Card exists with the required pronouns for TTS.
    assert_eq!(session.world.npcs.len(), roster_before + 1);
    let card = session.world.npcs.get(&npc_id).expect("card inserted");
    assert_eq!(card.name, "Тихон Ржавый");
    assert_eq!(card.pronouns, "М");
    assert_eq!(card.secret, CANNED_SECRET, "secret lives on the card");

    // Canon actor present at the player's place.
    let actor = session
        .world
        .world_canon
        .actors
        .get(&npc_id)
        .expect("canon actor created");
    assert!(actor.is_at(&here), "actor placed at the player place");
    assert!(
        session.world.scene.present_npcs.contains(&npc_id),
        "generated NPC is present in the live scene"
    );
    assert!(
        session
            .world
            .extra_proper_nouns
            .contains(&"Тихон Ржавый".to_string()),
        "generated name registered as a proper noun"
    );

    // GM-only actor-scoped memory holds the secret with NO player visibility.
    assert!(
        session.world.world_canon.memory.units.values().any(|unit| {
            unit.created_by == "character_generator"
                && unit.details.contains(CANNED_SECRET)
                && unit.visibility_scopes.is_empty()
        }),
        "secret must be stored in a GM-only actor memory unit"
    );

    // REDACTION: neither the tool result nor any SSE event may leak the secret or
    // the mechanics block.
    assert!(
        !result.full.contains(CANNED_SECRET)
            && !result.full.contains("mechanics")
            && !result.full.contains("Прячет письмо"),
        "tool result must be redacted: {}",
        result.full
    );
    for event in events.iter().filter(|e| e.kind == "world_state_update") {
        let text = event.data.to_string();
        assert!(
            !text.contains(CANNED_SECRET) && !text.contains("mechanics"),
            "world_state_update must use the redacted payload: {text}"
        );
    }
    assert!(
        events.iter().any(|e| e.kind == "scene_update"),
        "a committed NPC publishes a scene update"
    );
    assert!(
        !session.character_generator_client_state.model.is_empty(),
        "character generator keeps its own persisted client state"
    );
    assert!(
        !session.character_generator_anti_repeat.is_empty(),
        "anti-repeat keys are tracked across generator calls"
    );
}

/// A missing `request` / `role` is rejected BEFORE any generator client is
/// created (the client state stays empty).
#[test]
fn generate_npc_rejects_missing_request_or_role_before_client() {
    let _guard = DEDUP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("GM_NPC_DEDUP_ENABLED", "0");
    let mut session = seeded_session();
    let before = session.world.world_canon.clone();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({ "request": "  ", "role": "" }),
    ));
    std::env::remove_var("GM_NPC_DEDUP_ENABLED");

    assert!(
        result.model.contains("code: missing_generator_request"),
        "missing request/role must be rejected: {}",
        result.model
    );
    assert_eq!(
        session.world.world_canon, before,
        "a rejected generate_npc must not mutate canon"
    );
    assert!(
        session.character_generator_client_state.model.is_empty(),
        "generator client must not be created for a rejected request"
    );
}

/// The per-turn budget gate fires before any generation when the turn already hit
/// `gen_budget.max_npcs_per_turn`.
#[test]
fn generate_npc_budget_exhaustion_rejects() {
    let _guard = DEDUP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("GM_NPC_DEDUP_ENABLED", "0");
    let mut session = seeded_session();
    session.world.world_canon.gen_budget.max_npcs_per_turn = 0;
    let before = session.world.npcs.len();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({ "request": "Нужен ещё один персонаж.", "role": "стражник" }),
    ));
    std::env::remove_var("GM_NPC_DEDUP_ENABLED");

    assert!(
        result.model.contains("code: npc_budget_exhausted"),
        "budget exhaustion must be reported: {}",
        result.model
    );
    assert_eq!(
        session.world.npcs.len(),
        before,
        "budget-exhausted generate_npc commits no card"
    );
    assert!(
        session.character_generator_client_state.model.is_empty(),
        "generator client must not be created when the budget is exhausted"
    );
}

/// With dedup disabled the gate is SKIPPED (status `disabled`) and generation
/// proceeds.
#[test]
fn generate_npc_dedup_disabled_skips_gate() {
    let _guard = DEDUP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("GM_NPC_DEDUP_ENABLED", "0");
    let mut session = seeded_session();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({ "request": "Игрок обращается к бармену.", "role": "бармен" }),
    ));
    std::env::remove_var("GM_NPC_DEDUP_ENABLED");

    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true), "payload: {payload}");
    assert_eq!(payload["dedup"]["reason"], json!("disabled"));
    assert_eq!(payload["dedup"]["enabled"], json!(false));
}

/// An unroutable reranker DEGRADES the gate (status `rerank_error`, degraded) and
/// generation still proceeds — the gate never blocks the turn.
#[test]
fn generate_npc_dedup_degraded_proceeds() {
    let _guard = DEDUP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("GM_NPC_DEDUP_ENABLED", "1");
    // Port 9 (discard) refuses/drops fast → transport error → degrade.
    std::env::set_var("GM_RAG_RERANK_URL", "http://127.0.0.1:9/rerank");
    std::env::set_var("GM_RAG_TIMEOUT_SECONDS", "0.3");
    let mut session = seeded_session();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({ "request": "Игрок обращается к бармену.", "role": "бармен" }),
    ));
    for k in [
        "GM_NPC_DEDUP_ENABLED",
        "GM_RAG_RERANK_URL",
        "GM_RAG_TIMEOUT_SECONDS",
    ] {
        std::env::remove_var(k);
    }

    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(
        payload["ok"],
        json!(true),
        "degraded gate must not block: {payload}"
    );
    assert_eq!(payload["dedup"]["degraded"], json!(true));
    assert_eq!(payload["dedup"]["reason"], json!("rerank_error"));
}

/// A high-scoring reranker match fires the gate → `duplicate_candidates` (no
/// commit), and resending with `retry=true` bypasses it and generates.
#[test]
fn generate_npc_duplicate_candidates_then_retry_bypasses() {
    let _guard = DEDUP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let stub = RerankStub::start(0.97);
    std::env::set_var("GM_NPC_DEDUP_ENABLED", "1");
    std::env::set_var("GM_NPC_DEDUP_THRESHOLD", "0.5");
    std::env::set_var("GM_RAG_RERANK_URL", &stub.url);
    std::env::set_var("GM_RAG_TIMEOUT_SECONDS", "2");
    let mut session = seeded_session();
    let roster_before = session.world.npcs.len();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({ "request": "Ещё один страж у ворот.", "role": "стражник" }),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(
        payload["status"],
        json!("duplicate_candidates"),
        "payload: {payload}"
    );
    assert_eq!(payload["ok"], json!(false));
    assert!(
        payload["candidates"]
            .as_array()
            .is_some_and(|c| !c.is_empty()),
        "duplicate gate lists existing candidates: {payload}"
    );
    assert_eq!(
        session.world.npcs.len(),
        roster_before,
        "a fired dedup gate commits nothing"
    );

    // retry=true bypasses the gate and generates the canned NPC.
    let (_events2, result2) = block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({
            "request": "Ещё один страж у ворот; предложенные кандидаты не подходят — этот из другой фракции.",
            "role": "стражник",
            "retry": true,
        }),
    ));
    for k in [
        "GM_NPC_DEDUP_ENABLED",
        "GM_NPC_DEDUP_THRESHOLD",
        "GM_RAG_RERANK_URL",
        "GM_RAG_TIMEOUT_SECONDS",
    ] {
        std::env::remove_var(k);
    }
    drop(stub);

    let payload2: Value = serde_json::from_str(&result2.full).expect("full is JSON");
    assert_eq!(
        payload2["ok"],
        json!(true),
        "retry must bypass the gate: {payload2}"
    );
    assert_eq!(payload2["committed"], json!(true));
    assert_eq!(payload2["dedup"]["reason"], json!("retry_forced"));
    assert_eq!(
        session.world.npcs.len(),
        roster_before + 1,
        "retry commits exactly one new card"
    );
}

/// Review finding (major): `present=false` with NO distinct `place_id` used to
/// land the NPC in the current scene anyway. It must stay OFF-scene: card+actor
/// exist, but the actor is not at the player place and not in present_npcs.
#[test]
fn generate_npc_present_false_without_place_stays_offscene() {
    let _guard = DEDUP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("GM_NPC_DEDUP_ENABLED", "0");

    let mut session = seeded_session();
    let here = session.world.world_canon.player_place_id.clone();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({
            "request": "Скрытный человек, который следит за игроком, но пока не в сцене.",
            "role": "соглядатай",
            "present": false,
        }),
    ));
    std::env::remove_var("GM_NPC_DEDUP_ENABLED");

    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true), "payload: {payload}");
    assert_eq!(
        payload["npc"]["present"],
        json!(false),
        "reported presence must reflect the derived scene: {payload}"
    );
    let npc_id = payload["npc"]["npc_id"]
        .as_str()
        .expect("npc id")
        .to_string();

    let actor = session
        .world
        .world_canon
        .actors
        .get(&npc_id)
        .expect("canon actor created");
    assert!(
        !actor.is_at(&here),
        "present=false must not place the actor at the player place"
    );
    assert!(
        !session.world.scene.present_npcs.contains(&npc_id),
        "present=false must keep the NPC out of present_npcs"
    );
    assert!(
        session.world.npcs.contains_key(&npc_id),
        "the card still exists for a later entrance"
    );
}

/// `retry=true` on a FRESH request (no prior `duplicate_candidates` this session)
/// must NOT bypass the gate: the duplicate check still runs and still fires.
/// Guards against a confused GM stamping duplicates by preemptively sending retry.
#[test]
fn generate_npc_retry_without_prior_duplicate_is_ignored() {
    let _guard = DEDUP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let stub = RerankStub::start(0.97);
    std::env::set_var("GM_NPC_DEDUP_ENABLED", "1");
    std::env::set_var("GM_NPC_DEDUP_THRESHOLD", "0.5");
    std::env::set_var("GM_RAG_RERANK_URL", &stub.url);
    std::env::set_var("GM_RAG_TIMEOUT_SECONDS", "2");
    let mut session = seeded_session();
    let roster_before = session.world.npcs.len();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({
            "request": "Ещё один страж у ворот.",
            "role": "стражник",
            "retry": true,
        }),
    ));
    for k in [
        "GM_NPC_DEDUP_ENABLED",
        "GM_NPC_DEDUP_THRESHOLD",
        "GM_RAG_RERANK_URL",
        "GM_RAG_TIMEOUT_SECONDS",
    ] {
        std::env::remove_var(k);
    }
    drop(stub);

    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(
        payload["status"],
        json!("duplicate_candidates"),
        "unarmed retry must not bypass the gate: {payload}"
    );
    assert_eq!(payload["ok"], json!(false));
    assert_ne!(payload["dedup"]["reason"], json!("retry_forced"));
    assert_eq!(
        session.world.npcs.len(),
        roster_before,
        "unarmed retry commits nothing"
    );
}

/// The character generator's rolling history / anti-repeat / client identity
/// survive a to_payload -> from_payload round-trip.
#[test]
fn generate_npc_generator_state_round_trips_through_payload() {
    let _guard = DEDUP_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("GM_NPC_DEDUP_ENABLED", "0");
    let mut session = seeded_session();

    block_on(run_tool_collect(
        &mut session,
        "generate_npc",
        &json!({ "request": "Игрок обращается к бармену.", "role": "бармен" }),
    ));
    std::env::remove_var("GM_NPC_DEDUP_ENABLED");

    assert_eq!(
        session.character_generator_messages.len(),
        2,
        "one request/result exchange is stored"
    );
    assert!(!session.character_generator_anti_repeat.is_empty());
    assert!(!session.character_generator_client_state.model.is_empty());

    let payload = session.to_payload();
    let restored =
        Session::from_payload(&payload, client(), factory()).expect("session payload restores");
    assert_eq!(
        restored.character_generator_messages, session.character_generator_messages,
        "character generator history is persisted"
    );
    assert_eq!(
        restored.character_generator_anti_repeat, session.character_generator_anti_repeat,
        "character generator anti-repeat ring is persisted"
    );
    assert_eq!(
        restored.character_generator_client_state.model,
        session.character_generator_client_state.model,
        "character generator client identity is persisted"
    );
    assert_eq!(
        restored.character_generator_client_state.thread_id,
        session.character_generator_client_state.thread_id,
    );
}

// --- §И3 take_item / drop_item dispatch ------------------------------------

/// An item-rich seed: a VISIBLE portable coin, a VISIBLE non-portable statue,
/// and an INVISIBLE portable key (only takeable by item_id).
fn item_scene_seed() -> serde_json::Value {
    json!({
        "id": "item-scene",
        "title": "Комната с предметами",
        "public_intro": "Пыльная комната.",
        "hidden_truth": "За гобеленом дверь.",
        "npcs": [{"id": "warden", "name": "Смотритель", "persona": "страж", "role": "warden"}],
        "scene": {
            "id": "vault_scene",
            "location_id": "vault",
            "title": "Хранилище",
            "description": "Каменное хранилище с сундуками.",
            "present_npcs": ["warden"],
            "items": [
                {"id": "coin", "name": "Медная монета", "location": "на полу",
                 "portable": true, "details": "потёртая"},
                {"id": "statue", "name": "Статуя", "location": "в нише"},
                {"id": "vault_key", "name": "Ключ", "location": "в замке",
                 "portable": true, "visible": false}
            ],
            "exits": [
                {"id": "door", "name": "Дверь", "destination": "corridor"}
            ]
        }
    })
}

fn item_session() -> Session {
    let world = World::from_seed_with_dice_seed(&item_scene_seed(), 20260622);
    Session::with_world(client(), world, factory())
}

#[test]
fn take_item_moves_scene_item_into_card_and_emits_updates() {
    let mut session = item_session();
    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "take_item",
        &json!({"item_id": "coin", "reason": "беру монету"}),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["status"], json!("taken"));
    assert_eq!(
        payload["inventory_entry"],
        json!("Медная монета — потёртая")
    );
    // The scene item is gone; the card carries the entry.
    assert!(!session
        .world
        .scene
        .items
        .iter()
        .any(|i| i.item_id == "coin"));
    assert!(session
        .world
        .player_character
        .inventory
        .iter()
        .any(|e| e == "Медная монета — потёртая"));
    // Both a card update AND a scene update are emitted; NO canon event written.
    assert!(events.iter().any(|e| e.kind == "player_character_update"));
    assert!(events.iter().any(|e| e.kind == "scene_update"));
    assert!(
        !session
            .world
            .world_canon
            .event_log
            .events
            .iter()
            .any(|e| e.kind == "take_item"),
        "§0: take_item must NOT write a canon event"
    );
}

#[test]
fn take_item_ambiguous_is_a_clean_tool_error_that_takes_nothing() {
    let mut session = item_session();
    // Add a second visible coin so a name match is ambiguous.
    session.world.scene.items.push(gml_world::SceneItem {
        item_id: "coin2".to_string(),
        name: "Медная монета".to_string(),
        location: "на столе".to_string(),
        visible: true,
        portable: true,
        owner: String::new(),
        details: String::new(),
    });
    let before = session.world.scene.items.len();
    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "take_item",
        &json!({"name": "Медная монета"}),
    ));
    assert!(
        result.model.contains("code: ambiguous_item"),
        "ambiguity must surface the validator-style code: {}",
        result.model
    );
    assert!(events.iter().any(|e| e.kind == "error"));
    assert!(!events.iter().any(|e| e.kind == "scene_update"));
    assert_eq!(session.world.scene.items.len(), before, "nothing removed");
}

#[test]
fn take_item_non_portable_is_rejected_as_fiction() {
    let mut session = item_session();
    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "take_item",
        &json!({"item_id": "statue"}),
    ));
    assert!(
        result.model.contains("code: not_portable"),
        "non-portable take must be a clean rejection: {}",
        result.model
    );
    assert!(events.iter().any(|e| e.kind == "error"));
    assert!(session
        .world
        .scene
        .items
        .iter()
        .any(|i| i.item_id == "statue"));
}

#[test]
fn take_item_invisible_only_by_id() {
    let mut session = item_session();
    // By name: invisible key is not a candidate.
    let (_events, by_name) = block_on(run_tool_collect(
        &mut session,
        "take_item",
        &json!({"name": "Ключ"}),
    ));
    assert!(
        by_name.model.contains("code: item_not_here"),
        "{}",
        by_name.model
    );
    // By id: GM-trusted path succeeds.
    let (_events, by_id) = block_on(run_tool_collect(
        &mut session,
        "take_item",
        &json!({"item_id": "vault_key"}),
    ));
    let payload: Value = serde_json::from_str(&by_id.full).unwrap();
    assert_eq!(payload["ok"], json!(true));
    assert!(!session
        .world
        .scene
        .items
        .iter()
        .any(|i| i.item_id == "vault_key"));
}

#[test]
fn drop_item_puts_inventory_entry_back_into_the_scene() {
    let mut session = item_session();
    session
        .world
        .player_character
        .inventory
        .push("Верёвка — 15 метров".to_string());
    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "drop_item",
        &json!({"name": "Верёвка", "location": "у двери", "reason": "оставляю"}),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["status"], json!("dropped"));
    // Removed from card, inserted into the scene as a visible portable item.
    assert!(!session
        .world
        .player_character
        .inventory
        .iter()
        .any(|e| e.starts_with("Верёвка")));
    let dropped = session
        .world
        .scene
        .items
        .iter()
        .find(|i| i.name == "Верёвка")
        .expect("dropped item is in the scene");
    assert_eq!(dropped.details, "15 метров");
    assert!(dropped.visible && dropped.portable);
    assert_eq!(dropped.location, "у двери");
    assert!(events.iter().any(|e| e.kind == "player_character_update"));
    assert!(events.iter().any(|e| e.kind == "scene_update"));
}

#[test]
fn drop_item_unknown_is_a_clean_tool_error() {
    let mut session = item_session();
    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "drop_item",
        &json!({"name": "чего-нет"}),
    ));
    assert!(
        result.model.contains("code: unknown_item"),
        "dropping an item the player does not carry must be a clean error: {}",
        result.model
    );
    assert!(events.iter().any(|e| e.kind == "error"));
    assert!(!events.iter().any(|e| e.kind == "scene_update"));
}

#[test]
fn take_then_move_does_not_leak_and_drop_lands_in_the_new_place() {
    // End-to-end §И2 leak fix at the dispatch level: take a coin, move to a new
    // place — the coin does not travel — then drop it there; it stays there
    // across a round-trip out and back.
    let mut session = item_session();
    block_on(run_tool_collect(
        &mut session,
        "take_item",
        &json!({"item_id": "coin"}),
    ));
    let start = session.world.world_canon.player_place_id.clone();
    let out = a_valid_transition(&session);
    block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": out}),
    ));
    let arrived = session.world.world_canon.player_place_id.clone();
    assert_ne!(arrived, start);
    // The start place's non-taken items did not travel here.
    assert!(
        !session
            .world
            .scene
            .items
            .iter()
            .any(|i| i.item_id == "statue"),
        "scene items must not leak across a move"
    );
    // Drop the coin in the new place.
    block_on(run_tool_collect(
        &mut session,
        "drop_item",
        &json!({"name": "Медная монета", "location": "на камне"}),
    ));
    assert!(session
        .world
        .scene
        .items
        .iter()
        .any(|i| i.name == "Медная монета"));
}

// --- §С2 cast_spell dispatch -----------------------------------------------

/// A caster session: one level-1 concentration spell, one free level-1 slot.
fn caster_session() -> Session {
    let seed = json!({
        "id": "caster-scene",
        "title": "Кабинет мага",
        "public_intro": "Тесная башня.",
        "hidden_truth": "Скрытый круг.",
        "npcs": [],
        "player": {
            "name": "Аэлин",
            "spells": [
                {"name": "Огненная хватка", "level": 1, "concentration": true,
                 "ritual": false, "effect": "конц.; 2d6 огнём"}
            ],
            "spell_slots": {"1": 1},
            "spell_slots_max": {"1": 2},
            "concentration": ""
        },
        "scene": {
            "id": "tower_scene",
            "location_id": "tower",
            "title": "Башня",
            "description": "Свитки и реторты.",
            "present_npcs": [],
            "items": [],
            "exits": []
        }
    });
    let world = World::from_seed_with_dice_seed(&seed, 20260622);
    Session::with_world(client(), world, factory())
}

#[test]
fn cast_spell_spends_slot_sets_concentration_and_emits_card_update_only() {
    let mut session = caster_session();
    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "cast_spell",
        &json!({"name": "огненная хватка", "reason": "кастую"}),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["slot_spent_level"], json!(1));
    assert_eq!(payload["concentration_started"], json!("Огненная хватка"));
    // Engine state mutated: slot decremented, concentration held.
    assert_eq!(
        session.world.player_character.spell_slots.get("1"),
        Some(&json!(0))
    );
    assert_eq!(
        session.world.player_character.concentration,
        "Огненная хватка"
    );
    // A card update is emitted; NO scene update (the scene is untouched) and NO
    // canon event (§0).
    assert!(events.iter().any(|e| e.kind == "player_character_update"));
    assert!(!events.iter().any(|e| e.kind == "scene_update"));
    assert!(
        !session
            .world
            .world_canon
            .event_log
            .events
            .iter()
            .any(|e| e.kind == "cast_spell"),
        "§0: cast_spell must NOT write a canon event"
    );
}

#[test]
fn cast_spell_unknown_is_a_clean_tool_error_with_known_hint() {
    let mut session = caster_session();
    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "cast_spell",
        &json!({"name": "метеор"}),
    ));
    assert!(
        result.model.contains("code: unknown_spell"),
        "an unknown spell must surface the validator-style code: {}",
        result.model
    );
    assert!(events.iter().any(|e| e.kind == "error"));
    assert!(!events.iter().any(|e| e.kind == "player_character_update"));
}

#[test]
fn cast_spell_no_slots_is_rejected_as_fiction() {
    let mut session = caster_session();
    // Spend the only level-1 slot, then try again -> no_slots.
    block_on(run_tool_collect(
        &mut session,
        "cast_spell",
        &json!({"name": "огненная хватка"}),
    ));
    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "cast_spell",
        &json!({"name": "огненная хватка"}),
    ));
    assert!(
        result.model.contains("code: no_slots"),
        "a slotless cast must be a clean rejection: {}",
        result.model
    );
    assert!(events.iter().any(|e| e.kind == "error"));
}

// --- NPC card snapshot-once (GM_CONTEXT_TZ §7) -----------------------------

fn card_test_npc(id: &str, revision: i64) -> Npc {
    serde_json::from_value(json!({
        "npc_id": id,
        "name": format!("{id}_internal"),
        "persona": "p",
        "voice": "v",
        "goals": "g",
        "knowledge": "k",
        "secret": "s",
        "role": "роль",
        "card_revision": revision,
    }))
    .expect("npc from json")
}

#[test]
fn ensure_npc_card_injected_puts_the_card_at_history_head_once() {
    let mut session = seeded_session();
    session
        .world
        .npcs
        .insert("borin".to_string(), card_test_npc("borin", 0));

    // First contact: card becomes history[0]; marker records the revision.
    session.ensure_npc_card_injected("borin");
    let history = session.npc_messages.get("borin").expect("history");
    assert_eq!(history.len(), 1);
    assert!(gml_agents::is_npc_card_message(&history[0]));
    assert_eq!(session.npc_injected_card_revision.get("borin"), Some(&0));

    // Idempotent: a second call with the same revision adds nothing.
    session.ensure_npc_card_injected("borin");
    assert_eq!(session.npc_messages.get("borin").unwrap().len(), 1);
}

#[test]
fn ensure_npc_card_injected_appends_exactly_one_update_on_revision_bump() {
    let mut session = seeded_session();
    session
        .world
        .npcs
        .insert("borin".to_string(), card_test_npc("borin", 0));
    session.ensure_npc_card_injected("borin");

    // A durable card edit bumps card_revision; the next contact appends ONE
    // NPC CARD UPDATED notice (append-only), and never a second time.
    session.world.npcs.get_mut("borin").unwrap().card_revision = 1;
    session.ensure_npc_card_injected("borin");
    session.ensure_npc_card_injected("borin");

    let history = session.npc_messages.get("borin").unwrap();
    assert_eq!(history.len(), 2, "one card + one update, no duplicate");
    assert!(gml_agents::is_npc_card_message(&history[0]));
    assert!(gml_agents::is_npc_card_message(&history[1]));
    let last = history[1]
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("");
    assert!(last.starts_with(gml_agents::NPC_CARD_UPDATE_HEADER));
    assert_eq!(session.npc_injected_card_revision.get("borin"), Some(&1));
}

#[test]
fn ensure_npc_card_injected_migrates_a_legacy_cardless_history() {
    let mut session = seeded_session();
    session
        .world
        .npcs
        .insert("borin".to_string(), card_test_npc("borin", 0));
    // Legacy history: prior exchanges but no card message.
    session.npc_messages.insert(
        "borin".to_string(),
        vec![
            json!({"role": "user", "content": "CURRENT SITUATION (what's happening now, what you react to): привет"}),
            json!({"role": "assistant", "content": "Здравствуй."}),
        ],
    );

    session.ensure_npc_card_injected("borin");
    let history = session.npc_messages.get("borin").unwrap();
    assert_eq!(history.len(), 3, "card inserted, exchanges kept");
    assert!(
        gml_agents::is_npc_card_message(&history[0]),
        "legacy migration puts the card at history[0]"
    );
}

// --- long_rest dispatch ----------------------------------------------------

/// A rested-down caster: 8/12 HP missing, both level-1 slots spent, one level-2
/// slot spent, actively concentrating — so long_rest has something to restore in
/// every field.
fn rest_session() -> Session {
    let seed = json!({
        "id": "rest-scene",
        "title": "Ночной лагерь",
        "public_intro": "Костёр догорает.",
        "hidden_truth": "—",
        "npcs": [],
        "player": {
            "name": "Аэлин",
            "hp": {"current": 3, "max": 12},
            "spells": [
                {"name": "Огненная хватка", "level": 1, "concentration": true,
                 "ritual": false, "effect": "конц.; 2d6 огнём"}
            ],
            "spell_slots": {"1": 0, "2": 1},
            "spell_slots_max": {"1": 2, "2": 2},
            "concentration": "Огненная хватка"
        },
        "scene": {
            "id": "camp_scene",
            "location_id": "camp",
            "title": "Лагерь",
            "description": "Тлеющие угли и одеяла.",
            "present_npcs": [],
            "items": [],
            "exits": []
        }
    });
    let world = World::from_seed_with_dice_seed(&seed, 20260622);
    Session::with_world(client(), world, factory())
}

#[test]
fn long_rest_restores_slots_hp_concentration_and_advances_eight_hours() {
    let mut session = rest_session();
    let start_minutes = session.world.time.absolute_minutes;

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "long_rest",
        &json!({"reason": "разбили лагерь на ночь"}),
    ));

    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["status"], json!("long_rest"));
    assert_eq!(payload["elapsed_minutes"], json!(480));
    assert_eq!(payload["concentration_dropped"], json!(true));

    // Slots restored EXACTLY to max; hp.current == hp.max; concentration cleared.
    let pc = &session.world.player_character;
    assert_eq!(pc.spell_slots, pc.spell_slots_max, "slots back to full");
    assert_eq!(pc.spell_slots.get("1"), Some(&json!(2)));
    assert_eq!(pc.spell_slots.get("2"), Some(&json!(2)));
    assert_eq!(pc.hp.get("current"), pc.hp.get("max"));
    assert_eq!(pc.hp.get("current"), Some(&json!(12)));
    assert_eq!(pc.concentration, "");

    // The clock advanced by 8h through the shared advance_time mechanics.
    assert_eq!(
        session.world.time.absolute_minutes,
        start_minutes + 480,
        "long rest advances the game clock by 480 minutes"
    );

    // Exactly one PLAYER_CHARACTER_UPDATE + one TIME event; no dice, no error.
    assert!(
        events.iter().any(|e| e.kind == "player_character_update"),
        "long_rest emits a card update"
    );
    assert!(
        events.iter().any(|e| e.kind == "time"),
        "long_rest emits the advance-time event"
    );
    assert!(!events.iter().any(|e| e.kind == "dice" || e.kind == "error"));

    // The result text carries the new-time line (as advance_time renders it).
    assert!(
        result.model.contains(&session.world.time_summary()),
        "result must carry the new time line: {}",
        result.model
    );
    assert!(
        result.model.contains("восстановлено"),
        "result must summarize the restoration: {}",
        result.model
    );
}

#[test]
fn long_rest_reason_optional_but_empty_is_fine() {
    let mut session = rest_session();
    let (_events, result) = block_on(run_tool_collect(&mut session, "long_rest", &json!({})));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(
        payload["ok"],
        json!(true),
        "empty reason is accepted: {payload}"
    );
    assert_eq!(payload["elapsed_minutes"], json!(480));
}

#[test]
fn no_slots_then_long_rest_lets_the_spell_be_cast_again() {
    let mut session = caster_session();
    // Spend the only free level-1 slot, then confirm a second cast has no slots.
    block_on(run_tool_collect(
        &mut session,
        "cast_spell",
        &json!({"name": "огненная хватка"}),
    ));
    let (_events, no_slots) = block_on(run_tool_collect(
        &mut session,
        "cast_spell",
        &json!({"name": "огненная хватка"}),
    ));
    assert!(
        no_slots.model.contains("code: no_slots"),
        "second cast must fizzle: {}",
        no_slots.model
    );

    // A long rest refills the slots; the same spell casts again.
    block_on(run_tool_collect(&mut session, "long_rest", &json!({})));
    assert_eq!(
        session.world.player_character.spell_slots.get("1"),
        Some(&json!(2))
    );
    let (_events, after) = block_on(run_tool_collect(
        &mut session,
        "cast_spell",
        &json!({"name": "огненная хватка"}),
    ));
    let payload: Value = serde_json::from_str(&after.full).expect("full is JSON");
    assert_eq!(
        payload["ok"],
        json!(true),
        "cast works after long rest: {payload}"
    );
    assert_eq!(payload["slot_spent_level"], json!(1));
}

// --- unknown tool hint -----------------------------------------------------

#[test]
fn unknown_tool_error_hints_at_tool_search() {
    let mut session = seeded_session();
    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "definitely_not_a_tool",
        &json!({}),
    ));
    let payload: Value = serde_json::from_str(&result.full)
        .ok()
        .unwrap_or_else(|| json!({}));
    // The human channel and/or the structured code both name the miss and the fix.
    assert!(
        result.model.contains("code: unknown_tool"),
        "must surface the unknown_tool code: {}",
        result.model
    );
    assert!(
        result.full.contains("tool not loaded") && result.full.contains("tool_search"),
        "unknown-tool error must hint at tool_search: {} / {:?}",
        result.full,
        payload
    );
}

// --- staleness maps persist across the save/restore seam -------------------

#[test]
fn tool_staleness_maps_round_trip_through_payload() {
    let mut session = seeded_session();
    // Executing any tool records a last-used turn; loading a deferred schema
    // records a loaded turn.
    block_on(run_tool_collect(
        &mut session,
        "advance_time",
        &json!({"minutes": 5, "reason": "ждём"}),
    ));
    block_on(run_tool_collect(
        &mut session,
        "load_tool_schema",
        &json!({"name": "move_npc"}),
    ));
    assert!(
        session.tool_last_used.contains_key("advance_time"),
        "executed tool is recorded in tool_last_used"
    );
    assert!(
        session.tool_loaded_turn.contains_key("move_npc"),
        "loaded schema is recorded in tool_loaded_turn"
    );
    assert!(
        session.loaded_gm_tools.contains("move_npc"),
        "loading a schema admits the tool into the session set"
    );
    // A loaded schema need not have either staleness signal yet. Its membership
    // is still exact runtime state and must survive a checkpoint round-trip.
    session.loaded_gm_tools.insert("world_debug".to_string());

    let payload = session.to_payload();
    let restored =
        Session::from_payload(&payload, client(), factory()).expect("session payload restores");
    assert_eq!(
        restored.tool_last_used, session.tool_last_used,
        "tool_last_used survives the round-trip"
    );
    assert_eq!(
        restored.tool_loaded_turn, session.tool_loaded_turn,
        "tool_loaded_turn survives the round-trip"
    );
    assert_eq!(
        restored.loaded_gm_tools, session.loaded_gm_tools,
        "the exact loaded-tool set survives the round-trip"
    );

    // A legacy payload with staleness maps reconstructs a deterministic safe
    // set: initial tools plus valid names evidenced by those maps.
    let mut legacy_with_maps = payload.clone();
    legacy_with_maps
        .as_object_mut()
        .unwrap()
        .remove("loaded_gm_tools");
    let restored_legacy_with_maps =
        Session::from_payload(&legacy_with_maps, client(), factory()).expect("legacy restores");
    assert!(restored_legacy_with_maps
        .loaded_gm_tools
        .is_superset(&gml_agents::initial_gm_tool_names(false)));
    assert!(restored_legacy_with_maps
        .loaded_gm_tools
        .contains("move_npc"));
    assert!(!restored_legacy_with_maps
        .loaded_gm_tools
        .contains("world_debug"));

    // A fully legacy payload has empty staleness maps and the safe initial set.
    let mut legacy = payload.clone();
    legacy.as_object_mut().unwrap().remove("loaded_gm_tools");
    legacy.as_object_mut().unwrap().remove("tool_last_used");
    legacy.as_object_mut().unwrap().remove("tool_loaded_turn");
    let legacy_session =
        Session::from_payload(&legacy, client(), factory()).expect("legacy payload restores");
    assert!(legacy_session.tool_last_used.is_empty());
    assert!(legacy_session.tool_loaded_turn.is_empty());
    assert_eq!(
        legacy_session.loaded_gm_tools,
        gml_agents::initial_gm_tool_names(false)
    );
}
