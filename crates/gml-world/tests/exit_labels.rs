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
    assert_eq!(normalize_slug_like("doki_seroy_gavani"), "doki_seroy_gavani");
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
fn set_scene_string_exit_with_arrow_carries_real_destination() {
    let mut world = World::from_worldgen_with_lore(
        &WorldSpec::from_seed("exit-labels"),
        gml_world::canon::WorldLore::default(),
    );
    world.set_scene(
        "Зал таверны",
        "Низкий зал.",
        "tavern_hall",
        &json!(["хозяйка"]),
        &json!([]),
        &json!(["двор к пирсу -> pyer_ryadom", "служебная дверь"]),
        &json!([]),
        "",
    );
    let exits = &world.scene.exits;
    let arrow = exits
        .iter()
        .find(|e| e.name == "двор к пирсу")
        .expect("arrow exit split into a clean label");
    assert_eq!(arrow.destination, "pyer_ryadom");
    let plain = exits
        .iter()
        .find(|e| e.name == "служебная дверь")
        .expect("plain exit kept whole");
    assert_eq!(plain.destination, "unknown destination");
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
fn lazy_place_is_never_titled_with_placeholder_or_slug() {
    let mut canon = worldgen::generate(&WorldSpec::from_seed("lazy-name"));
    let from = canon.player_place_id.clone();

    // A dangling architect-style exit: placeholder hint + unsplit arrow label
    // (the shape old saves carry in canon).
    canon.insert_transition(Transition {
        transition_id: "arrow_exit".to_string(),
        source_exit_id: "arrow_exit".to_string(),
        from_place: from.clone(),
        to_place: String::new(),
        destination_hint: "unknown destination".to_string(),
        label: "двор к пирсу -> pyer_ryadom_s_tavernoy".to_string(),
        kind: "door".to_string(),
        visible: true,
        passable: true,
        time_cost: 3,
        risk: "none: короткий проход".to_string(),
        provenance: Provenance::by("test", "arrow exit", 0),
        ..Default::default()
    });

    engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::MovePlayer {
                transition_id: "arrow_exit".to_string(),
            },
            "test",
            "walk",
        ),
        1,
    )
    .expect("move through the dangling exit");

    let place = canon
        .place(&canon.player_place_id)
        .expect("lazily materialised place");
    assert_eq!(
        place.name, "двор к пирсу",
        "lazy place takes the cleaned transition label, not the placeholder"
    );

    // A slug destination hint must not become a title either.
    canon.insert_transition(Transition {
        transition_id: "slug_exit".to_string(),
        source_exit_id: "slug_exit".to_string(),
        from_place: canon.player_place_id.clone(),
        to_place: String::new(),
        destination_hint: "doki_seroy_gavani".to_string(),
        label: "вход в доки".to_string(),
        kind: "door".to_string(),
        visible: true,
        passable: true,
        time_cost: 3,
        risk: "none: короткий проход".to_string(),
        provenance: Provenance::by("test", "slug exit", 0),
        ..Default::default()
    });
    engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::MovePlayer {
                transition_id: "slug_exit".to_string(),
            },
            "test",
            "walk",
        ),
        2,
    )
    .expect("move through the slug exit");
    let place = canon
        .place(&canon.player_place_id)
        .expect("second lazily materialised place");
    assert_eq!(place.name, "вход в доки");
}
