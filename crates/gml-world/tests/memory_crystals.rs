use std::collections::BTreeSet;

use gml_world::{
    Actor, Containment, Faction, MemoryTier, MemoryUnit, MersenneTwister, Place, World,
};

fn test_world() -> World {
    let mut world = World::empty_with_rng(MersenneTwister::from_u128_seed(7));
    world.world_canon.world_seed = "memory-test".to_string();
    world.world_canon.places.insert(
        "tavern".to_string(),
        Place {
            place_id: "tavern".to_string(),
            name: "The Bent Nail".to_string(),
            region_id: "riverlands".to_string(),
            ..Default::default()
        },
    );
    world.world_canon.actors.insert(
        "borin".to_string(),
        Actor {
            actor_id: "borin".to_string(),
            location: Containment::Place {
                place_id: "tavern".to_string(),
            },
            faction_id: "city_guard".to_string(),
            ..Default::default()
        },
    );
    world.world_canon.actors.insert(
        "lysa".to_string(),
        Actor {
            actor_id: "lysa".to_string(),
            location: Containment::Place {
                place_id: "tavern".to_string(),
            },
            ..Default::default()
        },
    );
    world.world_canon.factions.insert(
        "city_guard".to_string(),
        Faction {
            faction_id: "city_guard".to_string(),
            member_ids: vec!["borin".to_string()],
            ..Default::default()
        },
    );
    world
}

fn memory(id: &str, owner_scope: &str, text: &str) -> MemoryUnit {
    MemoryUnit {
        memory_id: id.to_string(),
        owner_scope: owner_scope.to_string(),
        summary: text.to_string(),
        topic_tags: vec!["sentinel".to_string()],
        ..Default::default()
    }
}

fn ids(rows: Vec<serde_json::Value>) -> BTreeSet<String> {
    rows.into_iter()
        .filter_map(|row| {
            row.get("memory_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .collect()
}

#[test]
fn hierarchical_memory_access_matrix_filters_before_ranking() {
    let mut world = test_world();
    for unit in [
        memory("borin_private", "actor:borin", "BORIN_ONLY_SENTINEL"),
        memory("lysa_private", "actor:lysa", "LYSA_ONLY_SENTINEL"),
        memory("tavern_local", "place:tavern", "TAVERN_LOCAL_SENTINEL"),
        memory("region_news", "region:riverlands", "REGION_SENTINEL"),
        memory("guard_secret", "faction:city_guard", "GUARD_SENTINEL"),
        memory("gm_secret", "gm_private", "GM_ONLY_SENTINEL"),
        memory("player_note", "player", "PLAYER_ONLY_SENTINEL"),
        memory("public_news", "public", "PUBLIC_SENTINEL"),
    ] {
        world.add_memory_unit(unit);
    }

    let borin = world.memory_access_for_actor("borin");
    let borin_ids = ids(world.memory_rows_for_access(&borin, "sentinel", 20, false, false));
    assert!(borin_ids.contains("borin_private"));
    assert!(borin_ids.contains("tavern_local"));
    assert!(borin_ids.contains("region_news"));
    assert!(borin_ids.contains("guard_secret"));
    assert!(borin_ids.contains("public_news"));
    assert!(!borin_ids.contains("lysa_private"));
    assert!(!borin_ids.contains("gm_secret"));
    assert!(!borin_ids.contains("player_note"));

    let lysa = world.memory_access_for_actor("lysa");
    let lysa_ids = ids(world.memory_rows_for_access(&lysa, "sentinel", 20, false, false));
    assert!(lysa_ids.contains("lysa_private"));
    assert!(lysa_ids.contains("tavern_local"));
    assert!(lysa_ids.contains("region_news"));
    assert!(lysa_ids.contains("public_news"));
    assert!(!lysa_ids.contains("borin_private"));
    assert!(!lysa_ids.contains("guard_secret"));

    let player = world.memory_access_for_player();
    let player_ids = ids(world.memory_rows_for_access(&player, "sentinel", 20, false, false));
    assert!(player_ids.contains("player_note"));
    assert!(player_ids.contains("public_news"));
    assert!(!player_ids.contains("tavern_local"));
    assert!(!player_ids.contains("region_news"));
    assert!(!player_ids.contains("guard_secret"));
    assert!(!player_ids.contains("gm_secret"));
}

#[test]
fn consumed_memory_keeps_sources_and_removes_default_injection_only() {
    let mut world = test_world();
    world.add_memory_unit(memory("raw_a", "actor:borin", "ambush raw clue one"));
    world.add_memory_unit(memory("raw_b", "actor:borin", "ambush raw clue two"));

    let (crystal_id, consumed) = world.consolidate_memory_unit(MemoryUnit {
        memory_id: "crystal_a".to_string(),
        tier: MemoryTier::Episode,
        owner_scope: "actor:borin".to_string(),
        summary: "Borin remembers the ambush as one episode.".to_string(),
        source_memory_ids: vec!["raw_a".to_string(), "raw_b".to_string()],
        topic_tags: vec!["ambush".to_string()],
        ..Default::default()
    });

    assert_eq!(crystal_id, "crystal_a");
    assert_eq!(consumed, vec!["raw_a".to_string(), "raw_b".to_string()]);
    assert!(world.world_canon.memory.get("raw_a").is_some());
    assert!(world.world_canon.memory.get("raw_b").is_some());
    assert_eq!(
        world.world_canon.memory.get("raw_a").unwrap().consumed_by,
        vec!["crystal_a".to_string()]
    );

    let access = world.memory_access_for_actor("borin");
    let default_ids = ids(world.memory_rows_for_access(&access, "ambush", 20, false, false));
    assert!(default_ids.contains("crystal_a"));
    assert!(!default_ids.contains("raw_a"));
    assert!(!default_ids.contains("raw_b"));

    let debug_ids = ids(world.memory_rows_for_access(&access, "ambush", 20, true, false));
    assert!(debug_ids.contains("crystal_a"));
    assert!(debug_ids.contains("raw_a"));
    assert!(debug_ids.contains("raw_b"));
}

#[test]
fn detailed_cards_are_explicit_tool_payload_only() {
    let mut world = test_world();
    world.add_memory_unit(MemoryUnit {
        memory_id: "detail_card".to_string(),
        owner_scope: "actor:borin".to_string(),
        summary: "Borin knows there is a hidden witness.".to_string(),
        details: "The hidden witness is under protection in the old granary.".to_string(),
        topic_tags: vec!["witness".to_string()],
        ..Default::default()
    });

    let access = world.memory_access_for_actor("borin");
    let short = world.memory_rows_for_access(&access, "witness", 5, false, false);
    assert!(short[0].get("details").is_none());

    let detailed = world.memory_rows_for_access(&access, "witness", 5, false, true);
    assert_eq!(
        detailed[0].get("details").and_then(|v| v.as_str()),
        Some("The hidden witness is under protection in the old granary.")
    );
}

#[test]
fn detail_only_terms_do_not_match_short_memory_recall() {
    let mut world = test_world();
    world.add_memory_unit(MemoryUnit {
        memory_id: "detail_card".to_string(),
        owner_scope: "actor:borin".to_string(),
        summary: "Borin remembers a protected witness.".to_string(),
        details: "ZXQHIDDENLOOKUPSENTINEL under the old granary.".to_string(),
        ..Default::default()
    });

    let access = world.memory_access_for_actor("borin");
    let short = world.memory_rows_for_access(&access, "ZXQHIDDENLOOKUPSENTINEL", 5, false, false);
    assert!(short.is_empty(), "{short:?}");

    let detailed = world.memory_rows_for_access(&access, "ZXQHIDDENLOOKUPSENTINEL", 5, false, true);
    assert_eq!(
        ids(detailed),
        ["detail_card".to_string()].into_iter().collect()
    );
}

#[test]
fn gm_memory_context_is_short_and_access_gated() {
    let mut world = test_world();
    world.world_canon.player_place_id = "tavern".to_string();
    world.add_memory_unit(memory(
        "player_note",
        "player",
        "PLAYER_VISIBLE_SENTINEL remembers Borin's warning.",
    ));
    world.add_memory_unit(memory(
        "local_note",
        "place:tavern",
        "LOCAL_VISIBLE_SENTINEL the tavern regulars keep glancing at the cellar.",
    ));
    world.add_memory_unit(memory(
        "public_note",
        "public",
        "PUBLIC_VISIBLE_SENTINEL travelers discuss a broken caravan.",
    ));
    world.add_memory_unit(memory(
        "actor_secret",
        "actor:borin",
        "BORIN_PRIVATE_SENTINEL should stay behind NPC tools.",
    ));
    world.add_memory_unit(memory(
        "gm_secret",
        "gm_private",
        "GM_PRIVATE_SENTINEL should never enter ordinary GM context.",
    ));
    world.add_memory_unit(memory(
        "true_secret",
        "true_canon",
        "TRUE_CANON_SENTINEL should never enter ordinary GM context.",
    ));
    world.add_memory_unit(MemoryUnit {
        memory_id: "detail_only".to_string(),
        owner_scope: "place:tavern".to_string(),
        summary: "DETAIL_SUMMARY_SENTINEL visible short summary.".to_string(),
        details: "DETAIL_PAYLOAD_SENTINEL hidden card details stay tool-only.".to_string(),
        ..Default::default()
    });

    let context = world.gm_memory_context();
    assert!(
        context.starts_with("Access-gated short memory snapshot"),
        "{context}"
    );
    assert!(context.contains("PLAYER_VISIBLE_SENTINEL"), "{context}");
    assert!(context.contains("LOCAL_VISIBLE_SENTINEL"), "{context}");
    assert!(context.contains("PUBLIC_VISIBLE_SENTINEL"), "{context}");
    assert!(context.contains("DETAIL_SUMMARY_SENTINEL"), "{context}");
    assert!(!context.contains("BORIN_PRIVATE_SENTINEL"), "{context}");
    assert!(!context.contains("GM_PRIVATE_SENTINEL"), "{context}");
    assert!(!context.contains("TRUE_CANON_SENTINEL"), "{context}");
    assert!(!context.contains("DETAIL_PAYLOAD_SENTINEL"), "{context}");
    assert!(!context.contains("player_note"), "{context}");
}

#[test]
fn auto_consolidation_crystallizes_four_hot_sources() {
    let mut world = test_world();
    for idx in 0..4 {
        world.add_memory_unit(MemoryUnit {
            memory_id: format!("raw_{idx}"),
            owner_scope: "actor:borin".to_string(),
            summary: format!("Borin raw scene note {idx}"),
            time_start: idx,
            time_end: idx,
            topic_tags: vec!["scene".to_string()],
            ..Default::default()
        });
    }

    let created = world.auto_consolidate_memory();
    assert_eq!(created.len(), 1);
    let crystal = world
        .world_canon
        .memory
        .get(&created[0])
        .expect("auto crystal exists");
    assert_eq!(crystal.tier, MemoryTier::Episode);
    assert_eq!(crystal.source_memory_ids.len(), 4);
    assert_eq!(crystal.created_by, "memory_crystal_auto");

    let access = world.memory_access_for_actor("borin");
    let default_ids =
        ids(world.memory_rows_for_access(&access, "Borin raw scene", 20, false, false));
    assert!(default_ids.contains(&created[0]));
    assert!(!default_ids.contains("raw_0"));

    let raw = world.world_canon.memory.get("raw_0").unwrap();
    assert_eq!(raw.consumed_by, vec![created[0].clone()]);
}

#[test]
fn auto_consolidation_does_not_mix_truth_or_visibility_classes() {
    let mut world = test_world();
    for idx in 0..3 {
        world.add_memory_unit(MemoryUnit {
            memory_id: format!("claim_{idx}"),
            owner_scope: "actor:borin".to_string(),
            summary: format!("Borin claim note {idx}"),
            time_start: idx,
            truth_status: gml_world::MemoryTruthStatus::Claim,
            ..Default::default()
        });
    }
    world.add_memory_unit(MemoryUnit {
        memory_id: "actual_0".to_string(),
        owner_scope: "actor:borin".to_string(),
        summary: "Borin actual note".to_string(),
        time_start: 4,
        truth_status: gml_world::MemoryTruthStatus::Actual,
        ..Default::default()
    });

    assert!(world.auto_consolidate_memory().is_empty());

    world.add_memory_unit(MemoryUnit {
        memory_id: "claim_3".to_string(),
        owner_scope: "actor:borin".to_string(),
        summary: "Borin claim note 3".to_string(),
        time_start: 5,
        truth_status: gml_world::MemoryTruthStatus::Claim,
        ..Default::default()
    });
    let created = world.auto_consolidate_memory();
    assert_eq!(created.len(), 1);
    let crystal = world.world_canon.memory.get(&created[0]).unwrap();
    assert_eq!(crystal.truth_status, gml_world::MemoryTruthStatus::Claim);
    assert!(crystal
        .source_memory_ids
        .iter()
        .all(|id| id.starts_with("claim_")));
}
