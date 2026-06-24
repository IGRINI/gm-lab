//! Living-world engine acceptance tests (LIVING_WORLD_ARCHITECTURE_TZ.md §14).
//!
//! One focused test per acceptance criterion, plus determinism (same seed =>
//! byte-identical canon and identical replay), validator enforcement (a
//! contradictory action returns Err and mutates nothing), and a hidden-knowledge
//! leak test (the gated player view never exposes GmPrivate/TrueCanon content).

use gml_world::canon::action::{Action, ProposedAction};
use gml_world::canon::engine;
use gml_world::canon::worldgen::{self, WorldSpec};
use gml_world::canon::{Place, Provenance, Scope, Transition};

fn gen(seed: &str) -> gml_world::WorldCanon {
    worldgen::generate(&WorldSpec::from_seed(seed))
}

fn propose(action: Action) -> ProposedAction {
    ProposedAction::new(action, "test", "test action")
}

#[test]
fn facts_are_durable_scoped_state_and_reveal_widens_scope() {
    // create_or_update_fact writes real queryable state (not just an event
    // effect); reveal_information widens its scope (TZ §8.2).
    let mut canon = gen("facts");
    engine::apply(
        &mut canon,
        &propose(Action::CreateOrUpdateFact {
            fact_id: "secret_tunnel".into(),
            text: "туннель под криптой".into(),
            scope: Scope::GmPrivate,
        }),
        1,
    )
    .expect("create fact");
    assert_eq!(
        canon.facts.get("secret_tunnel").map(|f| f.scope.clone()),
        Some(Scope::GmPrivate),
        "fact is stored as durable scoped state"
    );
    // While GM-private, it is NOT player-visible.
    assert!(!canon.facts["secret_tunnel"].scope.visible_to_player());

    engine::apply(
        &mut canon,
        &propose(Action::RevealInformation {
            fact_id: "secret_tunnel".into(),
            to: Scope::Player,
        }),
        2,
    )
    .expect("reveal");
    assert_eq!(
        canon.facts.get("secret_tunnel").map(|f| f.scope.clone()),
        Some(Scope::Player),
        "reveal widens the fact scope so the player may now know it"
    );
    assert!(canon.facts["secret_tunnel"].scope.visible_to_player());
}

#[test]
fn committed_events_carry_a_nonzero_seq() {
    // CreateEvent must return the stamped seq (not 0).
    let mut canon = gen("seq");
    let evs = engine::apply(
        &mut canon,
        &propose(Action::CreateEvent {
            kind: "noise".into(),
            place_id: String::new(),
            actors: vec![],
            causes: vec![],
            effects: vec!["a clang".into()],
            visible_to_player: true,
            scope: Scope::Public,
            traces: vec![],
        }),
        1,
    )
    .expect("create event");
    assert!(evs[0].seq > 0, "returned event must carry its stamped seq");
}

#[test]
fn committed_place_events_are_linked_from_their_place() {
    let mut canon = gen("place-events");
    let place_id = canon.player_place_id.clone();
    let evs = engine::apply(
        &mut canon,
        &propose(Action::CreateEvent {
            kind: "place_note".into(),
            place_id: place_id.clone(),
            actors: vec![],
            causes: vec![],
            effects: vec!["a marked floorboard".into()],
            visible_to_player: true,
            scope: Scope::Public,
            traces: vec![],
        }),
        1,
    )
    .expect("create place event");
    assert!(
        canon
            .place(&place_id)
            .unwrap()
            .event_ids
            .contains(&evs[0].event_id),
        "place.event_ids must link the event that touched the place"
    );
}

/// Helper: the first visible, passable exit from the player's current place.
fn first_open_exit(canon: &gml_world::WorldCanon) -> String {
    canon
        .exits_from(&canon.player_place_id)
        .into_iter()
        .find(|t| t.visible && t.passable)
        .map(|t| t.transition_id.clone())
        .expect("an open exit from the start place")
}

// =========================================================================
// §14: a new campaign creates a region, a settlement, a start place, several
// neighbouring places, and an initial history.
// =========================================================================
#[test]
fn new_campaign_has_region_settlement_places_and_history() {
    let canon = gen("camp1");

    assert!(!canon.regions.is_empty(), "at least one region");
    assert!(!canon.settlements.is_empty(), "at least one settlement");

    // The settlement has a real function (TZ §6.3 / §15 antipattern).
    let settlement = canon.settlements.values().next().unwrap();
    assert!(settlement.has_function(), "settlement must have a function");

    // A start place exists and is the player's location.
    assert!(
        canon.place(&canon.player_place_id).is_some(),
        "player starts on a real place"
    );

    // Several neighbouring places (square + smithy + gate + road + crypt >= 4).
    assert!(
        canon.places.len() >= 4,
        "several places, got {}",
        canon.places.len()
    );

    // An initial history exists.
    assert!(
        !canon.event_log.events.is_empty(),
        "a new campaign has an initial history"
    );
    assert!(
        !canon.event_log.accounts.is_empty(),
        "and at least one account/rumour"
    );
}

// =========================================================================
// §14: the player moves over the transition graph, not via free scene
// replacement.
// =========================================================================
#[test]
fn player_moves_over_the_transition_graph() {
    let mut canon = gen("camp2");
    let start = canon.player_place_id.clone();
    let tid = first_open_exit(&canon);
    let target = canon.transition(&tid).unwrap().to_place.clone();
    let travel_minutes = canon.transition(&tid).unwrap().time_cost;

    let events = engine::apply(
        &mut canon,
        &propose(Action::MovePlayer { transition_id: tid }),
        1,
    )
    .expect("move succeeds");

    assert_ne!(canon.player_place_id, start, "player actually moved");
    assert_eq!(canon.player_place_id, target, "moved to the edge's target");
    assert_eq!(
        canon.clock_minutes, travel_minutes,
        "move_player advances the canon clock by the transition time"
    );
    assert!(
        events.iter().any(|e| e.kind == "move_player"),
        "a move_player event was committed"
    );
}

#[test]
fn generated_transitions_have_time_and_risk_profiles() {
    let canon = gen("travel-profiles");
    assert!(
        !canon.transitions.is_empty(),
        "worldgen must create transitions"
    );
    for transition in canon.transitions.values() {
        assert!(
            transition.time_cost > 0,
            "transition {} has no travel time",
            transition.transition_id
        );
        assert!(
            !transition.risk.trim().is_empty(),
            "transition {} has no risk profile",
            transition.transition_id
        );
    }
}

#[test]
fn short_travel_spends_time_without_road_situation() {
    let mut canon = gen("short-travel");
    let tid = first_open_exit(&canon);
    let minutes = canon.transition(&tid).unwrap().time_cost;
    assert!(
        minutes <= gml_world::canon::travel::SITUATION_THRESHOLD_MINUTES,
        "first generated local exit should be a short route"
    );

    let events = engine::apply(
        &mut canon,
        &propose(Action::MovePlayer { transition_id: tid }),
        1,
    )
    .unwrap();

    assert_eq!(canon.clock_minutes, minutes);
    assert!(
        !events.iter().any(|e| e.kind == "travel_situation"),
        "short local travel should not create an encounter"
    );
}

#[test]
fn long_risky_travel_can_stop_at_a_road_situation_before_destination() {
    let mut canon = gen("road-situation");
    let from = canon.player_place_id.clone();
    let destination = "distant_monastery".to_string();
    canon.insert_place(Place {
        place_id: destination.clone(),
        name: "Дальний монастырь".to_string(),
        kind: "site".to_string(),
        parent: String::new(),
        region_id: String::new(),
        default_description: "Монастырь далеко за лесной дорогой.".to_string(),
        provenance: Provenance::by("test", "destination", 0),
        ..Default::default()
    });
    let transition_id = "forced_long_road".to_string();
    canon.insert_transition(Transition {
        transition_id: transition_id.clone(),
        source_exit_id: transition_id.clone(),
        from_place: from.clone(),
        to_place: destination.clone(),
        destination_hint: destination.clone(),
        label: "Долгая лесная дорога".to_string(),
        kind: "road".to_string(),
        visible: true,
        passable: true,
        time_cost: 48 * 60,
        risk: "certain wild road: test-only guaranteed situation".to_string(),
        provenance: Provenance::by("test", "long road", 0),
        ..Default::default()
    });

    let events = engine::apply(
        &mut canon,
        &propose(Action::MovePlayer {
            transition_id: transition_id.clone(),
        }),
        1,
    )
    .unwrap();

    assert_ne!(
        canon.player_place_id, destination,
        "a road situation interrupts before final arrival"
    );
    let site = canon
        .place(&canon.player_place_id)
        .expect("player is at the generated road site");
    assert!(
        site.has_flag("travel_site"),
        "interruption creates a temporary travel-site place"
    );
    assert!(
        canon.clock_minutes > 0 && canon.clock_minutes < 48 * 60,
        "only the elapsed part of the route is spent"
    );
    assert!(
        events.iter().any(|e| e.kind == "travel_situation"),
        "the situation is recorded in the event log"
    );

    let continue_edge = canon
        .exits_from(&canon.player_place_id)
        .into_iter()
        .find(|t| t.to_place == destination)
        .expect("travel site has a continue edge to the original destination");
    assert!(
        continue_edge.time_cost > 0 && continue_edge.time_cost < 48 * 60,
        "continue edge stores the remaining travel time"
    );
    let back_edge = canon
        .exits_from(&canon.player_place_id)
        .into_iter()
        .find(|t| t.to_place == from)
        .expect("travel site has a return edge");
    assert_eq!(
        back_edge.time_cost, canon.clock_minutes,
        "return edge stores the already travelled time"
    );
}

// =========================================================================
// §14: the player can return to the start place and it keeps its state.
// =========================================================================
#[test]
fn player_can_return_to_start_and_state_persists() {
    let mut canon = gen("camp3");
    let start = canon.player_place_id.clone();

    // Mark a distinguishing state flag on the start place.
    canon
        .place_mut(&start)
        .unwrap()
        .state_flags
        .insert("marked_by_test".to_string());

    // Move out along the smithy/square link (square <-> smithy is two-way).
    let out = first_open_exit(&canon);
    let dest = canon.transition(&out).unwrap().to_place.clone();
    engine::apply(
        &mut canon,
        &propose(Action::MovePlayer { transition_id: out }),
        1,
    )
    .unwrap();
    assert_eq!(canon.player_place_id, dest);

    // Find the back edge dest -> start and return.
    let back = canon
        .exits_from(&dest)
        .into_iter()
        .find(|t| t.to_place == start)
        .map(|t| t.transition_id.clone())
        .expect("a back edge to start");
    engine::apply(
        &mut canon,
        &propose(Action::MovePlayer {
            transition_id: back,
        }),
        2,
    )
    .unwrap();

    assert_eq!(canon.player_place_id, start, "returned to start");
    assert!(
        canon.place(&start).unwrap().has_flag("marked_by_test"),
        "the start place kept its state on return"
    );
}

// =========================================================================
// §14: a new room / point-of-interest can be lazy-generated on first entry and
// then becomes canon.
// =========================================================================
#[test]
fn poi_is_lazy_generated_on_first_entry_then_canon() {
    let mut canon = gen("camp4");

    // Walk to the road, then into the crypt shell.
    let road_id = ids_place(&canon, "road");
    walk_to(&mut canon, &road_id);

    let places_before = canon.places.len();
    // The crypt edge from the road.
    let crypt_edge = canon
        .exits_from(&road_id)
        .into_iter()
        .find(|t| {
            canon
                .place(&t.to_place)
                .map(|p| p.has_flag("shell"))
                .unwrap_or(false)
        })
        .map(|t| t.transition_id.clone())
        .expect("a shell crypt edge from the road");
    let crypt_id = canon.transition(&crypt_edge).unwrap().to_place.clone();

    assert!(
        canon.place(&crypt_id).unwrap().has_flag("shell"),
        "crypt is a shell first"
    );

    let events = engine::apply(
        &mut canon,
        &propose(Action::MovePlayer {
            transition_id: crypt_edge,
        }),
        3,
    )
    .expect("enter crypt");

    // Entering expanded the interior.
    assert!(
        !canon.place(&crypt_id).unwrap().has_flag("shell"),
        "shell flag removed after first entry"
    );
    assert!(
        canon.places.len() > places_before,
        "lazy generation added interior rooms (now canon)"
    );
    assert!(
        events.iter().any(|e| e.kind == "create_place"),
        "create_place events recorded for the new rooms"
    );

    // The interior rooms persist (are canon) and are reachable / leavable.
    let interior_rooms: Vec<_> = canon
        .places
        .values()
        .filter(|p| p.parent == crypt_id)
        .collect();
    assert!(!interior_rooms.is_empty(), "interior rooms became canon");
    for room in &interior_rooms {
        // Every interior room has at least one outgoing edge (can always leave).
        assert!(
            !canon.exits_from(&room.place_id).is_empty(),
            "interior room {} has an exit (TZ §7.4 can always leave)",
            room.place_id
        );
    }
}

// =========================================================================
// §14: an NPC exists outside the current scene and can physically be in another
// place.
// =========================================================================
#[test]
fn npc_exists_outside_scene_and_can_be_elsewhere() {
    let mut canon = gen("camp5");
    let start = canon.player_place_id.clone();

    // The generated "warden" actor lives at the gate, not the start square.
    let warden = canon
        .actors
        .values()
        .find(|a| a.role == "guard")
        .expect("a guard actor")
        .clone();
    assert!(
        !warden.is_at(&start),
        "the guard is not at the player's place"
    );
    assert!(
        warden.location.place().is_some(),
        "but the guard is physically in some other place"
    );

    // It can be moved to yet another place via the engine.
    let gate_id = ids_place(&canon, "gate");
    let smithy_id = ids_place(&canon, "smithy");
    let dest = if warden.location.place() == Some(gate_id.as_str()) {
        smithy_id
    } else {
        gate_id
    };
    engine::apply(
        &mut canon,
        &propose(Action::MoveActor {
            actor_id: warden.actor_id.clone(),
            to_place: dest.clone(),
        }),
        1,
    )
    .expect("move actor");
    assert!(canon.actor(&warden.actor_id).unwrap().is_at(&dest));
    assert!(
        canon
            .place(&dest)
            .unwrap()
            .occupant_ids
            .contains(&warden.actor_id),
        "destination place lists the actor as an occupant"
    );
}

// =========================================================================
// §14: important world changes are written to the event log.
// =========================================================================
#[test]
fn important_changes_are_written_to_the_event_log() {
    let mut canon = gen("camp6");
    let before = canon.event_log.events.len();
    let tid = first_open_exit(&canon);
    engine::apply(
        &mut canon,
        &propose(Action::MovePlayer { transition_id: tid }),
        1,
    )
    .unwrap();
    assert!(
        canon.event_log.events.len() > before,
        "the move was recorded in the event log"
    );
}

// =========================================================================
// §14: the LLM cannot directly make a contradictory canon commit without the
// validator (a bad action returns Err and mutates nothing).
// =========================================================================
#[test]
fn validator_blocks_contradictory_commits_and_mutates_nothing() {
    let mut canon = gen("camp7");
    let snapshot = canon.clone();

    // 1. Move through a non-existent transition.
    let err = engine::apply(
        &mut canon,
        &propose(Action::MovePlayer {
            transition_id: "no_such_edge".to_string(),
        }),
        1,
    )
    .unwrap_err();
    assert_eq!(err.code, "unknown_transition");
    assert_eq!(canon, snapshot, "rejected move must not mutate the canon");

    // 2. Create a duplicate place id (the start place already exists).
    let dup = canon.player_place_id.clone();
    let err = engine::apply(
        &mut canon,
        &propose(Action::CreatePlace {
            place_id: dup,
            name: "dup".to_string(),
            kind: String::new(),
            parent: String::new(),
            region_id: String::new(),
            description: String::new(),
            features: Vec::new(),
            visited: false,
            shell: false,
        }),
        1,
    )
    .unwrap_err();
    assert_eq!(err.code, "duplicate_id");
    assert_eq!(canon, snapshot, "rejected create must not mutate the canon");

    // 3. Move an actor to a nonexistent place.
    let actor_id = canon.actors.keys().next().unwrap().clone();
    let err = engine::apply(
        &mut canon,
        &propose(Action::MoveActor {
            actor_id,
            to_place: "nowhere".to_string(),
        }),
        1,
    )
    .unwrap_err();
    assert_eq!(err.code, "unknown_place");
    assert_eq!(
        canon, snapshot,
        "rejected actor move must not mutate the canon"
    );

    // 4. Move through a blocked exit: craft a blocked edge and try it.
    let from = canon.player_place_id.clone();
    let blocked_tid = "blocked_edge".to_string();
    engine::apply(
        &mut canon,
        &propose(Action::CreatePlace {
            place_id: "vault".to_string(),
            name: "Vault".to_string(),
            kind: String::new(),
            parent: String::new(),
            region_id: String::new(),
            description: String::new(),
            features: Vec::new(),
            visited: false,
            shell: false,
        }),
        1,
    )
    .unwrap();
    engine::apply(
        &mut canon,
        &propose(Action::CreateTransition {
            transition_id: blocked_tid.clone(),
            from_place: from,
            to_place: "vault".to_string(),
            destination_hint: String::new(),
            label: "Locked door".to_string(),
            kind: String::new(),
            visible: Some(true),
            passable: Some(true),
            blocked_by: "heavy lock".to_string(),
            time_cost: 0,
            risk: String::new(),
        }),
        1,
    )
    .unwrap();
    let mid = canon.clone();
    let err = engine::apply(
        &mut canon,
        &propose(Action::MovePlayer {
            transition_id: blocked_tid,
        }),
        1,
    )
    .unwrap_err();
    assert_eq!(err.code, "blocked");
    assert_eq!(
        canon, mid,
        "rejected blocked move must not mutate the canon"
    );
}

// =========================================================================
// §14: hidden knowledge does not leak into the player-facing view.
// =========================================================================
#[test]
fn player_view_does_not_leak_hidden_knowledge() {
    let mut canon = gen("camp8");

    // Commit a GmPrivate event with a damning secret in its effects.
    let secret = ProposedAction {
        action: Action::CreateEvent {
            kind: "betrayal".to_string(),
            place_id: String::new(),
            actors: Vec::new(),
            causes: Vec::new(),
            effects: vec!["SECRET_THE_WARDEN_IS_A_TRAITOR".to_string()],
            visible_to_player: false,
            scope: Scope::GmPrivate,
            traces: Vec::new(),
        },
        source: "gm".to_string(),
        reason: "hidden truth".to_string(),
        scope: Scope::GmPrivate,
        time_delta: 0,
        confidence: None,
    };
    engine::apply(&mut canon, &secret, 1).unwrap();

    // Also schedule + resolve an offscreen event (GmPrivate trail).
    let now = canon.clock_minutes;
    engine::apply(
        &mut canon,
        &propose(Action::ScheduleEvent {
            kind: "caravan_missing".to_string(),
            due_minutes: now + 30,
            place_id: String::new(),
            actors: Vec::new(),
            causes: Vec::new(),
        }),
        1,
    )
    .unwrap();
    engine::apply(
        &mut canon,
        &propose(Action::AdvanceClock { minutes: 60 }),
        1,
    )
    .unwrap();

    let view = engine::player_view(&canon);
    let blob = serde_json::to_string(&view).unwrap();

    assert!(
        !blob.contains("SECRET_THE_WARDEN_IS_A_TRAITOR"),
        "GmPrivate effect must not appear in the player view"
    );
    assert!(
        !blob.contains("betrayal"),
        "GmPrivate event kind must not appear in the player view"
    );
    // Every event the view DOES surface must be a player-visible scope.
    for ev in &view.recent_events {
        assert_ne!(
            ev.kind, "resolved_caravan_missing",
            "offscreen resolution stays hidden"
        );
    }
    // Shell/interior secrets must not be present as exits either (no hidden
    // exits in the view).
    for ex in &view.exits {
        let t = canon.transition(&ex.transition_id).unwrap();
        assert!(t.visible, "only visible exits reach the player view");
    }
}

// =========================================================================
// §14: there is a debug/replay way to understand why the world reached its
// current state.
// =========================================================================
#[test]
fn debug_dump_and_causal_log_explain_world_state() {
    let mut canon = gen("camp9");
    let tid = first_open_exit(&canon);
    engine::apply(
        &mut canon,
        &propose(Action::MovePlayer { transition_id: tid }),
        1,
    )
    .unwrap();

    let dump = engine::debug_dump(&canon);
    assert!(
        dump.get("places").is_some(),
        "debug dump exposes the full canon"
    );

    let log = engine::causal_log(&canon);
    assert!(!log.is_empty(), "causal log has entries");
    assert!(
        log.iter().any(|line| line.contains("move_player")),
        "the causal log explains the player move"
    );

    let bundle = engine::debug_bundle(&canon);
    assert!(bundle.get("canon").is_some() && bundle.get("causal_log").is_some());
}

// =========================================================================
// Determinism: same seed => byte-identical canon, and identical replay of a
// fixed action sequence (TZ §7.3, §12).
// =========================================================================
#[test]
fn generation_is_deterministic_for_a_seed() {
    let a = gen("det-seed");
    let b = gen("det-seed");
    let aj = serde_json::to_string(&a).unwrap();
    let bj = serde_json::to_string(&b).unwrap();
    assert_eq!(
        aj, bj,
        "two generations of the same seed are byte-identical"
    );

    // Different seed => different canon.
    let c = gen("other-seed");
    assert_ne!(serde_json::to_string(&c).unwrap(), aj);
}

#[test]
fn replay_of_a_fixed_action_sequence_is_identical() {
    let run = || {
        let mut canon = gen("replay-seed");
        // A fixed sequence: move to road, enter the crypt (lazy expand), advance
        // the clock.
        let road_id = ids_place(&canon, "road");
        walk_to(&mut canon, &road_id);
        let crypt_edge = canon
            .exits_from(&road_id)
            .into_iter()
            .find(|t| {
                canon
                    .place(&t.to_place)
                    .map(|p| p.has_flag("shell"))
                    .unwrap_or(false)
            })
            .map(|t| t.transition_id.clone())
            .unwrap();
        engine::apply(
            &mut canon,
            &propose(Action::MovePlayer {
                transition_id: crypt_edge,
            }),
            5,
        )
        .unwrap();
        engine::apply(
            &mut canon,
            &propose(Action::AdvanceClock { minutes: 120 }),
            6,
        )
        .unwrap();
        canon
    };
    let first = serde_json::to_string(&run()).unwrap();
    let second = serde_json::to_string(&run()).unwrap();
    assert_eq!(
        first, second,
        "replaying the same sequence yields identical canon"
    );
}

// =========================================================================
// Bug fixes (LOCKED DECISION #6).
// =========================================================================

/// AdvanceClock is the SOLE clock mutator for its action: a proposal carrying
/// both `time_delta` and `AdvanceClock { minutes }` must advance the clock ONCE
/// by `minutes`, not by `minutes + time_delta`.
#[test]
fn advance_clock_does_not_double_advance() {
    let mut canon = gen("clock1");
    let before = canon.clock_minutes;
    let proposed = ProposedAction {
        action: Action::AdvanceClock { minutes: 30 },
        source: "gm".to_string(),
        reason: "rest".to_string(),
        scope: Scope::Player,
        // A non-zero time_delta that MUST be ignored for AdvanceClock.
        time_delta: 45,
        confidence: None,
    };
    engine::apply(&mut canon, &proposed, 1).unwrap();
    assert_eq!(
        canon.clock_minutes,
        before + 30,
        "AdvanceClock advances exactly by `minutes`, ignoring time_delta"
    );
}

/// A non-AdvanceClock action still advances by its `time_delta` exactly once.
#[test]
fn time_delta_advances_once_for_non_clock_actions() {
    let mut canon = gen("clock2");
    let before = canon.clock_minutes;
    let tid = first_open_exit(&canon);
    let proposed = ProposedAction {
        action: Action::MovePlayer { transition_id: tid },
        source: "gm".to_string(),
        reason: "walk".to_string(),
        scope: Scope::Player,
        time_delta: 15,
        confidence: None,
    };
    engine::apply(&mut canon, &proposed, 1).unwrap();
    assert_eq!(canon.clock_minutes, before + 15);
}

/// The event log is append-only: resolving a scheduled event does NOT mutate the
/// recorded event's `scheduled` flag; resolution is tracked as a projection and
/// `due_scheduled` excludes resolved ones.
#[test]
fn resolving_a_scheduled_event_is_append_only() {
    let mut canon = gen("resolve1");
    let now = canon.clock_minutes;
    engine::apply(
        &mut canon,
        &propose(Action::ScheduleEvent {
            kind: "ambush".to_string(),
            due_minutes: now + 10,
            place_id: String::new(),
            actors: Vec::new(),
            causes: Vec::new(),
        }),
        1,
    )
    .unwrap();
    let sched = canon
        .event_log
        .events
        .iter()
        .find(|e| e.kind == "ambush")
        .cloned()
        .unwrap();
    assert!(
        sched.scheduled,
        "the scheduled event records scheduled=true"
    );

    // Resolve it.
    engine::apply(
        &mut canon,
        &propose(Action::ResolveEvent {
            event_id: sched.event_id.clone(),
        }),
        2,
    )
    .unwrap();

    // The original recorded event is UNCHANGED (still scheduled=true).
    let after = canon
        .event_log
        .events
        .iter()
        .find(|e| e.event_id == sched.event_id)
        .unwrap();
    assert!(
        after.scheduled,
        "the recorded event must not be mutated (append-only)"
    );
    // But it is now resolved (a projection) and no longer pending / due.
    assert!(canon.event_log.is_resolved(&sched.event_id));
    assert!(!canon.event_log.is_pending(&sched.event_id));
    assert!(
        canon
            .event_log
            .due_scheduled(now + 1000)
            .iter()
            .all(|id| id != &sched.event_id),
        "a resolved event is never due again"
    );

    // Resolving again is rejected (no pending event) and mutates nothing.
    let err = engine::apply(
        &mut canon,
        &propose(Action::ResolveEvent {
            event_id: sched.event_id.clone(),
        }),
        3,
    )
    .unwrap_err();
    assert_eq!(err.code, "no_pending_event");
}

/// Every committed event carries a non-zero, monotonic seq.
#[test]
fn every_committed_event_has_a_nonzero_seq() {
    let mut canon = gen("seq1");
    let tid = first_open_exit(&canon);
    engine::apply(
        &mut canon,
        &propose(Action::MovePlayer { transition_id: tid }),
        1,
    )
    .unwrap();
    let mut last = 0;
    for e in &canon.event_log.events {
        assert!(e.seq > 0, "event '{}' has zero seq", e.event_id);
        assert!(e.seq > last, "seqs must be strictly monotonic");
        last = e.seq;
    }
}

/// The validator rejects a CreateEvent flagged player-visible under a hidden
/// scope (the hidden-knowledge leak), mutating nothing.
#[test]
fn validator_rejects_visible_flag_under_hidden_scope() {
    let mut canon = gen("leak1");
    let snapshot = canon.clone();
    let bad = ProposedAction {
        action: Action::CreateEvent {
            kind: "secret_pact".to_string(),
            place_id: String::new(),
            actors: Vec::new(),
            causes: Vec::new(),
            effects: vec!["LEAK".to_string()],
            visible_to_player: true,
            scope: Scope::GmPrivate,
            traces: Vec::new(),
        },
        source: "gm".to_string(),
        reason: "leak attempt".to_string(),
        scope: Scope::GmPrivate,
        time_delta: 0,
        confidence: None,
    };
    let err = engine::apply(&mut canon, &bad, 1).unwrap_err();
    assert_eq!(err.code, "hidden_leak");
    assert_eq!(canon, snapshot, "a rejected leak must mutate nothing");

    // The same event under a player-visible scope is accepted.
    engine::apply(
        &mut canon,
        &ProposedAction {
            action: Action::CreateEvent {
                kind: "public_notice".to_string(),
                place_id: String::new(),
                actors: Vec::new(),
                causes: Vec::new(),
                effects: vec!["ok".to_string()],
                visible_to_player: true,
                scope: Scope::Public,
                traces: Vec::new(),
            },
            source: "gm".to_string(),
            reason: "notice".to_string(),
            scope: Scope::Public,
            time_delta: 0,
            confidence: None,
        },
        1,
    )
    .expect("a player-visible scope accepts the visible flag");
}

/// A CreateEvent whose `visible_to_player=false` but with a hidden scope is fine,
/// and player_view never surfaces it (defence-in-depth against the leak).
#[test]
fn player_visible_gate_is_scope_only() {
    let mut canon = gen("leak2");
    // A GmPrivate event with visible_to_player=false (the legitimate hidden case).
    engine::apply(
        &mut canon,
        &propose(Action::CreateEvent {
            kind: "hidden_plot".to_string(),
            place_id: String::new(),
            actors: Vec::new(),
            causes: Vec::new(),
            effects: vec!["HIDDEN_PLOT_DETAIL".to_string()],
            visible_to_player: false,
            scope: Scope::GmPrivate,
            traces: Vec::new(),
        }),
        1,
    )
    .unwrap();
    let visible = canon.event_log.player_visible();
    assert!(
        visible.iter().all(|e| e.scope.visible_to_player()),
        "player_visible() returns only player-visible scopes"
    );
    let blob = serde_json::to_string(&engine::player_view(&canon)).unwrap();
    assert!(!blob.contains("HIDDEN_PLOT_DETAIL"));
}

/// ScheduleEvent validates actor refs: a non-existent actor is rejected.
#[test]
fn schedule_event_validates_actor_refs() {
    let mut canon = gen("sched1");
    let now = canon.clock_minutes;
    let err = engine::apply(
        &mut canon,
        &propose(Action::ScheduleEvent {
            kind: "raid".to_string(),
            due_minutes: now + 10,
            place_id: String::new(),
            actors: vec!["ghost_actor".to_string()],
            causes: Vec::new(),
        }),
        1,
    )
    .unwrap_err();
    assert_eq!(err.code, "unknown_actor");
}

/// The generation budget is enforced: a tight `max_transitions_per_turn` caps the
/// interior expansion's edge count.
#[test]
fn lazy_interior_respects_transition_budget() {
    let mut canon = gen("budget1");
    // Clamp the transition budget so at most one interior room (2 edges) is wired.
    canon.gen_budget.max_transitions_per_turn = 2;
    canon.gen_budget.max_rooms_per_turn = 8;

    let road_id = ids_place(&canon, "road");
    walk_to(&mut canon, &road_id);
    let crypt_edge = canon
        .exits_from(&road_id)
        .into_iter()
        .find(|t| {
            canon
                .place(&t.to_place)
                .map(|p| p.has_flag("shell"))
                .unwrap_or(false)
        })
        .map(|t| t.transition_id.clone())
        .unwrap();
    let crypt_id = canon.transition(&crypt_edge).unwrap().to_place.clone();
    let transitions_before = canon.transitions.len();
    engine::apply(
        &mut canon,
        &propose(Action::MovePlayer {
            transition_id: crypt_edge,
        }),
        1,
    )
    .unwrap();
    let interior_rooms = canon
        .places
        .values()
        .filter(|p| p.parent == crypt_id)
        .count();
    // With a 2-edge budget only the first room (forward+back) is wired.
    assert!(
        interior_rooms <= 1,
        "transition budget capped interior rooms"
    );
    assert!(
        canon.transitions.len() - transitions_before <= 2,
        "no more than max_transitions_per_turn edges were added"
    );
}

// =========================================================================
// helpers
// =========================================================================

/// The generated place id for a settlement sub-place by its salt (square /
/// smithy / gate / road), reproducing worldgen's stable id derivation.
fn ids_place(canon: &gml_world::WorldCanon, salt: &str) -> String {
    let settlement_id = canon.settlements.keys().next().unwrap().clone();
    gml_world::canon::ids::stable_id(&canon.world_seed, &settlement_id, "place", salt)
}

/// Walk the player from the start square to an adjacent place by id (one hop).
fn walk_to(canon: &mut gml_world::WorldCanon, target: &str) {
    let tid = canon
        .exits_from(&canon.player_place_id)
        .into_iter()
        .find(|t| t.to_place == target)
        .map(|t| t.transition_id.clone())
        .unwrap_or_else(|| panic!("no direct edge to {target}"));
    engine::apply(
        canon,
        &propose(Action::MovePlayer { transition_id: tid }),
        0,
    )
    .unwrap();
}
