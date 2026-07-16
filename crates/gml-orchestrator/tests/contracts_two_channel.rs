//! Contract tests for the two-channel tool-result split on the NON-worldstate
//! tools, ported from `gm-lab/test_contracts.py`:
//!   roll_dice, get_npc_profile, advance_time, update_player_character,
//!   get_world_fact, ask_npc.
//!
//! The contract (PORT_PLAN §5.2): `.full` is the machine payload (JSON or the
//! roll `detail` string) WITHOUT a `<system-reminder>`; `.model` is compact
//! structured text WITH the exact trailing `<system-reminder>` string. The plain
//! model text (reminder stripped) must be non-empty structured text, never raw
//! JSON. The reminder strings come from `helpers::tool_reminder` and must match
//! the Python `_TOOL_REMINDERS` verbatim.
//!
//! Driven via `run_tool_collect` — the Rust analogue of `_drive(_run_tool(...))`.

use std::sync::Arc;

use serde_json::{json, Value};

use gml_llm::Backend;
use gml_mock::MockClient;
use gml_orchestrator::worldstate::{get_memory, note_memory, npc_memory_recall};
use gml_orchestrator::{run_tool_collect, Session};

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

fn tokio_block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(fut)
}

const REMINDER_OPEN: &str = "<system-reminder>";

/// `_tool_model_plain(result)` — strip the trailing system reminder from the
/// model channel (Python: split on "\n\n<system-reminder>").
fn model_plain(model: &str) -> String {
    model
        .split("\n\n<system-reminder>")
        .next()
        .unwrap_or("")
        .to_string()
}

/// `_assert_text_is_structured_tool_result(text)` — non-empty, not raw JSON.
fn assert_structured_text(text: &str) {
    let t = text.trim();
    assert!(!t.is_empty(), "model tool result must not be empty");
    assert!(
        !t.starts_with('{') && !t.starts_with('['),
        "must not start like JSON: {t}"
    );
    // Must not parse as a JSON document (it is structured plain text).
    if let Ok(v) = serde_json::from_str::<Value>(t) {
        // Scalars like a bare number can "parse"; only object/array is forbidden.
        assert!(
            !v.is_object() && !v.is_array(),
            "model tool result unexpectedly raw machine payload: {t}"
        );
    }
}

#[test]
fn legacy_world_state_tools_are_not_raw_dispatchable() {
    let mut s = session();
    let before_memory = s.world.world_canon.memory.units.len();
    let before_events = s.world.world_canon.event_log.events.len();

    for tool_name in ["query_world_state", "update_world_state"] {
        let (events, result) = tokio_block_on(run_tool_collect(
            &mut s,
            tool_name,
            &json!({
                "query": "anything",
                "items": [{
                    "type": "fact",
                    "scope": "public",
                    "text": "LEGACY_RAW_DISPATCH_SENTINEL"
                }]
            }),
        ));
        assert!(
            result.full.contains("tool error"),
            "full error should be a tool-error string: {}",
            result.full
        );
        assert!(
            result.model.contains("unknown tool"),
            "model error should mention unknown tool: {}",
            result.model
        );
        assert!(
            result.model.contains("unknown_tool"),
            "model error should carry unknown_tool code: {}",
            result.model
        );
        assert!(
            events
                .iter()
                .all(|event| event.kind != "world_state_update" && event.kind != "world_query"),
            "legacy raw tool must not emit legacy state/query events: {events:?}"
        );
    }

    assert_eq!(s.world.world_canon.memory.units.len(), before_memory);
    assert_eq!(s.world.world_canon.event_log.events.len(), before_events);
}

#[test]
fn scoped_memory_tools_two_channel_do_not_leak_foreign_memory() {
    let mut s = session();
    let (_events, borin_note) = tokio_block_on(run_tool_collect(
        &mut s,
        "note_memory",
        &json!({
            "summary": "BORIN_TOOL_SENTINEL знает тайный знак.",
            "details": "Тайный знак нарисован на внутренней стороне щита.",
            "owner_scope": "actor:borin",
            "topic_tags": ["tool_sentinel"],
        }),
    ));
    assert_structured_text(&model_plain(&borin_note.model));
    let borin_full: Value = serde_json::from_str(&borin_note.full).expect("full is JSON");
    assert_eq!(borin_full["ok"], json!(true));

    let (_events, lysa_note) = tokio_block_on(run_tool_collect(
        &mut s,
        "note_memory",
        &json!({
            "summary": "LYSA_TOOL_SENTINEL знает другой знак.",
            "owner_scope": "actor:lysa",
            "topic_tags": ["tool_sentinel"],
        }),
    ));
    assert_structured_text(&model_plain(&lysa_note.model));
    let lysa_full: Value = serde_json::from_str(&lysa_note.full).expect("full is JSON");
    assert_eq!(lysa_full["ok"], json!(true));

    let (_events, recall) = tokio_block_on(run_tool_collect(
        &mut s,
        "get_memory",
        &json!({"scope": "actor", "npc_id": "borin", "query": "tool_sentinel", "max_results": 10}),
    ));
    assert_structured_text(&model_plain(&recall.model));
    let full: Value = serde_json::from_str(&recall.full).expect("full is JSON");
    let full_text = serde_json::to_string(&full).unwrap();
    assert!(full_text.contains("BORIN_TOOL_SENTINEL"), "{full_text}");
    assert!(!full_text.contains("LYSA_TOOL_SENTINEL"), "{full_text}");
    assert!(!model_plain(&recall.model).contains("внутренней стороне щита"));

    let (_events, detailed) = tokio_block_on(run_tool_collect(
        &mut s,
        "get_memory",
        &json!({
            "scope": "actor",
            "npc_id": "borin",
            "query": "тайный знак",
            "include_details": true
        }),
    ));
    let detailed_full: Value = serde_json::from_str(&detailed.full).expect("full is JSON");
    assert!(serde_json::to_string(&detailed_full)
        .unwrap()
        .contains("внутренней стороне щита"));
}

#[test]
fn memory_consolidation_tool_two_channel_is_append_only() {
    let mut s = session();
    let (_events, a) = tokio_block_on(run_tool_collect(
        &mut s,
        "note_memory",
        &json!({
            "summary": "RAW_TOOL_A караван оставил следы у дороги.",
            "owner_scope": "actor:borin",
            "topic_tags": ["tool_caravan"],
        }),
    ));
    let a_full: Value = serde_json::from_str(&a.full).unwrap();
    let (_events, b) = tokio_block_on(run_tool_collect(
        &mut s,
        "note_memory",
        &json!({
            "summary": "RAW_TOOL_B Борин слышал о нападении на караван.",
            "owner_scope": "actor:borin",
            "topic_tags": ["tool_caravan"],
        }),
    ));
    let b_full: Value = serde_json::from_str(&b.full).unwrap();
    let source_ids = vec![
        a_full["memory_id"].as_str().unwrap().to_string(),
        b_full["memory_id"].as_str().unwrap().to_string(),
    ];

    let (_events, crystal) = tokio_block_on(run_tool_collect(
        &mut s,
        "consolidate_memory",
        &json!({
            "source_memory_ids": source_ids,
            "summary": "CRYSTAL_TOOL караванные следы связаны с нападением.",
            "owner_scope": "actor:borin",
            "topic_tags": ["tool_caravan"],
        }),
    ));
    assert_structured_text(&model_plain(&crystal.model));
    let crystal_full: Value = serde_json::from_str(&crystal.full).expect("full is JSON");
    assert_eq!(crystal_full["not_deleted"], json!(true));
    assert_eq!(
        crystal_full["consumed_source_ids"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let (_events, default_recall) = tokio_block_on(run_tool_collect(
        &mut s,
        "get_memory",
        &json!({"scope": "actor", "npc_id": "borin", "query": "tool_caravan", "max_results": 10}),
    ));
    let default_text = default_recall.full.clone();
    assert!(default_text.contains("CRYSTAL_TOOL"), "{default_text}");
    assert!(!default_text.contains("RAW_TOOL_A"), "{default_text}");
    assert!(!default_text.contains("RAW_TOOL_B"), "{default_text}");

    let (_events, audit_recall) = tokio_block_on(run_tool_collect(
        &mut s,
        "get_memory",
        &json!({
            "scope": "actor",
            "npc_id": "borin",
            "query": "tool_caravan",
            "max_results": 10,
            "include_cold": true
        }),
    ));
    let audit_text = audit_recall.full;
    assert!(audit_text.contains("CRYSTAL_TOOL"), "{audit_text}");
    assert!(audit_text.contains("RAW_TOOL_A"), "{audit_text}");
    assert!(audit_text.contains("RAW_TOOL_B"), "{audit_text}");
}

// =========================================================================
// roll_dice (Python ≈ 1759-1795)
// =========================================================================

#[test]
fn roll_dice_two_channel() {
    let mut s = session();
    s.world.forced_die_next = Some(17);
    let (events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "roll_dice",
        &json!({
            "roll_kind": "check",
            "notation": "1d20+3",
            "target_number": 20,
            "target_kind": "DC",
            "check_name": "Wisdom (Perception)",
            "reason": "Scan room.",
        }),
    ));
    assert_structured_text(&model_plain(&result.model));

    // .full (= the roll detail string) carries the graded outcome, no [forced].
    assert!(
        result.full.contains("grade=success"),
        "full: {}",
        result.full
    );
    assert!(result.full.contains("margin=+0"), "full: {}", result.full);
    assert!(!result.full.contains("[forced]"));
    assert!(
        !result.full.contains(REMINDER_OPEN),
        ".full must NOT carry a reminder"
    );

    // .model carries the exact roll reminder verbatim.
    assert!(
        result.model.contains(REMINDER_OPEN),
        ".model must carry a reminder"
    );
    assert!(result
        .model
        .contains("Use the returned total, grade, and margin as fixed"));
    assert!(result.model.contains("If a damage roll was made"));
    assert!(result.model.contains("failed detonation"));
    assert!(result
        .model
        .contains("critical success means the best plausible version"));
    assert!(result.model.contains("concrete benefit from the success"));

    // The plain model line is the compact RESULT, with no machine details.
    let plain = model_plain(&result.model);
    assert_eq!(plain, "RESULT: total 20, success, margin +0, natural 17.");
    assert!(!plain.contains("1d20+3"));
    assert!(!plain.contains("Wisdom"));
    assert!(!plain.contains("DC"));
    assert!(!plain.contains("detail"));
    assert!(!plain.contains("rolls"));
    assert!(!result.model.contains("forced"));

    assert!(events.iter().any(|e| e.kind == "dice"));
}

// =========================================================================
// get_npc_profile (Python ≈ 1797-1823)
// =========================================================================

#[test]
fn get_npc_profile_two_channel() {
    let mut s = session();
    let secret = s.world.npc("borin").expect("borin").secret.clone();
    let (_events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "get_npc_profile",
        &json!({
            "npc_id": "borin",
            "preset": "mechanics",
            "fields": ["passive_perception", "abilities"],
        }),
    ));
    assert_structured_text(&model_plain(&result.model));

    // .full JSON has the mechanics, no secrets.
    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(full["npc_id"], "borin");
    assert_eq!(full["profile"]["passive_perception"], 13);
    assert_eq!(full["profile"]["abilities"]["WIS"], 13);
    let full_text = result.full.clone();
    if !secret.is_empty() {
        assert!(!full_text.contains(&secret));
    }
    assert!(!result.full.contains(REMINDER_OPEN));

    // .model plain has the compact field, no secret; .model has the reminder.
    let plain = model_plain(&result.model);
    assert!(plain.contains("passive_perception: 13"));
    if !secret.is_empty() {
        assert!(!plain.contains(&secret));
    }
    assert!(result.model.contains(REMINDER_OPEN));
    assert!(result.model.contains("player sees only observable fiction"));
    assert!(result.model.contains("do not reveal raw NPC stats"));
}

// =========================================================================
// advance_time (Python ≈ 1825-1841)
// =========================================================================

#[test]
fn advance_time_two_channel() {
    let mut s = session();
    let before = s.world.time_export()["absolute_minutes"].as_i64().unwrap();
    let (_events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "advance_time",
        &json!({"minutes": 7, "reason": "допрос у стойки"}),
    ));
    assert_structured_text(&model_plain(&result.model));

    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(full["elapsed_minutes"], 7);
    assert_eq!(full["current"]["absolute_minutes"], before + 7);

    let plain = model_plain(&result.model);
    assert!(plain.contains("elapsed: 7 min"), "plain: {plain}");
    // The reason is internal — it must NOT surface in the model text.
    assert!(!plain.contains("допрос у стойки"));

    assert_eq!(s.world.time_export()["absolute_minutes"], before + 7);
    assert_eq!(s.world.time_export()["last_advance_minutes"], 7);
    assert_eq!(
        s.world.time_export()["last_advance_reason"],
        "допрос у стойки"
    );
}

// =========================================================================
// update_player_character (Python ≈ 1857-1873)
// =========================================================================

#[test]
fn update_player_character_two_channel() {
    let mut s = session();
    let (events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "update_player_character",
        &json!({
            "fields": {"condition": "ранен", "hp": {"current": 5, "max": 9}},
            "reason": "получил ранение",
        }),
    ));
    assert_structured_text(&model_plain(&result.model));

    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(full["updated"], json!(["condition", "hp"]));
    // The model channel never echoes the whole "player_character" card.
    assert!(!model_plain(&result.model).contains("player_character"));
    assert_eq!(s.world.player_character.condition, "ранен");
    assert_eq!(s.world.player_character.hp["current"], json!(5));
    assert!(events.iter().any(|e| e.kind == "player_character_update"));
}

#[test]
fn update_player_character_inventory_add_only_emits_event_and_bumps_revision() {
    // K2.2: the delta-op path (only inventory_add, no full-array field) must fire
    // the PLAYER_CHARACTER_UPDATE event and bump card_revision exactly like a
    // full rewrite.
    let mut s = session();
    let before_rev = s.world.player_character.card_revision;
    let before_len = s.world.player_character.inventory.len();

    let (events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "update_player_character",
        &json!({
            "fields": {"inventory_add": ["найденный ключ"]},
            "reason": "подобрал ключ",
        }),
    ));
    assert_structured_text(&model_plain(&result.model));

    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(full["updated"], json!(["inventory"]));
    assert_eq!(
        s.world.player_character.inventory.len(),
        before_len + 1,
        "delta add must append exactly one entry"
    );
    assert!(s
        .world
        .player_character
        .inventory
        .iter()
        .any(|item| item == "найденный ключ"));
    assert_eq!(s.world.player_character.card_revision, before_rev + 1);
    assert!(events.iter().any(|e| e.kind == "player_character_update"));
}

// =========================================================================
// get_world_fact: known -> already_delivered de-dup (Python ≈ 1716-1757)
// =========================================================================

#[test]
fn get_world_fact_unknown_and_dedup() {
    // Pin the deterministic keyword path (RAG disabled), so the result does not
    // depend on a live embeddings server. The orchestrator-layer contract under
    // test is the two-channel split + the `already_delivered` de-dup, not the
    // RAG ranking (that lives in gml-rag). Python's get_world_fact "sources"
    // come from the RAG layer; here we assert the de-dup contract that the
    // orchestrator owns and that holds on the keyword path too.
    let mut s = session();

    // Unknown lookup -> status unknown, compact model keys only, reminder present.
    let (_e, unknown) = tokio_block_on(run_tool_collect(
        &mut s,
        "get_world_fact",
        &json!({"query": "unknown thing"}),
    ));
    assert_structured_text(&model_plain(&unknown.model));
    let unknown_full: Value = serde_json::from_str(&unknown.full).unwrap();
    assert_eq!(unknown_full["status"], "unknown");
    assert_eq!(unknown_full["retrieval"]["enabled"], json!(false));
    assert_eq!(unknown_full["retrieval"]["backend"], json!("lexical"));
    assert_eq!(unknown_full["retrieval"]["reason"], json!("disabled"));
    assert!(
        !unknown.full.contains(REMINDER_OPEN),
        ".full must NOT carry a reminder"
    );
    assert!(
        unknown.model.contains(REMINDER_OPEN),
        ".model must carry a reminder"
    );
    assert!(unknown
        .model
        .contains("only lore the player can know right now"));
    assert!(unknown.model.contains("do not reveal hidden sources"));

    // Known lookup (keyword path) -> status known, model text WORLD FACT, no score.
    let (_e, known) = tokio_block_on(run_tool_collect(
        &mut s,
        "get_world_fact",
        &json!({"query": "Где искать Капитана Марет?"}),
    ));
    let known_full: Value = serde_json::from_str(&known.full).unwrap();
    assert_eq!(known_full["status"], "known");
    let known_plain = model_plain(&known.model);
    assert!(known_plain.contains("WORLD FACT"));
    assert!(!known_plain.contains("score"));
    assert!(!known_plain.contains("retrieval"));
    assert!(known.model.contains(REMINDER_OPEN));
    assert!(!known.full.contains(REMINDER_OPEN));

    // Repeat the SAME known lookup -> already_delivered de-dup, no sources.
    let (_e, repeat) = tokio_block_on(run_tool_collect(
        &mut s,
        "get_world_fact",
        &json!({"query": "Где искать Капитана Марет?"}),
    ));
    let repeat_full: Value = serde_json::from_str(&repeat.full).unwrap();
    assert_eq!(repeat_full["status"], "already_delivered");
    assert!(repeat_full["already_delivered"].as_i64().unwrap() >= 1);
    assert!(repeat_full
        .get("sources")
        .map(|v| v.as_array().map(|a| a.is_empty()).unwrap_or(true))
        .unwrap_or(true));
    assert!(repeat_full["text"]
        .as_str()
        .unwrap()
        .contains("already delivered"));
}

// =========================================================================
// tool_search / load_tool_schema: discovery is separate from cache-stable invocation
// =========================================================================

#[test]
fn tool_search_returns_metadata_without_loading_tools() {
    let mut s = session();
    assert!(!s.loaded_gm_tools.contains("move_npc"));
    assert!(!s.loaded_gm_tools.contains("set_scene"));

    let (events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "tool_search",
        &json!({"query": "select:move_npc,set_scene"}),
    ));
    assert_structured_text(&model_plain(&result.model));

    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert!(
        full.get("loaded_tools").is_none(),
        "tool_search must not load schemas"
    );
    let matches = full["matches"].as_array().expect("matches array");
    let names: Vec<&str> = matches
        .iter()
        .map(|row| row["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"move_npc"));
    assert!(names.contains(&"set_scene"));
    for row in matches {
        assert!(row.get("title").and_then(Value::as_str).is_some());
        assert!(row.get("description").and_then(Value::as_str).is_some());
        assert!(row.get("keywords").and_then(Value::as_array).is_some());
        assert!(row.get("aliases").and_then(Value::as_array).is_some());
        assert!(row.get("capabilities").and_then(Value::as_array).is_some());
        assert_eq!(row["load_schema"]["tool"], "load_tool_schema");
        assert!(row.get("schema").is_none());
        assert!(row.get("function").is_none());
        assert!(row.get("parameters").is_none());
    }
    assert!(!s.loaded_gm_tools.contains("move_npc"));
    assert!(!s.loaded_gm_tools.contains("set_scene"));
    assert!(result.model.contains("TOOL SEARCH"));
    assert!(result.model.contains("matches"));
    assert!(result.model.contains("load_tool_schema"));
    assert!(result.model.contains("invoke_loaded_tool"));
    assert!(events.iter().any(|e| e.kind == "tool_search"));
}

#[test]
fn load_tool_schema_returns_schema_and_tracks_loaded_tool() {
    let mut s = session();

    let (_events, loaded) = tokio_block_on(run_tool_collect(
        &mut s,
        "load_tool_schema",
        &json!({"name": "move_npc"}),
    ));
    assert_structured_text(&model_plain(&loaded.model));
    let loaded_full: Value = serde_json::from_str(&loaded.full).expect("full is JSON");
    assert_eq!(loaded_full["status"], "loaded_schema");
    assert_eq!(loaded_full["loaded_schema"], "move_npc");
    assert_eq!(loaded_full["invoke_tool"], "invoke_loaded_tool");
    assert!(loaded_full.get("loaded_tools").is_none());
    assert_eq!(loaded_full["schema"]["function"]["name"], "move_npc");
    assert!(s.loaded_gm_tools.contains("move_npc"));
    assert!(loaded.model.contains("LOAD TOOL SCHEMA"));
    assert!(loaded.model.contains("schema: {\"type\":\"function\""));
    assert!(loaded.model.contains("invoke_loaded_tool"));

    let (_events, repeat) = tokio_block_on(run_tool_collect(
        &mut s,
        "load_tool_schema",
        &json!({"name": "move_npc"}),
    ));
    let repeat_full: Value = serde_json::from_str(&repeat.full).expect("full is JSON");
    assert_eq!(repeat_full["status"], "loaded_schema");
    assert_eq!(repeat_full["loaded_schema"], "move_npc");
    assert!(repeat_full.get("loaded_tools").is_none());
    assert_eq!(repeat_full["already_loaded"], json!([]));
    assert_eq!(repeat_full["schema"]["function"]["name"], "move_npc");
    assert!(s.loaded_gm_tools.contains("move_npc"));

    let (_events, missing) = tokio_block_on(run_tool_collect(
        &mut s,
        "load_tool_schema",
        &json!({"name": "does_not_exist"}),
    ));
    let missing_full: Value = serde_json::from_str(&missing.full).expect("full is JSON");
    assert_eq!(missing_full["status"], "missing");
    assert_eq!(missing_full["missing"], json!(["does_not_exist"]));
    assert!(missing_full["schema"].is_null());
}

#[test]
fn invoke_loaded_tool_dispatches_and_keeps_loaded_tool_tracked() {
    let mut s = session();

    let (_events, loaded) = tokio_block_on(run_tool_collect(
        &mut s,
        "load_tool_schema",
        &json!({"name": "get_npc_profile"}),
    ));
    let loaded_full: Value = serde_json::from_str(&loaded.full).expect("full is JSON");
    assert_eq!(loaded_full["status"], "loaded_schema");
    assert_eq!(loaded_full["loaded_schema"], "get_npc_profile");
    assert!(s.loaded_gm_tools.contains("get_npc_profile"));

    let (_events, invoked) = tokio_block_on(run_tool_collect(
        &mut s,
        "invoke_loaded_tool",
        &json!({
            "name": "get_npc_profile",
            "arguments": {
                "npc_id": "borin",
                "preset": "mechanics",
                "fields": ["passive_perception"]
            }
        }),
    ));
    assert_structured_text(&model_plain(&invoked.model));
    let invoked_full: Value = serde_json::from_str(&invoked.full).expect("full is JSON");
    assert_eq!(invoked_full["npc_id"], "borin");
    assert!(invoked.model.contains("NPC PROFILE"));
    assert!(s.loaded_gm_tools.contains("get_npc_profile"));
}

// =========================================================================
// ask_npc success: two-channel + npc label + emitted speech (Python ≈ 1914-1961)
// =========================================================================

#[test]
fn ask_npc_success_two_channel_and_label() {
    let mut s = session();
    let (events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_npc",
        &json!({
            "npc_id": "borin",
            "situation": "Игрок тихо спрашивает Борина, что он знает об Алдрике.",
        }),
    ));
    assert_structured_text(&model_plain(&result.model));

    // .full JSON has the GM instruction; the model plain does NOT.
    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert!(full.get("gm_instruction").is_some());
    let plain = model_plain(&result.model);
    assert!(!plain.contains("gm_instruction"));

    // .model carries the ask_npc reminder verbatim; .full does not.
    assert!(result.model.contains(REMINDER_OPEN));
    assert!(result.model.contains("call note_memory"));
    assert!(result
        .model
        .contains("Store relationship and goal changes as scoped memory cards"));
    assert!(result.model.contains("call advance_time"));
    assert!(result
        .model
        .contains("durable testimony, rumor, npc_memory"));
    assert!(result
        .model
        .contains("Private leads from an NPC to the player"));
    assert!(result.model.contains("nothing durable changed"));
    assert!(result.model.contains("update_player_character"));
    assert!(!result.full.contains(REMINDER_OPEN));

    // Model plain uses the player-facing "Борин (borin)" line, never npc_name.
    assert!(plain.contains("npc: Борин (borin)"), "plain: {plain}");
    assert!(!plain.contains("npc_name"));
    assert!(plain.contains("already_emitted: yes"));
    assert!(plain.contains("final_narration:"));
    assert!(plain.contains("ask_npc"));

    assert!(events.iter().any(|e| e.kind == "npc_speech"));
}

#[test]
fn ask_npc_runs_remember_as_an_npc_tool_not_a_gm_tool() {
    let mut s = session();
    let (_events, stored) = tokio_block_on(run_tool_collect(
        &mut s,
        "note_memory",
        &json!({
            "summary": "BORIN_REMEMBER_TOOL_SENTINEL хранится только в памяти Борина.",
            "owner_scope": "actor:borin",
            "topic_tags": ["REMEMBER_TOOL_SENTINEL"],
        }),
    ));
    let stored_full: Value = serde_json::from_str(&stored.full).expect("stored JSON");
    assert_eq!(stored_full["ok"], json!(true));

    let (events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_npc",
        &json!({
            "npc_id": "borin",
            "situation": "Игрок просит Борина вспомнить REMEMBER_TOOL_SENTINEL.",
        }),
    ));
    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert!(
        full["speech_ru"]
            .as_str()
            .unwrap_or("")
            .contains("BORIN_REMEMBER_TOOL_SENTINEL"),
        "{full}"
    );
    assert!(events.iter().any(|e| {
        e.kind == "npc_tool_call"
            && e.agent.as_deref() == Some("Борин")
            && e.data.get("name").and_then(Value::as_str) == Some("remember")
    }));
    assert!(events.iter().any(|e| {
        e.kind == "npc_tool_result"
            && e.agent.as_deref() == Some("Борин")
            && e.data
                .as_str()
                .unwrap_or("")
                .contains("BORIN_REMEMBER_TOOL_SENTINEL")
    }));
    assert!(!events.iter().any(|e| {
        e.kind == "gm_tool_call"
            && e.data.get("name").and_then(Value::as_str) == Some("npc_remember")
    }));
}

#[test]
fn ask_npc_runs_npc_note_memory_as_actor_private_tool() {
    let mut s = session();
    let (events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_npc",
        &json!({
            "npc_id": "borin",
            "situation": "Игрок угрожает Борину: NPC_NOTE_MEMORY_TOOL_SENTINEL.",
        }),
    ));
    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert!(
        full["response_ru"]
            .as_str()
            .unwrap_or("")
            .contains("запоминая"),
        "{full}"
    );
    assert!(events.iter().any(|e| {
        e.kind == "npc_tool_call"
            && e.agent.as_deref() == Some("Борин")
            && e.data.get("name").and_then(Value::as_str) == Some("npc_note_memory")
    }));
    let note_result = events
        .iter()
        .find(|e| {
            e.kind == "npc_tool_result"
                && e.agent.as_deref() == Some("Борин")
                && e.data
                    .as_str()
                    .map(|text| text.contains("\"status\":\"stored\""))
                    .unwrap_or(false)
        })
        .and_then(|e| e.data.as_str())
        .expect("npc_note_memory result event");
    assert!(
        !note_result.contains("NPC_NOTE_MEMORY_TOOL_SENTINEL"),
        "private note text must not be echoed through the event result: {note_result}"
    );

    let borin_recall = npc_memory_recall(
        &mut s,
        &json!({
            "npc_id": "borin",
            "query": "NPC_NOTE_MEMORY_TOOL_SENTINEL",
            "max_results": 10,
            "include_cold": true
        }),
    );
    assert!(
        serde_json::to_string(&borin_recall)
            .unwrap()
            .contains("NPC_NOTE_MEMORY_TOOL_SENTINEL"),
        "{borin_recall}"
    );

    let player_recall = get_memory(
        &mut s,
        &json!({
            "scope": "player",
            "query": "NPC_NOTE_MEMORY_TOOL_SENTINEL",
            "max_results": 10,
            "include_cold": true
        }),
    );
    assert_eq!(player_recall["status"], json!("unknown"), "{player_recall}");
    assert!(
        player_recall["results"]
            .as_array()
            .map(|rows| rows.is_empty())
            .unwrap_or(true),
        "NPC private note must not become player memory: {player_recall}"
    );
}

#[test]
fn ask_npc_runs_relationship_recall_through_actor_memory() {
    let mut s = session();
    let stored = note_memory(
        &mut s,
        &json!({
            "summary": "NPC_RELATIONSHIP_MEMORY_SENTINEL Борин помнит, что игрок однажды прикрыл его перед стражей.",
            "owner_scope": "actor:borin",
            "topic_tags": ["relationship", "player"],
        }),
    );
    assert_eq!(stored["ok"], json!(true), "{stored}");

    let (events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_npc",
        &json!({
            "npc_id": "borin",
            "situation": "Игрок просит Борина вспомнить их прошлые дела: NPC_RELATIONSHIP_TOOL_SENTINEL.",
        }),
    ));
    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert!(
        full["response_ru"].as_str().unwrap_or("").contains("мягче"),
        "{full}"
    );
    assert!(events.iter().any(|e| {
        e.kind == "npc_tool_call"
            && e.agent.as_deref() == Some("Борин")
            && e.data.get("name").and_then(Value::as_str) == Some("npc_recall_relationship")
    }));
    assert!(events.iter().any(|e| {
        e.kind == "npc_tool_result"
            && e.agent.as_deref() == Some("Борин")
            && e.data
                .as_str()
                .map(|text| text.contains("NPC_RELATIONSHIP_MEMORY_SENTINEL"))
                .unwrap_or(false)
    }));
}

#[test]
fn ask_npc_player_label_for_unnamed_npc() {
    let mut s = session();
    let (_events, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_npc",
        &json!({
            "npc_id": "lysa",
            "situation": "Игрок тихо спрашивает служанку, как её зовут.",
        }),
    ));
    let full: Value = serde_json::from_str(&result.full).expect("full is JSON");
    assert_eq!(full["npc_name"], "Лиза");
    let plain = model_plain(&result.model);
    // The model channel uses the player label (служанка), not the known name.
    assert!(plain.contains("npc: служанка (lysa)"), "plain: {plain}");
    assert!(!plain.contains("npc_name"));
}

// =========================================================================
// tool errors: structured model text, error code in model plain (Python ≈ 955-966)
// =========================================================================

#[test]
fn tool_errors_are_structured_with_codes() {
    let mut s = session();

    // ask_npc unknown NPC -> "no such NPC" in full, code in model plain.
    let (_e, missing) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_npc",
        &json!({"npc_id": "no_such_npc", "situation": "x"}),
    ));
    assert_structured_text(&model_plain(&missing.model));
    assert!(missing.full.contains("no such NPC"));
    assert!(model_plain(&missing.model).contains("code: unknown_npc"));

    // move_npc unknown NPC -> "tool error" in full, "tool: move_npc" in model.
    let (_e, bad_move) = tokio_block_on(run_tool_collect(
        &mut s,
        "move_npc",
        &json!({"npc_id": "ghost", "present": true, "reason": "тест"}),
    ));
    assert_structured_text(&model_plain(&bad_move.model));
    assert!(bad_move.full.contains("tool error"));
    assert!(model_plain(&bad_move.model).contains("tool: move_npc"));

    // ask_npc missing situation -> missing_situation code + error event.
    let (events, missing_sit) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_npc",
        &json!({"npc_id": "borin"}),
    ));
    assert_structured_text(&model_plain(&missing_sit.model));
    assert!(missing_sit.full.contains("tool error"));
    assert!(model_plain(&missing_sit.model).contains("code: missing_situation"));
    assert!(events.iter().any(|e| e.kind == "error"
        && e.data
            .as_str()
            .map(|s| s.contains("situation"))
            .unwrap_or(false)));
}

// =========================================================================
// ask_npc not-present: structured error, no pending draft (Python ≈ 1875-1912)
// =========================================================================

#[test]
fn ask_npc_not_present_no_pending() {
    let mut s = session();
    // Move Марет offscreen with known whereabouts.
    let _ = tokio_block_on(run_tool_collect(
        &mut s,
        "set_npc_whereabouts",
        &json!({
            "npc_id": "mareth",
            "location_id": "turnvale_guardhouse",
            "location_name": "караульная Тёрнвейла",
            "status": "known",
            "details": "её там ждут по делу Алдрика",
            "source": "стражник сказал игроку",
        }),
    ));
    let (_e, result) = tokio_block_on(run_tool_collect(
        &mut s,
        "ask_npc",
        &json!({
            "npc_id": "mareth",
            "situation": "The player tries to question Марет in the tavern.",
        }),
    ));
    assert_structured_text(&model_plain(&result.model));
    assert!(result.full.contains("not present"));
    assert!(result.full.contains("Known whereabouts"));
    assert!(model_plain(&result.model).contains("code: npc_not_present"));
    assert!(s.pending.is_empty());
}
