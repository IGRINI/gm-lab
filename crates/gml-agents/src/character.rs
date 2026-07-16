//! Dedicated significant-NPC / character generator.
//!
//! This is deliberately separate from the GM role. The GM decides *when* a new
//! significant character is needed and passes a qualitative brief; this
//! generator only drafts one bounded, fully-realized NPC card that the
//! orchestrator can validate/commit into canon. Background extras stay GM-voiced
//! narration and never reach this generator.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError};
use gml_prompts::{render_prompt, PromptId};
use gml_types::Role;
use gml_world::World;

fn render_character_generator_system() -> String {
    render_prompt(PromptId::CharacterGeneratorSystem, json!({}))
        .expect("embedded character generator system prompt must render")
}

fn render_character_generator_user(
    scene: &str,
    canon_world: &str,
    entity_refs: &str,
    player_sheet: &str,
    roster: &str,
    recent: &str,
    request_json: &str,
) -> String {
    render_prompt(
        PromptId::CharacterGeneratorUser,
        json!({
            "scene": scene,
            "canon_world": canon_world,
            "entity_refs": entity_refs,
            "player_sheet": player_sheet,
            "roster": roster,
            "recent": recent,
            "request_json": request_json,
        }),
    )
    .expect("embedded character generator user prompt must render")
}

pub fn character_generator_messages(
    world: &mut World,
    request: &Value,
    recent_anti_repeat: &[String],
    history: &[Value],
) -> Vec<Value> {
    let scene = world.scene_context();
    let canon_world = world.canon_world_context();
    let player_sheet = world.player_character_context();
    let roster = if world.npcs.is_empty() {
        "(none)".to_string()
    } else {
        world
            .npcs
            .values()
            .map(|npc| {
                let persona = clip_phrase(&npc.persona);
                format!("- {}; {}; {}", npc.name, npc.role, persona)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    // `entity_reference_context` takes `&mut self`; compute it after the
    // immutable borrows above have produced owned Strings.
    let entity_refs = world.entity_reference_context();
    let request_json = serde_json::to_string(request).unwrap_or_else(|_| "{}".to_string());
    let recent = if recent_anti_repeat.is_empty() {
        "(none)".to_string()
    } else {
        recent_anti_repeat
            .iter()
            .rev()
            .take(12)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    };
    let user = render_character_generator_user(
        &scene,
        &canon_world,
        &entity_refs,
        &player_sheet,
        &roster,
        &recent,
        &request_json,
    );
    let mut messages =
        vec![json!({"role": "system", "content": render_character_generator_system()})];
    messages.extend(history.iter().filter_map(character_history_message));
    messages.push(json!({"role": "user", "content": user}));
    messages
}

pub async fn generate_character(
    client: &dyn Backend,
    world: &mut World,
    request: &Value,
    recent_anti_repeat: &[String],
    history: &[Value],
) -> Result<Map<String, Value>, BackendError> {
    let messages = Value::Array(character_generator_messages(
        world,
        request,
        recent_anti_repeat,
        history,
    ));
    client
        .chat_json(&messages, Some(true), Role::Character.as_str())
        .await
}

/// One-phrase persona for the anti-duplication roster line: trimmed and clipped
/// to a short lead so the block stays compact.
fn clip_phrase(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "(без описания)".to_string();
    }
    let clipped: String = trimmed.chars().take(100).collect();
    if clipped.chars().count() < trimmed.chars().count() {
        format!("{clipped}…")
    } else {
        clipped
    }
}

fn character_history_message(message: &Value) -> Option<Value> {
    let object = message.as_object()?;
    let role = object.get("role").and_then(Value::as_str)?;
    if !matches!(role, "user" | "assistant") {
        return None;
    }
    let content = object.get("content").and_then(Value::as_str)?.trim();
    if content.is_empty() {
        return None;
    }
    Some(json!({"role": role, "content": content}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_generator_templates_render_the_legacy_message_shape() {
        let system = render_character_generator_system();
        assert!(system.starts_with("You are the GM-Lab NPC generator"));
        assert!(system.ends_with("Return JSON only."));
        assert_eq!(
            render_character_generator_user(
                "scene",
                "world",
                "refs",
                "sheet",
                "roster",
                "recent",
                "{\"request\":true}",
            ),
            "## Current Scene\nscene\n\n## Canon World Context\nworld\n\n## Entity Refs\nrefs\n\n## Player Character Sheet\nsheet\n\n## Existing NPC Roster\nroster\n\n## Recent Anti-Repeat Keys\nrecent\n\n## Generation Request JSON\n{\"request\":true}\n\nGenerate the structured NPC now. Return JSON only."
        );
    }
}
