//! GM message assembly — faithful port of the GM-side functions in `agents.py`.
//!
//! Cache-prefix discipline (PORT_PLAN §4.1): the request is
//! `[system GM_SYSTEM][system world_setup (PUBLIC INTRO only)]`
//! `[optional system "STORY SO FAR (compact): "+summary]`
//! `[*append-only gm_messages]`. The mutable per-turn state (roster, public
//! facts, scene, player card, entity refs, constraints, player action) lives in
//! the late user turn produced by [`gm_user_message`] / [`gm_turn_context`].

use serde_json::{json, Value};

use gml_world::World;

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

/// `TURN_RESOLUTION_CHECKLIST` — verbatim module constant.
pub const TURN_RESOLUTION_CHECKLIST: &str = "<system-reminder>
TURN RESOLUTION CHECK:
- First verify material possibility from PLAYER CHARACTER CARD and
  CURRENT SCENE STATE: required inventory/equipment/features, spells, tools, training,
  authority, body access, scene objects, materials, time, and position must exist.
  If the action rests on a missing or unsupported premise, stop with a reality correction:
  say what cannot happen and why, mention possible established remainders, and do not call
  roll_dice, ask_npc, advance_time, or state-update tools for that attempted premise.
  Only after the player deliberately continues with a physically possible remainder may you
  resolve that remainder; the missing item/spell/feature/expertise/effect stays absent.
- Before final narration, decide whether the latest player action needs roll_dice.
  Active observation/search/listening, including \"я осматриваюсь\", \"смотрю вокруг\",
  \"прислушиваюсь\", or \"ищу\", must roll Perception/Investigation/etc before hidden or
  non-obvious clues/details are revealed. Without the roll, reveal only obvious visible
  facts.
- Respect resolved rolls. A success gives a real benefit; a critical success gives the
  best plausible benefit. If the player asks why a strong roll did not produce the
  expected effect, explain the established constraint clearly and never invent a new
  post-roll reason to cancel the success.
- If any in-world time passed, call advance_time once before final narration. advance_time
  records elapsed time only; it does not replace a needed roll, NPC reaction, scene
  update, memory update, or player-sheet update.
</system-reminder>
";

/// `_gm_turn_context(world, player_text, include_player_options_tool)`.
///
/// Takes `&mut World` because the entity-reference projection memoizes/derives
/// from world state (Python calls `world.entity_reference_context()`).
pub fn gm_turn_context(
    world: &mut World,
    player_text: &str,
    include_player_options_tool: bool,
) -> String {
    let roster: String = world
        .npcs
        .values()
        .map(|npc| {
            let mut line = format!(
                "- id={}; internal_name={}; player_label={}; role={}",
                npc.npc_id,
                npc.name,
                world.npc_player_label(&npc.npc_id, "player"),
                npc.role
            );
            if !npc.pronouns.is_empty() {
                line.push_str(&format!("; род={}", crate::public_gender(&npc.pronouns)));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");

    let public_facts: Vec<String> = world
        .fact_records
        .iter()
        .filter(|r| r.kind == "public")
        .map(|r| r.text.clone())
        .collect();

    let mut system = String::from("CURRENT TURN CONTEXT (latest engine state snapshot):\n");
    system.push_str(&format!("\nTIME STATE:\n{}", world.time_context()));
    system.push_str(&format!(
        "\nINTERNAL NPC ROSTER (tool ids; internal_name is GM-only unless player_label \
matches it):\n{}",
        if roster.is_empty() {
            "(none)".to_string()
        } else {
            roster
        }
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
    system.push_str(&format!("\n\nCURRENT SCENE STATE:\n{}", world.scene_context()));
    system.push_str(&format!(
        "\n\nENTITY REFERENCE MARKUP:\n{}",
        world.entity_reference_context()
    ));
    if !world.constraints.is_empty() {
        system.push_str("\n\nSCENE CONSTRAINTS (must enforce when reviewing NPC responses):\n");
        let lines: Vec<String> = world.constraints.iter().map(|c| format!("- {c}")).collect();
        system.push_str(&lines.join("\n"));
    }
    if include_player_options_tool {
        system.push_str(
            "\n\nPLAYER OPTION SUGGESTIONS:\n\
enabled. After resolving all needed tools for this player action, call ask_player \
as the last tool before final narration with 4-8 useful Russian quick replies. \
This is mandatory for every completed turn while the feature is enabled: do not \
finish with narration only. Do not call more tools after ask_player unless the \
ask_player result reports invalid arguments. After the ask_player tool result \
confirms the buttons were shown, write the final player-facing narration and \
stop. The engine does not create fallback buttons. Do not put a textual choice \
menu in final narration; the quick-reply buttons handle it. Each option needs a \
short label and a fuller message that can be sent as the player's next action. \
Keep free text input available by offering suggestions, not commands.",
        );
    } else {
        system.push_str("\n\nPLAYER OPTION SUGGESTIONS:\ndisabled. Do not call ask_player.");
    }
    system.push_str(&format!("\n\n{TURN_RESOLUTION_CHECKLIST}"));
    system.push_str("\n\nPLAYER ACTION (latest user input, free roleplay text):\n");
    system.push_str(player_text.trim());
    system
}

/// `gm_user_message(world, player_text, include_player_options_tool)`.
pub fn gm_user_message(
    world: &mut World,
    player_text: &str,
    include_player_options_tool: bool,
) -> Value {
    json!({
        "role": "user",
        "content": gm_turn_context(world, player_text, include_player_options_tool),
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
