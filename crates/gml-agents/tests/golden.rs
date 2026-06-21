//! Byte-for-byte golden tests against the captured Python reference fixtures in
//! `tests/reference/agents/`. The world is the default story `turnvale-murder`
//! seed built via `gml_stories::story_seed` -> `gml_world::World::from_seed`,
//! with the exact `capture_fixtures.py::capture_agents` inputs.

use std::collections::BTreeSet;

use serde_json::Value;

use gml_agents as agents;
use gml_world::World;

const PLAYER_TEXT: &str = "Я осматриваю площадь и подхожу к воротам.";
const DICE_SEED: u128 = 424242;

fn ref_path(rel: &str) -> std::path::PathBuf {
    // crate dir is .../crates/gml-agents
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/reference/agents")
        .join(rel)
}

fn read_fixture_bytes(rel: &str) -> Vec<u8> {
    std::fs::read(ref_path(rel)).unwrap_or_else(|e| panic!("read fixture {rel}: {e}"))
}

fn read_fixture_str(rel: &str) -> String {
    String::from_utf8(read_fixture_bytes(rel)).expect("fixture utf-8")
}

/// Serialize a JSON value the way Python `json.dumps(..., ensure_ascii=False,
/// indent=2)` does — 2-space indent, `": "` key separator, no trailing newline.
/// serde_json's pretty printer matches this exactly for the shapes we produce.
fn dumps_indent2(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("pretty json")
}

/// Compact serialization == Python `json.dumps(..., ensure_ascii=False,
/// separators=(",",":"))`. serde_json default compact + preserve_order matches.
fn dumps_compact(value: &Value) -> String {
    serde_json::to_string(value).expect("compact json")
}

fn build_world() -> World {
    let seed = gml_stories::story_seed(gml_stories::DEFAULT_STORY_ID).expect("default seed");
    World::from_seed_with_dice_seed(&seed, DICE_SEED)
}

fn assert_text_eq(got: &str, fixture: &str) {
    let expected = read_fixture_str(fixture);
    assert_eq!(got, expected, "text mismatch vs {fixture}");
}

fn assert_json_indent2(got: &Value, fixture: &str) {
    let expected = read_fixture_str(fixture);
    assert_eq!(dumps_indent2(got), expected, "indent2 json mismatch vs {fixture}");
}

// --- GM assembly -----------------------------------------------------------

#[test]
fn gm_world_setup_byte_identical() {
    let w = build_world();
    assert_text_eq(&agents::gm_world_setup(&w), "gm_world_setup.txt");
}

#[test]
fn gm_turn_context_noopts_byte_identical() {
    let mut w = build_world();
    let got = agents::gm_turn_context(&mut w, PLAYER_TEXT, false);
    assert_text_eq(&got, "gm_turn_context_noopts.txt");
}

#[test]
fn gm_turn_context_opts_byte_identical() {
    let mut w = build_world();
    let got = agents::gm_turn_context(&mut w, PLAYER_TEXT, true);
    assert_text_eq(&got, "gm_turn_context_opts.txt");
}

#[test]
fn gm_world_setup_excludes_roster_and_public_facts() {
    let w = build_world();
    let setup = agents::gm_world_setup(&w);
    // The roster and public facts belong to the LATE turn context, never the
    // cacheable early setup (prompt_cache_architecture.md P3).
    assert!(!setup.contains("INTERNAL NPC ROSTER"));
    assert!(!setup.contains("CURRENT PUBLIC FACTS"));
    assert!(!setup.contains("id=borin"));
    assert!(setup.contains("PUBLIC INTRO:"));
}

#[test]
fn gm_turn_context_contains_roster_and_facts_and_ordering() {
    let mut w = build_world();
    let ctx = agents::gm_turn_context(&mut w, PLAYER_TEXT, false);
    assert!(ctx.contains("INTERNAL NPC ROSTER"));
    assert!(ctx.contains("CURRENT PUBLIC FACTS"));
    // TURN RESOLUTION CHECK / <system-reminder> precede PLAYER ACTION.
    let reminder = ctx.find("<system-reminder>").expect("reminder present");
    let action = ctx.find("PLAYER ACTION").expect("player action present");
    assert!(reminder < action, "reminder must precede PLAYER ACTION");
}

#[test]
fn gm_request_messages_empty_byte_identical() {
    let mut w = build_world();
    let gum = agents::gm_user_message(&mut w, PLAYER_TEXT, false);
    let req = agents::gm_request_messages(&w, &[gum], "");
    assert_json_indent2(&Value::Array(req), "gm_request_messages_empty.json");
}

#[test]
fn gm_request_messages_summary_byte_identical() {
    let mut w = build_world();
    let gum = agents::gm_user_message(&mut w, PLAYER_TEXT, false);
    let req = agents::gm_request_messages(&w, &[gum], "Краткое содержание прошлых сцен.");
    assert_json_indent2(&Value::Array(req), "gm_request_messages_summary.json");
}

#[test]
fn gm_system_byte_stable_across_summary() {
    // GM_SYSTEM is byte-identical regardless of summary (cache prefix stability).
    assert_eq!(agents::gm_system(), gml_prompts::GM_SYSTEM);
}

// --- Tool catalog ----------------------------------------------------------

#[test]
fn gm_tools_byte_identical_indent2() {
    let tools = Value::Array(agents::build_gm_tools());
    assert_json_indent2(&tools, "gm_tools.json");
}

#[test]
fn gm_tools_byte_identical_compact() {
    let tools = Value::Array(agents::build_gm_tools());
    let expected = read_fixture_str("gm_tools.compact.json");
    assert_eq!(dumps_compact(&tools), expected, "compact gm_tools mismatch");
}

#[test]
fn gm_tools_have_no_dynamic_enums_or_roster() {
    let tools = agents::build_gm_tools();
    let json = serde_json::to_string(&Value::Array(tools.clone())).unwrap();
    // No human-readable roster injected into descriptions.
    assert!(!json.contains("Available NPCs"), "tools leak Available NPCs prose");
    // The only enums present are closed engine types — none may contain a live
    // npc id / location id / item id.
    let live_ids = ["borin", "lysa", "mareth", "grey_griffon"];
    let mut enums: Vec<Vec<String>> = Vec::new();
    fn collect_enums(v: &Value, out: &mut Vec<Vec<String>>) {
        match v {
            Value::Object(m) => {
                if let Some(Value::Array(e)) = m.get("enum") {
                    out.push(
                        e.iter()
                            .map(|x| x.as_str().unwrap_or("").to_string())
                            .collect(),
                    );
                }
                for (_, vv) in m {
                    collect_enums(vv, out);
                }
            }
            Value::Array(a) => {
                for vv in a {
                    collect_enums(vv, out);
                }
            }
            _ => {}
        }
    }
    collect_enums(&Value::Array(tools), &mut enums);
    assert!(!enums.is_empty(), "expected static engine enums to exist");
    for e in &enums {
        for id in &live_ids {
            assert!(
                !e.iter().any(|x| x == id),
                "tool enum must not contain live id {id}: {e:?}"
            );
        }
    }
}

#[test]
fn initial_gm_tool_names_byte_identical() {
    let names: Vec<String> = agents::initial_gm_tool_names(false).into_iter().collect();
    assert_json_indent2(&serde_json::to_value(&names).unwrap(), "initial_gm_tool_names.json");
}

#[test]
fn initial_gm_tool_names_with_player_byte_identical() {
    let names: Vec<String> = agents::initial_gm_tool_names(true).into_iter().collect();
    assert_json_indent2(
        &serde_json::to_value(&names).unwrap(),
        "initial_gm_tool_names_with_player.json",
    );
}

#[test]
fn build_for_model_filters_loaded_set() {
    // None -> all (minus ask_player when player options off).
    let all = agents::build_gm_tools_for_model(None, false);
    assert_eq!(all.len(), 12); // 13 tools minus ask_player
    let all_with = agents::build_gm_tools_for_model(None, true);
    assert_eq!(all_with.len(), 13);
    // Loaded set: only initial 8 + loaded extras visible.
    let loaded: BTreeSet<String> = ["move_npc".to_string()].into_iter().collect();
    let visible = agents::build_gm_tools_for_model(Some(&loaded), false);
    let names: BTreeSet<String> = visible
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert!(names.contains("move_npc"));
    assert!(names.contains("ask_npc"));
    assert!(!names.contains("set_scene"));
    assert!(!names.contains("ask_player"));
}

#[test]
fn search_select_and_keyword() {
    // select: exact loading.
    let res = agents::search_gm_tools("select:move_npc,set_scene", 5, None, false);
    let loaded: Vec<&str> = res["loaded_tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(loaded.contains(&"move_npc"));
    assert!(loaded.contains(&"set_scene"));
    // keyword search hits the move_npc hint.
    let res2 = agents::search_gm_tools("персонаж входит в сцену", 5, None, false);
    assert!(!res2["matches"].as_array().unwrap().is_empty());
    // empty query -> empty matches + canned message.
    let res3 = agents::search_gm_tools("   ", 5, None, false);
    assert_eq!(res3["matches"].as_array().unwrap().len(), 0);
    assert_eq!(
        res3["message"].as_str().unwrap(),
        "Запрос пустой. Используй keywords или select:tool_name."
    );
}

// --- NPC contract ----------------------------------------------------------

#[test]
fn npc_schema_byte_identical() {
    assert_json_indent2(&agents::npc_schema(), "npc_schema.json");
}

#[test]
fn npc_system_message_byte_identical() {
    assert_json_indent2(&agents::npc_system_message(), "npc_system_message.json");
}

#[test]
fn npc_card_block_byte_identical() {
    let w = build_world();
    let first_id = w.npcs.keys().next().expect("at least one npc").clone();
    let npc = &w.npcs[&first_id];
    assert_text_eq(&agents::npc_card_block(npc), "npc_card_block.txt");
}

fn npc_fixture_inputs() -> (String, String, String, Vec<String>) {
    (
        "Игрок подошёл к стойке и спрашивает о слухах.".to_string(),
        "Ты видел, как капитан стражи говорил с торговцем.".to_string(),
        "Ты уже сказал, что таверна закрывается в полночь.".to_string(),
        Vec::new(),
    )
}

#[test]
fn npc_user_message_byte_identical() {
    let mut w = build_world();
    let first_id = w.npcs.keys().next().unwrap().clone();
    let (situation, observations, commitments, _) = npc_fixture_inputs();
    let constraints: Vec<String> = w.constraints.clone();
    let scene_slice = w.npc_scene_slice(&first_id);
    let num = agents::npc_user_message(
        &situation,
        &observations,
        &commitments,
        None,
        &constraints,
        &scene_slice,
    );
    assert_json_indent2(&num, "npc_user_message.json");
}

#[test]
fn npc_user_message_feedback_byte_identical() {
    let mut w = build_world();
    let first_id = w.npcs.keys().next().unwrap().clone();
    let (situation, observations, commitments, _) = npc_fixture_inputs();
    let constraints: Vec<String> = w.constraints.clone();
    let scene_slice = w.npc_scene_slice(&first_id);
    let num = agents::npc_user_message(
        &situation,
        &observations,
        &commitments,
        Some("Так нельзя: задней двери нет."),
        &constraints,
        &scene_slice,
    );
    assert_json_indent2(&num, "npc_user_message_feedback.json");
}

#[test]
fn npc_request_messages_empty_byte_identical() {
    let mut w = build_world();
    let first_id = w.npcs.keys().next().unwrap().clone();
    let (situation, observations, commitments, _) = npc_fixture_inputs();
    let constraints: Vec<String> = w.constraints.clone();
    let scene_slice = w.npc_scene_slice(&first_id);
    let num = agents::npc_user_message(
        &situation,
        &observations,
        &commitments,
        None,
        &constraints,
        &scene_slice,
    );
    let npc = w.npcs[&first_id].clone();
    let req = agents::npc_request_messages(&npc, &[], "", &num);
    assert_json_indent2(&Value::Array(req), "npc_request_messages_empty.json");
}

#[test]
fn npc_card_absent_from_history_present_in_last_turn() {
    let w = build_world();
    let first_id = w.npcs.keys().next().unwrap().clone();
    let num = agents::npc_user_message("hello", "", "", None, &[], "");
    let npc = w.npcs[&first_id].clone();
    // history carries one prior user turn (card-free).
    let history = vec![agents::npc_user_message("earlier", "", "", None, &[], "")];
    let req = agents::npc_request_messages(&npc, &history, "", &num);
    // First message is the static system prompt.
    assert_eq!(req[0]["role"], "system");
    assert_eq!(req[0]["content"], gml_prompts::NPC_SYSTEM_STATIC);
    // The historical turn must NOT contain the CURRENT NPC CARD.
    let hist_msg = req[1]["content"].as_str().unwrap();
    assert!(!hist_msg.contains("CURRENT NPC CARD"));
    assert!(hist_msg.starts_with("HISTORICAL NPC EXCHANGE"));
    // The final user turn DOES lead with the CURRENT NPC CARD.
    let last = req.last().unwrap()["content"].as_str().unwrap();
    assert!(last.starts_with("CURRENT NPC CARD"));
}

// --- coercion --------------------------------------------------------------

#[test]
fn norm_npc_coercion() {
    use serde_json::json;
    let out = agents::norm_npc(&json!({
        "reasoning": "  думаю  ",
        "speech": "Привет",
        "action": 123,
        "claims": ["a", "", "  b  ", 7, null],
        "extra": "dropped",
    }));
    assert_eq!(out["reasoning"], "думаю");
    assert_eq!(out["speech"], "Привет");
    assert_eq!(out["action"], "123");
    assert_eq!(out["claims"], json!(["a", "b", "7"]));
    // exactly the four canonical keys, in order.
    let keys: Vec<&String> = out.keys().collect();
    assert_eq!(keys, vec!["reasoning", "speech", "action", "claims"]);
    // non-dict input -> all-empty shape.
    let empty = agents::norm_npc(&json!("not an object"));
    assert_eq!(empty["reasoning"], "");
    assert_eq!(empty["claims"], json!([]));
}
