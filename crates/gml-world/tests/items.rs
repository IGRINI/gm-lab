//! Phase-И item tests (`docs/ITEMS_AND_SPELLS_TZ.md` §1): the §И1 name↔desc
//! convention, the §И2 per-place item store (stash on leave / restore on
//! return / set_scene overwrite), and the §И3 take_item/drop_item matching rules
//! (id / visible-name / ambiguity / invisible-only-by-id / non-portable).

use serde_json::{json, Value};

use gml_world::canon::{ids, PassageDirectionality, Place, Provenance, Transition};
use gml_world::helpers::{item_entry_string, item_head, item_tail};
use gml_world::{SceneItem, World};

/// A tavern seed with two items: a VISIBLE non-portable mug and a VISIBLE
/// portable coin — plus an INVISIBLE portable key that is only takeable by id.
fn tavern_seed() -> Value {
    json!({
        "id": "test-tavern",
        "title": "Тестовый трактир",
        "public_intro": "Дымный зал придорожного трактира.",
        "hidden_truth": "Тайник под полом.",
        "npcs": [
            {"id": "borin", "name": "Борин", "persona": "хозяин", "role": "innkeeper"}
        ],
        "scene": {
            "id": "tavern_hall_scene",
            "location_id": "tavern_hall",
            "title": "Зал трактира",
            "description": "Длинный зал с очагом и дубовой стойкой.",
            "present_npcs": ["borin"],
            "items": [
                {"id": "mug", "name": "Глиняная кружка", "location": "на стойке"},
                {"id": "coin", "name": "Медная монета", "location": "на столе",
                 "portable": true, "details": "потёртая, с профилем короля"},
                {"id": "hidden_key", "name": "Ключ", "location": "под половицей",
                 "portable": true, "visible": false}
            ],
            "exits": [
                {"id": "north_gate", "name": "Северные ворота", "destination": "village_square"}
            ]
        }
    })
}

fn seeded_world() -> World {
    World::from_seed_with_dice_seed(&tavern_seed(), 20260622)
}

fn item_ids(w: &World) -> Vec<String> {
    w.scene.items.iter().map(|i| i.item_id.clone()).collect()
}

// --- §И1 name↔description convention ---------------------------------------

#[test]
fn item_convention_head_and_tail_split_on_em_dash() {
    // Head is the part before the FIRST ' — ', trimmed; tail is the rest.
    assert_eq!(item_head("кинжал — 1d4, скрыт в сапоге"), "кинжал");
    assert_eq!(
        item_tail("кинжал — 1d4, скрыт в сапоге"),
        "1d4, скрыт в сапоге"
    );
    // No separator -> whole string is the head, empty tail.
    assert_eq!(item_head("верёвка"), "верёвка");
    assert_eq!(item_tail("верёвка"), "");
    // A bare hyphen is NOT the separator (only ' — ' with spaces + em dash).
    assert_eq!(item_head("сумка-мешок"), "сумка-мешок");
    assert_eq!(item_tail("сумка-мешок"), "");
    // Only the FIRST separator splits; later ' — ' stays in the tail.
    assert_eq!(item_head("меч — острый — очень"), "меч");
    assert_eq!(item_tail("меч — острый — очень"), "острый — очень");
}

#[test]
fn item_entry_string_composes_head_and_tail() {
    assert_eq!(item_entry_string("монета", "потёртая"), "монета — потёртая");
    // Empty details -> just the name (no trailing separator).
    assert_eq!(item_entry_string("монета", ""), "монета");
    assert_eq!(
        item_entry_string("  монета  ", "  потёртая  "),
        "монета — потёртая"
    );
}

// --- §И3 take_item ----------------------------------------------------------

#[test]
fn take_item_by_id_moves_body_into_inventory_with_details() {
    let mut w = seeded_world();
    let before_rev = w.player_character.card_revision;
    let out = w.take_item("coin", "", "поднимаю монету").expect("take ok");
    assert_eq!(out["status"], json!("taken"));
    // Removed from the scene.
    assert!(!item_ids(&w).contains(&"coin".to_string()));
    // Appended to inventory as "name — details" (the §И1 convention).
    assert!(w
        .player_character
        .inventory
        .iter()
        .any(|e| e == "Медная монета — потёртая, с профилем короля"));
    // card_revision bumped.
    assert_eq!(w.player_character.card_revision, before_rev + 1);
}

#[test]
fn take_item_by_visible_name_matches_case_insensitively() {
    let mut w = seeded_world();
    let out = w.take_item("", "медная монета", "беру").expect("take ok");
    assert_eq!(out["name"], json!("Медная монета"));
    assert!(!item_ids(&w).contains(&"coin".to_string()));
}

#[test]
fn take_item_without_details_is_just_the_name() {
    let mut w = seeded_world();
    // The mug has no details; but it is non-portable, so make it portable first
    // via a fresh scene item to isolate the "no details" formatting.
    w.scene.items.push(SceneItem {
        item_id: "rope".to_string(),
        name: "Верёвка".to_string(),
        location: "на крюке".to_string(),
        visible: true,
        portable: true,
        owner: String::new(),
        details: String::new(),
    });
    w.take_item("rope", "", "").expect("take ok");
    assert!(w.player_character.inventory.iter().any(|e| e == "Верёвка"));
}

#[test]
fn take_item_missing_name_returns_item_not_here_with_visible_hint() {
    let mut w = seeded_world();
    let err = w.take_item("", "дракон", "").expect_err("no such item");
    assert_eq!(err["code"], json!("item_not_here"));
    // The visible-item names are surfaced as a hint (NOT the invisible key).
    let hint = err["visible_items"]
        .as_array()
        .expect("visible_items array");
    let names: Vec<&str> = hint.iter().filter_map(Value::as_str).collect();
    assert!(names.contains(&"Медная монета"));
    assert!(
        !names.contains(&"Ключ"),
        "invisible item must not leak into the hint"
    );
}

#[test]
fn take_item_ambiguous_name_lists_candidates_and_takes_nothing() {
    let mut w = seeded_world();
    // Two visible items with the SAME name -> ambiguous.
    w.scene.items.push(SceneItem {
        item_id: "coin2".to_string(),
        name: "Медная монета".to_string(),
        location: "на полу".to_string(),
        visible: true,
        portable: true,
        owner: String::new(),
        details: String::new(),
    });
    let before = item_ids(&w);
    let inv_before = w.player_character.inventory.clone();
    let err = w.take_item("", "Медная монета", "").expect_err("ambiguous");
    assert_eq!(err["code"], json!("ambiguous_item"));
    let cands = err["candidates"].as_array().expect("candidates");
    assert_eq!(
        cands.len(),
        2,
        "both same-named visible items are candidates"
    );
    assert_eq!(
        item_ids(&w),
        before,
        "an ambiguous take must remove nothing"
    );
    assert_eq!(
        w.player_character.inventory, inv_before,
        "an ambiguous take must not touch the inventory"
    );
}

#[test]
fn take_item_invisible_only_reachable_by_id_not_by_name() {
    let mut w = seeded_world();
    // By name: the invisible key is not a visible candidate -> item_not_here.
    let by_name = w
        .take_item("", "ключ", "")
        .expect_err("invisible not by name");
    assert_eq!(by_name["code"], json!("item_not_here"));
    assert!(item_ids(&w).contains(&"hidden_key".to_string()));
    // By id: the GM-trusted path takes it even though it is invisible.
    let by_id = w
        .take_item("hidden_key", "", "беру ключ")
        .expect("take by id");
    assert_eq!(by_id["name"], json!("Ключ"));
    assert!(!item_ids(&w).contains(&"hidden_key".to_string()));
}

#[test]
fn take_item_non_portable_is_rejected_and_scene_unchanged() {
    let mut w = seeded_world();
    let before = item_ids(&w);
    let inv_before = w.player_character.inventory.clone();
    let err = w
        .take_item("mug", "", "хочу кружку")
        .expect_err("mug is fixed");
    assert_eq!(err["code"], json!("not_portable"));
    assert_eq!(
        item_ids(&w),
        before,
        "a non-portable take must remove nothing"
    );
    assert_eq!(
        w.player_character.inventory, inv_before,
        "a non-portable take must not touch the inventory"
    );
}

#[test]
fn take_item_unknown_id_is_a_clean_error() {
    let mut w = seeded_world();
    let err = w.take_item("nope", "", "").expect_err("unknown id");
    assert_eq!(err["code"], json!("unknown_item"));
}

#[test]
fn take_item_dedups_by_head_when_inventory_already_has_it() {
    let mut w = seeded_world();
    // Seed an inventory entry with the SAME head but a different (older) tail
    // AND different case — §И1 head matching is trim + lowercase, so the
    // lowercase entry must still dedup against the scene's «Медная монета».
    w.player_character
        .inventory
        .push("медная монета — старая запись".to_string());
    let count_before = w.player_character.inventory.len();
    w.take_item("coin", "", "").expect("take ok");
    // No duplicate head appended.
    assert_eq!(w.player_character.inventory.len(), count_before);
    // The scene item is still removed and the revision still bumps (it is a
    // successful, idempotent re-take).
    assert!(!item_ids(&w).contains(&"coin".to_string()));
}

// --- §И3 drop_item ----------------------------------------------------------

#[test]
fn drop_item_by_head_moves_entry_into_scene() {
    let mut w = seeded_world();
    w.player_character
        .inventory
        .push("Факел — горит 1 час".to_string());
    let before_rev = w.player_character.card_revision;
    let out = w
        .drop_item("факел", "у входа", "бросаю факел")
        .expect("drop ok");
    assert_eq!(out["status"], json!("dropped"));
    // Removed from inventory.
    assert!(!w
        .player_character
        .inventory
        .iter()
        .any(|e| item_head(e).to_lowercase() == "факел"));
    // Inserted into the scene: head/tail split, visible + portable, at location.
    let dropped = w
        .scene
        .items
        .iter()
        .find(|i| i.name == "Факел")
        .expect("factor in scene");
    assert_eq!(dropped.details, "горит 1 час");
    assert!(dropped.visible && dropped.portable);
    assert_eq!(dropped.location, "у входа");
    assert_eq!(w.player_character.card_revision, before_rev + 1);
}

#[test]
fn drop_item_defaults_location_to_ryadom() {
    let mut w = seeded_world();
    w.player_character.inventory.push("Свисток".to_string());
    w.drop_item("свисток", "", "").expect("drop ok");
    let dropped = w.scene.items.iter().find(|i| i.name == "Свисток").unwrap();
    assert_eq!(dropped.location, "рядом");
    assert_eq!(dropped.details, "", "no ' — ' means empty details");
}

#[test]
fn drop_item_generates_a_scene_unique_id() {
    let mut w = seeded_world();
    // Force a base id collision: an existing scene item already uses the id the
    // head would generate ("kinzhal" -> collides), so a suffix is appended.
    w.scene.items.push(SceneItem {
        item_id: "kinzhal".to_string(),
        name: "Другой кинжал".to_string(),
        location: "на стене".to_string(),
        visible: true,
        portable: true,
        owner: String::new(),
        details: String::new(),
    });
    w.player_character.inventory.push("kinzhal".to_string());
    w.drop_item("kinzhal", "", "").expect("drop ok");
    let ids = item_ids(&w);
    // Both the pre-existing and the dropped item are present, with distinct ids.
    assert!(ids.iter().filter(|id| id.starts_with("kinzhal")).count() >= 2);
    assert_eq!(
        ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
        ids.len(),
        "all scene item ids must be unique"
    );
}

#[test]
fn drop_item_unknown_returns_error_with_inventory_hint() {
    let mut w = seeded_world();
    w.player_character.inventory.push("Верёвка".to_string());
    let err = w.drop_item("топор", "", "").expect_err("not carried");
    assert_eq!(err["code"], json!("unknown_item"));
    let inv = err["inventory"].as_array().expect("inventory hint");
    assert!(inv.iter().filter_map(Value::as_str).any(|e| e == "Верёвка"));
}

// --- §И2 per-place item store: stash / restore / set_scene ------------------

/// Take the visible, passable exit out of the start place; returns the new
/// place id after the canon move + scene refresh.
fn move_out_and_refresh(w: &mut World) -> String {
    use gml_world::canon::action::{Action, ProposedAction};
    use gml_world::canon::engine;
    let here = w.world_canon.player_place_id.clone();
    let route = w
        .world_canon
        .exits_from(&here)
        .into_iter()
        .find(|t| t.visible && t.passable && t.blocked_by.is_empty())
        .cloned()
        .expect("an open exit");
    let tid = route.transition_id.clone();
    let destination = if route.to_place.is_empty() {
        format!("{}_destination", tid)
    } else {
        route.to_place.clone()
    };
    if w.world_canon.place(&destination).is_none() {
        w.world_canon.insert_place(Place {
            place_id: destination.clone(),
            name: "Явно заданная тестовая локация".to_string(),
            kind: "test_place".to_string(),
            provenance: Provenance::by("test", "explicit item-test destination", 0),
            ..Default::default()
        });
    }
    {
        let transition = w
            .world_canon
            .transitions
            .get_mut(&tid)
            .expect("selected transition");
        transition.to_place = destination.clone();
        transition.passage_id = "item_test_round_trip".to_string();
        transition.directionality = PassageDirectionality::Bidirectional;
        transition.kind = "path".to_string();
        transition.time_cost = 3;
        transition.risk = "none".to_string();
    }
    let return_id = ids::stable_id(&w.world_canon.world_seed, &destination, "transition", &here);
    if w.world_canon.transition(&return_id).is_none() {
        w.world_canon.insert_transition(Transition {
            transition_id: return_id.clone(),
            source_exit_id: return_id,
            passage_id: "item_test_round_trip".to_string(),
            directionality: PassageDirectionality::Bidirectional,
            from_place: destination.clone(),
            to_place: here,
            label: "Вернуться".to_string(),
            kind: "path".to_string(),
            visible: true,
            passable: true,
            time_cost: 3,
            risk: "none".to_string(),
            provenance: Provenance::by("test", "explicit item-test return", 0),
            ..Default::default()
        });
    }
    engine::apply(
        &mut w.world_canon,
        &ProposedAction::new(Action::MovePlayer { transition_id: tid }, "gm", "move"),
        1,
    )
    .unwrap();
    w.refresh_scene_from_canon();
    w.world_canon.player_place_id.clone()
}

#[test]
fn items_do_not_travel_on_move_player_and_are_stashed_under_the_old_place() {
    let mut w = seeded_world();
    let start = w.world_canon.player_place_id.clone();
    let start_items = item_ids(&w);
    assert!(!start_items.is_empty(), "start place has items");

    let arrived = move_out_and_refresh(&mut w);
    assert_ne!(arrived, start, "player moved");
    // The leak is fixed: the explicitly configured destination carries NO leftover
    // items from the start place.
    assert!(
        w.scene.items.is_empty(),
        "items must not travel with the player: {:?}",
        item_ids(&w)
    );
    // The start place's items were stashed for later.
    let stashed = w.place_items.get(&start).expect("start items stashed");
    let stashed_ids: Vec<String> = stashed.iter().map(|i| i.item_id.clone()).collect();
    assert_eq!(stashed_ids, start_items);
}

#[test]
fn items_restore_on_return_to_the_original_place() {
    let mut w = seeded_world();
    let start = w.world_canon.player_place_id.clone();
    let start_items = item_ids(&w);

    let arrived = move_out_and_refresh(&mut w);

    // Walk back to the start place along the guaranteed return edge.
    use gml_world::canon::action::{Action, ProposedAction};
    use gml_world::canon::engine;
    let back = w
        .world_canon
        .exits_from(&arrived)
        .into_iter()
        .find(|t| t.to_place == start && t.visible && t.passable)
        .map(|t| t.transition_id.clone())
        .expect("a way back to start");
    engine::apply(
        &mut w.world_canon,
        &ProposedAction::new(
            Action::MovePlayer {
                transition_id: back,
            },
            "gm",
            "back",
        ),
        2,
    )
    .unwrap();
    w.refresh_scene_from_canon();

    assert_eq!(w.world_canon.player_place_id, start, "returned to start");
    assert_eq!(
        item_ids(&w),
        start_items,
        "the start place's items are restored"
    );
}

#[test]
fn set_scene_overwrites_the_stored_items_for_that_place() {
    let mut w = seeded_world();
    let start_place = w.world_canon.player_place_id.clone();
    let out = w.set_scene(
        "Новый зал",
        "Пустой каменный зал.",
        &start_place,
        &json!(["borin"]),
        &json!([{"id": "lamp", "name": "Лампа", "location": "на крюке"}]),
        &json!([]),
        &json!([]),
        "",
    );
    let items = out["items"].as_array().expect("scene items");
    let names: Vec<&str> = items
        .iter()
        .filter_map(|i| i.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(
        names,
        vec!["Лампа"],
        "set_scene shows the updated current-place items"
    );
    assert_eq!(
        item_ids(&w),
        vec!["lamp".to_string()],
        "the live scene carries the updated current-place item"
    );
    assert_eq!(w.scene.location_id, start_place);

    use gml_world::canon::action::{Action, ProposedAction};
    use gml_world::canon::engine;
    let destination = "item_test_destination";
    w.world_canon.insert_place(Place {
        place_id: destination.to_string(),
        name: "Соседний зал".to_string(),
        kind: "room".to_string(),
        default_description: "Соседний пустой зал.".to_string(),
        provenance: Provenance::by("test", "explicit item test destination", 0),
        ..Default::default()
    });
    let departure_id = "item_test_departure";
    w.world_canon.insert_transition(Transition {
        transition_id: departure_id.to_string(),
        source_exit_id: departure_id.to_string(),
        passage_id: departure_id.to_string(),
        directionality: PassageDirectionality::OneWay,
        from_place: start_place.clone(),
        to_place: destination.to_string(),
        label: "В соседний зал".to_string(),
        kind: "passage".to_string(),
        visible: true,
        passable: true,
        time_cost: 1,
        risk: "none".to_string(),
        provenance: Provenance::by("test", "explicit item test route", 0),
        ..Default::default()
    });
    let departure = w
        .world_canon
        .exits_from(&start_place)
        .into_iter()
        .find(|transition| {
            transition.visible
                && transition.passable
                && transition.blocked_by.is_empty()
                && !transition.to_place.is_empty()
                && !transition.kind.is_empty()
                && transition.time_cost > 0
                && gml_world::canon::travel::TravelRisk::parse(&transition.risk).is_some()
        })
        .map(|t| t.transition_id.clone())
        .expect("seeded current place has a complete departure");
    engine::apply(
        &mut w.world_canon,
        &ProposedAction::new(
            Action::MovePlayer {
                transition_id: departure,
            },
            "gm",
            "leave",
        ),
        3,
    )
    .unwrap();
    w.refresh_scene_from_canon();
    let stashed = w
        .place_items
        .get(&start_place)
        .expect("current place items stashed");
    let stashed_ids: Vec<String> = stashed.iter().map(|i| i.item_id.clone()).collect();
    assert_eq!(
        stashed_ids,
        vec!["lamp".to_string()],
        "the set_scene-updated items become this place's store on the next leave"
    );
}
