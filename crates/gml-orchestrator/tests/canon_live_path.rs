//! Live-turn canon-authority integration test (LOCKED DECISIONS #1/#2/#3).
//!
//! Proves that the LIVE turn path is canon-authoritative: after a `move_player`
//! or `set_scene` driven through the exact `run_tool` dispatch the turn loop
//! uses, the orchestrator's player-facing `scene_export` AND the GM
//! `scene_context` both anchor on `canon.player_place_id` — not a stale legacy
//! scene. Before Stage 2 the live scene was owned by `World.scene`; now it is a
//! derived cache rebuilt from the canon after every canon-affecting tool.

use std::sync::Arc;

use serde_json::{json, Value};

use gml_llm::Backend;
use gml_mock::MockClient;
use gml_orchestrator::{run_tool_collect, ClientFactory, Session};
use gml_stories::StoryStore;
use gml_world::{Place, Provenance, Transition, World};

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
fn move_player_makes_scene_export_and_gm_context_follow_the_canon() {
    let mut session = seeded_session();
    let start = session.world.world_canon.player_place_id.clone();
    let transition_id = a_valid_transition(&session);

    // Before: the live scene is anchored on the start place.
    assert_eq!(
        session.world.scene.location_id, start,
        "precondition: the derived scene starts at the canon start place"
    );

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

    let new_place = session.world.world_canon.player_place_id.clone();
    assert_ne!(
        new_place, start,
        "player must have left the start place in the canon"
    );

    // (1) The live scene cache is rebuilt FROM the canon: scene.location_id now
    // equals canon.player_place_id, NOT the stale start scene.
    assert_eq!(
        session.world.scene.location_id, new_place,
        "the live scene must be rebuilt from the canon after move_player"
    );

    // (2) scene_export (what /state and the web UI serialize) reflects the canon.
    let export = session.world.scene_export();
    assert_eq!(
        export["location_id"].as_str().unwrap_or(""),
        new_place,
        "scene_export must anchor on canon.player_place_id"
    );

    // (3) The GM context (gm.rs scene_context) reflects the canon place.
    let gm_ctx = session.world.scene_context();
    assert!(
        gm_ctx.contains(&format!("Location: {new_place}")),
        "GM scene_context must report the canon place, got:\n{gm_ctx}"
    );

    // The turn loop emitted FULL scene updates (not stale stubs): the lazy
    // destination is auto-filled by the location generator (its commit emits a
    // scene_update of its own), then the move emits the final canon-derived one.
    let scene_updates: Vec<&gml_types::Event> =
        events.iter().filter(|e| e.kind == "scene_update").collect();
    assert!(
        !scene_updates.is_empty(),
        "a successful move emits scene updates"
    );
    for update in &scene_updates {
        assert_eq!(
            update.data["location_id"].as_str().unwrap_or(""),
            new_place,
            "every emitted scene_update is the canon-derived scene"
        );
    }

    // The unresolved exit was completed by the location creator before entry.
    assert_eq!(
        payload["applied"]["entered"],
        json!(true),
        "location creator reported the completed entry: {payload}"
    );
    let place = session
        .world
        .world_canon
        .place(&new_place)
        .expect("destination place");
    assert!(
        !place.default_description.trim().is_empty(),
        "the lazy destination must be filled with a description on first entry"
    );

    // Re-entering a filled place must NOT re-generate: leave and come back.
    let back = session
        .world
        .world_canon
        .exits_from(&new_place)
        .first()
        .map(|t| t.transition_id.clone())
        .expect("a way back");
    let (_events, result) = block_on(run_tool_collect(
        &mut session,
        "move_player",
        &json!({"transition_id": back, "reason": "возвращаюсь"}),
    ));
    let payload_back: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(payload_back["ok"], json!(true));
}

#[test]
fn set_scene_only_updates_the_current_canon_place() {
    let mut session = seeded_session();
    let current = session.world.world_canon.player_place_id.clone();
    let before_places = session.world.world_canon.places.len();
    let before_transitions = session.world.world_canon.transitions.len();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "set_scene",
        &json!({
            "title": "Обновлённый зал",
            "description": "В зале погасла половина свечей.",
            "location_id": current.clone(),
            "reason": "видимая обстановка изменилась",
        }),
    ));

    let payload: Value = serde_json::from_str(&result.full).expect("set_scene JSON result");
    assert_eq!(payload["location_id"], current);
    assert_eq!(session.world.world_canon.player_place_id, current);
    assert_eq!(session.world.world_canon.places.len(), before_places);
    assert_eq!(
        session.world.world_canon.transitions.len(),
        before_transitions,
        "a current-scene patch must not author routes"
    );
    assert_eq!(session.world.scene.title, "Обновлённый зал");
    assert!(events.iter().any(|event| event.kind == "scene_update"));
}

#[test]
fn set_scene_cannot_recreate_a_missing_current_place() {
    let mut session = seeded_session();
    let current = session.world.world_canon.player_place_id.clone();
    session.world.world_canon.places.remove(&current);
    let before = session.world.world_canon.clone();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "set_scene",
        &json!({
            "title": "Подменённая сцена",
            "description": "Этого места не должно появиться.",
            "location_id": current,
        }),
    ));

    assert!(result.model.contains("code: location_requires_generator"));
    assert_eq!(session.world.world_canon, before);
    assert!(events.iter().any(|event| event.kind == "error"));
    assert!(!events.iter().any(|event| event.kind == "scene_update"));
}

#[test]
fn set_scene_cannot_use_a_hidden_transition_profile_to_create_a_location() {
    let mut session = seeded_session();
    let before = session.world.world_canon.clone();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "set_scene",
        &json!({
            "title": "Старая дозорная башня",
            "description": "Каменная башня над дорогой.",
            "location_id": "old_watchtower",
            "entry_transition": {
                "label": "К башне",
                "return_label": "Вернуться",
                "kind": "path",
                "time_cost_minutes": 3,
                "risk": "none"
            },
            "reason": "попытка обойти создатель локаций",
        }),
    ));

    assert!(result.model.contains("code: location_requires_generator"));
    assert_eq!(session.world.world_canon, before);
    assert!(events.iter().any(|event| event.kind == "error"));
    assert!(!events.iter().any(|event| event.kind == "scene_update"));
}

#[test]
fn set_scene_does_not_bypass_an_existing_transition() {
    let mut session = seeded_session();
    let start = session.world.world_canon.player_place_id.clone();
    let destination = "blocked_courtyard".to_string();
    session.world.world_canon.insert_place(Place {
        place_id: destination.clone(),
        name: "Запертый двор".to_string(),
        kind: "site".to_string(),
        default_description: "Двор за запертой калиткой.".to_string(),
        provenance: Provenance::by("test", "blocked destination", 0),
        ..Default::default()
    });
    let transition_id = "locked_gate_to_courtyard".to_string();
    session.world.world_canon.insert_transition(Transition {
        transition_id: transition_id.clone(),
        source_exit_id: transition_id.clone(),
        from_place: start.clone(),
        to_place: destination.clone(),
        destination_hint: "запертый двор".to_string(),
        label: "Через запертую калитку".to_string(),
        kind: "gate".to_string(),
        visible: true,
        passable: false,
        blocked_by: "калитка заперта".to_string(),
        provenance: Provenance::by("test", "locked route", 0),
        ..Default::default()
    });
    let before = session.world.world_canon.clone();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "set_scene",
        &json!({
            "title": "Запертый двор",
            "description": "Игрок внезапно оказывается за калиткой.",
            "location_id": destination,
            "reason": "обход запертой калитки",
        }),
    ));

    assert!(
        result.model.contains("code: use_move_player"),
        "set_scene must reject existing-route travel bypasses: {}",
        result.model
    );
    assert_eq!(
        session.world.world_canon, before,
        "rejected set_scene must not mutate canon"
    );
    assert!(events.iter().any(|event| event.kind == "error"));
    assert!(
        !events.iter().any(|event| event.kind == "scene_update"),
        "rejected set_scene must not refresh the scene"
    );
}

#[test]
fn set_scene_does_not_match_an_unresolved_transition_by_title_or_hint() {
    let mut session = seeded_session();
    let start = session.world.world_canon.player_place_id.clone();
    let transition_id = "shell_exit_to_garden".to_string();
    session.world.world_canon.insert_transition(Transition {
        transition_id: transition_id.clone(),
        source_exit_id: transition_id.clone(),
        from_place: start.clone(),
        to_place: String::new(),
        destination_hint: "Сад за таверной".to_string(),
        label: "В сад за таверной".to_string(),
        kind: "garden_path".to_string(),
        visible: true,
        passable: false,
        blocked_by: "дверь на засове".to_string(),
        provenance: Provenance::by("test", "blocked shell route", 0),
        ..Default::default()
    });
    let before = session.world.world_canon.clone();

    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "set_scene",
        &json!({
            "title": "Сад за таверной",
            "description": "Игрок внезапно оказывается в саду за дверью.",
            "location_id": "garden_behind_tavern",
            "reason": "обход shell-перехода",
        }),
    ));

    assert!(
        result.model.contains("code: location_requires_generator"),
        "set_scene must not identify an unresolved route from title text: {}",
        result.model
    );
    assert_eq!(
        session.world.world_canon, before,
        "rejected shell-route set_scene must not mutate canon"
    );
    assert!(events.iter().any(|event| event.kind == "error"));
    assert!(
        !events.iter().any(|event| event.kind == "scene_update"),
        "rejected shell-route set_scene must not refresh the scene"
    );
}

#[test]
fn set_scene_authoring_is_recorded_in_the_event_log_and_causal_log() {
    use gml_world::canon::engine;
    let mut session = seeded_session();
    let before = session.world.world_canon.event_log.events.len();
    let current = session.world.world_canon.player_place_id.clone();

    block_on(run_tool_collect(
        &mut session,
        "set_scene",
        &json!({
            "title": "Зал после обыска",
            "description": "Стулья сдвинуты, на полу рассыпана зола.",
            "location_id": current.clone(),
            "reason": "видимое состояние текущего места изменилось",
        }),
    ));

    assert!(
        session.world.world_canon.event_log.events.len() > before,
        "set_scene must append a canon event (important change in the log)"
    );
    let causal = engine::causal_log(&session.world.world_canon);
    assert!(
        causal.iter().any(|l| l.contains("set_scene")),
        "the causal log must explain the set_scene authoring; got:\n{}",
        causal.join("\n")
    );
}

#[test]
fn move_npc_persists_through_canon_refresh_and_npc_can_react() {
    // A legacy presence mutation must write THROUGH to the canon, so the change
    // survives a subsequent refresh_scene_from_canon (single source of truth),
    // and the derived presence must let the npc react (ask_npc needs both
    // present_npcs AND a presence entry).
    let mut session = seeded_session();
    // Find an npc NOT currently in the scene.
    let absent: String = session
        .world
        .npcs
        .keys()
        .find(|id| !session.world.scene.present_npcs.contains(*id))
        .cloned()
        .expect("seeded roster has an offscreen npc");

    session
        .world
        .set_npc_presence(&absent, true, "у стойки", true, true, "наливает эль", "")
        .expect("set presence");

    // Present + has a presence entry + can react (ask_npc precondition).
    assert!(session.world.scene.present_npcs.contains(&absent));
    assert!(
        session.world.scene.presence.contains_key(&absent),
        "derived presence must have an entry so ask_npc works"
    );
    assert!(
        session.world.npc_can_react(&absent),
        "a present canon actor must be able to react"
    );

    // The canon actor is physically at the player's place...
    let here = session.world.world_canon.player_place_id.clone();
    assert!(
        session
            .world
            .world_canon
            .actor(&absent)
            .map(|a| a.is_at(&here))
            .unwrap_or(false),
        "move_npc must write through to the canon actor location"
    );
    // ...so an INDEPENDENT refresh keeps them present (not overwritten).
    session.world.refresh_scene_from_canon();
    assert!(
        session.world.scene.present_npcs.contains(&absent),
        "presence must survive a canon refresh (canon is the source of truth)"
    );
}

#[test]
fn advance_time_advances_the_canon_clock_and_resolves_due_events() {
    use gml_world::canon::{engine, Action, ProposedAction};

    let mut session = seeded_session();
    let before = session.world.world_canon.clock_minutes;

    // Schedule an offscreen event due in 30 minutes, through the engine.
    let here = session.world.world_canon.player_place_id.clone();
    let sched = ProposedAction::new(
        Action::ScheduleEvent {
            kind: "patrol_returns".to_string(),
            due_minutes: before + 30,
            place_id: here,
            actors: vec![],
            causes: vec![],
        },
        "gm",
        "patrol scheduled",
    );
    let evs = engine::apply(&mut session.world.world_canon, &sched, 0).expect("schedule ok");
    let event_id = evs[0].event_id.clone();
    assert!(!session.world.world_canon.event_log.is_resolved(&event_id));

    // A live advance_time past the due time must move the canon clock AND run
    // the offscreen tick that resolves the due event.
    session
        .world
        .advance_time(&serde_json::json!(60), "ждём")
        .expect("advance");
    assert!(
        session.world.world_canon.clock_minutes >= before + 60,
        "advance_time must advance the canonical clock"
    );
    assert!(
        session.world.world_canon.event_log.is_resolved(&event_id),
        "advance_time must run the offscreen tick that resolves due events"
    );
}

#[test]
fn set_scene_present_npc_becomes_a_living_actor_at_the_current_place() {
    let mut session = seeded_session();
    let current = session.world.world_canon.player_place_id.clone();
    let npc_id = session
        .world
        .npcs
        .keys()
        .next()
        .expect("seeded world has at least one npc")
        .clone();

    block_on(run_tool_collect(
        &mut session,
        "set_scene",
        &json!({
            "title": "Тихий зал",
            "description": "Зал почти опустел.",
            "location_id": current.clone(),
            "present_npcs": [npc_id.clone()],
            "reason": "NPC вошёл в текущую сцену",
        }),
    ));

    // The present npc is now a living canon actor physically at the new place,
    // so the DERIVED present_npcs (actors_at) surfaces them.
    let actor = session
        .world
        .world_canon
        .actor(&npc_id)
        .expect("present npc must have a canon actor");
    assert!(
        actor.is_at(&current),
        "present npc must be located at the current canon place"
    );
    assert!(
        session.world.scene.present_npcs.contains(&npc_id),
        "the derived scene present_npcs must include the actor at the place"
    );
}
