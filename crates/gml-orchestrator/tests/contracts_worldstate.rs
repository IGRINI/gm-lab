//! Contract tests for `update_world_state` and `query_world_state`, ported from
//! the corresponding block of `gm-lab/test_contracts.py` (≈ lines 755-1626).
//!
//! These drive the tool handlers directly (`worldstate::apply_world_state_batch`
//! / `worldstate::query_world_state`), exactly as the Python tests drive
//! `_apply_world_state_batch` / `_query_world_state` via `_run_tool`. The model
//! channel is reminder-wrapped in `turn.rs`; the assertions here are all on the
//! `full`-channel payload (Python `_tool_full_json`), which is what the merge/
//! dedup branches report.

use std::sync::Arc;

use serde_json::{json, Map, Value};

use gml_llm::Backend;
use gml_mock::MockClient;
use gml_orchestrator::worldstate::{
    apply_world_state_batch, consolidate_memory, get_memory, note_memory, npc_memory_recall,
    query_world_state,
};
use gml_orchestrator::Session;
use gml_world::{MemoryUnit, Npc, StateRecord, StateRecordQuery};

/// Default story seed from a HERMETIC store over a tempdir. There is no global
/// store; constructing a `StoryStore` materializes the builtins into the
/// throwaway directory, so these tests never touch the real user library.
fn default_story_seed() -> serde_json::Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = gml_stories::StoryStore::new(dir.path()).expect("open store");
    store.default_seed()
}

fn session() -> Session {
    std::env::set_var("GM_RAG_ENABLED", "0");
    let client: Arc<dyn Backend> = Arc::new(MockClient::new());
    let world = gml_world::World::from_seed(&default_story_seed());
    Session::with_world(
        client,
        world,
        Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>),
    )
}

fn upd(s: &mut Session, items: Value) -> Value {
    apply_world_state_batch(s, &json!({ "items": items }))
}

fn qry(s: &mut Session, args: Value) -> Value {
    query_world_state(s, &args)
}

fn mem_get(s: &mut Session, args: Value) -> Value {
    get_memory(s, &args)
}

fn mem_note(s: &mut Session, args: Value) -> Value {
    note_memory(s, &args)
}

fn mem_npc(s: &mut Session, args: Value) -> Value {
    npc_memory_recall(s, &args)
}

fn mem_cons(s: &mut Session, args: Value) -> Value {
    consolidate_memory(s, &args)
}

fn to_str(v: &Value) -> String {
    serde_json::to_string(v).unwrap()
}

fn state_memories(s: &Session) -> Vec<&MemoryUnit> {
    s.world
        .world_canon
        .memory
        .units
        .values()
        .filter(|unit| {
            !unit.source_state_record_ids.is_empty() && unit.created_by == "world_state_memory"
        })
        .collect()
}

fn state_memory_by_id<'a>(s: &'a Session, record_id: &str) -> Option<&'a MemoryUnit> {
    state_memories(s).into_iter().find(|unit| {
        unit.metadata
            .get("record_id")
            .and_then(Value::as_str)
            .map(|id| id == record_id)
            .unwrap_or(false)
            || unit
                .source_state_record_ids
                .iter()
                .any(|id| id == record_id)
    })
}

fn state_meta(unit: &MemoryUnit, key: &str) -> String {
    unit.metadata
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn state_meta_contains(unit: &MemoryUnit, key: &str, needle: &str) -> bool {
    unit.metadata
        .get(key)
        .and_then(Value::as_array)
        .map(|items| items.iter().any(|item| item.as_str() == Some(needle)))
        .unwrap_or(false)
}

/// First `applied` row.
fn applied0(v: &Value) -> &Value {
    &v["applied"][0]
}
/// First `errors` row.
fn errors0(v: &Value) -> &Value {
    &v["errors"][0]
}

// =========================================================================
// Living-world scoped memory
// =========================================================================

#[test]
fn scoped_hierarchical_memory_query_contract() {
    let mut s = session();
    let borin = mem_note(
        &mut s,
        json!({
            "summary": "BORIN_ONLY_SENTINEL помнит тайный пароль северных ворот.",
            "details": "Пароль сказал капитан после ночного обхода.",
            "owner_scope": "actor:borin",
            "topic_tags": ["sentinel", "gate"],
        }),
    );
    assert_eq!(borin["ok"], json!(true), "{borin}");
    let lysa = mem_note(
        &mut s,
        json!({
            "summary": "LYSA_ONLY_SENTINEL слышала разговор о пропавшем ключе.",
            "owner_scope": "actor:lysa",
            "topic_tags": ["sentinel", "key"],
        }),
    );
    assert_eq!(lysa["ok"], json!(true), "{lysa}");
    let player = mem_note(
        &mut s,
        json!({
            "summary": "PLAYER_ONLY_SENTINEL игрок запомнил знак на двери.",
            "owner_scope": "player",
            "topic_tags": ["sentinel", "door"],
        }),
    );
    assert_eq!(player["ok"], json!(true), "{player}");
    let public = mem_note(
        &mut s,
        json!({
            "summary": "PUBLIC_SENTINEL весь трактир судачит о ночной тревоге.",
            "owner_scope": "public",
            "topic_tags": ["sentinel", "rumor"],
            "truth_status": "rumor",
        }),
    );
    assert_eq!(public["ok"], json!(true), "{public}");
    let gm = mem_note(
        &mut s,
        json!({
            "summary": "GM_ONLY_SENTINEL истинная причина тревоги скрыта.",
            "owner_scope": "gm_private",
            "topic_tags": ["sentinel", "truth"],
        }),
    );
    assert_eq!(gm["ok"], json!(true), "{gm}");

    let borin_recall = mem_npc(
        &mut s,
        json!({"npc_id": "borin", "query": "sentinel", "max_results": 10}),
    );
    let borin_text = to_str(&borin_recall);
    assert!(borin_text.contains("BORIN_ONLY_SENTINEL"), "{borin_text}");
    assert!(borin_text.contains("PUBLIC_SENTINEL"), "{borin_text}");
    assert!(!borin_text.contains("LYSA_ONLY_SENTINEL"), "{borin_text}");
    assert!(!borin_text.contains("GM_ONLY_SENTINEL"), "{borin_text}");
    assert!(!borin_text.contains("PLAYER_ONLY_SENTINEL"), "{borin_text}");
    assert!(borin_recall["results"][0].get("memory_id").is_some());
    assert!(borin_recall["results"][0].get("tier").is_some());
    assert!(borin_recall["results"][0].get("owner_scope").is_some());
    assert!(borin_recall["results"][0].get("truth_status").is_some());

    let player_recall = mem_get(
        &mut s,
        json!({"scope": "player", "query": "sentinel", "max_results": 10}),
    );
    let player_text = to_str(&player_recall);
    assert!(
        player_text.contains("PLAYER_ONLY_SENTINEL"),
        "{player_text}"
    );
    assert!(player_text.contains("PUBLIC_SENTINEL"), "{player_text}");
    assert!(
        !player_text.contains("BORIN_ONLY_SENTINEL"),
        "{player_text}"
    );
    assert!(!player_text.contains("LYSA_ONLY_SENTINEL"), "{player_text}");
    assert!(!player_text.contains("GM_ONLY_SENTINEL"), "{player_text}");

    let hidden_short = mem_get(
        &mut s,
        json!({"scope": "actor", "npc_id": "borin", "query": "пароль"}),
    );
    assert!(!to_str(&hidden_short).contains("капитан после ночного обхода"));
    let hidden_detail = mem_get(
        &mut s,
        json!({
            "scope": "actor",
            "npc_id": "borin",
            "query": "пароль",
            "include_details": true
        }),
    );
    assert!(to_str(&hidden_detail).contains("капитан после ночного обхода"));

    let bad = mem_get(
        &mut s,
        json!({"scope": "actor", "npc_id": "no_such_npc", "query": "x"}),
    );
    assert_eq!(bad["status"], "error");
}

#[test]
fn public_memory_scope_does_not_read_player_private_memory() {
    let mut s = session();
    let stored = mem_note(
        &mut s,
        json!({
            "summary": "PLAYER_ONLY_MEMORY_SENTINEL",
            "owner_scope": "player",
            "visibility_scopes": ["player"],
            "topic_tags": ["private_player_note"],
        }),
    );
    assert_eq!(stored["ok"], json!(true), "{stored}");

    let player = mem_get(
        &mut s,
        json!({"scope": "player", "query": "PLAYER_ONLY_MEMORY_SENTINEL"}),
    );
    assert_eq!(player["status"], "known", "{player}");

    let public = mem_get(
        &mut s,
        json!({"scope": "public", "query": "PLAYER_ONLY_MEMORY_SENTINEL"}),
    );
    assert_eq!(public["scope"], "public");
    assert_eq!(public["status"], "unknown", "{public}");
    assert!(
        public
            .get("results")
            .and_then(Value::as_array)
            .map(|rows| rows.is_empty())
            .unwrap_or(true),
        "{public}"
    );
}

#[test]
fn memory_tools_report_retrieval_mode() {
    let mut s = session();
    let stored = mem_note(
        &mut s,
        json!({
            "summary": "RETRIEVAL_STATUS_SENTINEL public memory row.",
            "owner_scope": "public",
            "topic_tags": ["retrieval_status"],
        }),
    );
    assert_eq!(stored["ok"], json!(true), "{stored}");

    let result = mem_get(
        &mut s,
        json!({"scope": "public", "query": "RETRIEVAL_STATUS_SENTINEL"}),
    );
    assert_eq!(result["status"], "known", "{result}");
    assert_eq!(result["retrieval"]["enabled"], json!(false), "{result}");
    assert_eq!(result["retrieval"]["backend"], json!("lexical"), "{result}");
    assert_eq!(result["retrieval"]["reason"], json!("disabled"), "{result}");
}

#[test]
fn note_memory_records_player_known_npc_name_without_legacy_state_record() {
    let mut s = session();
    s.world.npcs.insert(
        "masked_traveler".to_string(),
        Npc {
            npc_id: "masked_traveler".to_string(),
            name: "Илья".to_string(),
            public_label: "путник в сером плаще".to_string(),
            role: "путник".to_string(),
            persona: String::new(),
            voice: String::new(),
            goals: String::new(),
            knowledge: String::new(),
            secret: String::new(),
            pronouns: "he/him".to_string(),
            color: String::new(),
            age: String::new(),
            physical_type: String::new(),
            distinctive_features: String::new(),
            current_appearance: String::new(),
            life_status: "alive".to_string(),
            life_status_note: String::new(),
            condition: String::new(),
            personality: String::new(),
            values: String::new(),
            habits: String::new(),
            pressure_response: String::new(),
            boundaries: String::new(),
            abilities: Default::default(),
            skills: Default::default(),
            saving_throws: Default::default(),
            passive_perception: None,
            ac: Value::Null,
            hp: Default::default(),
            speed: String::new(),
            senses: String::new(),
            languages: String::new(),
            default_whereabouts: None,
            card_revision: 0,
        },
    );
    assert_eq!(
        s.world.npc_player_label("masked_traveler", "player"),
        "путник в сером плаще"
    );

    let stored = mem_note(
        &mut s,
        json!({
            "summary": "Игрок узнал, что путника в сером плаще зовут Илья.",
            "owner_scope": "player",
            "visibility_scopes": ["player"],
            "entity_id": "masked_traveler",
            "known_name": "Илья",
            "topic_tags": ["known_name"],
            "truth_status": "actual",
        }),
    );
    assert_eq!(stored["ok"], json!(true), "{stored}");
    assert_eq!(stored["result"]["metadata"]["known_name"], json!("Илья"));
    assert_eq!(
        stored["result"]["metadata"]["entity_id"],
        json!("masked_traveler")
    );
    assert_eq!(
        s.world.npc_player_label("masked_traveler", "player"),
        "Илья"
    );

    let state_rows = s
        .world
        .state_records_for(&StateRecordQuery::new("player"))
        .into_iter()
        .filter(|record| record.entity_id == "masked_traveler")
        .count();
    assert_eq!(state_rows, 0, "known_name must not create StateRecord rows");
}

#[test]
fn memory_consolidation_tool_payload_is_append_only() {
    let mut s = session();
    let a = mem_note(
        &mut s,
        json!({
            "summary": "RAW_A_SENTINEL Борин видел сломанный караван.",
            "owner_scope": "actor:borin",
            "topic_tags": ["caravan"],
        }),
    );
    let b = mem_note(
        &mut s,
        json!({
            "summary": "RAW_B_SENTINEL Борин слышал, что караван ограбили.",
            "owner_scope": "actor:borin",
            "topic_tags": ["caravan"],
        }),
    );
    let source_ids = vec![
        a["memory_id"].as_str().unwrap().to_string(),
        b["memory_id"].as_str().unwrap().to_string(),
    ];
    let crystal = mem_cons(
        &mut s,
        json!({
            "source_memory_ids": source_ids,
            "summary": "CRYSTAL_SENTINEL Борин связывает сломанный караван с ограблением.",
            "owner_scope": "actor:borin",
            "tier": "episode",
            "topic_tags": ["caravan"],
        }),
    );
    assert_eq!(crystal["ok"], json!(true), "{crystal}");
    assert_eq!(crystal["not_deleted"], json!(true));
    assert_eq!(crystal["consumed_source_ids"].as_array().unwrap().len(), 2);

    let default_query = mem_npc(
        &mut s,
        json!({"npc_id": "borin", "query": "caravan", "max_results": 10}),
    );
    let default_text = to_str(&default_query);
    assert!(default_text.contains("CRYSTAL_SENTINEL"), "{default_text}");
    assert!(!default_text.contains("RAW_A_SENTINEL"), "{default_text}");
    assert!(!default_text.contains("RAW_B_SENTINEL"), "{default_text}");

    let drilldown = mem_npc(
        &mut s,
        json!({
            "npc_id": "borin",
            "query": "caravan",
            "max_results": 10,
            "include_cold": true
        }),
    );
    let drilldown_text = to_str(&drilldown);
    assert!(
        drilldown_text.contains("CRYSTAL_SENTINEL"),
        "{drilldown_text}"
    );
    assert!(
        drilldown_text.contains("RAW_A_SENTINEL"),
        "{drilldown_text}"
    );
    assert!(
        drilldown_text.contains("RAW_B_SENTINEL"),
        "{drilldown_text}"
    );
}

#[test]
fn memory_consolidation_rejects_mixed_source_scopes_without_consuming_sources() {
    let mut s = session();
    let borin = mem_note(
        &mut s,
        json!({
            "summary": "BORIN_SOURCE_SENTINEL Борин видел следы у тракта.",
            "owner_scope": "actor:borin",
            "topic_tags": ["mixed_scope_guard"],
        }),
    );
    let lysa = mem_note(
        &mut s,
        json!({
            "summary": "LYSA_SOURCE_SENTINEL Лиса слышала другой слух.",
            "owner_scope": "actor:lysa",
            "topic_tags": ["mixed_scope_guard"],
        }),
    );
    let source_ids = vec![
        borin["memory_id"].as_str().unwrap().to_string(),
        lysa["memory_id"].as_str().unwrap().to_string(),
    ];

    let rejected = mem_cons(
        &mut s,
        json!({
            "source_memory_ids": source_ids,
            "summary": "BAD_CRYSTAL_SENTINEL смешивает чужие воспоминания.",
            "owner_scope": "actor:borin",
            "topic_tags": ["mixed_scope_guard"],
        }),
    );
    assert_eq!(rejected["ok"], json!(false), "{rejected}");
    assert_eq!(rejected["status"], json!("error"), "{rejected}");

    let borin_recall = mem_npc(
        &mut s,
        json!({
            "npc_id": "borin",
            "query": "BORIN_SOURCE_SENTINEL",
            "max_results": 10
        }),
    );
    assert_eq!(borin_recall["status"], json!("known"), "{borin_recall}");
    assert!(to_str(&borin_recall).contains("BORIN_SOURCE_SENTINEL"));
}

#[test]
fn query_world_state_does_not_read_unsynced_raw_state_records() {
    let mut s = session();
    s.world.state_records.push(StateRecord {
        record_id: "raw_unsynced_state".to_string(),
        kind: "fact".to_string(),
        text: "RAW_UNSYNCED_STATE_SENTINEL".to_string(),
        scope: "public".to_string(),
        active: true,
        owner: String::new(),
        subject: String::new(),
        source: String::new(),
        status: "known".to_string(),
        tags: Vec::new(),
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

    let raw = qry(
        &mut s,
        json!({"scope": "player", "query": "RAW_UNSYNCED_STATE_SENTINEL"}),
    );
    assert!(
        !to_str(&raw).contains("RAW_UNSYNCED_STATE_SENTINEL"),
        "query_world_state must not read raw StateRecord rows directly: {raw}"
    );

    let legacy_len_before_update = s.world.state_records.len();
    let synced = upd(
        &mut s,
        json!([{
            "type": "fact",
            "text": "SYNCED_STATE_MEMORY_SENTINEL",
            "scope": "public"
        }]),
    );
    assert_eq!(
        s.world.state_records.len(),
        legacy_len_before_update,
        "update_world_state must not append live rows to legacy StateRecord storage"
    );
    assert!(
        state_memories(&s)
            .iter()
            .any(|unit| unit.summary == "SYNCED_STATE_MEMORY_SENTINEL"),
        "update_world_state must persist new rows as canon memory"
    );
    assert!(
        matches!(
            applied0(&synced)["status"].as_str(),
            Some("added" | "stored")
        ),
        "{synced}"
    );
    let visible = qry(
        &mut s,
        json!({"scope": "player", "query": "SYNCED_STATE_MEMORY_SENTINEL"}),
    );
    assert!(
        to_str(&visible).contains("SYNCED_STATE_MEMORY_SENTINEL"),
        "legacy add must still be visible after syncing into scoped memory"
    );
}

// =========================================================================
// known_name / entity identity (Python ≈ 755-790)
// =========================================================================

#[test]
fn known_name_identity_and_missing_entity_id() {
    let mut s = session();
    assert_eq!(s.world.npc_player_label("lysa", "player"), "служанка");

    let ret = upd(
        &mut s,
        json!([{
            "type": "fact",
            "text": "Игрок узнал от Борина, что служанку зовут Лиза.",
            "scope": "shared",
            "npc_id": "borin",
            "target": "player",
            "entity_id": "lysa",
            "known_name": "Лиза",
        }]),
    );
    assert_eq!(applied0(&ret)["known_name"], "Лиза");
    assert_eq!(applied0(&ret)["entity_id"], "lysa");
    assert_eq!(s.world.npc_known_name("lysa", "player"), "Лиза");
    assert_eq!(s.world.npc_player_label("lysa", "player"), "Лиза");
    let refs = to_str(&s.world.entity_refs());
    assert!(
        refs.contains("\"label\":\"Лиза\""),
        "entity_refs missing label: {refs}"
    );

    let q = qry(
        &mut s,
        json!({"scope": "player", "query": "known_name Лиза"}),
    );
    let rows: Vec<&Value> = q["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["known_name"] == json!("Лиза"))
        .collect();
    assert!(
        !rows.is_empty()
            && rows[0]["hash"]
                .as_str()
                .map(|s| !s.is_empty())
                .unwrap_or(false)
    );

    let bad = upd(
        &mut s,
        json!([{
            "type": "fact",
            "text": "Игрок узнал имя без entity_id.",
            "known_name": "Ошибка",
        }]),
    );
    assert_eq!(bad["ok"], json!(false));
    assert!(errors0(&bad)["error"]
        .as_str()
        .unwrap()
        .contains("entity_id is required"));
}

// =========================================================================
// Big mixed batch + scope isolation (Python ≈ 1055-1166)
// =========================================================================

fn batch_session() -> Session {
    let mut s = session();
    let ret = upd(
        &mut s,
        json!([
            {
                "type": "fact",
                "text": "На площади закрыли ворота.",
                "scope": "public",
                "location_id": "turnvale_square",
                "location_name": "Площадь Тёрнвейля",
                "region_id": "turnvale",
                "region_name": "Тёрнвейль",
                "scene_id": "turnvale_square_gate",
                "importance": "clue",
                "aliases": ["Тёрнвейл", "Тёрнвейле", "Turnvale", "turnvale"],
            },
            {"type": "fact", "text": "GM_SECRET_SENTINEL прячется под сценой.", "scope": "gm"},
            {"type": "rumor", "text": "PUBLIC_RUMOR_SENTINEL видели у лавки.", "npc_id": "borin"},
            {
                "type": "rumor",
                "text": "SHARED_RUMOR_SENTINEL сказала Лиза только игроку.",
                "npc_id": "lysa",
                "target": "player",
                "scope": "shared",
            },
            {
                "type": "rumor",
                "text": "ENTITY_FACT_SENTINEL Борин утверждает, что Лизе 24.",
                "npc_id": "borin",
                "target": "player",
                "scope": "shared",
                "entity_id": "lysa",
                "source_npc": "borin",
            },
            {
                "type": "npc_memory",
                "text": "NPC_PRIVATE_SENTINEL хранить молчание.",
                "npc_id": "borin",
                "location_id": "secret_cellar",
                "location_name": "Тайный подвал",
                "region_id": "turnvale",
                "region_name": "Тёрнвейль",
                "aliases": ["PRIVATE_ALIAS_SENTINEL"],
            },
            {"type": "relationship", "text": "стал доверять осторожнее", "npc_id": "borin", "target": "player"},
            {"type": "goal", "text": "GOAL_SENTINEL проверить кладовую.", "npc_id": "borin", "mode": "append"},
        ]),
    );
    assert_eq!(ret["ok"], json!(true));
    assert_eq!(
        ret["applied"].as_array().unwrap().len(),
        8,
        "batch applied: {ret}"
    );
    let anchor = applied0(&ret);
    assert_eq!(anchor["location_id"], "turnvale_square");
    assert_eq!(anchor["region_id"], "turnvale");
    assert_eq!(anchor["scene_id"], "turnvale_square_gate");
    assert_eq!(
        anchor["aliases"],
        json!(["Тёрнвейл", "Тёрнвейле", "Turnvale", "turnvale"])
    );

    let memories = state_memories(&s);
    assert!(memories
        .iter()
        .any(|unit| unit.summary == "На площади закрыли ворота."
            && state_meta(unit, "legacy_kind") == "fact"
            && state_meta(unit, "legacy_scope") == "public"
            && state_meta(unit, "location_id") == "turnvale_square"
            && state_meta(unit, "location_name") == "Площадь Тёрнвейля"
            && state_meta(unit, "region_id") == "turnvale"
            && state_meta(unit, "region_name") == "Тёрнвейль"
            && state_meta(unit, "scene_id") == "turnvale_square_gate"
            && state_meta(unit, "importance") == "clue"
            && state_meta_contains(unit, "aliases", "Тёрнвейле")));
    assert!(memories.iter().any(
        |unit| unit.summary == "GM_SECRET_SENTINEL прячется под сценой."
            && state_meta(unit, "legacy_kind") == "fact"
            && state_meta(unit, "legacy_scope") == "gm"
    ));
    assert!(memories.iter().any(
        |unit| unit.summary == "NPC_PRIVATE_SENTINEL хранить молчание."
            && state_meta(unit, "legacy_kind") == "npc_memory"
            && state_meta(unit, "owner") == "borin"
            && state_meta(unit, "location_id") == "secret_cellar"
            && state_meta_contains(unit, "aliases", "PRIVATE_ALIAS_SENTINEL")
    ));
    assert!(memories.iter().any(|unit| unit.summary
        == "SHARED_RUMOR_SENTINEL сказала Лиза только игроку."
        && state_meta(unit, "legacy_kind") == "rumor"
        && state_meta(unit, "legacy_scope") == "participants"
        && state_meta(unit, "owner") == "lysa"
        && state_meta(unit, "subject") == "player"));
    assert!(memories.iter().any(|unit| unit.summary
        == "ENTITY_FACT_SENTINEL Борин утверждает, что Лизе 24."
        && state_meta(unit, "legacy_kind") == "rumor"
        && state_meta(unit, "legacy_scope") == "participants"
        && state_meta(unit, "owner") == "borin"
        && state_meta(unit, "subject") == "player"
        && state_meta(unit, "entity_id") == "lysa"
        && state_meta(unit, "source_npc") == "borin"));
    assert!(memories
        .iter()
        .any(|unit| state_meta(unit, "legacy_kind") == "goal"
            && unit.summary.contains("GOAL_SENTINEL")
            && state_meta(unit, "owner") == "borin"));
    s
}

#[test]
fn big_batch_and_player_scope_isolation() {
    let mut s = batch_session();

    let player_q = qry(
        &mut s,
        json!({"scope": "player", "query": "GM_SECRET_SENTINEL NPC_PRIVATE_SENTINEL GOAL_SENTINEL"}),
    );
    assert_eq!(player_q["scope"], "player");
    let dump = to_str(&player_q);
    assert!(!dump.contains("GM_SECRET_SENTINEL"));
    assert!(!dump.contains("NPC_PRIVATE_SENTINEL"));
    assert!(!dump.contains("GOAL_SENTINEL"));

    // Shared rumor with player target IS visible to player.
    let shared = qry(
        &mut s,
        json!({"scope": "player", "query": "SHARED_RUMOR_SENTINEL"}),
    );
    assert!(to_str(&shared).contains("SHARED_RUMOR_SENTINEL"));
    assert!(shared["results"].as_array().unwrap().iter().any(|r| {
        r.get("id")
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            && r["target"] == json!("player")
    }));
    assert!(shared["results"].as_array().unwrap().iter().any(|r| r
        .get("hash")
        .and_then(Value::as_str)
        .map(|s| !s.is_empty())
        .unwrap_or(false)));

    let entity_q = qry(
        &mut s,
        json!({"scope": "player", "query": "ENTITY_FACT_SENTINEL lysa borin"}),
    );
    assert!(to_str(&entity_q).contains("ENTITY_FACT_SENTINEL"));
    let entity_row = entity_q["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["entity_id"] == json!("lysa"))
        .expect("entity row");
    assert_eq!(entity_row["source_npc"], "borin");
    assert_eq!(entity_row["target"], "player");
}

#[test]
fn player_place_query_and_gm_scope() {
    let mut s = batch_session();

    let place = qry(
        &mut s,
        json!({"scope": "player", "query": "что было в Тёрнвейле"}),
    );
    assert_eq!(place["scope"], "player");
    let place_row = place["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["location_id"] == json!("turnvale_square"))
        .expect("place row");
    assert_eq!(place_row["region_id"], "turnvale");
    assert_eq!(place_row["location_name"], "Площадь Тёрнвейля");
    // state-record query rows render aliases as a ", "-joined string.
    assert!(place_row["aliases"].as_str().unwrap().contains("Тёрнвейле"));
    assert!(place_row["hash"]
        .as_str()
        .map(|s| !s.is_empty())
        .unwrap_or(false));

    let gm = qry(
        &mut s,
        json!({"scope": "gm", "query": "GM_SECRET_SENTINEL"}),
    );
    assert_eq!(gm["scope"], "gm");
    assert!(to_str(&gm).contains("GM_SECRET_SENTINEL"));
}

#[test]
fn default_gm_query_excludes_hidden_truth() {
    let mut s = session();
    let gm = qry(
        &mut s,
        json!({"scope": "gm", "query": "Борин тайник метка для встречи край стойки Дарра"}),
    );
    let results = gm["results"].as_array().unwrap();
    assert!(results
        .iter()
        .all(|r| !(r["kind"] == json!("truth_fact") && r["id"] == json!("hidden_truth"))));
    assert!(results.iter().any(|r| r["kind"] == json!("gm_canon")));
    let canon_intro = results
        .iter()
        .filter(|r| {
            r.get("text")
                .and_then(Value::as_str)
                .map(|t| t.starts_with("Прошлой ночью в городе Тёрнвейл"))
                .unwrap_or(false)
        })
        .count();
    assert_eq!(canon_intro, 1);
}

// =========================================================================
// participants merge / not_added (Python ≈ 1191-1288)
// =========================================================================

#[test]
fn participants_merge_then_not_added() {
    let mut s = session();
    let add = upd(
        &mut s,
        json!([{
            "type": "rumor",
            "text": "MULTI_PARTICIPANT_SENTINEL Борин дал одно показание Дарре и Марет.",
            "npc_id": "borin",
            "target": "player",
            "participants": ["mareth"],
            "scope": "shared",
        }]),
    );
    assert_eq!(applied0(&add)["participants"], json!(["mareth"]));
    let add_id = applied0(&add)["id"].as_str().unwrap().to_string();

    // Visible to player and mareth, not to lysa (not a participant yet).
    let pl = qry(
        &mut s,
        json!({"scope": "player", "query": "MULTI_PARTICIPANT_SENTINEL"}),
    );
    assert!(to_str(&pl).contains("MULTI_PARTICIPANT_SENTINEL"));
    let mareth = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "mareth", "query": "MULTI_PARTICIPANT_SENTINEL"}),
    );
    assert!(to_str(&mareth).contains("MULTI_PARTICIPANT_SENTINEL"));
    let lysa = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "lysa", "query": "MULTI_PARTICIPANT_SENTINEL"}),
    );
    assert!(!to_str(&lysa).contains("MULTI_PARTICIPANT_SENTINEL"));

    // Re-add identical text with target=lysa -> merge participants (lysa added).
    let merge = upd(
        &mut s,
        json!([{
            "type": "rumor",
            "text": "MULTI_PARTICIPANT_SENTINEL Борин дал одно показание Дарре и Марет.",
            "npc_id": "borin",
            "target": "lysa",
            "scope": "shared",
        }]),
    );
    assert_eq!(applied0(&merge)["status"], "merged");
    assert_eq!(applied0(&merge)["id"], json!(add_id));
    assert!(applied0(&merge)["participants"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p == "lysa"));

    // Now lysa can see it.
    let lysa_after = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "lysa", "query": "MULTI_PARTICIPANT_SENTINEL"}),
    );
    assert!(to_str(&lysa_after).contains("MULTI_PARTICIPANT_SENTINEL"));

    // Identical text + identical participant set -> not_added (no change).
    let dup = upd(
        &mut s,
        json!([{
            "type": "rumor",
            "text": "MULTI_PARTICIPANT_SENTINEL Борин дал одно показание Дарре и Марет.",
            "npc_id": "borin",
            "target": "player",
            "participants": ["mareth", "lysa"],
            "scope": "shared",
        }]),
    );
    assert_eq!(dup["ok"], json!(false));
    assert_eq!(errors0(&dup)["status"], "not_added");
    assert_eq!(errors0(&dup)["existing_id"], json!(add_id));
}

#[test]
fn targetless_participants_array() {
    let mut s = session();
    let add = upd(
        &mut s,
        json!([{
            "type": "rumor",
            "text": "TARGETLESS_PARTICIPANTS_SENTINEL одна запись известна сразу Дарре и Марет.",
            "npc_id": "borin",
            "participants": ["player", "mareth"],
            "scope": "shared",
        }]),
    );
    assert_eq!(add["ok"], json!(true));
    // target dropped (empty) by drop_empty; participants kept in insertion order.
    assert_eq!(
        add["applied"][0]
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or(""),
        ""
    );
    assert_eq!(applied0(&add)["participants"], json!(["player", "mareth"]));

    let pl = qry(
        &mut s,
        json!({"scope": "player", "query": "TARGETLESS_PARTICIPANTS_SENTINEL"}),
    );
    assert!(to_str(&pl).contains("TARGETLESS_PARTICIPANTS_SENTINEL"));
    let mareth = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "mareth", "query": "TARGETLESS_PARTICIPANTS_SENTINEL"}),
    );
    assert!(to_str(&mareth).contains("TARGETLESS_PARTICIPANTS_SENTINEL"));
}

#[test]
fn bad_shared_target_rejected() {
    let mut s = session();
    let bad = upd(
        &mut s,
        json!([{
            "type": "rumor",
            "text": "BAD_SHARED_TARGET_SENTINEL не должен сохраниться.",
            "npc_id": "borin",
            "target": "turnvale_square",
            "scope": "shared",
        }]),
    );
    assert_eq!(bad["ok"], json!(false));
    assert!(errors0(&bad)["error"]
        .as_str()
        .unwrap()
        .contains("target for shared scope must be player or a known npc_id"));
}

// =========================================================================
// query de-dup cache + pagination + compaction reset (Python ≈ 1329-1430)
// =========================================================================

#[test]
fn query_cache_dedup_and_new_rows() {
    let mut s = session();
    let add1 = upd(
        &mut s,
        json!([{"type": "fact", "text": "QUERY_CACHE_SENTINEL первая улика для проверки выдачи.", "scope": "gm"}]),
    );
    assert_eq!(applied0(&add1)["status"], "stored");

    let first = qry(
        &mut s,
        json!({"scope": "gm", "query": "QUERY_CACHE_SENTINEL"}),
    );
    assert_eq!(first["status"], "known");
    assert!(to_str(&first).contains("QUERY_CACHE_SENTINEL первая"));

    let repeat = qry(
        &mut s,
        json!({"scope": "gm", "query": "QUERY_CACHE_SENTINEL первая улика"}),
    );
    assert_eq!(repeat["status"], "already_delivered");
    assert!(repeat["already_delivered"].as_i64().unwrap() >= 1);
    assert!(repeat["results"]
        .as_array()
        .map(|a| a.is_empty())
        .unwrap_or(true));
    assert!(repeat["text"]
        .as_str()
        .unwrap()
        .contains("already delivered"));

    let add2 = upd(
        &mut s,
        json!([{"type": "fact", "text": "QUERY_CACHE_SENTINEL вторая новая улика после первого поиска.", "scope": "gm"}]),
    );
    assert_eq!(applied0(&add2)["status"], "stored");
    let new = qry(
        &mut s,
        json!({"scope": "gm", "query": "QUERY_CACHE_SENTINEL"}),
    );
    assert_eq!(new["status"], "known");
    assert!(to_str(&new).contains("QUERY_CACHE_SENTINEL вторая"));
    assert!(!to_str(&new["results"]).contains("QUERY_CACHE_SENTINEL первая"));
}

#[test]
fn query_cache_resets_after_compaction() {
    let mut s = session();
    upd(
        &mut s,
        json!([{"type": "fact", "text": "QUERY_CACHE_SENTINEL первая улика для проверки выдачи.", "scope": "gm"}]),
    );
    // Deliver once so the scope cache is populated.
    let _ = qry(
        &mut s,
        json!({"scope": "gm", "query": "QUERY_CACHE_SENTINEL"}),
    );
    assert!(!s.world_query_seen.is_empty());

    // GM history compaction resets the world-query delivery cache.
    s.reset_world_query_cache();
    assert!(s.world_query_seen.is_empty());

    let after = qry(
        &mut s,
        json!({"scope": "gm", "query": "QUERY_CACHE_SENTINEL"}),
    );
    assert_eq!(after["status"], "known");
    assert!(to_str(&after).contains("QUERY_CACHE_SENTINEL первая"));
}

#[test]
fn query_pagination_with_limit() {
    let mut s = session();
    let add = upd(
        &mut s,
        json!([
            {"type": "fact", "text": "QUERY_LIMIT_SENTINEL первая строка при лимите.", "scope": "gm"},
            {"type": "fact", "text": "QUERY_LIMIT_SENTINEL вторая строка за пределом первого лимита.", "scope": "gm"},
        ]),
    );
    assert_eq!(add["applied"].as_array().unwrap().len(), 2);

    let first = qry(
        &mut s,
        json!({"scope": "gm", "query": "QUERY_LIMIT_SENTINEL", "max_results": 1}),
    );
    assert_eq!(first["results"].as_array().unwrap().len(), 1);
    let first_text = first["results"][0]["text"].as_str().unwrap().to_string();

    let second = qry(
        &mut s,
        json!({"scope": "gm", "query": "QUERY_LIMIT_SENTINEL", "max_results": 2}),
    );
    assert_eq!(second["status"], "known");
    assert_eq!(second["results"].as_array().unwrap().len(), 1);
    assert_ne!(second["results"][0]["text"].as_str().unwrap(), first_text);
    assert!(second["results"][0]["text"]
        .as_str()
        .unwrap()
        .contains("QUERY_LIMIT_SENTINEL"));
}

// =========================================================================
// NPC-scope isolation + relationship lookup (Python ≈ 1432-1479)
// =========================================================================

#[test]
fn npc_scope_isolation_and_relationship_lookup() {
    let mut s = batch_session();

    let borin = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "borin", "query": "NPC_PRIVATE_SENTINEL GOAL_SENTINEL"}),
    );
    assert_eq!(borin["scope"], "npc");
    let borin_dump = to_str(&borin);
    assert!(borin_dump.contains("NPC_PRIVATE_SENTINEL"));
    assert!(borin_dump.contains("GOAL_SENTINEL"));
    let lysa_secret = s.world.npc("lysa").unwrap().secret.clone();
    if !lysa_secret.is_empty() {
        assert!(!borin_dump.contains(&lysa_secret));
    }

    // Shared rumor (lysa->player) is NOT visible in borin scope...
    let borin_shared = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "borin", "query": "SHARED_RUMOR_SENTINEL"}),
    );
    assert!(!to_str(&borin_shared).contains("SHARED_RUMOR_SENTINEL"));
    // ...but IS visible in lysa scope (she is the owner/participant).
    let lysa_shared = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "lysa", "query": "SHARED_RUMOR_SENTINEL"}),
    );
    assert!(to_str(&lysa_shared).contains("SHARED_RUMOR_SENTINEL"));

    // entity_id record (borin->player about lysa) is not visible to lysa.
    let lysa_entity = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "lysa", "query": "ENTITY_FACT_SENTINEL"}),
    );
    assert!(!to_str(&lysa_entity).contains("ENTITY_FACT_SENTINEL"));

    let relation = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "borin", "query": "relationship borin player"}),
    );
    let rel_rows: Vec<&Value> = relation["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == json!("state_relationship") && r["npc_id"] == json!("borin"))
        .collect();
    assert!(!rel_rows.is_empty());
    assert_eq!(rel_rows[0]["target"], "player");
    assert!(rel_rows[0]["hash"]
        .as_str()
        .map(|s| !s.is_empty())
        .unwrap_or(false));
}

// =========================================================================
// bad batch error reporting (Python ≈ 1509-1519)
// =========================================================================

#[test]
fn bad_batch_unknown_npc() {
    let mut s = batch_session();
    let bad = upd(
        &mut s,
        json!([{"type": "npc_memory", "text": "x", "npc_id": "ghost"}]),
    );
    assert_eq!(bad["ok"], json!(false));
    assert_eq!(errors0(&bad)["index"], json!(1));
}

// =========================================================================
// add/update/delete fact round-trip (Python ≈ 1521-1538)
// =========================================================================

#[test]
fn mutable_fact_add_update_delete() {
    let mut s = session();
    let legacy_len = s.world.state_records.len();
    let add = upd(
        &mut s,
        json!([{"op": "add", "type": "fact", "text": "MUTABLE_FACT_SENTINEL стоит у колодца.", "scope": "public"}]),
    );
    let id = applied0(&add)["id"].as_str().unwrap().to_string();
    assert_eq!(s.world.state_records.len(), legacy_len);
    assert!(
        state_memory_by_id(&s, &id)
            .map(|unit| unit.summary.contains("стоит у колодца"))
            .unwrap_or(false),
        "added fact must be persisted as canon memory"
    );

    let update = upd(
        &mut s,
        json!([{"op": "update", "id": id, "text": "MUTABLE_FACT_SENTINEL ушёл к воротам."}]),
    );
    assert_eq!(applied0(&update)["status"], "updated");
    assert_eq!(s.world.state_records.len(), legacy_len);
    assert!(
        state_memory_by_id(&s, &id)
            .map(|unit| unit.summary.contains("ушёл к воротам"))
            .unwrap_or(false),
        "updated fact must update the canon memory row"
    );
    let payload = s
        .world
        .fact("MUTABLE_FACT_SENTINEL", "player", None)
        .as_tool_payload();
    assert!(payload["text"].as_str().unwrap().contains("ушёл к воротам"));

    let del = upd(&mut s, json!([{"op": "delete", "id": id}]));
    assert_eq!(applied0(&del)["status"], "deleted");
    assert_eq!(s.world.state_records.len(), legacy_len);
    assert_eq!(
        state_memory_by_id(&s, &id)
            .map(|unit| unit.injection_state.as_str())
            .unwrap_or(""),
        "archived"
    );
    let payload2 = s
        .world
        .fact("MUTABLE_FACT_SENTINEL", "player", None)
        .as_tool_payload();
    assert!(!payload2["text"]
        .as_str()
        .unwrap()
        .contains("MUTABLE_FACT_SENTINEL"));
}

// =========================================================================
// relationship lifecycle: add -> update (hash) -> conflict -> not_added ->
// delete (Python ≈ 1540-1626)
// =========================================================================

#[test]
fn relationship_lifecycle() {
    let mut s = session();

    let add = upd(
        &mut s,
        json!([{
            "op": "add",
            "type": "relationship",
            "text": "RELATION_SENTINEL относится к игроку настороженно.",
            "npc_id": "borin",
            "target": "player",
        }]),
    );
    assert_eq!(applied0(&add)["status"], "stored");

    let q = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "borin", "query": "relationship borin player"}),
    );
    let rel_row = q["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == json!("state_relationship") && r["target"] == json!("player"))
        .expect("relationship row");
    let relation_id = rel_row["id"].as_str().unwrap().to_string();
    let relation_hash = rel_row["hash"].as_str().unwrap().to_string();

    // op=update with the current hash succeeds.
    let update = upd(
        &mut s,
        json!([{
            "op": "update",
            "id": relation_id,
            "expected_hash": relation_hash,
            "type": "relationship",
            "text": "RELATION_SENTINEL доверяет игроку, но скрывает тревогу.",
            "npc_id": "borin",
            "target": "player",
        }]),
    );
    assert_eq!(applied0(&update)["status"], "updated");
    let updated_hash = applied0(&update)["hash"].as_str().unwrap().to_string();
    assert!(!updated_hash.is_empty());

    // Exactly one active relationship; text changed.
    let active: Vec<&MemoryUnit> = state_memories(&s)
        .into_iter()
        .filter(|unit| {
            unit.injection_state.is_default_visible()
                && state_meta(unit, "legacy_kind") == "relationship"
                && state_meta(unit, "owner") == "borin"
                && state_meta(unit, "subject") == "player"
        })
        .collect();
    assert_eq!(active.len(), 1);
    assert!(active[0].summary.contains("доверяет игроку"));

    // Stale hash -> conflict, not stored.
    let conflict = upd(
        &mut s,
        json!([{
            "op": "update",
            "id": relation_id,
            "expected_hash": relation_hash,
            "type": "relationship",
            "text": "RELATION_SENTINEL конфликт не должен записаться.",
            "npc_id": "borin",
            "target": "player",
        }]),
    );
    assert_eq!(conflict["ok"], json!(false));
    assert_eq!(errors0(&conflict)["status"], "conflict");
    assert_eq!(errors0(&conflict)["expected_hash"], json!(relation_hash));
    assert_eq!(errors0(&conflict)["actual_hash"], json!(updated_hash));
    // Re-read: text unchanged.
    let active2 = state_memory_by_id(&s, &relation_id).expect("relationship memory");
    assert!(!active2.summary.contains("конфликт не должен"));

    // op=add of a duplicate relationship -> not_added (relationship-exists branch).
    let dup = upd(
        &mut s,
        json!([{
            "op": "add",
            "type": "relationship",
            "text": "RELATION_SENTINEL второй дубль.",
            "npc_id": "borin",
            "target": "player",
        }]),
    );
    assert_eq!(dup["ok"], json!(false));
    assert_eq!(errors0(&dup)["status"], "not_added");
    assert_eq!(errors0(&dup)["existing_id"], json!(relation_id));
    assert_eq!(errors0(&dup)["existing_hash"], json!(updated_hash));
    assert_eq!(errors0(&dup)["target"], "player");

    // delete with the current hash.
    let del = upd(
        &mut s,
        json!([{"op": "delete", "id": relation_id, "expected_hash": updated_hash}]),
    );
    assert_eq!(applied0(&del)["status"], "deleted");
    let gone = qry(
        &mut s,
        json!({"scope": "npc", "npc_id": "borin", "query": "RELATION_SENTINEL"}),
    );
    assert!(!to_str(&gone).contains("RELATION_SENTINEL"));
}

// =========================================================================
// goal replace-mode delete-then-add (Python merge semantics, lines 1421-1429)
// =========================================================================

#[test]
fn goal_replace_mode_deletes_prior_goals() {
    let mut s = session();
    let g1 = upd(
        &mut s,
        json!([{"type": "goal", "text": "GOAL_ONE найти улики.", "npc_id": "borin"}]),
    );
    assert_eq!(applied0(&g1)["status"], "stored");
    let g2 = upd(
        &mut s,
        json!([{"type": "goal", "text": "GOAL_TWO защитить таверну.", "npc_id": "borin", "mode": "replace"}]),
    );
    assert_eq!(applied0(&g2)["status"], "stored");
    assert_eq!(applied0(&g2)["mode"], "replace");

    // Only the replacement goal is active for borin.
    let active: Vec<&MemoryUnit> = state_memories(&s)
        .into_iter()
        .filter(|unit| {
            unit.injection_state.is_default_visible()
                && state_meta(unit, "legacy_kind") == "goal"
                && state_meta(unit, "owner") == "borin"
        })
        .collect();
    let texts: Vec<&str> = active.iter().map(|unit| unit.summary.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("GOAL_TWO")),
        "GOAL_TWO active: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("GOAL_ONE")),
        "GOAL_ONE should be inactive: {texts:?}"
    );
}
