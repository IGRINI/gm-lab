//! Byte-for-byte golden tests against the captured Python reference fixtures in
//! `tests/reference/agents/`. The world is the default story `turnvale-murder`
//! seed built via a hermetic tempdir `StoryStore::default_seed` ->
//! `gml_world::World::from_seed`,
//! with the exact `capture_fixtures.py::capture_agents` inputs.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use gml_agents as agents;
use gml_world::{MemoryUnit, World, WorldLore};

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

/// Default story seed from a HERMETIC store over a tempdir. There is no global
/// store; constructing a `StoryStore` over a tempdir materializes the builtins
/// into the throwaway directory, so this byte-golden test never touches the real
/// library. The tempdir-store seed is byte-identical to the built-in package (a
/// materialize + scan of the embedded catalog), so the fixtures stay valid.
fn default_seed() -> serde_json::Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = gml_stories::StoryStore::new(dir.path()).expect("open store");
    store.default_seed()
}

fn build_world() -> World {
    World::from_seed_with_dice_seed(&default_seed(), DICE_SEED)
}

fn test_world_lore() -> WorldLore {
    WorldLore {
        name: "Пепельная Сеть".to_string(),
        public_premise: "Машинный постапокалипсис, где люди выживают вокруг старых узлов."
            .to_string(),
        location_rules: vec![
            "каждая новая локация должна учитывать доступ к энергии, воде, деталям или сигналу"
                .to_string(),
        ],
        prohibited_elements: vec!["классическая магия без технологического объяснения".to_string()],
        creatures: vec!["ремонтные дроны".to_string()],
        ..Default::default()
    }
}

/// Rewrite the fixture with the produced output when `GML_BLESS=1` is set
/// (after an intentional behaviour change). Returns true when it blessed.
fn maybe_bless(content: &str, fixture: &str) -> bool {
    if std::env::var("GML_BLESS").as_deref() == Ok("1") {
        std::fs::write(ref_path(fixture), content)
            .unwrap_or_else(|e| panic!("bless {fixture}: {e}"));
        true
    } else {
        false
    }
}

fn assert_text_eq(got: &str, fixture: &str) {
    if maybe_bless(got, fixture) {
        return;
    }
    let expected = read_fixture_str(fixture);
    assert_eq!(got, expected, "text mismatch vs {fixture}");
}

fn assert_json_indent2(got: &Value, fixture: &str) {
    let rendered = dumps_indent2(got);
    if maybe_bless(&rendered, fixture) {
        return;
    }
    let expected = read_fixture_str(fixture);
    assert_eq!(rendered, expected, "indent2 json mismatch vs {fixture}");
}

// --- GM assembly -----------------------------------------------------------

#[test]
fn gm_world_setup_byte_identical() {
    let w = build_world();
    assert_text_eq(&agents::gm_world_setup(&w), "gm_world_setup.txt");
}

// NOTE (GM_CONTEXT_TZ): `gm_turn_context` was split into `gm_world_snapshot`
// (state-only, no checklist / no PLAYER ACTION) + a bare `gm_user_message`.
// These byte-identical fixtures capture the NEW snapshot text; the reference
// `.txt` files are re-blessed by the fixtures stage (currently RED here).
#[test]
fn gm_world_snapshot_noopts_byte_identical() {
    let mut w = build_world();
    let got = agents::gm_world_snapshot(&mut w, &BTreeSet::new(), false);
    assert_text_eq(&got, "gm_turn_context_noopts.txt");
}

#[test]
fn gm_world_snapshot_opts_byte_identical() {
    let mut w = build_world();
    let got = agents::gm_world_snapshot(&mut w, &BTreeSet::new(), true);
    assert_text_eq(&got, "gm_turn_context_opts.txt");
}

#[test]
fn worldgen_world_surfaces_canon_world_context_to_the_gm() {
    // A procedurally generated world must reach the GM: its region / settlement
    // / factions appear in the turn context (not just legacy public facts).
    let mut w = World::from_worldgen_with_lore(
        &gml_world::canon::WorldSpec::from_seed("777"),
        test_world_lore(),
    );
    let ctx = agents::gm_world_snapshot(&mut w, &BTreeSet::new(), false);
    assert!(
        ctx.contains("CANON WORLD"),
        "GM context must surface the structured canon world"
    );
    assert!(
        ctx.contains("Region:") || ctx.contains("Settlement:") || ctx.contains("Factions:"),
        "canon world must include region/settlement/faction, got:\n{ctx}"
    );
    assert!(
        ctx.contains("World:") && ctx.contains("Location generation rules"),
        "canon world must include high-level world lore guardrails, got:\n{ctx}"
    );
}

#[test]
fn location_generator_receives_world_lore_guardrails() {
    let spec = gml_world::canon::WorldSpec {
        seed: "loc-lore".to_string(),
        genre: "postapocalyptic machine world".to_string(),
        tone: "bleak".to_string(),
        scale: "outpost".to_string(),
    };
    let mut w = World::from_worldgen_with_lore(&spec, test_world_lore());
    let messages = agents::location_generator_messages(
        &mut w,
        &json!({
            "reason": "player follows a road into an unknown place",
            "kind": "road_stop"
        }),
        &[],
        &[],
    );
    let user = messages
        .last()
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .expect("last user message");
    assert!(
        user.to_lowercase().contains("машин") || user.contains("Machine"),
        "{user}"
    );
    assert!(user.contains("Do not add without cause"), "{user}");
    assert!(user.contains("классическая магия"), "{user}");
    assert!(user.contains("ремонтные дроны"), "{user}");
}

#[test]
fn character_generator_receives_world_lore_guardrails() {
    let spec = gml_world::canon::WorldSpec {
        seed: "char-lore".to_string(),
        genre: "postapocalyptic machine world".to_string(),
        tone: "bleak".to_string(),
        scale: "outpost".to_string(),
    };
    let mut w = World::from_worldgen_with_lore(&spec, test_world_lore());
    let messages = agents::character_generator_messages(
        &mut w,
        &json!({
            "request": "капитан стражи, намного сильнее игрока, держит ворота",
            "role": "капитан стражи"
        }),
        &[],
        &[],
    );
    let user = messages
        .last()
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .expect("last user message");
    // World-lore guardrails from canon_world_context reach the generator prompt.
    assert!(
        user.to_lowercase().contains("машин") || user.contains("Machine"),
        "{user}"
    );
    assert!(user.contains("Do not add without cause"), "{user}");
    assert!(user.contains("классическая магия"), "{user}");
    assert!(user.contains("ремонтные дроны"), "{user}");
    // The player character sheet block reaches the generator user message.
    assert!(user.contains("## Player Character Sheet"), "{user}");
    assert!(user.contains("Pronouns:"), "{user}");
}

#[test]
fn gm_turn_context_includes_access_gated_living_memory_snapshot() {
    let mut w = World::from_worldgen(&gml_world::canon::WorldSpec::from_seed("778"));
    w.add_memory_unit(MemoryUnit {
        memory_id: "visible_memory".to_string(),
        owner_scope: "public".to_string(),
        summary: "GM_CONTEXT_MEMORY_SENTINEL travelers saw fresh hoofprints.".to_string(),
        ..Default::default()
    });
    w.add_memory_unit(MemoryUnit {
        memory_id: "hidden_memory".to_string(),
        owner_scope: "gm_private".to_string(),
        summary: "GM_CONTEXT_HIDDEN_SENTINEL the ambush is already prepared.".to_string(),
        ..Default::default()
    });

    let ctx = agents::gm_world_snapshot(&mut w, &BTreeSet::new(), false);
    let canon = ctx.find("CANON WORLD").expect("canon block");
    let memory = ctx.find("LIVING MEMORY SNAPSHOT").expect("memory block");
    let entity = ctx.find("ENTITY REFERENCE MARKUP").expect("entity refs");
    assert!(
        canon < memory && memory < entity,
        "memory snapshot belongs in late context between canon and entity refs"
    );
    assert!(ctx.contains("GM_CONTEXT_MEMORY_SENTINEL"), "{ctx}");
    assert!(!ctx.contains("GM_CONTEXT_HIDDEN_SENTINEL"), "{ctx}");
    assert!(!ctx.contains("visible_memory"), "{ctx}");
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
fn gm_world_snapshot_has_roster_and_facts_but_no_checklist_or_action() {
    let mut w = build_world();
    let snapshot = agents::gm_world_snapshot(&mut w, &BTreeSet::new(), false);
    // Snapshot carries dynamic roster + public facts (state), but NOT the
    // turn-resolution checklist (now standing GM_SYSTEM policy) nor a PLAYER
    // ACTION block (that is the bare per-turn user message).
    assert!(snapshot.contains("DYNAMIC NPC ROSTER"));
    assert!(snapshot.contains("CURRENT PUBLIC FACTS"));
    assert!(!snapshot.contains("<system-reminder>"), "checklist moved to GM_SYSTEM");
    assert!(!snapshot.contains("PLAYER ACTION"), "action is a separate bare message");

    // The bare per-turn user message is action-only: no roster, no checklist.
    let action = agents::gm_user_message(PLAYER_TEXT);
    let content = action["content"].as_str().expect("action content");
    assert!(content.starts_with("PLAYER ACTION:"));
    assert!(content.contains(PLAYER_TEXT));
    assert!(!content.contains("DYNAMIC NPC ROSTER"));
    assert!(!content.contains("<system-reminder>"));
}

// Snapshot-once shape (GM_CONTEXT_TZ §1-2): gm_messages now holds a WORLD
// SNAPSHOT user message followed by the bare PLAYER ACTION user message. The
// reference JSON fixtures are re-blessed to this shape by the fixtures stage
// (currently RED here; they also fold in the GM_SYSTEM edits that stage makes).
fn snapshot_then_action(w: &mut World, opts: bool) -> Vec<Value> {
    let snapshot = agents::gm_snapshot_message(&agents::gm_world_snapshot(w, &BTreeSet::new(), opts));
    vec![snapshot, agents::gm_user_message(PLAYER_TEXT)]
}

#[test]
fn gm_request_messages_empty_byte_identical() {
    let mut w = build_world();
    let messages = snapshot_then_action(&mut w, false);
    let req = agents::gm_request_messages(&w, &messages, "");
    assert_json_indent2(&Value::Array(req), "gm_request_messages_empty.json");
}

#[test]
fn gm_request_messages_summary_byte_identical() {
    let mut w = build_world();
    let messages = snapshot_then_action(&mut w, false);
    let req = agents::gm_request_messages(&w, &messages, "Краткое содержание прошлых сцен.");
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
    let rendered = dumps_compact(&tools);
    if maybe_bless(&rendered, "gm_tools.compact.json") {
        return;
    }
    let expected = read_fixture_str("gm_tools.compact.json");
    assert_eq!(rendered, expected, "compact gm_tools mismatch");
}

#[test]
fn gm_tools_have_no_dynamic_enums_or_roster() {
    let tools = agents::build_gm_tools();
    let json = serde_json::to_string(&Value::Array(tools.clone())).unwrap();
    // No human-readable roster injected into descriptions.
    assert!(
        !json.contains("Available NPCs"),
        "tools leak Available NPCs prose"
    );
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
    assert_json_indent2(
        &serde_json::to_value(&names).unwrap(),
        "initial_gm_tool_names.json",
    );
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
    // The full catalog is the static tools PLUS living-world canon tools and
    // the stable loader/invoker tools appended at the end.
    let all = agents::build_gm_tools_for_model(None, false);
    assert_eq!(all.len(), 24); // catalog minus ask_player (+take_item/drop_item/cast_spell/generate_npc/read_state/long_rest)
    let all_with = agents::build_gm_tools_for_model(None, true);
    assert_eq!(all_with.len(), 25);
    // Hidden loaded names no longer mutate top-level tools; move_player is a
    // PRIMARY/initial tool, world_debug and move_npc are invoked through the
    // stable schema loader path.
    let loaded: BTreeSet<String> = ["move_npc".to_string()].into_iter().collect();
    let visible = agents::build_gm_tools_for_model(Some(&loaded), false);
    let names: BTreeSet<String> = visible
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !names.contains("move_npc"),
        "loaded hidden tools must not change top-level tools"
    );
    assert!(names.contains("ask_npc"));
    assert!(
        names.contains("move_player"),
        "move_player is a primary/initial tool"
    );
    assert!(
        names.contains("get_memory"),
        "get_memory is a primary/initial memory tool"
    );
    assert!(
        names.contains("note_memory"),
        "note_memory is a primary/initial memory tool"
    );
    assert!(
        !names.contains("npc_remember"),
        "npc_remember is not a GM tool; NPCs use remember"
    );
    assert!(
        !names.contains("world_debug"),
        "world_debug is search-loaded only"
    );
    assert!(!names.contains("consolidate_memory"));
    assert!(!names.contains("set_scene"));
    assert!(!names.contains("update_world_state"));
    assert!(!names.contains("query_world_state"));
    assert!(!names.contains("ask_player"));
}

#[test]
fn native_tool_search_catalog_is_cache_stable() {
    let native = agents::build_gm_tools_for_native_tool_search(false);
    let function_names: BTreeSet<String> = native
        .iter()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect();
    assert!(function_names.contains("ask_npc"));
    assert!(function_names.contains("move_player"));
    assert!(function_names.contains("get_memory"));
    assert!(function_names.contains("note_memory"));
    assert!(!function_names.contains("npc_remember"));
    assert!(!function_names.contains("tool_search"));
    assert!(!function_names.contains("load_tool_schema"));
    assert!(!function_names.contains("invoke_loaded_tool"));
    assert!(!function_names.contains("move_npc"));
    assert!(!function_names.contains("set_scene"));

    assert!(native
        .iter()
        .any(|tool| tool.get("type").and_then(Value::as_str) == Some("tool_search")));
    let namespace = native
        .iter()
        .find(|tool| tool.get("type").and_then(Value::as_str) == Some("namespace"))
        .expect("deferred namespace");
    assert_eq!(namespace.get("name").unwrap(), "gm_deferred");
    let deferred = namespace.get("tools").and_then(Value::as_array).unwrap();
    let deferred_names: BTreeSet<String> = deferred
        .iter()
        .map(|tool| {
            tool.pointer("/function/name")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert!(deferred_names.contains("move_npc"));
    assert!(deferred_names.contains("set_scene"));
    assert!(deferred_names.contains("world_debug"));
    assert!(deferred_names.contains("consolidate_memory"));
    assert!(!function_names.contains("update_world_state"));
    assert!(!function_names.contains("query_world_state"));
    assert!(!deferred_names.contains("update_world_state"));
    assert!(!deferred_names.contains("query_world_state"));
    for tool in deferred {
        assert_eq!(tool.get("defer_loading").unwrap(), &json!(true));
    }
}

#[test]
fn npc_tool_catalog_has_only_actor_bound_tools() {
    let tools = agents::build_npc_tools();
    let names: Vec<String> = tools
        .iter()
        .filter_map(|tool| {
            tool.pointer("/function/name")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .collect();
    assert_eq!(
        names,
        vec!["remember", "npc_note_memory", "npc_recall_relationship"]
    );
    for tool in &tools {
        let schema = &tool["function"]["parameters"];
        assert!(
            schema.pointer("/properties/npc_id").is_none(),
            "NPC cannot choose another actor identity: {tool}"
        );
    }
    assert!(tools[0]["function"]["parameters"]
        .pointer("/properties/query")
        .and_then(Value::as_object)
        .is_some());
    assert!(tools[1]["function"]["parameters"]
        .pointer("/properties/text")
        .and_then(Value::as_object)
        .is_some());
    assert!(tools[2]["function"]["parameters"]
        .pointer("/properties/target")
        .and_then(Value::as_object)
        .is_some());
}

#[test]
fn search_select_and_keyword() {
    // select: exact catalog lookup; schema loading is a separate step.
    let res = agents::search_gm_tools("select:move_npc,set_scene", 5, None, false);
    assert!(
        res.get("loaded_tools").is_none(),
        "tool_search must not load schemas"
    );
    let matches = res["matches"].as_array().unwrap();
    let names: Vec<&str> = matches
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"move_npc"));
    assert!(names.contains(&"set_scene"));
    for row in matches {
        assert!(row.get("title").and_then(Value::as_str).is_some());
        assert!(row.get("description").and_then(Value::as_str).is_some());
        assert!(row.get("keywords").and_then(Value::as_array).is_some());
        assert!(row.get("aliases").and_then(Value::as_array).is_some());
        assert!(row.get("capabilities").and_then(Value::as_array).is_some());
        assert_eq!(row["load_tool"], "load_tool_schema");
        assert_eq!(row["load_schema"]["tool"], "load_tool_schema");
        assert!(row.get("schema").is_none());
        assert!(row.get("function").is_none());
        assert!(row.get("parameters").is_none());
    }

    let loaded_schema = agents::load_gm_tool_schema("move_npc", None, false);
    assert_eq!(loaded_schema["status"], "loaded_schema");
    assert_eq!(loaded_schema["loaded_schema"], "move_npc");
    assert_eq!(loaded_schema["invoke_tool"], "invoke_loaded_tool");
    assert!(loaded_schema.get("loaded_tools").is_none());
    assert_eq!(
        loaded_schema["schema"]["function"]["name"]
            .as_str()
            .unwrap(),
        "move_npc"
    );

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

    let legacy = agents::search_gm_tools(
        "select:update_world_state,query_world_state",
        5,
        None,
        false,
    );
    assert!(
        legacy["matches"].as_array().unwrap().is_empty(),
        "legacy flat world-state tools must not be discoverable by tool_search"
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
    assert_eq!(out["response"], "123 и говорит: «Привет»");
    assert_eq!(
        out["beats"],
        json!([
            {"kind": "action", "text": "123"},
            {"kind": "speech", "text": "Привет"}
        ])
    );
    assert_eq!(out["speech"], "Привет");
    assert_eq!(out["action"], "123");
    assert_eq!(out["claims"], json!(["a", "b", "7"]));
    // Primary current keys first, followed by compatibility fields.
    let keys: Vec<&String> = out.keys().collect();
    assert_eq!(
        keys,
        vec![
            "reasoning",
            "response",
            "beats",
            "speech",
            "action",
            "claims"
        ]
    );
    // non-dict input -> all-empty shape.
    let empty = agents::norm_npc(&json!("not an object"));
    assert_eq!(empty["reasoning"], "");
    assert_eq!(empty["response"], "");
    assert_eq!(empty["beats"], json!([]));
    assert_eq!(empty["claims"], json!([]));
}

#[test]
fn world_architect_has_static_prompt_and_draft_tool() {
    use serde_json::json;

    let messages = agents::world_architect_messages(
        &[json!({"role": "user", "content": "Хочу иссекай про клятвы."})],
        "Добавь богов и историю.",
    );
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].as_str().unwrap();
    assert!(system.contains("GM-Lab world architect"));
    // The field list lives in the tool schema now (not restated in the prompt), so
    // the prompt asserts on its stable behavioral markers, not the field names.
    assert!(system.contains("draft_world_bible"));
    assert!(system.contains("hidden_premise"));
    assert!(system.contains("world_size"));
    assert!(system.contains("population"));
    assert!(!system.contains("story_brief"));
    assert!(!system.contains("public_intro"));
    assert!(!system.contains("\"scale\""));
    // CACHE INVARIANT: the tail is the RAW user text (state comes only through
    // read_world_bible), byte-equal to the history entry the server stores.
    assert_eq!(
        messages.last().unwrap()["content"],
        "Добавь богов и историю."
    );

    let tools = agents::world_architect_tools();
    assert_eq!(tools.len(), 3);
    assert_eq!(
        tools[0]["function"]["name"], "draft_world_bible",
        "architect builds with draft_world_bible"
    );
    assert_eq!(
        tools[1]["function"]["name"], "edit_world_bible",
        "architect patches with edit_world_bible"
    );
    assert_eq!(
        tools[2]["function"]["name"], "read_world_bible",
        "architect reads full sections on demand (the digest truncates)"
    );
    // The edit tool exposes the patch ops.
    let edit_props = &tools[1]["function"]["parameters"]["properties"];
    assert!(edit_props["set"].is_object());
    assert!(edit_props["add"].is_object());
    assert!(edit_props["remove"].is_object());
    assert!(edit_props["replace"].is_object());
    let props = &tools[0]["function"]["parameters"]["properties"];
    // The schema is FLAT now: bible sections are top-level fields, not nested in a
    // `world_lore` object. The backend folds them back into `world_lore` for storage.
    assert!(
        props["world_lore"].is_null(),
        "no nested world_lore in the flat schema"
    );
    assert!(props["world_size"].is_object());
    assert!(props["population"].is_object());
    assert!(props["public_premise"].is_object());
    assert!(props["hidden_premise"].is_object());
    assert!(props["dogmas"]["type"] == "array");
    assert!(props["religions"]["type"] == "array");
    assert!(props["prohibited_elements"]["type"] == "array");
    assert!(props["scale"].is_null());
    assert!(props["story_brief"].is_null());
    assert!(props["public_intro"].is_null());
}

#[test]
fn character_architect_has_static_prompt_and_draft_tool() {
    use serde_json::json;

    let messages = agents::character_architect_messages(
        &[json!({"role": "user", "content": "Хочу следопыта."})],
        &[],
        "Добавь заклинания.",
    );
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].as_str().unwrap();
    assert!(system.contains("GM-Lab character architect"));
    assert!(system.contains("draft_player_character"));
    // A standalone hero (no base world/story blocks) — exactly ONE system.
    let system_count = messages.iter().filter(|m| m["role"] == "system").count();
    assert_eq!(system_count, 1);
    // CACHE INVARIANT: the tail is the RAW user text, byte-equal to stored history.
    assert_eq!(messages.last().unwrap()["content"], "Добавь заклинания.");

    let tools = agents::character_architect_tools();
    assert_eq!(tools.len(), 3);
    assert_eq!(tools[0]["function"]["name"], "draft_player_character");
    assert_eq!(tools[1]["function"]["name"], "edit_player_character");
    assert_eq!(tools[2]["function"]["name"], "read_player_character");
    // The edit tool exposes the patch ops AND is non-strict (the strict Responses
    // conversion would otherwise empty the properties-less section maps).
    let edit_props = &tools[1]["function"]["parameters"]["properties"];
    assert!(edit_props["set"].is_object());
    assert!(edit_props["add"].is_object());
    assert!(edit_props["remove"].is_object());
    assert!(edit_props["replace"].is_object());
    assert_eq!(tools[1]["function"]["strict"], json!(false));
    // The draft schema is the FLAT hero sheet: stats/lists are top-level fields.
    let props = &tools[0]["function"]["parameters"]["properties"];
    assert!(props["abilities"]["type"] == "object");
    assert!(props["hp"]["type"] == "object");
    assert!(props["inventory"]["type"] == "array");
    assert!(props["spells"]["type"] == "array");
    let required = tools[0]["function"]["parameters"]["required"]
        .as_array()
        .unwrap();
    assert_eq!(required, &vec![json!("name")]);
}

#[test]
fn world_architect_model_history_keeps_prior_user_message_stable() {
    use serde_json::json;

    // History entries are the RAW user/assistant texts; the current tail is the
    // raw text too — so every position is byte-stable across turns (the whole
    // prefix stays cacheable, and nothing draft-shaped ever enters history).
    let history = vec![
        json!({"role": "user", "content": "Собери основу мира."}),
        json!({"role": "assistant", "content": "Собрал первый черновик."}),
    ];
    let messages = agents::world_architect_messages(&history, "Теперь добавь власть и религию.");

    assert_eq!(messages[1], history[0]);
    assert_eq!(messages[2], history[1]);
    assert_eq!(
        messages.last().unwrap()["content"],
        "Теперь добавь власть и религию."
    );
}

#[test]
fn world_architect_model_history_is_not_windowed() {
    use serde_json::json;

    let history: Vec<_> = (0..36)
        .map(|index| json!({"role": "user", "content": format!("history-{index}")}))
        .collect();
    let messages = agents::world_architect_messages(&history, "Продолжай.");

    assert_eq!(messages.len(), history.len() + 2);
    assert_eq!(messages[1]["content"], "history-0");
    assert_eq!(messages[36]["content"], "history-35");
    assert_eq!(messages.last().unwrap()["content"], "Продолжай.");
}
