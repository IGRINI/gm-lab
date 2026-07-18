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
use gml_world::{Npc, PassageDirectionality, Place, Provenance, Transition, World};

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
        self.inner.chat_json(messages, think, reasoning_role).await
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
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        self.inner
            .chat_json_stream(messages, think, reasoning_role, sink)
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
fn move_player_completes_an_unresolved_exit_through_the_location_creator() {
    let mut session = seeded_session();
    let start = session.world.world_canon.player_place_id.clone();
    let transition_id = a_valid_transition(&session);
    let route = session
        .world
        .world_canon
        .transitions
        .get_mut(&transition_id)
        .expect("selected transition exists");
    route.kind = "path".to_string();
    route.time_cost = 9;
    route.risk = "medium".to_string();
    route.passage_id = "passage:creator-authored-exit".to_string();
    route.directionality = PassageDirectionality::Bidirectional;
    route.provenance = Provenance::by("location_generator", "creator-authored exit", 0);
    let before_minutes = session.world.time.absolute_minutes;

    let (events, result) = block_on(run_tool_collect(
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
    assert_eq!(payload["status"], json!("generated"));
    assert_eq!(payload["applied"]["entered"], json!(true));
    assert_eq!(
        payload["request"]["creator_established_entry_profile"],
        json!({
            "kind": "path",
            "time_cost_minutes": 9,
            "risk": "medium",
            "passage_id": "passage:creator-authored-exit",
            "directionality": "bidirectional"
        }),
        "a valid unresolved profile authored by the location creator remains explicit context"
    );

    let new_place = session.world.world_canon.player_place_id.clone();
    assert_ne!(new_place, start, "player must have left the start place");
    assert_eq!(
        payload["applied"]["place_id"].as_str().unwrap_or(""),
        new_place,
        "reported place must match canon player_place_id"
    );
    let elapsed_minutes = payload["applied"]["elapsed_minutes"]
        .as_i64()
        .expect("creator-backed entry reports elapsed minutes");
    assert!(elapsed_minutes > 0);
    assert!(session
        .world
        .world_canon
        .event_log
        .events
        .iter()
        .any(|event| event.kind == "move_player" && event.place_id == new_place));
    assert_eq!(
        session.world.time.absolute_minutes,
        before_minutes + elapsed_minutes
    );
    assert_eq!(
        session.world.time.absolute_minutes, session.world.world_canon.clock_minutes,
        "ordinary movement must keep the visible and canonical clocks aligned"
    );
    assert!(events.iter().any(|event| {
        event.kind == "time" && event.data["elapsed_minutes"] == json!(elapsed_minutes)
    }));
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

    let forward = session
        .world
        .world_canon
        .transition(&out)
        .expect("forward transition remains in canon");
    assert_eq!(forward.directionality, PassageDirectionality::Bidirectional);
    assert!(!forward.passage_id.is_empty());
    let forward_passage_id = forward.passage_id.clone();

    // Find the return edge back to start and take it.
    let back = session
        .world
        .world_canon
        .exits_from(&arrived)
        .into_iter()
        .find(|t| t.to_place == start && t.visible && t.passable)
        .map(|t| t.transition_id.clone())
        .expect("there must be a way back to the start place (TZ §7.4)");
    let return_transition = session
        .world
        .world_canon
        .transition(&back)
        .expect("return transition remains in canon");
    assert_eq!(
        return_transition.directionality,
        PassageDirectionality::Bidirectional
    );
    assert_eq!(return_transition.passage_id, forward_passage_id);

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
fn one_way_drop_has_no_reverse_and_remains_reusable_from_its_source() {
    let scripted: Map<String, Value> = serde_json::from_value(json!({
        "name": "Дно обрыва",
        "kind": "dungeon_point",
        "visible_summary": "Каменное дно под отвесным уступом.",
        "description": "Обратно на уступ отсюда не подняться тем же путём.",
        "features": ["отвесный уступ"],
        "choices": ["искать другой выход"],
        "entry_transition": {
            "label": "Прыгнуть в обрыв",
            "directionality": "one_way",
            "kind": "drop",
            "time_cost_minutes": 1,
            "risk": "high"
        },
        "transitions": [],
        "anti_repeat_key": "test-one-way-chasm",
        "memory_note": "Спуск в обрыв односторонний."
    }))
    .expect("scripted location is an object");
    let generator_factory: ClientFactory = Arc::new(move || {
        Arc::new(IdentityBackend::with_scripted_json(
            "one-way-location-generator",
            scripted.clone(),
        )) as Arc<dyn Backend>
    });
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260717);
    let mut session = Session::with_world(client(), world, generator_factory);
    let cave_id = session.world.world_canon.player_place_id.clone();

    let (_events, generated) = block_on(run_tool_collect(
        &mut session,
        "generate_location",
        &json!({
            "purpose": "dungeon_point",
            "request": "Игрок прыгает с уступа в обрыв; этот физический спуск необратим.",
            "commit": true,
            "player_observed": true,
            "enter_after_commit": true,
        }),
    ));
    let payload: Value = serde_json::from_str(&generated.full).expect("generation result JSON");
    assert_eq!(payload["ok"], true, "{payload}");
    let fall_id = payload["applied"]["entry_transition_id"]
        .as_str()
        .expect("fall transition id")
        .to_string();
    let chasm_id = session.world.world_canon.player_place_id.clone();
    assert_ne!(chasm_id, cave_id);

    let fall = session
        .world
        .world_canon
        .transition(&fall_id)
        .expect("fall transition")
        .clone();
    assert_eq!(fall.from_place, cave_id);
    assert_eq!(fall.to_place, chasm_id);
    assert_eq!(fall.directionality, PassageDirectionality::OneWay);
    assert!(!fall.passage_id.is_empty());
    assert!(session
        .world
        .world_canon
        .exits_from(&fall.to_place)
        .into_iter()
        .all(|transition| transition.passage_id != fall.passage_id));

    let before_wrong_side = session.world.world_canon.clone();
    let (_events, rejected) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": fall_id}),
    ));
    assert!(
        rejected.model.contains("code: not_here"),
        "{}",
        rejected.model
    );
    assert_eq!(session.world.world_canon, before_wrong_side);

    let climb_id = "separate_chasm_climb".to_string();
    session.world.world_canon.insert_transition(Transition {
        transition_id: climb_id.clone(),
        source_exit_id: climb_id.clone(),
        passage_id: "separate_chasm_climb_passage".to_string(),
        directionality: PassageDirectionality::OneWay,
        from_place: chasm_id.clone(),
        to_place: cave_id.clone(),
        label: "Выбраться долгим обходом".to_string(),
        kind: "climb".to_string(),
        visible: true,
        passable: true,
        time_cost: 18,
        risk: "medium".to_string(),
        provenance: Provenance::by("test", "separate return route", 0),
        ..Default::default()
    });
    let (_events, climbed) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": climb_id}),
    ));
    assert_eq!(
        serde_json::from_str::<Value>(&climbed.full).unwrap()["ok"],
        true
    );
    assert_eq!(session.world.world_canon.player_place_id, cave_id);

    let (_events, repeated) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": fall_id}),
    ));
    assert_eq!(
        serde_json::from_str::<Value>(&repeated.full).unwrap()["ok"],
        true
    );
    assert_eq!(session.world.world_canon.player_place_id, chasm_id);
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
fn generated_entry_uses_current_place_and_syncs_symmetric_travel_time() {
    let mut session = seeded_session();
    let current_place_id = session.world.world_canon.player_place_id.clone();
    let logical_parent_id = "turnvale".to_string();
    session.world.world_canon.insert_place(Place {
        place_id: logical_parent_id.clone(),
        name: "Тёрнвейл".to_string(),
        kind: "settlement".to_string(),
        ..Default::default()
    });
    session.world.scene.scene_id = "market_backyard_scene".to_string();
    session.world.scene.constraints = vec!["Старое ограничение".to_string()];
    session.world.scene.tension = "Старое напряжение".to_string();
    session.world.scene.player_seen = vec!["Старое наблюдение".to_string()];
    let before_minutes = session.world.time.absolute_minutes;

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "generate_location",
        &json!({
            "purpose": "city_point",
            "request": "Игрок идёт два переулка к новой лавке и входит туда сейчас.",
            "parent_place_id": logical_parent_id,
            "commit": true,
            "enter_after_commit": true,
        }),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true), "payload: {payload}");
    assert_eq!(payload["applied"]["entered"], json!(true));
    assert_eq!(payload["applied"]["entry_time_minutes"], json!(7));
    assert_eq!(payload["applied"]["elapsed_minutes"], json!(7));

    let place_id = payload["applied"]["place_id"]
        .as_str()
        .expect("generated place id");
    let place = session
        .world
        .world_canon
        .place(place_id)
        .expect("generated place committed");
    assert_eq!(place.parent, "turnvale", "logical parent is preserved");

    let entry_transition_id = payload["applied"]["entry_transition_id"]
        .as_str()
        .expect("entry transition id");
    let entry = session
        .world
        .world_canon
        .transition(entry_transition_id)
        .expect("entry transition committed");
    assert_eq!(entry.from_place, current_place_id);
    assert_eq!(entry.to_place, place_id);
    assert_eq!(entry.time_cost, 7);
    let back = session
        .world
        .world_canon
        .exits_from(place_id)
        .into_iter()
        .find(|transition| transition.to_place == current_place_id)
        .expect("symmetric return transition");
    assert_eq!(back.time_cost, entry.time_cost);
    assert_eq!(back.risk, entry.risk);
    let back_transition_id = back.transition_id.clone();

    assert_eq!(session.world.time.absolute_minutes, before_minutes + 7);
    assert_eq!(
        session.world.time.absolute_minutes,
        session.world.world_canon.clock_minutes
    );
    assert!(events.iter().all(|event| event.kind != "error"));
    assert!(events
        .iter()
        .any(|event| { event.kind == "time" && event.data["elapsed_minutes"] == json!(7) }));
    assert_eq!(session.world.scene.scene_id, place_id);
    assert!(session.world.scene.constraints.is_empty());
    assert!(session.world.scene.tension.is_empty());
    assert!(session.world.scene.player_seen.is_empty());

    let (_events, return_result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": back_transition_id, "reason": "возвращаюсь"}),
    ));
    let return_payload: Value =
        serde_json::from_str(&return_result.full).expect("return result is JSON");
    assert_eq!(return_payload["elapsed_minutes"], json!(7));
    assert_eq!(session.world.scene.scene_id, "market_backyard_scene");
    assert_eq!(
        session.world.scene.constraints,
        vec!["Старое ограничение".to_string()]
    );
    assert_eq!(session.world.scene.tension, "Старое напряжение");
    assert_eq!(
        session.world.scene.player_seen,
        vec!["Старое наблюдение".to_string()]
    );
}

#[test]
fn move_player_profiles_a_legacy_route_without_guessing_its_reciprocal() {
    let mut session = seeded_session();
    let from_place = session.world.world_canon.player_place_id.clone();
    let to_place = "legacy_shop".to_string();
    session.world.world_canon.insert_place(Place {
        place_id: to_place.clone(),
        name: "Старая лавка".to_string(),
        kind: "shop".to_string(),
        provenance: Provenance::by("test", "legacy destination", 0),
        ..Default::default()
    });
    let forward_id = "legacy_shop_forward".to_string();
    let return_id = "legacy_shop_return".to_string();
    session.world.world_canon.insert_transition(Transition {
        transition_id: forward_id.clone(),
        source_exit_id: forward_id.clone(),
        from_place: from_place.clone(),
        to_place: to_place.clone(),
        label: "К лавке".to_string(),
        kind: "path".to_string(),
        visible: true,
        passable: true,
        time_cost: 4,
        risk: "low".to_string(),
        provenance: Provenance::by("test", "legacy forward", 0),
        ..Default::default()
    });
    session.world.world_canon.insert_transition(Transition {
        transition_id: return_id.clone(),
        source_exit_id: return_id.clone(),
        from_place: to_place.clone(),
        to_place: from_place,
        label: "К исходной локации".to_string(),
        kind: "door".to_string(),
        visible: true,
        passable: true,
        time_cost: 1,
        risk: "none".to_string(),
        provenance: Provenance::by("test", "legacy return", 0),
        ..Default::default()
    });

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": forward_id, "reason": "вхожу в лавку"}),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload["ok"], json!(true), "payload: {payload}");
    assert_eq!(payload["applied"]["entered"], json!(true));
    assert!(
        payload["request"]
            .get("creator_established_entry_profile")
            .is_none(),
        "an asymmetric legacy profile must not constrain the location creator: {payload}"
    );

    let forward = session
        .world
        .world_canon
        .transition("legacy_shop_forward")
        .expect("forward transition");
    assert_eq!(forward.directionality, PassageDirectionality::Bidirectional);
    assert!(!forward.passage_id.is_empty());
    assert_eq!(forward.time_cost, 7);

    let explicit_reciprocal = session
        .world
        .world_canon
        .exits_from(&to_place)
        .into_iter()
        .find(|candidate| {
            candidate.transition_id != return_id
                && candidate.to_place == forward.from_place
                && candidate.passage_id == forward.passage_id
        })
        .expect("creator adds the explicit reciprocal for the new passage identity");
    assert_eq!(explicit_reciprocal.kind, forward.kind);
    assert_eq!(explicit_reciprocal.time_cost, forward.time_cost);
    assert_eq!(explicit_reciprocal.risk, forward.risk);
    assert_eq!(
        explicit_reciprocal.directionality,
        PassageDirectionality::Bidirectional
    );

    let unrelated_legacy_reverse = session
        .world
        .world_canon
        .transition(&return_id)
        .expect("legacy reverse remains in canon");
    assert_eq!(unrelated_legacy_reverse.kind, "door");
    assert_eq!(unrelated_legacy_reverse.time_cost, 1);
    assert!(unrelated_legacy_reverse.passage_id.is_empty());
    assert_eq!(
        unrelated_legacy_reverse.directionality,
        PassageDirectionality::Unspecified
    );
}

#[test]
fn generate_location_leaves_invalid_route_rejection_to_the_canon_validator() {
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
        "entry_transition": {
            "label": "Войти во двор",
            "return_label": "Вернуться на площадь",
            "directionality": "bidirectional",
            "kind": "passage",
            "time_cost_minutes": 2,
            "risk": "none"
        },
        "transitions": [{
            "label": "Назад к рыночной площади",
            "destination_hint": format!("Возврат к [[loc:{here}|Рыночная площадь]]"),
            "directionality": "one_way",
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
    let before = session.world.world_canon.clone();

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
    assert_eq!(payload["applied"]["code"], "invalid_transition_profile");
    assert_eq!(session.world.world_canon, before);
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
        "entry_transition": {
            "label": "В тестовый проход",
            "return_label": "Вернуться",
            "directionality": "bidirectional",
            "kind": "path",
            "time_cost_minutes": 2,
            "risk": "none"
        },
        "transitions": [{
            "label": "В тестовый тупик",
            "destination_hint": "тестовый тупик",
            "directionality": "one_way",
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
    assert_eq!(payload["applied"]["code"], "invalid_transition_profile");
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
fn move_player_fills_a_contentless_destination_with_one_scene_update() {
    let mut session = seeded_session();
    let from = session.world.world_canon.player_place_id.clone();
    let destination = "raw_cell".to_string();
    session.world.world_canon.insert_place(Place {
        place_id: destination.clone(),
        name: "Сырая келья".to_string(),
        kind: "room".to_string(),
        provenance: Provenance::by("test", "lazy destination", 0),
        ..Default::default()
    });
    let transition_id = "follow_tracks".to_string();
    session.world.world_canon.insert_transition(Transition {
        transition_id: transition_id.clone(),
        source_exit_id: transition_id.clone(),
        passage_id: "follow_tracks_passage".to_string(),
        directionality: PassageDirectionality::OneWay,
        from_place: from,
        to_place: destination.clone(),
        destination_hint: "сырая келья".to_string(),
        label: "Дальше по следу".to_string(),
        kind: "passage".to_string(),
        visible: true,
        passable: true,
        time_cost: 2,
        risk: "low".to_string(),
        provenance: Provenance::by("test", "short passage", 0),
        ..Default::default()
    });

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": transition_id, "reason": "иду дальше по следу"}),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");

    assert_eq!(payload["ok"], json!(true));
    assert_eq!(session.world.world_canon.player_place_id, destination);
    assert!(
        payload["generated_destination"]["applied"]["place_id"]
            .as_str()
            .is_some(),
        "the contentless destination must be filled: {payload}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "scene_update")
            .count(),
        1,
        "one move must publish exactly one final scene update: {events:?}"
    );
}

#[test]
fn move_player_resolves_a_shell_through_the_location_creator_before_entry() {
    let mut session = seeded_session();
    let from = session.world.world_canon.player_place_id.clone();
    let destination = "sealed_crypt".to_string();
    session.world.world_canon.insert_place(Place {
        place_id: destination.clone(),
        name: "Запечатанная крипта".to_string(),
        kind: "dungeon".to_string(),
        state_flags: ["shell".to_string()].into_iter().collect(),
        provenance: Provenance::by("test", "unresolved shell", 0),
        ..Default::default()
    });
    let transition_id = "enter_sealed_crypt".to_string();
    session.world.world_canon.insert_transition(Transition {
        transition_id: transition_id.clone(),
        source_exit_id: transition_id.clone(),
        from_place: from.clone(),
        to_place: destination.clone(),
        destination_hint: "запечатанная крипта".to_string(),
        label: "Войти в крипту".to_string(),
        kind: "stairs".to_string(),
        visible: true,
        passable: true,
        time_cost: 2,
        risk: "low".to_string(),
        provenance: Provenance::by("test", "known shell entrance", 0),
        ..Default::default()
    });

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": transition_id, "reason": "спускаюсь в крипту"}),
    ));
    let payload: Value = serde_json::from_str(&result.full).expect("full is JSON");

    assert_eq!(payload["ok"], json!(true), "{payload}");
    assert_eq!(payload["applied"]["place_id"], destination);
    assert_eq!(session.world.world_canon.player_place_id, destination);
    let place = session
        .world
        .world_canon
        .place(&destination)
        .expect("creator completed the existing shell");
    assert!(!place.has_flag("shell"));
    assert!(place.has_flag("generated"));
    assert!(
        !session
            .world
            .world_canon
            .places
            .values()
            .any(|candidate| candidate.parent == destination),
        "the canon engine must not synthesize hard-coded interior rooms"
    );
    let forward = session
        .world
        .world_canon
        .transition("enter_sealed_crypt")
        .expect("creator configured the entry route");
    let reverse = session
        .world
        .world_canon
        .exits_from(&destination)
        .into_iter()
        .find(|route| route.to_place == from)
        .expect("creator configured the return route");
    assert_eq!(forward.time_cost, reverse.time_cost);
    assert_eq!(forward.risk, reverse.risk);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "scene_update")
            .count(),
        1
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
        passage_id: "test_long_road_passage".to_string(),
        directionality: PassageDirectionality::OneWay,
        from_place: from,
        to_place: destination.clone(),
        destination_hint: "дальняя башня".to_string(),
        label: "По старой дороге".to_string(),
        kind: "road".to_string(),
        visible: true,
        passable: true,
        time_cost: 48 * 60,
        risk: "certain".to_string(),
        provenance: Provenance::by("test", "long road", 0),
        ..Default::default()
    });

    let (events, result) = block_on(run_tool_collect(
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
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "scene_update")
            .count(),
        1,
        "an interrupted move must publish exactly one final scene update: {events:?}"
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
    // The core additive set is fixed; deferred/rest and distant-travel tools are
    // recognized trailing additions rather than part of the legacy ordering.
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
        assert!(
            matches!(
                extra.as_str(),
                "long_rest"
                    | "travel_to"
                    | "relocate_player"
                    | "create_passage"
                    | "set_passage_state"
            ),
            "unexpected extra canon tool: {extra}"
        );
    }
}

fn add_complete_place(session: &mut Session, place_id: &str, name: &str) {
    let mut place = Place {
        place_id: place_id.to_string(),
        name: name.to_string(),
        kind: "test_place".to_string(),
        default_description: format!("Complete test place: {name}"),
        provenance: Provenance::by("test", "dynamic passage endpoint", 0),
        ..Default::default()
    };
    place.mark_visited();
    session.world.world_canon.insert_place(place);
}

fn assert_place_card_unchanged(before: &Place, after: &Place) {
    assert_eq!(after.name, before.name);
    assert_eq!(after.kind, before.kind);
    assert_eq!(after.parent, before.parent);
    assert_eq!(after.region_id, before.region_id);
    assert_eq!(after.district_id, before.district_id);
    assert_eq!(after.default_description, before.default_description);
    assert_eq!(after.state_flags, before.state_flags);
    assert_eq!(after.features, before.features);
    assert_eq!(after.provenance, before.provenance);
}

#[test]
fn relocate_player_moves_once_without_creating_reusable_geography() {
    let mut session = seeded_session();
    let destination_id = "relocation_destination";
    add_complete_place(&mut session, destination_id, "Верх обрыва");
    let origin_id = session.world.world_canon.player_place_id.clone();
    let transitions_before = session.world.world_canon.transitions.clone();
    let clock_before = session.world.world_canon.clock_minutes;

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "relocate_player",
        &json!({
            "destination_place_id": destination_id,
            "elapsed_minutes": 3,
            "reason": "Игрок взлетел обратно на собственных крыльях"
        }),
    ));

    let payload: Value = serde_json::from_str(&result.full).expect("relocation payload");
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["status"], json!("relocated"));
    assert_eq!(payload["origin_place_id"], json!(origin_id));
    assert_eq!(payload["destination_place_id"], json!(destination_id));
    assert_eq!(payload["reusable_passage_created"], json!(false));
    assert_eq!(session.world.world_canon.player_place_id, destination_id);
    assert_eq!(session.world.world_canon.transitions, transitions_before);
    assert_eq!(session.world.world_canon.clock_minutes, clock_before + 3);
    assert!(events.iter().any(|event| event.kind == "scene_update"));
}

#[test]
fn create_passage_uses_location_creator_without_rewriting_endpoint_cards() {
    let scripted = json!({
        "entry_transition": {
            "label": "Через разбитое окно",
            "return_label": "Влезть обратно через окно",
            "directionality": "bidirectional",
            "kind": "window",
            "time_cost_minutes": 2,
            "risk": "low"
        },
        "anti_repeat_key": "broken-window-route"
    })
    .as_object()
    .expect("scripted passage profile")
    .clone();
    let generator_factory: ClientFactory = Arc::new(move || {
        Arc::new(IdentityBackend::with_scripted_json(
            "passage-location-generator",
            scripted.clone(),
        )) as Arc<dyn Backend>
    });
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260717);
    let mut session = Session::with_world(client(), world, generator_factory);
    let from_place_id = session.world.world_canon.player_place_id.clone();
    let to_place_id = "known_street_outside_window";
    add_complete_place(&mut session, to_place_id, "Улица под окном");
    let from_before = session
        .world
        .world_canon
        .place(&from_place_id)
        .expect("source place")
        .clone();
    let to_before = session
        .world
        .world_canon
        .place(to_place_id)
        .expect("target place")
        .clone();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "create_passage",
        &json!({
            "from_place_id": from_place_id,
            "to_place_id": to_place_id,
            "request": "Разбитое окно теперь образует постоянный проход между комнатой и улицей",
            "reason": "Окно разбито и проём остаётся доступным"
        }),
    ));

    let payload: Value = serde_json::from_str(&result.full).expect("passage payload");
    assert_eq!(payload["ok"], json!(true));
    assert_eq!(payload["status"], json!("passage_created"));
    assert_eq!(payload["directionality"], json!("bidirectional"));
    assert_eq!(payload["place_cards_updated"], json!(false));
    assert_place_card_unchanged(
        &from_before,
        session
            .world
            .world_canon
            .place(&from_place_id)
            .expect("source after passage"),
    );
    assert_place_card_unchanged(
        &to_before,
        session
            .world
            .world_canon
            .place(to_place_id)
            .expect("target after passage"),
    );

    let passage_id = payload["passage_id"].as_str().expect("passage id");
    let transition_ids = payload["transition_ids"]
        .as_array()
        .expect("transition ids");
    assert_eq!(transition_ids.len(), 2);
    for transition_id in transition_ids {
        let transition = session
            .world
            .world_canon
            .transition(transition_id.as_str().expect("transition id"))
            .expect("created transition");
        assert_eq!(transition.passage_id, passage_id);
        assert_eq!(
            transition.directionality,
            PassageDirectionality::Bidirectional
        );
        assert!(transition.passable);
    }
    let generator_history =
        serde_json::to_string(&session.location_generator_messages).expect("generator history");
    assert!(generator_history.contains("passage"));
    assert!(generator_history.contains(to_place_id));

    let selected_transition_id = transition_ids[0].as_str().expect("selected transition");
    let (_events, closed) = block_on(run_tool_collect(
        &mut session,
        "set_passage_state",
        &json!({
            "transition_id": selected_transition_id,
            "state": "closed",
            "reason": "Окно заколотили досками"
        }),
    ));
    let closed_payload: Value = serde_json::from_str(&closed.full).expect("closed payload");
    assert_eq!(closed_payload["status"], json!("passage_closed"));
    for transition in session
        .world
        .world_canon
        .transitions
        .values()
        .filter(|transition| transition.passage_id == passage_id)
    {
        assert!(!transition.passable);
        assert_eq!(transition.blocked_by, "Окно заколотили досками");
    }

    let (_events, opened) = block_on(run_tool_collect(
        &mut session,
        "set_passage_state",
        &json!({
            "transition_id": selected_transition_id,
            "state": "open",
            "reason": "Доски сняли"
        }),
    ));
    let opened_payload: Value = serde_json::from_str(&opened.full).expect("opened payload");
    assert_eq!(opened_payload["status"], json!("passage_opened"));
    for transition in session
        .world
        .world_canon
        .transitions
        .values()
        .filter(|transition| transition.passage_id == passage_id)
    {
        assert!(transition.passable);
        assert!(transition.blocked_by.is_empty());
    }
}

#[test]
fn invalid_location_creator_passage_profile_is_atomic() {
    let scripted = json!({"name": "No passage profile"})
        .as_object()
        .expect("invalid scripted response")
        .clone();
    let generator_factory: ClientFactory = Arc::new(move || {
        Arc::new(IdentityBackend::with_scripted_json(
            "invalid-passage-generator",
            scripted.clone(),
        )) as Arc<dyn Backend>
    });
    let world = World::from_seed_with_dice_seed(&default_story_seed(), 20260717);
    let mut session = Session::with_world(client(), world, generator_factory);
    let from_place_id = session.world.world_canon.player_place_id.clone();
    let to_place_id = "invalid_passage_target";
    add_complete_place(&mut session, to_place_id, "Другой двор");
    let before = session.world.world_canon.clone();

    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "create_passage",
        &json!({
            "from_place_id": from_place_id,
            "to_place_id": to_place_id,
            "request": "Новый постоянный проход"
        }),
    ));

    assert!(result.model.contains("code: invalid_generated_passage"));
    assert_eq!(session.world.world_canon, before);
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

/// The canned mock secret the generator returns for the "TaleShift NPC generator"
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
fn cast_spell_no_slots_is_a_clean_pre_resolution_rejection() {
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
        result.model.contains(&session.world.model_time_summary()),
        "result must carry the new time line: {}",
        result.model
    );
    assert!(
        result.model.contains("restored"),
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

    // Saves written before the public rename keep working without exposing the
    // retired schema name to the next model turn. Collisions keep the newest
    // staleness value.
    let mut pre_rename = payload.clone();
    let pre_rename_object = pre_rename.as_object_mut().expect("payload object");
    pre_rename_object.insert(
        "loaded_gm_tools".to_string(),
        json!(["advance_time", "update_player_character"]),
    );
    pre_rename_object.insert(
        "tool_last_used".to_string(),
        json!({"update_character": 2, "update_player_character": 7}),
    );
    pre_rename_object.insert(
        "tool_loaded_turn".to_string(),
        json!({"update_player_character": 5}),
    );
    let migrated =
        Session::from_payload(&pre_rename, client(), factory()).expect("old tool name migrates");
    assert!(migrated.loaded_gm_tools.contains("update_character"));
    assert!(!migrated.loaded_gm_tools.contains("update_player_character"));
    assert_eq!(migrated.tool_last_used.get("update_character"), Some(&7));
    assert_eq!(migrated.tool_loaded_turn.get("update_character"), Some(&5));
    assert!(!migrated
        .tool_last_used
        .contains_key("update_player_character"));

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
