use std::collections::BTreeSet;

use gml_world::{
    Actor, Containment, MersenneTwister, Place, Provenance, Settlement, Transition, World,
};

fn test_world() -> World {
    let mut world = World::empty_with_rng(MersenneTwister::from_u128_seed(11));
    world.world_canon.world_seed = "rumor-test".to_string();
    world.world_canon.player_place_id = "tavern".to_string();
    world.world_canon.settlements.insert(
        "elmstead".to_string(),
        Settlement {
            settlement_id: "elmstead".to_string(),
            name: "Elmstead".to_string(),
            region_id: "north".to_string(),
            ..Default::default()
        },
    );
    for (place_id, name) in [
        ("tavern", "The Copper Mug"),
        ("market", "Market Square"),
        ("road_end", "Roadside Shrine"),
    ] {
        world.world_canon.places.insert(
            place_id.to_string(),
            Place {
                place_id: place_id.to_string(),
                name: name.to_string(),
                parent: "elmstead".to_string(),
                region_id: "north".to_string(),
                ..Default::default()
            },
        );
    }
    world.world_canon.actors.insert(
        "borin".to_string(),
        Actor {
            actor_id: "borin".to_string(),
            location: Containment::Place {
                place_id: "tavern".to_string(),
            },
            ..Default::default()
        },
    );
    world.world_canon.actors.insert(
        "lysa".to_string(),
        Actor {
            actor_id: "lysa".to_string(),
            location: Containment::Place {
                place_id: "road_end".to_string(),
            },
            ..Default::default()
        },
    );
    world.world_canon.actors.insert(
        "mira".to_string(),
        Actor {
            actor_id: "mira".to_string(),
            location: Containment::Place {
                place_id: "tavern".to_string(),
            },
            ..Default::default()
        },
    );
    world.scene.location_id = "tavern".to_string();
    world.scene.present_npcs = set(&["borin", "mira"]);
    world.world_canon.transitions.insert(
        "old_road".to_string(),
        Transition {
            transition_id: "old_road".to_string(),
            source_exit_id: "old_road".to_string(),
            from_place: "tavern".to_string(),
            to_place: "road_end".to_string(),
            label: "Old forest road".to_string(),
            kind: "road".to_string(),
            visible: true,
            passable: true,
            time_cost: 45,
            risk: "wild road with travelers".to_string(),
            provenance: Provenance::by("test", "road", 0),
            ..Default::default()
        },
    );
    world
}

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| (*id).to_string()).collect()
}

fn rendered(rows: Vec<serde_json::Value>) -> String {
    serde_json::to_string(&rows).unwrap()
}

#[test]
fn record_rumor_creates_scoped_memory_without_settlement_leak() {
    let mut world = test_world();
    world.record_rumor(
        1,
        1,
        "borin",
        "TAVERN_ONLY_SENTINEL under the bar.",
        set(&["player", "borin", "mira"]),
        10,
    );

    let rumor = world.rumors.last().expect("rumor stored");
    assert!(rumor.known_in.contains("place:tavern"));
    assert!(rumor.known_in.contains("actor:borin"));
    assert!(!rumor.known_in.contains("settlement:elmstead"));
    assert!(
        world
            .world_canon
            .memory
            .units
            .values()
            .any(|unit| unit.summary.contains("TAVERN_ONLY_SENTINEL")),
        "rumor is mirrored into scoped memory"
    );

    let borin_rows = world.memory_rows_for_access(
        &world.memory_access_for_actor("borin"),
        "TAVERN_ONLY_SENTINEL",
        5,
        false,
        false,
    );
    assert!(rendered(borin_rows).contains("TAVERN_ONLY_SENTINEL"));

    let lysa_rows = world.memory_rows_for_access(
        &world.memory_access_for_actor("lysa"),
        "TAVERN_ONLY_SENTINEL",
        5,
        false,
        false,
    );
    assert!(
        !rendered(lysa_rows).contains("TAVERN_ONLY_SENTINEL"),
        "NPC elsewhere in the same settlement must not know a room-local rumor"
    );
}

#[test]
fn private_direct_rumor_does_not_leak_to_same_place_bystander() {
    let mut world = test_world();
    world.record_rumor(
        1,
        1,
        "borin",
        "PRIVATE_DIRECT_SENTINEL for the player only.",
        set(&["player", "borin"]),
        10,
    );

    let rumor = world.rumors.last().expect("rumor stored");
    assert!(!rumor.known_in.contains("place:tavern"));
    assert_eq!(rumor.origin_scope, "actor:borin");

    let mira_rows = world.memory_rows_for_access(
        &world.memory_access_for_actor("mira"),
        "PRIVATE_DIRECT_SENTINEL",
        5,
        false,
        false,
    );
    assert!(
        !rendered(mira_rows).contains("PRIVATE_DIRECT_SENTINEL"),
        "same-place bystander must not receive a private/direct exchange"
    );

    let player_rows = world.memory_rows_for_access(
        &world.memory_access_for_player(),
        "PRIVATE_DIRECT_SENTINEL",
        5,
        false,
        false,
    );
    assert!(rendered(player_rows).contains("PRIVATE_DIRECT_SENTINEL"));
}

#[test]
fn rumor_carrier_spreads_to_new_place_after_time_passes() {
    let mut world = test_world();
    world.record_rumor(
        1,
        1,
        "borin",
        "CARRIER_SPREAD_SENTINEL near the shrine.",
        set(&["borin"]),
        10,
    );
    world.world_canon.actors.get_mut("borin").unwrap().location = Containment::Place {
        place_id: "road_end".to_string(),
    };

    let changed = world.advance_rumors(61);
    assert_eq!(changed.len(), 1);
    assert!(world.rumors[0].known_in.contains("place:road_end"));

    let lysa_rows = world.memory_rows_for_access(
        &world.memory_access_for_actor("lysa"),
        "CARRIER_SPREAD_SENTINEL",
        5,
        false,
        false,
    );
    assert!(
        rendered(lysa_rows).contains("CARRIER_SPREAD_SENTINEL"),
        "after enough time at the destination, local NPC access can retrieve the spread rumor"
    );
}

#[test]
fn player_travel_on_road_adds_route_scope_for_carried_rumor() {
    let mut world = test_world();
    world.record_rumor(
        1,
        1,
        "borin",
        "ROUTE_SCOPE_SENTINEL beside the old road.",
        set(&["player"]),
        10,
    );
    world.world_canon.clock_minutes = 45;

    let changed = world.spread_rumors_on_transition("player", "old_road", 45);
    assert_eq!(changed.len(), 1);
    assert!(world.rumors[0].known_in.contains("route:old_road"));
    assert_eq!(world.rumors[0].distortion, 1);

    let route_rows = world.memory_rows_for_access(
        &world.memory_access_for_actor("lysa"),
        "ROUTE_SCOPE_SENTINEL",
        5,
        false,
        false,
    );
    assert!(
        rendered(route_rows).contains("ROUTE_SCOPE_SENTINEL"),
        "NPC at a route endpoint can retrieve route-scoped road gossip"
    );
}

#[test]
fn weak_old_rumor_decays_out_of_default_recall() {
    let mut world = test_world();
    world.record_rumor(
        1,
        1,
        "borin",
        "DECAY_SENTINEL old gossip.",
        set(&["borin"]),
        10,
    );

    world.advance_rumors(24 * 60);
    assert_eq!(world.rumors[0].strength, 0);

    let hot_rows = world.memory_rows_for_access(
        &world.memory_access_for_actor("borin"),
        "DECAY_SENTINEL",
        5,
        false,
        false,
    );
    assert!(
        !rendered(hot_rows).contains("DECAY_SENTINEL"),
        "decayed rumor is cold and absent from default recall"
    );

    let debug_rows = world.memory_rows_for_access(
        &world.memory_access_for_actor("borin"),
        "DECAY_SENTINEL",
        5,
        true,
        false,
    );
    assert!(
        rendered(debug_rows).contains("DECAY_SENTINEL"),
        "decayed rumor remains auditable with include_cold"
    );

    let docs = world.retrieval_documents("borin");
    let docs_text = docs
        .iter()
        .map(|doc| doc.contextual_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !docs_text.contains("DECAY_SENTINEL"),
        "RAG corpus must not re-index decayed rumors through the legacy raw rumor list"
    );
}

#[test]
fn rumor_transition_spread_is_deterministic() {
    let mut a = test_world();
    let mut b = test_world();
    for world in [&mut a, &mut b] {
        world.record_rumor(
            1,
            1,
            "borin",
            "DETERMINISTIC_ROUTE_SENTINEL.",
            set(&["player"]),
            10,
        );
        world.world_canon.clock_minutes = 45;
        world.spread_rumors_on_transition("player", "old_road", 45);
    }

    assert_eq!(a.rumors[0].known_in, b.rumors[0].known_in);
    assert_eq!(a.rumors[0].distortion, b.rumors[0].distortion);
    assert_eq!(
        a.world_canon.memory.units, b.world_canon.memory.units,
        "rumor memory projection is replay-stable"
    );
}

#[test]
fn rumor_cap_prunes_mirrored_memory_units() {
    let mut world = test_world();
    world.record_rumor(1, 1, "borin", "OLD_RUMOR_CAP_SENTINEL.", set(&["borin"]), 1);
    world.record_rumor(2, 2, "borin", "NEW_RUMOR_CAP_SENTINEL.", set(&["borin"]), 1);

    let memory_text = serde_json::to_string(&world.world_canon.memory.units).unwrap();
    assert!(!memory_text.contains("OLD_RUMOR_CAP_SENTINEL"));
    assert!(memory_text.contains("NEW_RUMOR_CAP_SENTINEL"));

    world.record_rumor(3, 3, "borin", "ZERO_CAP_SENTINEL.", set(&["borin"]), 0);
    let memory_text = serde_json::to_string(&world.world_canon.memory.units).unwrap();
    assert!(!memory_text.contains("NEW_RUMOR_CAP_SENTINEL"));
    assert!(!memory_text.contains("ZERO_CAP_SENTINEL"));
}
