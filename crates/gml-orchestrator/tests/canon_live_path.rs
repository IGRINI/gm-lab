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

use gml_llm::{Backend, MockClient};
use gml_orchestrator::{run_tool_collect, ClientFactory, Session};
use gml_stories::default_story_seed;
use gml_world::{Place, Provenance, Transition, World};

fn factory() -> ClientFactory {
    Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>)
}

fn client() -> Arc<dyn Backend> {
    Arc::new(MockClient::new())
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

    // The turn loop emitted a FULL scene update (not a stale stub).
    let scene_updates: Vec<&gml_types::Event> =
        events.iter().filter(|e| e.kind == "scene_update").collect();
    assert_eq!(
        scene_updates.len(),
        1,
        "exactly one scene_update on a successful move"
    );
    assert_eq!(
        scene_updates[0].data["location_id"].as_str().unwrap_or(""),
        new_place,
        "the emitted scene_update is the canon-derived scene"
    );
}

#[test]
fn set_scene_authors_a_canon_place_and_moves_the_player_there() {
    let mut session = seeded_session();
    let start = session.world.world_canon.player_place_id.clone();
    let before_places = session.world.world_canon.places.len();

    // set_scene is now a canon mutation: it must upsert a NEW Place, ensure a
    // transition from the current place, and set canon.player_place_id.
    let (events, result) = block_on(run_tool_collect(
        &mut session,
        "set_scene",
        &json!({
            "title": "Заброшенная часовня",
            "description": "Холодный неф под обвалившейся крышей.",
            "location_id": "abandoned_chapel",
            "reason": "игрок входит в часовню",
        }),
    ));
    assert!(!result.full.is_empty());
    let _ = events;

    let dest = session.world.world_canon.player_place_id.clone();
    assert_eq!(
        dest, "abandoned_chapel",
        "set_scene must move the player into the new canon place"
    );
    assert_ne!(dest, start);

    // The canon gained the authored place.
    assert!(
        session
            .world
            .world_canon
            .places
            .contains_key("abandoned_chapel"),
        "set_scene must upsert a canonical Place"
    );
    assert!(
        session.world.world_canon.places.len() > before_places,
        "a brand-new place was added to the canon"
    );

    // A transition from the start place to the destination now exists.
    assert!(
        session
            .world
            .world_canon
            .exits_from(&start)
            .iter()
            .any(|t| t.to_place == "abandoned_chapel"),
        "set_scene must ensure a transition from the current place to the destination"
    );

    // The derived live scene + GM context reflect the canon place and title.
    assert_eq!(session.world.scene.location_id, "abandoned_chapel");
    assert_eq!(session.world.scene.title, "Заброшенная часовня");
    let export = session.world.scene_export();
    assert_eq!(
        export["location_id"].as_str().unwrap_or(""),
        "abandoned_chapel"
    );
    let gm_ctx = session.world.scene_context();
    assert!(gm_ctx.contains("Location: abandoned_chapel"));
    assert!(gm_ctx.contains("Заброшенная часовня"));

    // And the player can always go back (guaranteed return edge).
    assert!(
        session
            .world
            .world_canon
            .exits_from("abandoned_chapel")
            .iter()
            .any(|t| t.to_place == start),
        "set_scene must leave a return path so the player can go back"
    );
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
fn set_scene_does_not_bypass_a_matching_shell_transition() {
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
            "reason": "обход shell-перехода",
        }),
    ));

    assert!(
        result.model.contains("code: use_move_player"),
        "set_scene must reject matching shell-route bypasses: {}",
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

    block_on(run_tool_collect(
        &mut session,
        "set_scene",
        &json!({
            "title": "Старая мельница",
            "description": "Скрипучая мельница над запрудой.",
            "location_id": "old_mill",
            "reason": "игрок входит на мельницу",
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
fn set_scene_present_npc_becomes_a_living_actor_at_the_new_place() {
    let mut session = seeded_session();
    // Pick a known npc from the seeded roster to mark present in the new place.
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
            "title": "Тихий двор",
            "description": "Мощёный двор за таверной.",
            "location_id": "quiet_yard",
            "present_npcs": [npc_id.clone()],
            "reason": "игрок выходит во двор, NPC уже там",
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
        actor.is_at("quiet_yard"),
        "present npc must be located at the new canon place"
    );
    assert!(
        session.world.scene.present_npcs.contains(&npc_id),
        "the derived scene present_npcs must include the actor at the place"
    );
}
