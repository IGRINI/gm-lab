//! Data-model + projection invariants (secret/hidden-canon isolation, scope
//! gating, card_revision discipline, whereabouts precedence, seed coercion).

use gml_world::state_record::RagDocument;
use gml_world::World;
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
    let docs = w.retrieval_documents("player");
    // No doc may carry the hidden canon text or any NPC secret.
    for d in &docs {
        assert!(!d.text.contains("оборотень"), "canon leaked: {}", d.text);
        assert!(!d.text.contains("Прячет долги"), "secret leaked: {}", d.text);
        assert!(!d.text.contains("Шпионка"), "secret leaked: {}", d.text);
        // kind=="truth" facts never become docs.
        assert_ne!(d.doc_id, "fact:hidden_truth");
    }
    // The gm-scoped state record must not appear in the player corpus.
    assert!(!docs.iter().any(|d| d.doc_id == "state:sr_gm"));
    // owner-scoped record (owner=borin) not visible to player either.
    assert!(!docs.iter().any(|d| d.doc_id == "state:sr_owner"));
    // public state record IS present.
    assert!(docs.iter().any(|d| d.doc_id == "state:sr_pub"));
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
    let profile = w.npc_profile("borin", "visible", &json!(["secret"])).unwrap();
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
