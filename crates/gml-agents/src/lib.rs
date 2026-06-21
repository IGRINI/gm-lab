//! gml-agents — the model-boundary layer for GM-Lab.
//!
//! Faithful port of `gm-lab/agents.py` (subsystem map "Model roles & tool
//! definitions"). Assembles the GM request messages (cache-prefix discipline,
//! PORT_PLAN §4.1), the STATIC GM tool catalog, the NPC sub-agent contract, and
//! the LLM-driven world-seed / scene-delta helpers.
//!
//! Modules:
//! - [`gm`] — `_gm_system`, `_gm_world_setup`, `_gm_turn_context`,
//!   `gm_user_message`, `_gm_request_messages`, `TURN_RESOLUTION_CHECKLIST`.
//! - [`tools`] — `build_gm_tools`, `gm_tool_catalog`, `build_gm_tools_for_model`,
//!   `search_gm_tools`, `initial_gm_tool_names`.
//! - [`npc`] — `NPC_SCHEMA`, `npc_system_message`, `npc_card_block`,
//!   `npc_user_message`, `_historical_npc_message`, `npc_request_messages`.
//! - [`seed`] — `build_world_seed`, `extract_scene_delta` + schemas + helpers.
//! - [`coerce`] — `_text`, `_as_list`, `_claims`, `_norm_npc`.
//!
//! Messages are `serde_json::Value` objects shaped exactly like the Python
//! dicts (`{"role","content"[,"tool_calls"/"tool_call_id"]}`), so they serialize
//! byte-identically and feed straight into the `gml_llm::Backend` surface.

// The STATIC GM tool schemas (`tools.rs`) are deeply-nested `json!` literals
// (e.g. `update_world_state.parameters.items.items.properties.*`) that exceed
// the default macro recursion limit during expansion.
#![recursion_limit = "1024"]

pub mod coerce;
pub mod gm;
pub mod npc;
pub mod seed;
mod tool_guidance;
pub mod tools;

use serde_json::{json, Map, Value};

use gml_llm::{
    Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
};
use gml_types::Role;
use gml_world::World;

// Re-exports mirroring the `agents.py` public surface.
pub use coerce::{as_list, claims, norm_npc, text};
pub use gm::{
    gm_request_messages, gm_system, gm_turn_context, gm_user_message, gm_world_setup,
    TURN_RESOLUTION_CHECKLIST,
};
pub use npc::{
    historical_npc_message, npc_card_block, npc_request_messages, npc_schema, npc_system_message,
    npc_user_message, NPC_PERCEPTION_BRIEF_RULES,
};
pub use seed::{
    build_world_seed, extract_scene_delta, scene_delta_schema, world_seed_schema,
};
pub use tools::{
    build_gm_tools, build_gm_tools_for_model, gm_tool_catalog, initial_gm_tool_names,
    search_gm_tools,
};

/// `world._public_gender(value)` — RU grammatical-gender label, faithful port
/// of `world.py::_public_gender` (which gml-world keeps private). Used by the
/// GM roster and scene-delta roster lines.
pub fn public_gender(value: &str) -> String {
    let raw = value.trim();
    match raw.to_lowercase().as_str() {
        "m" => "мужской род".to_string(),
        "f" => "женский род".to_string(),
        "n" => "средний род".to_string(),
        "pl" => "множественное число".to_string(),
        "other" => "другое".to_string(),
        _ => raw.to_string(),
    }
}

// --- LLM-call wrappers (gm_turn / gm_turn_stream / npc_turn / prelude) ------
// These are thin adapters over the Backend trait; they build the request via the
// assembly functions above and forward to the client, exactly like agents.py.

/// `gm_turn(client, world, gm_messages, summary, loaded_tool_names, include_player_options_tool)`.
#[allow(clippy::too_many_arguments)]
pub async fn gm_turn(
    client: &dyn Backend,
    world: &World,
    gm_messages: &[Value],
    summary: &str,
    loaded_tool_names: Option<&std::collections::BTreeSet<String>>,
    include_player_options_tool: bool,
) -> Result<ChatOutput, BackendError> {
    let messages = Value::Array(gm_request_messages(world, gm_messages, summary));
    let tools = Value::Array(build_gm_tools_for_model(
        loaded_tool_names,
        include_player_options_tool,
    ));
    client
        .chat(&messages, Some(&tools), Some(true), Role::Gm.as_str())
        .await
}

/// `gm_turn_stream(...)`.
#[allow(clippy::too_many_arguments)]
pub async fn gm_turn_stream(
    client: &dyn Backend,
    world: &World,
    gm_messages: &[Value],
    summary: &str,
    loaded_tool_names: Option<&std::collections::BTreeSet<String>>,
    include_player_options_tool: bool,
    sink: &mut (dyn DeltaSink + Send),
) -> Result<ChatStreamOutput, BackendError> {
    let messages = Value::Array(gm_request_messages(world, gm_messages, summary));
    let tools = Value::Array(build_gm_tools_for_model(
        loaded_tool_names,
        include_player_options_tool,
    ));
    client
        .chat_stream(&messages, Some(&tools), Some(true), Role::Gm.as_str(), sink)
        .await
}

/// `gm_prelude_stream(client, world, player_text, calls)` — player-facing setup
/// narration shown before visible tool resolution.
pub async fn gm_prelude_stream(
    client: &dyn Backend,
    world: &mut World,
    player_text: &str,
    calls: &[Value],
    prelude_callbrief_chars: usize,
    sink: &mut (dyn DeltaSink + Send),
) -> Result<ChatStreamOutput, BackendError> {
    let mut call_brief: Vec<Value> = Vec::new();
    for call in calls {
        let call = match call.as_object() {
            Some(c) => c,
            None => continue,
        };
        let args = call
            .get("arguments")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or(Value::Object(Map::new()));
        call_brief.push(json!({
            "name": call.get("name").cloned().unwrap_or(Value::String(String::new())),
            "arguments": args,
        }));
    }
    let system = "You are the Game Master writing visible scene narration BEFORE a pending tool resolution
in a tabletop D&D 5e roleplay scene.

Write in Russian only. Use the length the moment deserves: usually one vivid paragraph,
or two compact paragraphs when there is public attention, travel, threat, searching,
social pressure, or a tense pause.
Address the player character as \"ты\"; do not call them \"игрок\" in the visible text.
Describe only what is already visible or directly declared by the player: where they
stand, who they address, how loudly/quietly they speak, what the room can notice, and
what sensory details and unresolved tension matter.
Do not resolve the action. Do not make NPCs answer, obey, refuse, enter, leave, reveal
facts, or react personally. Do not mention tools, checks, prompts, or internal mechanics.
Keep proper nouns exactly as written.
When important people or places are mentioned and the id is listed in ENTITY REFERENCE
MARKUP, use refs in the same shape, with the current player-facing label.
";
    // json.dumps(call_brief, ensure_ascii=False)[:PRELUDE_CALLBRIEF_CHARS] — char slice.
    let brief_json = serde_json::to_string(&Value::Array(call_brief)).unwrap_or_default();
    let brief_clip: String = brief_json.chars().take(prelude_callbrief_chars).collect();
    let scene_context = world.scene_context();
    let entity_refs = world.entity_reference_context();
    let user = format!(
        "CURRENT SCENE STATE:\n{}\n\nENTITY REFERENCE MARKUP:\n{}\n\nPLAYER ACTION:\n{}\n\n\
PENDING RESOLUTION CONTEXT (do not mention this as mechanics):\n{}\n\n\
Write the pre-tool narration now.",
        scene_context,
        entity_refs,
        player_text.trim(),
        brief_clip
    );
    let messages = json!([
        {"role": "system", "content": system},
        {"role": "user", "content": user},
    ]);
    client
        .chat_stream(&messages, None, Some(false), Role::Gm.as_str(), sink)
        .await
}

/// `npc_turn(client, npc, situation, ...)` — non-streaming NPC reaction.
#[allow(clippy::too_many_arguments)]
pub async fn npc_turn(
    client: &dyn Backend,
    npc: &gml_world::Npc,
    situation: &str,
    observations: &str,
    commitments: &str,
    feedback: Option<&str>,
    constraints: &[String],
    scene_slice: &str,
    history: &[Value],
    summary: &str,
) -> Result<Map<String, Value>, BackendError> {
    let user_message = npc_user_message(
        situation, observations, commitments, feedback, constraints, scene_slice,
    );
    let msgs = Value::Array(npc_request_messages(npc, history, summary, &user_message));
    let data = client
        .chat_json(&msgs, &npc_schema(), Some(true), Role::Npc.as_str())
        .await?;
    Ok(norm_npc(&Value::Object(data)))
}

/// `npc_turn_stream(...)` — streaming NPC reaction. Returns `(normalized, stats)`.
#[allow(clippy::too_many_arguments)]
pub async fn npc_turn_stream(
    client: &dyn Backend,
    npc: &gml_world::Npc,
    situation: &str,
    observations: &str,
    commitments: &str,
    feedback: Option<&str>,
    constraints: &[String],
    scene_slice: &str,
    history: &[Value],
    summary: &str,
    sink: &mut (dyn DeltaSink + Send),
) -> Result<(Map<String, Value>, Map<String, Value>), BackendError> {
    let user_message = npc_user_message(
        situation, observations, commitments, feedback, constraints, scene_slice,
    );
    let msgs = Value::Array(npc_request_messages(npc, history, summary, &user_message));
    let JsonStreamOutput { data, stats } = client
        .chat_json_stream(&msgs, &npc_schema(), Some(true), Role::Npc.as_str(), sink)
        .await?;
    Ok((norm_npc(&Value::Object(data)), stats))
}
