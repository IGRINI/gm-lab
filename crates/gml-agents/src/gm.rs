//! GM message assembly — faithful port of the GM-side functions in `agents.py`.
//!
//! Cache-prefix discipline (PORT_PLAN §4.1): the request is
//! `[system GM_SYSTEM][system world_setup (PUBLIC INTRO only)]`
//! `[optional system "STORY SO FAR (compact): "+summary]`
//! `[*append-only gm_messages]`.
//!
//! Snapshot-once design (GM_CONTEXT_TZ): the full engine-state snapshot is
//! pushed into `gm_messages` ONCE at session start (and FRESH at every
//! compaction) via [`gm_world_snapshot`] / [`gm_snapshot_message`]. Every turn
//! after that appends only the bare player action ([`gm_user_message`]); state
//! deltas arrive in tool results, not re-sent snapshots. This keeps the whole
//! dialogue prefix cacheable between compactions.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use gml_prompts::{render_prompt, PromptId};
use gml_world::World;

fn render(prompt: PromptId, context: Value) -> String {
    render_prompt(prompt, context)
        .unwrap_or_else(|error| panic!("failed to render prompt {prompt:?}: {error:#}"))
}

/// Header prefix identifying a WORLD SNAPSHOT user message in `gm_messages`.
/// Used to detect whether a (possibly legacy) history already carries a
/// snapshot, and to exclude the snapshot from compaction summaries.
pub const SNAPSHOT_HEADER: &str =
    "WORLD SNAPSHOT (актуальное состояние на момент снимка; дальше следи за дельтами из результатов тулов)";

/// Prefix of the append-only player-options toggle notice.
pub const OPTIONS_NOTICE_PREFIX: &str = "PLAYER OPTION SUGGESTIONS: ";

/// Bare player-action header for the per-turn user message.
pub const PLAYER_ACTION_HEADER: &str = "PLAYER ACTION:";

/// `_gm_system(world, summary)` — returns the static GM prompt unchanged.
pub fn gm_system() -> &'static str {
    gml_prompts::GM_SYSTEM
}

/// `_gm_world_setup(world)` — stable public premise (PUBLIC INTRO only).
pub fn gm_world_setup(world: &World) -> String {
    render(
        PromptId::GmWorldSetup,
        json!({"public_intro": world.public}),
    )
}

/// `gm_world_snapshot(world, recent_contact_ids, include_player_options_tool)` —
/// the one-time engine-state snapshot pushed into `gm_messages` at session start
/// and rebuilt fresh at every compaction. Carries TIME / DYNAMIC ROSTER / PUBLIC
/// FACTS / PLAYER CARD / SCENE / CANON / MEMORY / ENTITY REFS / CONSTRAINTS plus
/// the current player-options STATE line. It deliberately does NOT include the
/// turn-resolution checklist or the player-options behavior body (those are
/// standing policy in GM_SYSTEM) nor a PLAYER ACTION block.
///
/// Takes `&mut World` because the entity-reference projection memoizes/derives
/// from world state. Consumes ZERO dice RNG.
pub fn gm_world_snapshot(
    world: &mut World,
    recent_contact_ids: &BTreeSet<String>,
    include_player_options_tool: bool,
) -> String {
    let roster = world.dynamic_roster_context(recent_contact_ids);

    let public_facts: Vec<String> = world
        .fact_records
        .iter()
        .filter(|r| r.kind == "public")
        .map(|r| r.text.clone())
        .collect();

    let public_facts = public_facts
        .iter()
        .take(12)
        .map(|fact| format!("- {fact}"))
        .collect::<Vec<_>>()
        .join("\n");
    let canon_world = world.canon_world_context();
    let memory_context = world.gm_memory_context();
    let constraints = world
        .constraints
        .iter()
        .map(|constraint| format!("- {constraint}"))
        .collect::<Vec<_>>()
        .join("\n");
    let time_state = world.time_context();
    let player_card = world.player_character_context();
    let scene_context = world.scene_context();
    let entity_refs = world.entity_reference_context();
    let options_state = if include_player_options_tool {
        "enabled"
    } else {
        "disabled"
    };
    render(
        PromptId::GmWorldSnapshot,
        json!({
            "snapshot_header": SNAPSHOT_HEADER,
            "time_state": time_state,
            "roster": roster,
            "public_facts": public_facts,
            "player_card": player_card,
            "scene_context": scene_context,
            "canon_world": canon_world,
            "memory_context": memory_context,
            "entity_refs": entity_refs,
            "constraints": constraints,
            "options_state": options_state,
        }),
    )
}

/// Wrap a snapshot string as the `role:"user"` snapshot message.
pub fn gm_snapshot_message(snapshot: &str) -> Value {
    json!({"role": "user", "content": snapshot})
}

/// True when a message is a WORLD SNAPSHOT user message.
pub fn is_snapshot_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_str)
            .map(|c| c.starts_with(SNAPSHOT_HEADER))
            .unwrap_or(false)
}

/// True when `gm_messages` already contains a WORLD SNAPSHOT message. A legacy
/// save (pre-snapshot history) returns false → the caller injects one lazily.
pub fn gm_messages_have_snapshot(gm_messages: &[Value]) -> bool {
    gm_messages.iter().any(is_snapshot_message)
}

/// Append-only player-options toggle notice (`role:"user"`), emitted only when
/// the setting changes mid-session. Cache-safe (append, never rewrite).
pub fn gm_options_notice_message(include_player_options_tool: bool) -> Value {
    let state = if include_player_options_tool {
        "enabled"
    } else {
        "disabled"
    };
    json!({
        "role": "user",
        "content": render(
            PromptId::GmOptionsNotice,
            json!({"prefix": OPTIONS_NOTICE_PREFIX, "state": state}),
        ),
    })
}

/// `gm_user_message(player_text)` — the bare per-turn player action message.
pub fn gm_user_message(player_text: &str) -> Value {
    json!({
        "role": "user",
        "content": render(
            PromptId::GmPlayerAction,
            json!({"header": PLAYER_ACTION_HEADER, "player_text": player_text.trim()}),
        ),
    })
}

/// `_gm_request_messages(world, gm_messages, summary)`.
pub fn gm_request_messages(world: &World, gm_messages: &[Value], summary: &str) -> Vec<Value> {
    let mut messages = vec![
        json!({"role": "system", "content": gm_system()}),
        json!({"role": "system", "content": gm_world_setup(world)}),
    ];
    if !summary.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": render(PromptId::GmStorySummary, json!({"summary": summary})),
        }));
    }
    messages.extend(gm_messages.iter().cloned());
    messages
}
