//! gml-agents — the model-boundary layer for GM-Lab.
//!
//! Faithful port of `gm-lab/agents.py` (subsystem map "Model roles & tool
//! definitions"). Assembles the GM request messages (cache-prefix discipline,
//! PORT_PLAN §4.1), the STATIC GM tool catalog, the NPC sub-agent contract, and
//! the LLM-driven world-seed / scene-delta helpers.
//!
//! Modules:
//! - [`gm`] — `gm_system`, `gm_world_setup`, `gm_world_snapshot`,
//!   `gm_user_message`, `gm_request_messages`.
//! - [`tools`] — `build_gm_tools`, `gm_tool_catalog`, `build_gm_tools_for_model`,
//!   `build_gm_tools_for_native_tool_search`, `search_gm_tools`,
//!   `load_gm_tool_schema`, `initial_gm_tool_names`, `build_canon_gm_tools`,
//!   `build_npc_tools`.
//! - [`npc`] — `npc_system_message`, `npc_card_block`,
//!   `npc_user_message`, `_historical_npc_message`, `npc_request_messages`.
//! - [`seed`] — `build_world_seed`, `extract_scene_delta`, and helpers.
//! - [`coerce`] — `_text`, `_as_list`, `_claims`, `_norm_npc`.
//!
//! Messages are `serde_json::Value` objects shaped exactly like the Python
//! dicts (`{"role","content"[,"tool_calls"/"tool_call_id"]}`), so they serialize
//! byte-identically and feed straight into the `gml_llm::Backend` surface.

// The STATIC GM tool schemas (`tools.rs`) are deeply-nested `json!` literals
// that exceed the default macro recursion limit during expansion.
#![recursion_limit = "1024"]

pub mod architect_runner;
pub mod character;
pub mod character_architect;
pub mod coerce;
pub mod gm;
pub mod location;
pub mod npc;
pub mod seed;
pub mod story_architect;
mod tool_guidance;
pub mod tools;
pub mod world_architect;

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput};
use gml_prompts::{render_prompt, PromptId};
use gml_types::Role;
use gml_world::World;

// Re-exports mirroring the `agents.py` public surface.
pub use architect_runner::{ArchitectOutput, ArchitectStream, NullArchitectStream};
pub use character::{character_generator_messages, generate_character};
pub use character_architect::{
    character_architect_base_unavailable_block, character_architect_messages,
    character_architect_story_block, character_architect_system, character_architect_tools,
    character_architect_turn, character_architect_world_block, CharacterArchitectOutput,
    CHARACTER_ARCHITECT_SYSTEM, CHARACTER_ARCHITECT_SYSTEM_BASED,
};
pub use coerce::{as_list, claims, norm_npc, norm_npc_with_reasoning, text};
pub use gm::{
    gm_messages_have_snapshot, gm_options_notice_message, gm_request_messages, gm_snapshot_message,
    gm_system, gm_user_message, gm_world_setup, gm_world_snapshot, is_snapshot_message,
    OPTIONS_NOTICE_PREFIX, PLAYER_ACTION_HEADER, SNAPSHOT_HEADER, SNAPSHOT_PREFIX,
};
pub use location::{generate_location, location_generator_messages};
pub use npc::{
    historical_npc_message, is_npc_card_message, npc_card_block, npc_card_message,
    npc_card_update_message, npc_messages_have_card, npc_request_messages, npc_system_message,
    npc_user_message, npc_user_message_with_contact, NPC_CARD_HEADER, NPC_CARD_UPDATE_HEADER,
    NPC_PERCEPTION_BRIEF_RULES,
};
pub use seed::{build_world_seed, extract_scene_delta};
pub use story_architect::{
    story_architect_messages, story_architect_system, story_architect_tools, story_architect_turn,
    story_architect_world_lore_block, StoryArchitectOutput, STORY_ARCHITECT_SYSTEM,
};
pub use tools::{
    build_canon_gm_tools, build_gm_tools, build_gm_tools_for_model,
    build_gm_tools_for_native_tool_search, build_npc_tools, gm_tool_catalog, initial_gm_tool_names,
    load_gm_tool_schema, search_gm_tools, CANON_GM_TOOL_NAMES,
};
pub use world_architect::{
    world_architect_messages, world_architect_tools, world_architect_tools_with_options,
    world_architect_turn, world_architect_turn_with_options, WorldArchitectOptions,
    WorldArchitectOutput,
};

/// Model-facing grammatical-gender label used by the GM roster and scene-delta
/// roster lines. Custom values remain untouched.
pub fn public_gender(value: &str) -> String {
    gml_world::model_gender_label(value)
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
    let tools = Value::Array(if client.supports_native_tool_search() {
        build_gm_tools_for_native_tool_search(include_player_options_tool)
    } else {
        build_gm_tools_for_model(loaded_tool_names, include_player_options_tool)
    });
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
    let tools = Value::Array(if client.supports_native_tool_search() {
        build_gm_tools_for_native_tool_search(include_player_options_tool)
    } else {
        build_gm_tools_for_model(loaded_tool_names, include_player_options_tool)
    });
    client
        .chat_stream(&messages, Some(&tools), Some(true), Role::Gm.as_str(), sink)
        .await
}

/// `gm_prelude_stream(client, world, player_text, calls)` — player-facing setup
/// narration shown before visible tool resolution.
fn render_gm_prelude_system() -> String {
    render_prompt(PromptId::GmPreludeSystem, json!({"trailing_newline": "\n"}))
        .expect("embedded GM prelude system prompt must render")
}

fn render_gm_prelude_user(
    scene_context: &str,
    entity_refs: &str,
    player_text: &str,
    pending_calls_json: &str,
) -> String {
    render_prompt(
        PromptId::GmPreludeUser,
        json!({
            "scene_context": scene_context,
            "entity_refs": entity_refs,
            "player_text": player_text,
            "pending_calls_json": pending_calls_json,
        }),
    )
    .expect("embedded GM prelude user prompt must render")
}

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
    let system = render_gm_prelude_system();
    // json.dumps(call_brief, ensure_ascii=False)[:PRELUDE_CALLBRIEF_CHARS] — char slice.
    let brief_json = serde_json::to_string(&Value::Array(call_brief)).unwrap_or_default();
    let brief_clip: String = brief_json.chars().take(prelude_callbrief_chars).collect();
    let scene_context = world.scene_context();
    let entity_refs = world.entity_reference_context();
    let user = render_gm_prelude_user(
        &scene_context,
        &entity_refs,
        player_text.trim(),
        &brief_clip,
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
        situation,
        observations,
        commitments,
        feedback,
        constraints,
        scene_slice,
    );
    let msgs = Value::Array(npc_request_messages(npc, history, summary, &user_message));
    let data = client
        .chat_json(&msgs, Some(true), Role::Npc.as_str())
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
        situation,
        observations,
        commitments,
        feedback,
        constraints,
        scene_slice,
    );
    let msgs = Value::Array(npc_request_messages(npc, history, summary, &user_message));
    let JsonStreamOutput { data, stats } = client
        .chat_json_stream(&msgs, Some(true), Role::Npc.as_str(), sink)
        .await?;
    Ok((norm_npc(&Value::Object(data)), stats))
}

#[cfg(test)]
mod prompt_tests {
    use super::*;

    #[test]
    fn gm_prelude_templates_render_the_legacy_message_shape() {
        let system = render_gm_prelude_system();
        assert!(system.starts_with("You are the Game Master writing visible scene narration"));
        assert!(system.ends_with("current player-facing label.\n"));
        assert_eq!(
            render_gm_prelude_user("scene", "refs", "action", "calls"),
            "CURRENT SCENE STATE:\nscene\n\nENTITY REFERENCE MARKUP:\nrefs\n\nPLAYER ACTION:\naction\n\nPENDING RESOLUTION CONTEXT (do not mention this as mechanics):\ncalls\n\nWrite the pre-tool narration now."
        );
    }
}
