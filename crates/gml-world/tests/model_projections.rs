//! Data-model + projection invariants (secret/hidden-canon isolation, scope
//! gating, card_revision discipline, whereabouts precedence, seed coercion).

use std::collections::BTreeSet;

use gml_world::state_record::RagDocument;
use gml_world::{
    MemoryInjectionState, MemoryTier, MemoryTruthStatus, MemoryUnit, NpcWhereabouts, StateRecord,
    World,
};
use serde_json::{json, Map, Value};

fn sample_seed() -> Value {
    json!({
        "id": "test_story",
        "title": "Тестовая история",
        "public_intro": "Таверна на закате.",
        "hidden_truth": "Капитан — оборотень.",
        "public_facts": [
            {"id": "f1", "text": "Ворота закрываются в полночь.", "kind": "public", "keywords": ["ворота"]}
        ],
        "state_records": [
            {"id": "sr_pub", "kind": "fact", "text": "Публичный факт", "scope": "public"},
            {"id": "sr_gm", "kind": "fact", "text": "Тайна ГМ", "scope": "gm", "owner": "gm"},
            {"id": "sr_owner", "kind": "npc_memory", "text": "Память Борина", "scope": "owner", "owner": "borin"}
        ],
        "npcs": [
            {
                "id": "borin", "name": "Борин", "role": "трактирщик",
                "pronouns": "M", "physical_type": "крепкий мужчина",
                "secret": "Прячет долги.", "knowledge": "Слухи таверны."
            },
            {
                "id": "lysa", "name": "Лиза", "role": "служанка",
                "pronouns": "F", "secret": "Шпионка."
            }
        ],
        "scene": {
            "id": "tavern", "title": "Таверна", "location_id": "tavern",
            "description": "Тёплый зал.",
            "present_npcs": ["borin", "lysa"],
            "items": [{"id": "mug", "name": "Кружка", "location": "на столе"}],
            "exits": [{"id": "door", "name": "Дверь", "destination": "Улица"}]
        }
    })
}

fn pinned_world() -> World {
    World::from_seed_with_dice_seed(&sample_seed(), 424242)
}

#[test]
fn seed_loads_core_fields() {
    let w = pinned_world();
    assert_eq!(w.story_id, "test_story");
    assert_eq!(w.story_title, "Тестовая история");
    assert_eq!(w.public, "Таверна на закате.");
    assert_eq!(w.canon, "Капитан — оборотень.");
    assert_eq!(w.npcs.len(), 2);
    assert!(w.npcs.contains_key("borin"));
    assert_eq!(w.scene.title, "Таверна");
    assert_eq!(w.scene.present_npcs.len(), 2);
    // hidden_truth fact record mirrors canon.
    assert!(w
        .fact_records
        .iter()
        .any(|r| r.fact_id == "hidden_truth" && r.kind == "truth" && r.text == w.canon));
}

#[test]
fn retrieval_excludes_truth_and_secrets() {
    let mut w = pinned_world();
    w.add_memory_unit(MemoryUnit {
        memory_id: "pub_gate_note".to_string(),
        tier: MemoryTier::Raw,
        owner_scope: "public".to_string(),
        visibility_scopes: vec!["public".to_string()],
        summary: "Публичный факт".to_string(),
        injection_state: MemoryInjectionState::Hot,
        truth_status: MemoryTruthStatus::Actual,
        created_by: "test".to_string(),
        ..MemoryUnit::default()
    });
    let docs = w.retrieval_documents("player");
    // No doc may carry the hidden canon text or any NPC secret.
    for d in &docs {
        assert!(!d.text.contains("оборотень"), "canon leaked: {}", d.text);
        assert!(
            !d.text.contains("Прячет долги"),
            "secret leaked: {}",
            d.text
        );
        assert!(!d.text.contains("Шпионка"), "secret leaked: {}", d.text);
        // kind=="truth" facts never become docs.
        assert_ne!(d.doc_id, "fact:hidden_truth");
    }
    // The gm-scoped state record must not appear in the player corpus.
    assert!(!docs.iter().any(|d| d.doc_id == "state:sr_gm"));
    // owner-scoped record (owner=borin) not visible to player either.
    assert!(!docs.iter().any(|d| d.doc_id == "state:sr_owner"));
    // Canon scoped memory is the RAG source; legacy StateRecord is not.
    assert!(docs.iter().any(|d| d.doc_id == "memory:pub_gate_note"));
    assert!(!docs.iter().any(|d| d.doc_id == "state:sr_pub"));
    assert!(!docs.iter().any(|d| d.doc_id.starts_with("state:")));
}

#[test]
fn model_context_labels_are_english_and_world_values_stay_verbatim() {
    let mut w = pinned_world();
    let borin = w.npcs.get_mut("borin").expect("seeded NPC");
    borin.distinctive_features = "шрам на подбородке".to_string();
    borin.current_appearance = "в синем фартуке".to_string();

    let roster = w.dynamic_roster_context(&BTreeSet::new());
    assert!(roster.contains("gender=masculine"), "{roster}");
    assert!(!roster.contains("род="), "{roster}");

    let scene = w.scene_context();
    assert!(
        scene.contains("distinctive features: шрам на подбородке"),
        "{scene}"
    );
    assert!(
        scene.contains("current appearance: в синем фартуке"),
        "{scene}"
    );
    assert!(scene.contains("gender: masculine"), "{scene}");
    assert!(scene.contains("Scene: Таверна"), "{scene}");

    let npc_slice = w.npc_scene_slice("borin");
    assert!(npc_slice.contains("gender: feminine"), "{npc_slice}");
    assert!(npc_slice.contains("M = masculine"), "{npc_slice}");

    let docs = w.retrieval_documents("player");
    let scene_doc = docs
        .iter()
        .find(|doc| doc.kind == "scene_state")
        .expect("scene retrieval document");
    assert!(scene_doc.text.starts_with("Current scene: Таверна."));
    let item_doc = docs
        .iter()
        .find(|doc| doc.kind == "scene_item")
        .expect("item retrieval document");
    assert!(item_doc
        .text
        .starts_with("Visible item in the current scene: Кружка; location: на столе."));
    let npc_doc = docs
        .iter()
        .find(|doc| doc.doc_id == "npc_public:borin")
        .expect("NPC retrieval document");
    assert!(npc_doc.text.contains("Gender: masculine (M)."));
    assert!(npc_doc
        .text
        .contains("Distinctive features: шрам на подбородке."));
    let whereabouts_doc = docs
        .iter()
        .find(|doc| doc.doc_id == "npc_whereabouts:borin")
        .expect("NPC whereabouts retrieval document");
    assert!(whereabouts_doc
        .text
        .starts_with("Борин is currently present in the current scene."));

    w.time.current_date_label.clear();
    assert!(
        w.time_context().contains("Current world time: Day 1,"),
        "{}",
        w.time_context()
    );
    assert_eq!(
        w.time_export()["current_date_label"],
        json!("День 1"),
        "UI/state fallback remains unchanged"
    );
}

#[test]
fn npc_scene_slice_excludes_legacy_state_records() {
    let mut w = pinned_world();
    w.add_state_records(&json!([{
        "id": "late_owner_memory",
        "kind": "npc_memory",
        "text": "LEGACY_NPC_CONTEXT_SENTINEL",
        "scope": "owner",
        "owner": "borin"
    }]));

    let slice = w.npc_scene_slice("borin");
    assert!(
        !slice.contains("Actor-visible state memory"),
        "legacy StateRecord block must not be injected into NPC context:\n{slice}"
    );
    assert!(
        !slice.contains("LEGACY_NPC_CONTEXT_SENTINEL"),
        "late legacy StateRecord text reached NPC context:\n{slice}"
    );
}

#[test]
fn fact_lookup_ignores_unsynced_legacy_state_records() {
    let mut w = pinned_world();
    w.state_records.push(StateRecord {
        record_id: "late_public_fact".to_string(),
        kind: "fact".to_string(),
        text: "XYZZY_DIRECT_SENTINEL".to_string(),
        scope: "public".to_string(),
        active: true,
        owner: String::new(),
        subject: String::new(),
        source: String::new(),
        status: "known".to_string(),
        tags: vec!["xyzzy_direct_sentinel".to_string()],
        entity_id: String::new(),
        source_npc: String::new(),
        participants: Vec::new(),
        location_id: String::new(),
        location_name: String::new(),
        region_id: String::new(),
        region_name: String::new(),
        scene_id: String::new(),
        importance: String::new(),
        aliases: Vec::new(),
        metadata: Map::new(),
    });

    let legacy = w.fact("XYZZY_DIRECT_SENTINEL", "player", None);
    assert_eq!(legacy.status, "unknown");
    assert!(
        !legacy.text.contains("XYZZY_DIRECT_SENTINEL"),
        "World::fact must not read StateRecord fallback directly"
    );

    w.add_memory_unit(MemoryUnit {
        memory_id: "scoped_fact_sentinel".to_string(),
        tier: MemoryTier::Raw,
        owner_scope: "public".to_string(),
        visibility_scopes: vec!["public".to_string()],
        summary: "SCOPED_FACT_SENTINEL".to_string(),
        truth_status: MemoryTruthStatus::Actual,
        created_by: "test".to_string(),
        ..MemoryUnit::default()
    });
    let scoped = w.fact("SCOPED_FACT_SENTINEL", "player", None);
    assert_eq!(scoped.status, "known");
    assert!(scoped.text.contains("SCOPED_FACT_SENTINEL"));
}

#[test]
fn seed_known_name_state_records_migrate_to_scoped_memory() {
    let mut seed = sample_seed();
    let records = seed
        .get_mut("state_records")
        .and_then(Value::as_array_mut)
        .expect("sample state_records array");
    records.push(json!({
        "id": "legacy_known_borin",
        "kind": "fact",
        "text": "Игрок знает, что трактирщика зовут Старый Борин.",
        "scope": "public",
        "entity_id": "borin",
        "metadata": {"known_name": "Старый Борин"}
    }));

    let w = World::from_seed_with_dice_seed(&seed, 424242);
    assert_eq!(w.npc_known_name("borin", "player"), "Старый Борин");
    assert_eq!(w.npc_player_label("borin", "player"), "Старый Борин");
    assert!(w.world_canon.memory.units.values().any(|unit| {
        unit.source_state_record_ids
            .iter()
            .any(|id| id == "legacy_known_borin")
            && unit.metadata.get("known_name").and_then(Value::as_str) == Some("Старый Борин")
    }));
}

#[test]
fn state_record_scope_gating() {
    let w = pinned_world();
    // player sees public only.
    let q = gml_world_state_query(&w, "player");
    assert!(q.iter().any(|r| r.record_id == "sr_pub"));
    assert!(!q.iter().any(|r| r.record_id == "sr_gm"));
    assert!(!q.iter().any(|r| r.record_id == "sr_owner"));
    // gm sees everything.
    let qg = gml_world_state_query(&w, "gm");
    assert!(qg.iter().any(|r| r.record_id == "sr_gm"));
    assert!(qg.iter().any(|r| r.record_id == "sr_owner"));
    // owner borin sees own owner-scoped record + public.
    let qb = gml_world_state_query(&w, "borin");
    assert!(qb.iter().any(|r| r.record_id == "sr_owner"));
    assert!(qb.iter().any(|r| r.record_id == "sr_pub"));
    assert!(!qb.iter().any(|r| r.record_id == "sr_gm"));
}

fn gml_world_state_query(w: &World, actor: &str) -> Vec<gml_world::StateRecord> {
    use gml_world::StateRecordQuery;
    w.state_records_for(&StateRecordQuery::new(actor))
        .into_iter()
        .cloned()
        .collect()
}

#[test]
fn update_npc_card_revision_discipline() {
    let mut w = pinned_world();
    assert_eq!(w.npcs["borin"].card_revision, 0);
    // color-only edit must NOT bump.
    w.update_npc("borin", &json!({"color": "#abcdef"}));
    assert_eq!(w.npcs["borin"].card_revision, 0);
    // content edit bumps once.
    w.update_npc("borin", &json!({"condition": "ранен"}));
    assert_eq!(w.npcs["borin"].card_revision, 1);
    // idempotent: same content value does not bump.
    w.update_npc("borin", &json!({"condition": "ранен"}));
    assert_eq!(w.npcs["borin"].card_revision, 1);
    // secret stays unreachable through npc_profile (not in NPC_PROFILE_FIELDS).
    let profile = w
        .npc_profile("borin", "visible", &json!(["secret"]))
        .unwrap();
    let prof = profile["profile"].as_object().unwrap();
    assert!(!prof.contains_key("secret"));
    // but the ignored list records it.
    let ignored = profile["ignored_fields"].as_array().unwrap();
    assert!(ignored.iter().any(|v| v == "secret"));
}

#[test]
fn player_character_card_revision() {
    let mut w = pinned_world();
    let pc0 = w.player_character.card_revision;
    w.update_player_character(&json!({"name": "Новое имя"}), "");
    assert_eq!(w.player_character.card_revision, pc0 + 1);
    // no-op update does not bump.
    w.update_player_character(&json!({"name": "Новое имя"}), "");
    assert_eq!(w.player_character.card_revision, pc0 + 1);
}

// --- K2.1 numeric normalization of stat dicts -----------------------------

#[test]
fn stat_dicts_coerce_numeric_strings_and_drop_junk() {
    let mut w = pinned_world();
    w.update_player_character(
        &json!({
            "abilities": {"STR": "14", "DEX": 12, "CON": " 11 ", "WIS": "abc"},
            "skills": {"Perception": "3.5", "Stealth": "not-a-number"},
            "saving_throws": {"DEX": "5"},
        }),
        "coerce",
    );
    let ab = &w.player_character.abilities;
    // "14" -> integer 14 (exact, not a float).
    assert_eq!(ab["STR"], json!(14));
    assert!(ab["STR"].is_i64(), "string int must land as i64");
    // already-numeric stays put.
    assert_eq!(ab["DEX"], json!(12));
    // surrounding whitespace tolerated.
    assert_eq!(ab["CON"], json!(11));
    // genuinely textual value kept verbatim (never destroyed / nulled).
    assert_eq!(ab["WIS"], json!("abc"));

    let sk = &w.player_character.skills;
    // "3.5" -> finite float 3.5.
    assert_eq!(sk["Perception"], json!(3.5));
    assert!(sk["Perception"].is_f64());
    assert_eq!(sk["Stealth"], json!("not-a-number"));

    assert_eq!(w.player_character.saving_throws["DEX"], json!(5));
}

#[test]
fn hp_mixed_dict_coerces_numeric_keys_and_keeps_notes() {
    let mut w = pinned_world();
    w.update_player_character(
        &json!({"hp": {"current": "5", "max": 9, "note": "истекает кровью"}}),
        "hurt",
    );
    let hp = &w.player_character.hp;
    assert_eq!(hp["current"], json!(5));
    assert!(hp["current"].is_i64());
    assert_eq!(hp["max"], json!(9));
    // a non-numeric hp annotation survives verbatim.
    assert_eq!(hp["note"], json!("истекает кровью"));
}

#[test]
fn ac_is_left_untouched_by_stat_normalization() {
    // `ac` is a bare Value, NOT a stat dict — a stringy annotated AC must survive
    // verbatim (spec §К2.1: leave ac alone).
    let mut w = pinned_world();
    w.update_player_character(&json!({"ac": "13 (кожаный доспех)"}), "armor");
    assert_eq!(w.player_character.ac, json!("13 (кожаный доспех)"));
}

#[test]
fn seed_path_normalizes_stat_dicts() {
    // §К2.1 "То же при сидинге": the launch-seed path shares the same choke point
    // (`seed_player_character` -> `apply_player_character_fields`). Build on the
    // strict-shape `sample_seed()` so `normalize_seed` preserves `player_character`
    // verbatim (the rebuild branch drops non-strict keys — pre-existing behavior).
    let mut seed = sample_seed();
    seed.as_object_mut().unwrap().insert(
        "player_character".to_string(),
        json!({
            "name": "Тестер",
            "abilities": {"STR": "16", "DEX": "13"},
            "skills": {"Perception": "4"},
            "hp": {"current": "10", "max": "10", "note": "устал"},
        }),
    );
    let w = World::from_seed_with_dice_seed(&seed, 424242);
    assert_eq!(w.player_character.name, "Тестер");
    assert_eq!(w.player_character.abilities["STR"], json!(16));
    assert!(w.player_character.abilities["STR"].is_i64());
    assert_eq!(w.player_character.abilities["DEX"], json!(13));
    assert_eq!(w.player_character.skills["Perception"], json!(4));
    assert_eq!(w.player_character.hp["current"], json!(10));
    assert_eq!(w.player_character.hp["note"], json!("устал"));
}

// --- K2.2 inventory / equipment delta ops ---------------------------------

#[test]
fn inventory_delta_add_appends_and_skips_duplicates() {
    let mut w = pinned_world();
    w.update_player_character(&json!({"inventory": ["меч", "щит"]}), "base");
    let rev = w.player_character.card_revision;

    w.update_player_character(&json!({"inventory_add": ["факел", "меч"]}), "add");
    // "факел" appended; "меч" already present -> skipped (idempotent add).
    assert_eq!(w.player_character.inventory, vec!["меч", "щит", "факел"]);
    assert_eq!(w.player_character.card_revision, rev + 1);
}

#[test]
fn inventory_delta_remove_drops_all_occurrences() {
    let mut w = pinned_world();
    w.update_player_character(
        &json!({"inventory": ["зелье", "зелье", "карта", " зелье "]}),
        "base",
    );
    w.update_player_character(&json!({"inventory_remove": ["зелье"]}), "drink");
    // trim-exact match removes ALL occurrences, including the padded " зелье ".
    assert_eq!(w.player_character.inventory, vec!["карта"]);
}

#[test]
fn inventory_delta_matches_by_head_case_insensitively() {
    // §И1: delta ops match by the entry HEAD (before « — »), trim + lowercase —
    // the same rule take_item/drop_item use. A remove by bare name must drop a
    // described entry; an add with an already-present head must be skipped.
    let mut w = pinned_world();
    w.update_player_character(
        &json!({"inventory": ["Кинжал — 1d4, скрыт в сапоге", "карта"]}),
        "base",
    );
    // Add with the same head (different case, no description) -> skipped.
    let rev = w.player_character.card_revision;
    w.update_player_character(&json!({"inventory_add": ["кинжал"]}), "dup");
    assert_eq!(
        w.player_character.inventory,
        vec!["Кинжал — 1d4, скрыт в сапоге", "карта"]
    );
    assert_eq!(w.player_character.card_revision, rev);
    // Remove by bare lowercase head drops the described, capitalized entry.
    w.update_player_character(&json!({"inventory_remove": ["кинжал"]}), "spend");
    assert_eq!(w.player_character.inventory, vec!["карта"]);
}

#[test]
fn delta_noop_when_add_already_present_does_not_bump_revision() {
    let mut w = pinned_world();
    w.update_player_character(&json!({"inventory": ["меч"]}), "base");
    let rev = w.player_character.card_revision;
    // Adding an entry that already exists changes nothing -> no revision bump.
    w.update_player_character(&json!({"inventory_add": ["меч"]}), "noop");
    assert_eq!(w.player_character.inventory, vec!["меч"]);
    assert_eq!(w.player_character.card_revision, rev);
}

#[test]
fn full_rewrite_then_delta_order_within_one_call() {
    let mut w = pinned_world();
    w.update_player_character(&json!({"inventory": ["старьё"]}), "base");
    // Full array wins first (replaces to [лук, стрелы]), THEN remove (стрелы),
    // THEN add (колчан) — all in one call.
    w.update_player_character(
        &json!({
            "inventory": ["лук", "стрелы"],
            "inventory_remove": ["стрелы"],
            "inventory_add": ["колчан", "лук"],
        }),
        "reorg",
    );
    // "лук" survives rewrite, "стрелы" removed, "колчан" added, duplicate "лук" skipped.
    assert_eq!(w.player_character.inventory, vec!["лук", "колчан"]);
}

#[test]
fn equipment_delta_independent_and_fires_change_detection() {
    let mut w = pinned_world();
    let rev = w.player_character.card_revision;
    w.update_player_character(&json!({"equipment_add": ["плащ"]}), "wear");
    assert_eq!(w.player_character.equipment, vec!["плащ"]);
    assert_eq!(w.player_character.card_revision, rev + 1);
    // A delta call that is a pure no-op (empty arrays) must not bump.
    w.update_player_character(
        &json!({"equipment_add": [], "equipment_remove": []}),
        "nothing",
    );
    assert_eq!(w.player_character.card_revision, rev + 1);
}

#[test]
fn whereabouts_present_precedence() {
    let mut w = pinned_world();
    // present NPCs synced to status=present, source="current scene".
    let export = w.npc_whereabouts_export(Some("borin"));
    assert_eq!(export["status"], "present");
    assert_eq!(export["source"], "current scene");
    assert_eq!(export["location_name"], "Таверна");
}

#[test]
fn stale_present_whereabouts_downgrades_when_npc_not_in_current_scene() {
    let mut w = pinned_world();
    w.scene.present_npcs.remove("borin");
    w.scene.presence.remove("borin");
    w.npc_whereabouts.insert(
        "borin".to_string(),
        NpcWhereabouts {
            npc_id: "borin".to_string(),
            location_id: "tavern".to_string(),
            location_name: "Таверна".to_string(),
            status: "present".to_string(),
            details: "за стойкой".to_string(),
            source: "current scene".to_string(),
        },
    );

    let export = w.npc_whereabouts_export(Some("borin"));
    assert_eq!(export["status"], "known");
    assert_eq!(export["source"], "stale current scene");

    let summary = w.npc_whereabouts_summary("borin");
    assert!(summary.contains("NOT in current scene"));
    assert!(summary.contains("status: known"));
    assert!(!summary.contains("status: present"));
}

#[test]
fn set_public_intro_sets_prefix_dirty() {
    let mut w = pinned_world();
    assert!(!w.prefix_dirty);
    assert!(w.set_public_intro("Новое вступление."));
    assert!(w.prefix_dirty);
    assert_eq!(w.public, "Новое вступление.");
    // empty text is a no-op.
    let mut w2 = pinned_world();
    assert!(!w2.set_public_intro("   "));
    assert!(!w2.prefix_dirty);
}

#[test]
fn fact_excludes_truth_and_hidden_events() {
    let mut w = pinned_world();
    w.set_hidden_events(&json!(["секретное событие про оборотня"]));
    // direct lookup for canon keyword must not surface truth or hidden events.
    let f = w.fact("оборотень", "player", None);
    assert!(!f.text.contains("Капитан — оборотень"));
    assert!(!f.text.contains("секретное событие"));
}

#[test]
fn rag_document_contextual_text_format() {
    let mut metadata = Map::new();
    metadata.insert("k".to_string(), Value::from(1));
    let doc = RagDocument::new(
        "d1".to_string(),
        "public".to_string(),
        "Городские ворота открыты на рассвете.".to_string(),
        "known".to_string(),
        "scene".to_string(),
        "player".to_string(),
        vec!["ворота".to_string(), "город".to_string()],
        metadata,
    );
    let expected = "RPG world memory block.\nKind: public.\nStatus: known.\nSource: scene.\nTags: ворота, город.\nText: Городские ворота открыты на рассвете.";
    assert_eq!(doc.contextual_text(), expected);
}

#[test]
fn rng_state_roundtrips_through_world() {
    let mut w = pinned_world();
    let (_t1, _d1) = w.roll("1d20");
    let saved = w.rng_state();
    let (t_a, _) = w.roll("1d20");
    // restore and replay -> identical next roll.
    let mut w2 = pinned_world();
    w2.set_rng_state(&saved).unwrap();
    let (t_b, _) = w2.roll("1d20");
    assert_eq!(t_a, t_b);
    assert_eq!(saved.version, 3);
    assert_eq!(saved.internal.len(), 625);
}
