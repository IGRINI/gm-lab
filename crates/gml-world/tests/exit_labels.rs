//! Scene-exit label handling: the story/world-architect string convention
//! `"label -> location_id"` must split into name/destination at every string
//! coercion point, and a lazily materialised place must never be titled with a
//! coercion placeholder ("unknown destination") or a bare id slug.

use serde_json::json;

use gml_world::canon::action::{Action, ProposedAction};
use gml_world::canon::engine;
use gml_world::canon::worldgen::{self, WorldSpec};
use gml_world::canon::{Provenance, Transition};
use gml_world::helpers::{is_placeholder_destination, split_exit_label};
use gml_world::World;

#[test]
fn split_exit_label_splits_the_arrow_convention() {
    assert_eq!(
        split_exit_label("двор к пирсу -> pyer_ryadom"),
        ("двор к пирсу".to_string(), "pyer_ryadom".to_string())
    );
    // No arrow: the whole string is the label, no target.
    assert_eq!(
        split_exit_label("Рыночная площадь"),
        ("Рыночная площадь".to_string(), String::new())
    );
    // Splits on the LAST arrow.
    assert_eq!(
        split_exit_label("a -> b -> c"),
        ("a -> b".to_string(), "c".to_string())
    );
    // Degenerate halves collapse to the legacy shape.
    assert_eq!(
        split_exit_label("-> doki"),
        ("-> doki".to_string(), String::new())
    );
    assert_eq!(
        split_exit_label("вперёд ->"),
        ("вперёд ->".to_string(), String::new())
    );
}

#[test]
fn slug_like_destinations_lose_stray_foreign_letters() {
    use gml_world::helpers::normalize_slug_like;
    // A model artifact: an ascii slug with a Cyrillic «с» inside.
    assert_eq!(
        normalize_slug_like("pyerс_ryadom_s_tavernoy"),
        "pyer_ryadom_s_tavernoy"
    );
    // Pure-ascii slugs, pure-Cyrillic words and human phrases pass through.
    assert_eq!(
        normalize_slug_like("doki_seroy_gavani"),
        "doki_seroy_gavani"
    );
    assert_eq!(normalize_slug_like("Пирс"), "Пирс");
    assert_eq!(normalize_slug_like("Рыночная площадь"), "Рыночная площадь");
}

#[test]
fn placeholder_destinations_are_recognised() {
    assert!(is_placeholder_destination("unknown destination"));
    assert!(is_placeholder_destination("Unknown Destination"));
    assert!(is_placeholder_destination("неизвестное направление"));
    assert!(!is_placeholder_destination("Пирс"));
    assert!(!is_placeholder_destination(""));
}

#[test]
fn runtime_set_scene_does_not_author_exits() {
    let mut world = World::from_worldgen_with_lore(
        &WorldSpec::from_seed("exit-labels"),
        gml_world::canon::WorldLore::default(),
    );
    let current_place = world.world_canon.player_place_id.clone();
    let before = world.world_canon.transitions.clone();
    world.set_scene(
        "Зал таверны",
        "Низкий зал.",
        &current_place,
        &json!(["хозяйка"]),
        &json!([]),
        &json!(["двор к пирсу -> pyer_ryadom", "служебная дверь"]),
        &json!([]),
        "",
    );
    assert_eq!(
        world.world_canon.transitions, before,
        "runtime scene patches must leave route authoring to the location creator"
    );
}

#[test]
fn set_scene_cannot_create_a_transition_from_location_text() {
    let mut world = World::from_worldgen_with_lore(
        &WorldSpec::from_seed("symmetric-shop-travel"),
        gml_world::canon::WorldLore::default(),
    );
    let alley_id = world.world_canon.player_place_id.clone();
    let shop_id = "aldrick_shop";

    let before = world.world_canon.clone();
    let result = world.set_scene(
        "Внутри лавки Алдрика",
        "Тесная лавка у переулка.",
        shop_id,
        &json!([]),
        &json!([]),
        &json!([]),
        &json!([]),
        "",
    );

    assert_eq!(result["ok"], false);
    assert_eq!(result["code"], "location_change_requires_transition");
    assert_eq!(world.world_canon, before);
    assert_eq!(world.world_canon.player_place_id, alley_id);
    assert!(world.world_canon.place(shop_id).is_none());
}

#[test]
fn seed_scene_string_exit_with_arrow_carries_real_destination() {
    let world = World::from_seed(&json!({
        "title": "Тест",
        "scene": {
            "title": "Зал",
            "description": "Зал.",
            "exits": ["вход в таверну -> doki_seroy_gavani"],
        },
    }));
    let exit = world
        .scene
        .exits
        .iter()
        .find(|e| e.name == "вход в таверну")
        .expect("seed exit split into a clean label");
    assert_eq!(exit.destination, "doki_seroy_gavani");
}

#[test]
fn dangling_exit_text_never_materialises_a_place() {
    let mut canon = worldgen::generate(&WorldSpec::from_seed("lazy-name"));
    let from = canon.player_place_id.clone();
    let original_place_count = canon.places.len();

    for (transition_id, hint, label) in [
        (
            "arrow_exit",
            "unknown destination",
            "двор к пирсу -> pyer_ryadom_s_tavernoy",
        ),
        ("slug_exit", "doki_seroy_gavani", "вход в доки"),
    ] {
        canon.insert_transition(Transition {
            transition_id: transition_id.to_string(),
            source_exit_id: transition_id.to_string(),
            from_place: from.clone(),
            to_place: String::new(),
            destination_hint: hint.to_string(),
            label: label.to_string(),
            kind: "door".to_string(),
            visible: true,
            passable: true,
            time_cost: 3,
            risk: "none".to_string(),
            provenance: Provenance::by("test", "legacy dangling exit", 0),
            ..Default::default()
        });

        let before = canon.clone();
        let rejection = engine::apply(
            &mut canon,
            &ProposedAction::new(
                Action::MovePlayer {
                    transition_id: transition_id.to_string(),
                },
                "test",
                "walk",
            ),
            1,
        )
        .expect_err("dangling transition must be configured before movement");
        assert_eq!(rejection.code, "needs_transition_profile");
        assert_eq!(canon, before);
        assert_eq!(canon.places.len(), original_place_count);
    }
}
