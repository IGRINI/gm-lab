//! Phase-1 living-world canon tests (gml-world side, self-contained seed).
//!
//! These prove the additive Place/Transition layer (LIVING_WORLD_ARCHITECTURE_TZ.md
//! §13 "Фаза 1"):
//!   - the canon is derived from the seeded scene with zero RNG use;
//!   - every exit becomes a first-class transition with a canonical target or
//!     an explicit shell;
//!   - the scene is *derivable back* from the canon (byte-identical projection);
//!   - the canon serializes round-trip-stably.
//!
//! Cross-catalog coverage over the three real stories lives in
//! `gml-stories/tests/canon_projection.rs` (gml-world cannot depend on the
//! story catalog without a dependency cycle).

use serde_json::{json, Value};

use gml_world::{World, GENERATOR_VERSION};

/// A self-contained seed: a tavern hall with two NPCs, one item, and three
/// exits (one blocked) — enough to exercise the full Phase-1 derivation.
fn tavern_seed() -> Value {
    json!({
        "id": "test-tavern",
        "title": "Тестовый трактир",
        "public_intro": "Дымный зал придорожного трактира.",
        "hidden_truth": "Под полом спрятан тайник контрабандистов.",
        "npcs": [
            {"id": "borin", "name": "Борин", "persona": "хозяин", "role": "innkeeper"},
            {"id": "lysa", "name": "Лиса", "persona": "служанка", "role": "serving girl"}
        ],
        "scene": {
            "id": "tavern_hall_scene",
            "location_id": "tavern_hall",
            "title": "Зал трактира",
            "description": "Длинный зал с очагом, лавками и тяжёлой дубовой стойкой.",
            "present_npcs": ["borin", "lysa"],
            "items": [
                {"id": "mug", "name": "Глиняная кружка", "location": "на стойке"}
            ],
            "exits": [
                {"id": "north_gate", "name": "Северные ворота", "destination": "village_square"},
                {"id": "cellar_hatch", "name": "Люк в погреб", "destination": "tavern_cellar",
                 "blocked_by": "тяжёлый засов"},
                {"id": "kitchen_door", "name": "Дверь на кухню", "destination": "tavern_kitchen",
                 "visible": false}
            ]
        }
    })
}

fn seeded_world() -> World {
    // Fixed dice seed: deterministic, and lets us assert provenance world_seed.
    World::from_seed_with_dice_seed(&tavern_seed(), 20260622)
}

#[test]
fn canon_is_derived_from_seed() {
    let world = seeded_world();
    let canon = &world.world_canon;

    assert!(!canon.is_empty(), "canon must be derived at seed time");
    assert_eq!(canon.generator_version, GENERATOR_VERSION);
    assert_eq!(
        canon.world_seed, "20260622",
        "world_seed records the dice seed"
    );

    // Exactly one place — the starting location — keyed by the scene location_id.
    assert_eq!(canon.places.len(), 1);
    let place = canon
        .place("tavern_hall")
        .expect("place keyed by scene.location_id");
    assert_eq!(place.name, "Зал трактира");
    assert_eq!(place.default_description, world.scene.description);
    assert!(place.is_visited(), "the starting place is visited");
    assert_eq!(place.provenance.origin, "seed");

    // Occupants mirror present_npcs.
    let occupants: Vec<&String> = place.occupant_ids.iter().collect();
    assert_eq!(occupants, vec!["borin", "lysa"]);
    // Item links mirror scene items.
    assert_eq!(place.item_ids, vec!["mug".to_string()]);
}

#[test]
fn every_exit_becomes_a_transition() {
    let world = seeded_world();
    let canon = &world.world_canon;

    // One transition per exit, count matches (guards against id collisions
    // collapsing the BTreeMap).
    assert_eq!(canon.transitions.len(), world.scene.exits.len());
    assert_eq!(canon.transitions.len(), 3);

    let place = canon.place("tavern_hall").unwrap();
    assert_eq!(place.transition_ids.len(), 3);

    for exit in &world.scene.exits {
        let t = canon
            .transition(&exit.exit_id)
            .unwrap_or_else(|| panic!("transition for exit {}", exit.exit_id));
        assert_eq!(t.from_place, "tavern_hall");
        assert_eq!(t.label, exit.name);
        assert_eq!(t.destination_hint, exit.destination);
        assert_eq!(t.visible, exit.visible);
        assert_eq!(t.blocked_by, exit.blocked_by);
        // Phase 1: targets are unresolved shells; the freetext destination is
        // preserved for later resolution.
        assert!(t.is_shell(), "Phase 1 transitions are shells");
        // Passability is derived from the legacy blocker.
        assert_eq!(t.passable, exit.blocked_by.is_empty());
    }

    // The blocked exit is recorded as not passable.
    let cellar = canon.transition("cellar_hatch").unwrap();
    assert!(!cellar.passable);
    assert_eq!(cellar.blocked_by, "тяжёлый засов");
}

#[test]
fn current_view_projection_is_byte_identical_to_scene_export() {
    let mut world = seeded_world();

    // The live scene_export bytes (what the UI / SCENE_UPDATE event consume).
    let original = world.scene_export();
    let original_str = serde_json::to_string(&original).unwrap();

    // Rebuild the scene purely from the canon (structural fields) plus the
    // carried-over ephemeral view fields, then export again.
    let view = world.build_current_view();
    let saved = std::mem::replace(&mut world.scene, view);
    let rebuilt = world.scene_export();
    world.scene = saved; // restore

    let rebuilt_str = serde_json::to_string(&rebuilt).unwrap();
    assert_eq!(
        original_str, rebuilt_str,
        "scene projected from canon must be byte-identical to the live scene"
    );
}

#[test]
fn canon_payload_round_trips() {
    let world = seeded_world();
    let canon = &world.world_canon;

    let payload = serde_json::to_value(canon).expect("serialize canon");
    let restored: gml_world::WorldCanon =
        serde_json::from_value(payload).expect("deserialize canon");
    assert_eq!(&restored, canon, "canon must round-trip through serde");
}

#[test]
fn duplicate_exit_ids_do_not_collapse_or_desync() {
    // A seed whose exits share an id (safe_id can collapse distinct labels to
    // the same id). The canon must keep one edge per exit AND still project a
    // byte-identical scene.
    let seed = json!({
        "id": "dup",
        "title": "Дубли",
        "public_intro": "Перекрёсток одинаковых дверей.",
        "npcs": [{"id": "a", "name": "А", "role": "npc"}],
        "scene": {
            "location_id": "crossroads",
            "title": "Перекрёсток",
            "description": "Три двери с одинаковыми табличками.",
            "present_npcs": ["a"],
            "exits": [
                {"id": "door", "name": "Левая дверь", "destination": "left"},
                {"id": "door", "name": "Правая дверь", "destination": "right"},
                {"id": "door", "name": "Дальняя дверь", "destination": "far", "visible": false}
            ]
        }
    });
    let mut world = World::from_seed_with_dice_seed(&seed, 7);

    assert_eq!(
        world.scene.exits.len(),
        3,
        "all colliding exits survive in the scene"
    );
    assert_eq!(
        world.world_canon.transitions.len(),
        3,
        "no BTreeMap collapse: one transition per exit"
    );
    // Look up by the actual derived location_id (normalize_seed may derive it
    // from the title when the scene omits `items`).
    let place = world
        .world_canon
        .place(&world.scene.location_id)
        .expect("starting place keyed by scene.location_id");
    assert_eq!(place.transition_ids.len(), 3);
    // All edges trace back to the single source exit id, with unique edge ids.
    for t in world.world_canon.transitions.values() {
        assert_eq!(t.source_exit_id, "door");
    }

    // The projection is still byte-identical despite the id collision.
    let original = serde_json::to_string(&world.scene_export()).unwrap();
    let view = world.build_current_view();
    let saved = std::mem::replace(&mut world.scene, view);
    let rebuilt = serde_json::to_string(&world.scene_export()).unwrap();
    world.scene = saved;
    assert_eq!(
        original, rebuilt,
        "duplicate-id seed must still round-trip exactly"
    );
}

#[test]
fn canon_content_is_rng_independent() {
    // Phase-1 done-criterion (TZ §13): canon derivation consumes zero RNG.
    // Two worlds from the SAME scene seed but DIFFERENT dice seeds must yield
    // byte-identical canon apart from the provenance `world_seed` label.
    let a = World::from_seed_with_dice_seed(&tavern_seed(), 1);
    let b = World::from_seed_with_dice_seed(&tavern_seed(), 999);
    let strip = |world: &World| {
        let mut v = serde_json::to_value(&world.world_canon).unwrap();
        v["world_seed"] = json!("");
        v
    };
    assert_eq!(
        strip(&a),
        strip(&b),
        "canon content must not depend on the dice seed (zero RNG entropy)"
    );
    assert_eq!(a.world_canon.world_seed, "1");
    assert_eq!(b.world_canon.world_seed, "999");
}

#[test]
fn empty_world_has_empty_canon() {
    // A world built via the __new__-style bypass (no seed) has no canon.
    let world = World::empty_with_rng(gml_world::MersenneTwister::from_u128_seed(1));
    assert!(world.world_canon.is_empty());
}

// =========================================================================
// LOCKED DECISION #1: the scene is a DERIVED cache built FROM the canon;
// build_current_view / refresh_scene_from_canon anchor on canon.player_place_id.
// =========================================================================

#[test]
fn build_current_view_anchors_on_canon_player_place() {
    use gml_world::canon::action::{Action, ProposedAction};
    use gml_world::canon::engine;

    let mut world = seeded_world();
    let start = world.world_canon.player_place_id.clone();
    assert_eq!(world.scene.location_id, start);

    // Move the player through the canon (north_gate -> a lazily materialised
    // place). The legacy scene is now STALE until refreshed.
    let tid = world
        .world_canon
        .exits_from(&start)
        .into_iter()
        .find(|t| t.visible && t.passable)
        .map(|t| t.transition_id.clone())
        .expect("an open exit");
    engine::apply(
        &mut world.world_canon,
        &ProposedAction::new(Action::MovePlayer { transition_id: tid }, "gm", "move"),
        1,
    )
    .unwrap();
    let new_place = world.world_canon.player_place_id.clone();
    assert_ne!(new_place, start, "the canon player place moved");

    // The view is anchored on the NEW canon place, not the stale scene.location_id.
    let view = world.build_current_view();
    assert_eq!(view.location_id, new_place);

    // refresh_scene_from_canon makes the live scene reflect the canon.
    world.refresh_scene_from_canon();
    assert_eq!(world.scene.location_id, new_place);
    let exported = world.scene_export();
    assert_eq!(exported["location_id"], serde_json::json!(new_place));
}

#[test]
fn refresh_scene_reflects_present_actors_from_canon() {
    use gml_world::canon::action::{Action, ProposedAction};
    use gml_world::canon::engine;

    let mut world = seeded_world();
    let start = world.world_canon.player_place_id.clone();
    // Both seed NPCs present at start.
    assert!(world.scene.present_npcs.contains("borin"));
    assert!(world.scene.present_npcs.contains("lysa"));

    // Move 'borin' elsewhere in the canon, then refresh: present_npcs follows.
    // First create a destination place + put borin there.
    engine::apply(
        &mut world.world_canon,
        &ProposedAction::new(
            Action::CreatePlace {
                place_id: "kitchen".to_string(),
                name: "Кухня".to_string(),
                kind: String::new(),
                parent: String::new(),
                region_id: String::new(),
                description: "Кухня".to_string(),
                features: Vec::new(),
                visited: false,
                shell: false,
            },
            "gm",
            "kitchen",
        ),
        1,
    )
    .unwrap();
    engine::apply(
        &mut world.world_canon,
        &ProposedAction::new(
            Action::MoveActor {
                actor_id: "borin".to_string(),
                to_place: "kitchen".to_string(),
            },
            "gm",
            "borin leaves",
        ),
        1,
    )
    .unwrap();

    world.refresh_scene_from_canon();
    assert_eq!(world.world_canon.player_place_id, start, "player stayed");
    assert!(
        !world.scene.present_npcs.contains("borin"),
        "borin left, so the derived scene drops him"
    );
    assert!(
        world.scene.present_npcs.contains("lysa"),
        "lysa is still present"
    );
}

// =========================================================================
// LOCKED DECISION #4: World::from_worldgen derives a legacy-facing World from a
// procedurally-generated canon.
// =========================================================================

#[test]
fn from_worldgen_derives_legacy_world_from_canon() {
    use gml_world::canon::WorldSpec;

    let world = World::from_worldgen_with_dice_seed(&WorldSpec::from_seed("42"), 42);

    // The canon is populated and authoritative.
    assert!(!world.world_canon.is_empty());
    assert_eq!(world.story_id, "procedural");
    assert_eq!(world.time.absolute_minutes, 480);
    assert_eq!(world.time_export()["time_of_day"], "08:00");
    assert_eq!(world.world_canon.clock_minutes, world.time.absolute_minutes);
    assert!(
        world.world_canon.world_lore.is_empty(),
        "plain worldgen must not infer heuristic lore"
    );
    let player_place = world.world_canon.player_place_id.clone();
    assert!(!player_place.is_empty());

    // The scene is DERIVED from the canon's start place.
    assert_eq!(world.scene.location_id, player_place);
    let canon_place = world.world_canon.place(&player_place).unwrap();
    assert_eq!(world.scene.title, canon_place.name);
    assert_eq!(world.scene.description, canon_place.default_description);

    // Procedural worlds seed ZERO actors, so the derived roster is empty:
    // significant NPCs are generated lazily at play time, not hardcoded.
    assert!(
        world.world_canon.actors.is_empty(),
        "procedural canon seeds no actors"
    );
    // Still one NPC card per canon actor (0 == 0) — the derive invariant holds.
    assert_eq!(world.npcs.len(), world.world_canon.actors.len());
    assert!(world.npcs.is_empty(), "procedural roster starts empty");

    // With no actors at the start place, the derived scene has no present NPCs.
    assert!(
        world.world_canon.actors_at(&player_place).is_empty(),
        "no actors stand at the start place"
    );
    assert!(world.scene.present_npcs.is_empty());
}

#[test]
fn plain_worldgen_does_not_infer_lore_from_genre() {
    use gml_world::canon::WorldSpec;

    let machine = World::from_worldgen_with_dice_seed(
        &WorldSpec {
            seed: "genre-1".to_string(),
            genre: "postapocalyptic machine world".to_string(),
            tone: "bleak".to_string(),
            scale: "outpost".to_string(),
        },
        1,
    );
    let fantasy = World::from_worldgen_with_dice_seed(
        &WorldSpec {
            seed: "genre-1".to_string(),
            genre: "fantasy isekai".to_string(),
            tone: "hopeful".to_string(),
            scale: "village".to_string(),
        },
        1,
    );

    let machine_lore = &machine.world_canon.world_lore;
    let fantasy_lore = &fantasy.world_canon.world_lore;
    assert!(machine_lore.is_empty(), "{machine_lore:#?}");
    assert!(fantasy_lore.is_empty(), "{fantasy_lore:#?}");
}

#[test]
fn provided_world_lore_populates_canon_lore() {
    use gml_world::canon::{WorldLore, WorldSpec};

    let world = World::from_worldgen_with_lore(
        &WorldSpec {
            seed: "architect-lore".to_string(),
            genre: "fantasy isekai".to_string(),
            tone: "tense".to_string(),
            scale: "region".to_string(),
        },
        WorldLore {
            name: "Город Железных Снов".to_string(),
            public_premise: "Люди живут в тени спящего машинного бога.".to_string(),
            world_image_prompt_en:
                "A vast dieselpunk city built around a sleeping machine god, brass towers, steam haze."
                    .to_string(),
            world_map_prompt_en:
                "A readable isometric city-region map with ring districts, machine temples, rail lines and labeled gates."
                    .to_string(),
            world_image_url: "/image-files/world-run/image_0.png".to_string(),
            world_map_url: "/image-files/world-map-run/image_0.png".to_string(),
            religions: vec!["церковь Спящего Механизма".to_string()],
            gods: vec!["Машинный Бог под городом".to_string()],
            regions: vec!["Нижние кольца города".to_string()],
            location_rules: vec![
                "новые места должны показывать связь с машинным культом".to_string()
            ],
            ..Default::default()
        },
    );

    let lore = &world.world_canon.world_lore;
    assert_eq!(lore.name, "Город Железных Снов");
    assert_eq!(lore.genre, "fantasy isekai");
    assert_eq!(
        lore.world_image_prompt_en,
        "A vast dieselpunk city built around a sleeping machine god, brass towers, steam haze."
    );
    assert_eq!(
        lore.world_map_prompt_en,
        "A readable isometric city-region map with ring districts, machine temples, rail lines and labeled gates."
    );
    assert_eq!(lore.world_image_url, "/image-files/world-run/image_0.png");
    assert_eq!(lore.world_map_url, "/image-files/world-map-run/image_0.png");
    assert!(!lore.lore_id.is_empty());
    let context = world.canon_world_context();
    assert!(context.contains("Город Железных Снов"));
    assert!(context.contains("Religions/creeds"));
    assert!(context.contains("Машинный Бог"));
    assert!(context.contains("Location generation rules"));
    assert!(!context.contains("dieselpunk city"));
    assert!(!context.contains("isometric city-region map"));
    assert!(!context.contains("/image-files/world-run/image_0.png"));
    assert!(!context.contains("/image-files/world-map-run/image_0.png"));
}

#[test]
fn from_worldgen_is_deterministic() {
    use gml_world::canon::WorldSpec;
    let a = World::from_worldgen_with_dice_seed(&WorldSpec::from_seed("det"), 7);
    let b = World::from_worldgen_with_dice_seed(&WorldSpec::from_seed("det"), 7);
    assert_eq!(
        serde_json::to_string(&a.world_canon).unwrap(),
        serde_json::to_string(&b.world_canon).unwrap(),
    );
    assert_eq!(
        a.scene, b.scene,
        "derived scenes are identical for the same seed"
    );
}
