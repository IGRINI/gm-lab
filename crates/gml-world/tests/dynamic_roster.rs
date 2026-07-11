//! Unit tests for `World::dynamic_roster_context` (GM_CONTEXT_TZ §3): the
//! deterministic, capped NPC roster the world snapshot and `read_state(scene)`
//! use. It must select ONLY NPCs that are (1) present in the scene, (2) at the
//! player's place or one transition away, (3) alive story-seed NPCs, or (4)
//! recently contacted — deduped, capped at 15 with an offscreen note — and must
//! consume no RNG / mutate nothing.

use std::collections::BTreeSet;

use gml_world::{Actor, Containment, MersenneTwister, Npc, Place, Provenance, Transition, World};
use serde_json::json;

fn npc(id: &str, life_status: &str) -> Npc {
    serde_json::from_value(json!({
        "npc_id": id,
        "name": format!("{id}_internal"),
        "persona": "",
        "voice": "",
        "goals": "",
        "knowledge": "",
        "secret": "",
        "role": "роль",
        "life_status": life_status,
    }))
    .expect("npc from json")
}

fn actor_at(id: &str, place: &str, provenance: Provenance) -> Actor {
    Actor {
        actor_id: id.to_string(),
        location: Containment::Place {
            place_id: place.to_string(),
        },
        provenance,
        ..Default::default()
    }
}

/// Player at `tavern`; `yard` is one transition away; `farlands` is unreachable.
fn base_world() -> World {
    let mut world = World::empty_with_rng(MersenneTwister::from_u128_seed(7));
    world.world_canon.world_seed = "roster-test".to_string();
    world.world_canon.player_place_id = "tavern".to_string();

    world.world_canon.places.insert(
        "tavern".to_string(),
        Place {
            place_id: "tavern".to_string(),
            name: "Таверна".to_string(),
            transition_ids: vec!["to_yard".to_string()],
            ..Default::default()
        },
    );
    world.world_canon.places.insert(
        "yard".to_string(),
        Place {
            place_id: "yard".to_string(),
            name: "Двор".to_string(),
            ..Default::default()
        },
    );
    world.world_canon.places.insert(
        "farlands".to_string(),
        Place {
            place_id: "farlands".to_string(),
            name: "Дальние земли".to_string(),
            ..Default::default()
        },
    );
    world.world_canon.transitions.insert(
        "to_yard".to_string(),
        Transition {
            transition_id: "to_yard".to_string(),
            from_place: "tavern".to_string(),
            to_place: "yard".to_string(),
            ..Default::default()
        },
    );
    world
}

#[test]
fn selects_present_nearby_seed_and_recent_but_excludes_others() {
    let mut world = base_world();

    // (1) present in the scene.
    world.npcs.insert("innkeeper".to_string(), npc("innkeeper", "alive"));
    world
        .world_canon
        .actors
        .insert("innkeeper".to_string(), actor_at("innkeeper", "tavern", Provenance::default()));
    world.scene.present_npcs.insert("innkeeper".to_string());

    // (2) at the player's place (not present) and one transition away.
    world.npcs.insert("smith".to_string(), npc("smith", "alive"));
    world
        .world_canon
        .actors
        .insert("smith".to_string(), actor_at("smith", "tavern", Provenance::default()));
    world.npcs.insert("guard".to_string(), npc("guard", "alive"));
    world
        .world_canon
        .actors
        .insert("guard".to_string(), actor_at("guard", "yard", Provenance::default()));

    // (3) alive story-seed NPC, far away.
    world.npcs.insert("elder".to_string(), npc("elder", "alive"));
    world
        .world_canon
        .actors
        .insert("elder".to_string(), actor_at("elder", "farlands", Provenance::seed()));

    // (3-negative) DEAD seed NPC, far away — must be excluded.
    world.npcs.insert("ghost".to_string(), npc("ghost", "dead"));
    world
        .world_canon
        .actors
        .insert("ghost".to_string(), actor_at("ghost", "farlands", Provenance::seed()));

    // (4) recently contacted, far away, not seed.
    world.npcs.insert("merchant".to_string(), npc("merchant", "alive"));
    world
        .world_canon
        .actors
        .insert("merchant".to_string(), actor_at("merchant", "farlands", Provenance::default()));

    // (excluded) far, non-seed, not present/nearby/recent.
    world.npcs.insert("wanderer".to_string(), npc("wanderer", "alive"));
    world
        .world_canon
        .actors
        .insert("wanderer".to_string(), actor_at("wanderer", "farlands", Provenance::default()));

    let before = world.world_canon.clone();
    let recent: BTreeSet<String> = ["merchant".to_string()].into_iter().collect();
    let roster = world.dynamic_roster_context(&recent);

    for id in ["innkeeper", "smith", "guard", "elder", "merchant"] {
        assert!(roster.contains(&format!("id={id}")), "must include {id}: {roster}");
    }
    for id in ["ghost", "wanderer"] {
        assert!(!roster.contains(&format!("id={id}")), "must exclude {id}: {roster}");
    }
    // Pure read: dynamic_roster_context mutates nothing.
    assert_eq!(world.world_canon, before, "roster build must not mutate canon");
}

#[test]
fn empty_selection_renders_none() {
    let mut world = base_world();
    // An NPC that exists but is far, non-seed, not present/recent.
    world.npcs.insert("wanderer".to_string(), npc("wanderer", "alive"));
    world
        .world_canon
        .actors
        .insert("wanderer".to_string(), actor_at("wanderer", "farlands", Provenance::default()));

    let roster = world.dynamic_roster_context(&BTreeSet::new());
    assert_eq!(roster, "(none)");
}

#[test]
fn caps_at_15_lines_with_offscreen_note() {
    let mut world = base_world();
    // 20 present NPCs → over the cap of 15.
    for i in 0..20 {
        let id = format!("npc{i:02}");
        world.npcs.insert(id.clone(), npc(&id, "alive"));
        world
            .world_canon
            .actors
            .insert(id.clone(), actor_at(&id, "tavern", Provenance::default()));
        world.scene.present_npcs.insert(id.clone());
    }

    let roster = world.dynamic_roster_context(&BTreeSet::new());
    let roster_lines = roster.lines().filter(|l| l.starts_with("- id=")).count();
    assert_eq!(roster_lines, 15, "roster must cap at 15 id lines: {roster}");
    assert!(
        roster.contains("+5 offscreen") && roster.contains("read_state(roster)"),
        "over-cap roster must note the offscreen count: {roster}"
    );
}

#[test]
fn full_roster_context_lists_every_card() {
    let mut world = base_world();
    world.npcs.insert("a".to_string(), npc("a", "alive"));
    world.npcs.insert("b".to_string(), npc("b", "alive"));
    let full = world.full_roster_context();
    assert!(full.contains("id=a") && full.contains("id=b"), "full roster lists all: {full}");
    // No canon actors needed: full roster is card-driven.
    let empty = World::empty_with_rng(MersenneTwister::from_u128_seed(1)).full_roster_context();
    assert_eq!(empty, "(none)");
}

/// Review fix: under the cap, PRIORITY (present > recent > nearby > seed) decides
/// who survives — not lexicographic id order. 16 distant seed NPCs with small ids
/// must not evict a present NPC and a recently-contacted one whose ids sort last.
#[test]
fn cap_keeps_priority_npcs_over_lexicographic_seeds() {
    let mut world = base_world();
    for i in 0..16 {
        let id = format!("a{i:02}");
        world.npcs.insert(id.clone(), npc(&id, "alive"));
        world
            .world_canon
            .actors
            .insert(id.clone(), actor_at(&id, "farlands", Provenance::seed()));
    }
    // Present in scene, id sorts AFTER every seed id.
    world.npcs.insert("zz_present".to_string(), npc("zz_present", "alive"));
    world.world_canon.actors.insert(
        "zz_present".to_string(),
        actor_at("zz_present", "tavern", Provenance::default()),
    );
    world.scene.present_npcs.insert("zz_present".to_string());
    // Recently contacted, off-scene, id also sorts last.
    world.npcs.insert("zz_recent".to_string(), npc("zz_recent", "alive"));
    world.world_canon.actors.insert(
        "zz_recent".to_string(),
        actor_at("zz_recent", "farlands", Provenance::default()),
    );
    let mut recent = BTreeSet::new();
    recent.insert("zz_recent".to_string());

    let roster = world.dynamic_roster_context(&recent);
    assert!(
        roster.contains("id=zz_present"),
        "present NPC must survive the cap: {roster}"
    );
    assert!(
        roster.contains("id=zz_recent"),
        "recently-contacted NPC must survive the cap: {roster}"
    );
    assert!(roster.contains("offscreen"), "cap note still present: {roster}");
}

/// Review fix: the seed filter follows the engine's own in-play convention —
/// only "dead" drops an NPC; wounded/missing statuses stay listed.
#[test]
fn seed_filter_drops_only_dead() {
    let mut world = base_world();
    world.npcs.insert("wounded".to_string(), npc("wounded", "ранен"));
    world
        .world_canon
        .actors
        .insert("wounded".to_string(), actor_at("wounded", "farlands", Provenance::seed()));
    world.npcs.insert("corpse".to_string(), npc("corpse", "dead"));
    world
        .world_canon
        .actors
        .insert("corpse".to_string(), actor_at("corpse", "farlands", Provenance::seed()));

    let roster = world.dynamic_roster_context(&BTreeSet::new());
    assert!(
        roster.contains("id=wounded"),
        "non-dead seed NPC stays in the roster: {roster}"
    );
    assert!(
        !roster.contains("id=corpse"),
        "dead seed NPC must be dropped: {roster}"
    );
}
