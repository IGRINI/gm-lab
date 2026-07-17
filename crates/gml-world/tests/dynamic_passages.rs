use gml_world::canon::engine;
use gml_world::{Action, PassageDirectionality, Place, ProposedAction, Provenance, WorldCanon};

fn place(place_id: &str, visited: bool) -> Place {
    let mut place = Place {
        place_id: place_id.to_string(),
        name: format!("Place {place_id}"),
        kind: "scene".to_string(),
        default_description: format!("Description for {place_id}"),
        features: vec![format!("feature:{place_id}")],
        provenance: Provenance::seed(),
        ..Default::default()
    };
    if visited {
        place.mark_visited();
    }
    place
}

fn canon_with_places() -> WorldCanon {
    let mut canon = WorldCanon {
        world_seed: "dynamic-passages-test".to_string(),
        player_place_id: "origin".to_string(),
        clock_minutes: 100,
        ..Default::default()
    };
    canon.insert_place(place("origin", true));
    canon.insert_place(place("destination", false));
    canon
}

fn create_passage(
    canon: &mut WorldCanon,
    passage_id: &str,
    forward_transition_id: &str,
    reverse_transition_id: &str,
    directionality: PassageDirectionality,
) {
    engine::apply(
        canon,
        &ProposedAction::new(
            Action::CreatePassage {
                passage_id: passage_id.to_string(),
                directionality,
                forward_transition_id: forward_transition_id.to_string(),
                reverse_transition_id: reverse_transition_id.to_string(),
                from_place: "origin".to_string(),
                to_place: "destination".to_string(),
                forward_label: "Go to destination".to_string(),
                reverse_label: if directionality == PassageDirectionality::Bidirectional {
                    "Return to origin".to_string()
                } else {
                    String::new()
                },
                kind: "door".to_string(),
                time_cost: 3,
                risk: "none".to_string(),
            },
            "test",
            "the physical passage was established",
        ),
        1,
    )
    .expect("valid passage");
}

fn place_without_graph(place: &Place) -> Place {
    let mut copy = place.clone();
    copy.transition_ids.clear();
    copy.event_ids.clear();
    copy
}

#[test]
fn one_off_relocation_moves_and_spends_time_without_creating_geography() {
    let mut canon = canon_with_places();
    let transitions_before = canon.transitions.clone();

    let events = engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::RelocatePlayer {
                destination_place_id: "destination".to_string(),
                elapsed_minutes: 12,
            },
            "gm",
            "the ferryman carries the player across once",
        ),
        4,
    )
    .expect("valid one-off relocation");

    assert_eq!(canon.player_place_id, "destination");
    assert_eq!(canon.clock_minutes, 112);
    assert!(canon.place("destination").unwrap().is_visited());
    assert_eq!(canon.transitions, transitions_before);
    assert_eq!(events[0].kind, "relocate_player");
    assert!(events[0]
        .effects
        .contains(&"reusable_passage:none".to_string()));
}

#[test]
fn bidirectional_passage_is_created_atomically_without_rewriting_place_cards() {
    let mut canon = canon_with_places();
    let origin_before = place_without_graph(canon.place("origin").unwrap());
    let destination_before = place_without_graph(canon.place("destination").unwrap());

    create_passage(
        &mut canon,
        "courtyard_door",
        "courtyard_door_out",
        "courtyard_door_back",
        PassageDirectionality::Bidirectional,
    );

    let forward = canon.transition("courtyard_door_out").unwrap();
    let reverse = canon.transition("courtyard_door_back").unwrap();
    assert_eq!(forward.passage_id, "courtyard_door");
    assert_eq!(reverse.passage_id, "courtyard_door");
    assert_eq!(forward.directionality, PassageDirectionality::Bidirectional);
    assert_eq!(reverse.directionality, PassageDirectionality::Bidirectional);
    assert_eq!(forward.from_place, reverse.to_place);
    assert_eq!(forward.to_place, reverse.from_place);
    assert_eq!(forward.time_cost, reverse.time_cost);
    assert_eq!(forward.risk, reverse.risk);
    assert_eq!(
        place_without_graph(canon.place("origin").unwrap()),
        origin_before
    );
    assert_eq!(
        place_without_graph(canon.place("destination").unwrap()),
        destination_before
    );
}

#[test]
fn invalid_passage_creation_is_rejected_without_partial_edge() {
    let mut canon = canon_with_places();
    let before = canon.clone();
    let rejection = engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::CreatePassage {
                passage_id: "broken_passage".to_string(),
                directionality: PassageDirectionality::Bidirectional,
                forward_transition_id: "broken_forward".to_string(),
                reverse_transition_id: String::new(),
                from_place: "origin".to_string(),
                to_place: "destination".to_string(),
                forward_label: "Go".to_string(),
                reverse_label: "Return".to_string(),
                kind: "path".to_string(),
                time_cost: 2,
                risk: "none".to_string(),
            },
            "test",
            "invalid fixture",
        ),
        1,
    )
    .expect_err("missing reverse transition id must reject");

    assert_eq!(rejection.code, "invalid_reverse_transition_id");
    assert_eq!(canon, before);
}

#[test]
fn passage_state_uses_exact_identity_and_updates_both_bidirectional_sides() {
    let mut canon = canon_with_places();
    create_passage(
        &mut canon,
        "shared_window",
        "window_out",
        "window_back",
        PassageDirectionality::Bidirectional,
    );
    create_passage(
        &mut canon,
        "separate_one_way_drop",
        "drop_out",
        "",
        PassageDirectionality::OneWay,
    );

    let close_events = engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::SetPassageState {
                transition_id: "window_out".to_string(),
                open: false,
                state_reason: "the window was boarded shut".to_string(),
            },
            "gm",
            "the window was boarded shut",
        ),
        2,
    )
    .expect("close bidirectional passage");

    for transition_id in ["window_out", "window_back"] {
        let transition = canon.transition(transition_id).unwrap();
        assert!(!transition.passable);
        assert_eq!(transition.blocked_by, "the window was boarded shut");
    }
    let independent = canon.transition("drop_out").unwrap();
    assert!(independent.passable);
    assert!(independent.blocked_by.is_empty());
    assert_eq!(close_events[0].kind, "set_passage_state");

    engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::SetPassageState {
                transition_id: "window_back".to_string(),
                open: true,
                state_reason: "the boards were removed".to_string(),
            },
            "gm",
            "the boards were removed",
        ),
        3,
    )
    .expect("reopen bidirectional passage from either side");

    for transition_id in ["window_out", "window_back"] {
        let transition = canon.transition(transition_id).unwrap();
        assert!(transition.passable);
        assert!(transition.blocked_by.is_empty());
    }
    assert!(canon.transition("drop_out").unwrap().passable);
    assert_eq!(
        canon.transitions.len(),
        3,
        "state changes never delete edges"
    );
}
