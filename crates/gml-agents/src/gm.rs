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

use gml_world::World;

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
    let parts = [
        "WORLD SETUP (stable public premise; cacheable):".to_string(),
        format!("PUBLIC INTRO:\n{}", world.public),
    ];
    parts.join("\n\n")
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

    let mut system = String::from(SNAPSHOT_HEADER);
    system.push('\n');
    system.push_str(&format!("\nTIME STATE:\n{}", world.time_context()));
    system.push_str(&format!(
        "\n\nDYNAMIC NPC ROSTER (relevant/nearby now; tool ids; internal_name is GM-only \
unless player_label matches it; use read_state(roster) for the full list):\n{roster}"
    ));
    if !public_facts.is_empty() {
        system.push_str("\n\nCURRENT PUBLIC FACTS:\n");
        let capped: Vec<String> = public_facts
            .iter()
            .take(12)
            .map(|f| format!("- {f}"))
            .collect();
        system.push_str(&capped.join("\n"));
    }
    system.push_str("\n\nPLAYER CHARACTER CARD (current sheet; GM-only notes may be present):\n");
    system.push_str(&world.player_character_context());
    system.push_str(&format!(
        "\n\nCURRENT SCENE STATE:\n{}",
        world.scene_context()
    ));
    let canon_world = world.canon_world_context();
    if !canon_world.is_empty() {
        system.push_str(&format!(
            "\n\nCANON WORLD (structured truth — region, settlement, factions, recent history):\n{canon_world}"
        ));
    }
    let memory_context = world.gm_memory_context();
    if !memory_context.is_empty() {
        system.push_str(&format!("\n\nLIVING MEMORY SNAPSHOT:\n{memory_context}"));
    }
    system.push_str(&format!(
        "\n\nENTITY REFERENCE MARKUP:\n{}",
        world.entity_reference_context()
    ));
    if !world.constraints.is_empty() {
        system.push_str("\n\nSCENE CONSTRAINTS (must enforce when reviewing NPC responses):\n");
        let lines: Vec<String> = world.constraints.iter().map(|c| format!("- {c}")).collect();
        system.push_str(&lines.join("\n"));
    }
    let options_state = if include_player_options_tool {
        "enabled"
    } else {
        "disabled"
    };
    system.push_str(&format!("\n\nPLAYER OPTION SUGGESTIONS:\n{options_state}"));
    system
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
    json!({"role": "user", "content": format!("{OPTIONS_NOTICE_PREFIX}{state}")})
}

/// `gm_user_message(player_text)` — the bare per-turn player action message.
pub fn gm_user_message(player_text: &str) -> Value {
    json!({
        "role": "user",
        "content": format!("{PLAYER_ACTION_HEADER}\n{}", player_text.trim()),
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
            "content": format!("STORY SO FAR (compact): {summary}"),
        }));
    }
    messages.extend(gm_messages.iter().cloned());
    messages
}
