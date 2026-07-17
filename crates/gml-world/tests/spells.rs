//! Phase-С spell tests (`docs/ITEMS_AND_SPELLS_TZ.md` §2): the §С1 card fields
//! (spells object-array apply, flat spell_slots coercion, concentration text),
//! the §С2 cast_spell semantics (unknown / cantrip / no-slots / decrement /
//! upcast / concentration swap incl. previous-ended), and the §С1 context render.

use serde_json::{json, Value};

use gml_world::{SpellEntry, World};

/// A caster seed: two known spells (a cantrip and a level-1 concentration
/// spell), one level-1 slot in three, and no active concentration.
fn caster_seed() -> Value {
    json!({
        "id": "test-caster",
        "title": "Тест мага",
        "public_intro": "Башня мага.",
        "hidden_truth": "Скрытый ритуал.",
        "npcs": [],
        "player": {
            "name": "Аэлин",
            "spells": [
                {"name": "Луч холода", "level": 0, "concentration": false, "ritual": false,
                 "effect": "1d8 урона холодом, дистанция 60 фт"},
                {"name": "Огненная хватка", "level": 1, "concentration": true, "ritual": false,
                 "effect": "конц., до 1 мин; 2d6 огнём в начале хода"},
                {"name": "Щит веры", "level": 1, "concentration": true, "ritual": false,
                 "effect": "конц.; +2 AC"}
            ],
            "spell_slots": {"1": 3, "2": 1},
            "spell_slots_max": {"1": 4, "2": 2},
            "concentration": ""
        },
        "scene": {
            "id": "tower_scene",
            "location_id": "tower",
            "title": "Кабинет мага",
            "description": "Тесная башня со свитками.",
            "present_npcs": [],
            "items": [],
            "exits": []
        }
    })
}

fn seeded_world() -> World {
    World::from_seed_with_dice_seed(&caster_seed(), 20260622)
}

// --- §С1 card fields: seed + apply -----------------------------------------

#[test]
fn seed_loads_spells_slots_and_concentration() {
    let w = seeded_world();
    let pc = &w.player_character;
    assert_eq!(pc.spells.len(), 3, "three known spells seeded");
    assert_eq!(pc.spells[0].name, "Луч холода");
    assert_eq!(pc.spells[0].level, 0);
    assert!(pc.spells[1].concentration);
    // Flat slot maps loaded verbatim.
    assert_eq!(pc.spell_slots.get("1"), Some(&json!(3)));
    assert_eq!(pc.spell_slots_max.get("1"), Some(&json!(4)));
    assert!(pc.concentration.is_empty());
}

#[test]
fn apply_spells_deserializes_objects_and_skips_junk() {
    let mut w = seeded_world();
    // A batch mixing a valid object with junk (a string and a number): only the
    // object survives; the junk is skipped, not fatal.
    let out = w.update_player_character(
        &json!({"spells": [
            {"name": "Свет", "level": 0, "effect": "яркий свет 20 фт"},
            "не заклинание",
            42,
            {"name": "Полёт", "level": 3, "concentration": true}
        ]}),
        "debug",
    );
    assert_eq!(out["ok"], json!(true));
    let pc = &w.player_character;
    assert_eq!(pc.spells.len(), 2, "junk entries dropped, objects kept");
    assert_eq!(pc.spells[0].name, "Свет");
    assert_eq!(pc.spells[1].name, "Полёт");
    assert_eq!(pc.spells[1].level, 3);
    // Missing keys default (effect empty, ritual false).
    assert!(pc.spells[1].effect.is_empty());
    assert!(!pc.spells[1].ritual);
}

#[test]
fn apply_spell_slots_coerces_stringy_ints_through_dict_fields() {
    let mut w = seeded_world();
    // A model may emit stringy counts; the K2.1 stat-dict coercion (spell_slots is
    // in dict_fields) turns "2" into 2 so the cast decrement path reads a number.
    w.update_player_character(&json!({"spell_slots": {"1": "2", "3": "1"}}), "debug");
    assert_eq!(w.player_character.spell_slots.get("1"), Some(&json!(2)));
    assert_eq!(w.player_character.spell_slots.get("3"), Some(&json!(1)));
}

#[test]
fn apply_concentration_is_a_text_field() {
    let mut w = seeded_world();
    w.update_player_character(&json!({"concentration": "Огненная хватка"}), "debug");
    assert_eq!(w.player_character.concentration, "Огненная хватка");
    // "" clears it.
    w.update_player_character(&json!({"concentration": ""}), "debug");
    assert!(w.player_character.concentration.is_empty());
}

// --- §С2 cast_spell --------------------------------------------------------

#[test]
fn cast_unknown_spell_errors_with_known_hint() {
    let mut w = seeded_world();
    let err = w.cast_spell("метеор", None, "").expect_err("not known");
    assert_eq!(err["code"], json!("unknown_spell"));
    let known: Vec<&str> = err["known_spells"]
        .as_array()
        .expect("known_spells array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(known.contains(&"Луч холода"));
    assert!(known.contains(&"Огненная хватка"));
}

#[test]
fn cast_cantrip_spends_no_slot() {
    let mut w = seeded_world();
    let slots_before = w.player_character.spell_slots.clone();
    let before_rev = w.player_character.card_revision;
    let out = w
        .cast_spell("луч холода", None, "стреляю холодом")
        .expect("cast ok");
    assert_eq!(out["level"], json!(0));
    assert_eq!(
        out["slot_spent_level"],
        Value::Null,
        "cantrip spends no slot"
    );
    assert_eq!(
        w.player_character.spell_slots, slots_before,
        "cantrip must not touch slots"
    );
    // A cantrip is still a cast: card_revision bumps.
    assert_eq!(w.player_character.card_revision, before_rev + 1);
}

#[test]
fn cast_leveled_spell_decrements_the_slot_written_as_number() {
    let mut w = seeded_world();
    let out = w
        .cast_spell("огненная хватка", None, "хватаю огнём")
        .expect("cast ok");
    assert_eq!(out["slot_spent_level"], json!(1));
    // 3 -> 2, stored as a NUMBER.
    assert_eq!(w.player_character.spell_slots.get("1"), Some(&json!(2)));
    // slots_remaining echoes the flat map post-decrement.
    assert_eq!(out["slots_remaining"]["1"], json!(2));
}

#[test]
fn cast_upcast_uses_max_of_base_and_requested_level() {
    let mut w = seeded_world();
    // Cast the level-1 spell in a level-2 slot: spends the level-2 slot.
    let out = w
        .cast_spell("огненная хватка", Some(2), "апкаст")
        .expect("cast ok");
    assert_eq!(out["slot_spent_level"], json!(2));
    assert_eq!(w.player_character.spell_slots.get("2"), Some(&json!(0)));
    assert_eq!(
        w.player_character.spell_slots.get("1"),
        Some(&json!(3)),
        "the level-1 slots are untouched by the upcast"
    );
}

#[test]
fn cast_requested_below_base_clamps_up_to_base() {
    let mut w = seeded_world();
    // Requesting slot_level 0 for a level-1 spell must not drop below level 1.
    let out = w
        .cast_spell("огненная хватка", Some(0), "")
        .expect("cast ok");
    assert_eq!(out["slot_spent_level"], json!(1));
}

#[test]
fn cast_with_no_free_slot_of_that_level_is_rejected() {
    let mut w = seeded_world();
    // Drain the single level-2 slot, then try to upcast into level 2.
    w.player_character.spell_slots.insert("2".into(), json!(0));
    let err = w
        .cast_spell("огненная хватка", Some(2), "")
        .expect_err("no level-2 slot");
    assert_eq!(err["code"], json!("no_slots"));
    assert_eq!(err["level"], json!(2));
    // Nothing changed: the level-1 slots are intact.
    assert_eq!(w.player_character.spell_slots.get("1"), Some(&json!(3)));
}

#[test]
fn cast_missing_level_reads_as_zero_slots_and_rejects() {
    let mut w = seeded_world();
    // A level-1 spell but the "1" key is absent entirely -> reads 0 -> no_slots.
    w.player_character.spell_slots.clear();
    let err = w
        .cast_spell("огненная хватка", None, "")
        .expect_err("no slots at all");
    assert_eq!(err["code"], json!("no_slots"));
    assert_eq!(err["level"], json!(1));
}

#[test]
fn cast_concentration_sets_field_and_reports_started() {
    let mut w = seeded_world();
    let out = w.cast_spell("огненная хватка", None, "").expect("cast ok");
    assert_eq!(out["concentration_started"], json!("Огненная хватка"));
    assert_eq!(out["concentration_ended"], Value::Null);
    assert_eq!(w.player_character.concentration, "Огненная хватка");
}

#[test]
fn cast_concentration_swap_ends_previous() {
    let mut w = seeded_world();
    // First concentration spell.
    w.cast_spell("огненная хватка", None, "").expect("cast 1");
    assert_eq!(w.player_character.concentration, "Огненная хватка");
    // A second concentration spell replaces it; the previous is reported ended.
    let out = w.cast_spell("щит веры", None, "").expect("cast 2");
    assert_eq!(out["concentration_ended"], json!("Огненная хватка"));
    assert_eq!(out["concentration_started"], json!("Щит веры"));
    assert_eq!(w.player_character.concentration, "Щит веры");
}

#[test]
fn cast_cantrip_leaves_concentration_untouched() {
    let mut w = seeded_world();
    w.cast_spell("огненная хватка", None, "")
        .expect("cast conc");
    // A cantrip (non-concentration) must NOT clear the held concentration.
    let out = w.cast_spell("луч холода", None, "").expect("cast cantrip");
    assert_eq!(out["concentration_started"], Value::Null);
    assert_eq!(out["concentration_ended"], Value::Null);
    assert_eq!(
        w.player_character.concentration, "Огненная хватка",
        "a non-concentration cast leaves the held effect in place"
    );
}

#[test]
fn cast_recasting_same_concentration_does_not_self_report_ended() {
    let mut w = seeded_world();
    w.cast_spell("огненная хватка", None, "").expect("cast 1");
    // Re-casting the SAME concentration spell must not report itself as ended.
    let out = w.cast_spell("огненная хватка", None, "").expect("cast 2");
    assert_eq!(out["concentration_ended"], Value::Null);
    assert_eq!(out["concentration_started"], json!("Огненная хватка"));
}

// --- §С1 context render ----------------------------------------------------

#[test]
fn context_renders_spells_slots_and_concentration() {
    let mut w = seeded_world();
    // Cast a concentration spell so all three lines appear (spells / slots /
    // active concentration).
    w.cast_spell("огненная хватка", None, "").expect("cast");
    let ctx = w.player_character_context();
    // Spell lines: name, level/cantrip, marks, effect prose.
    assert!(ctx.contains("Луч холода (cantrip)"), "cantrip label: {ctx}");
    assert!(
        ctx.contains("Огненная хватка (level 1, conc.)"),
        "concentration mark: {ctx}"
    );
    // Slots line reflects the post-cast remaining/max: level 1 is now 2/4.
    assert!(
        ctx.contains("Slots: level 1: 2/4, level 2: 1/2"),
        "slots line: {ctx}"
    );
    // Active concentration line.
    assert!(
        ctx.contains("Concentration: Огненная хватка"),
        "conc line: {ctx}"
    );
}

#[test]
fn context_omits_spell_lines_when_empty() {
    let mut w = seeded_world();
    w.player_character.spells.clear();
    w.player_character.spell_slots.clear();
    w.player_character.spell_slots_max.clear();
    w.player_character.concentration.clear();
    let ctx = w.player_character_context();
    assert!(!ctx.contains("Spells:"), "no spells line when empty");
    assert!(!ctx.contains("Slots:"), "no slots line when empty");
    assert!(
        !ctx.contains("Concentration:"),
        "no concentration line when empty"
    );
}

// --- back-compat: absent keys default cleanly ------------------------------

#[test]
fn spell_entry_default_is_empty() {
    let sp = SpellEntry::default();
    assert!(sp.name.is_empty());
    assert_eq!(sp.level, 0);
    assert!(!sp.concentration);
    assert!(!sp.ritual);
    assert!(sp.effect.is_empty());
}
