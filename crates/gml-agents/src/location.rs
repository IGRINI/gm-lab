//! Dedicated location / travel-situation generator.
//!
//! This is deliberately separate from the GM role. The GM decides *when* a new
//! place or road situation is needed; this generator only drafts bounded,
//! structured location content that the orchestrator can validate/commit into
//! canon.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError};
use gml_prompts::{render_prompt, PromptId};
use gml_types::Role;
use gml_world::World;

fn render_location_generator_system() -> String {
    render_prompt(PromptId::LocationGeneratorSystem, json!({}))
        .expect("embedded location generator system prompt must render")
}

fn render_location_generator_user(
    scene: &str,
    canon_world: &str,
    entity_refs: &str,
    recent: &str,
    request_json: &str,
) -> String {
    render_prompt(
        PromptId::LocationGeneratorUser,
        json!({
            "scene": scene,
            "canon_world": canon_world,
            "entity_refs": entity_refs,
            "recent": recent,
            "request_json": request_json,
        }),
    )
    .expect("embedded location generator user prompt must render")
}

pub fn location_generator_messages(
    world: &mut World,
    request: &Value,
    recent_anti_repeat: &[String],
    history: &[Value],
) -> Vec<Value> {
    let scene = world.scene_context();
    let canon_world = world.canon_world_context();
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
    let user =
        render_location_generator_user(&scene, &canon_world, &entity_refs, &recent, &request_json);
    let mut messages =
        vec![json!({"role": "system", "content": render_location_generator_system()})];
    messages.extend(history.iter().filter_map(location_history_message));
    messages.push(json!({"role": "user", "content": user}));
    messages
}

pub async fn generate_location(
    client: &dyn Backend,
    world: &mut World,
    request: &Value,
    recent_anti_repeat: &[String],
    history: &[Value],
) -> Result<Map<String, Value>, BackendError> {
    let messages = Value::Array(location_generator_messages(
        world,
        request,
        recent_anti_repeat,
        history,
    ));
    client
        .chat_json(&messages, Some(true), Role::Location.as_str())
        .await
}

fn location_history_message(message: &Value) -> Option<Value> {
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
    fn location_generator_templates_render_the_legacy_message_shape() {
        let system = render_location_generator_system();
        assert!(system.starts_with("You are the GM-Lab location generator"));
        assert!(system.contains(
            "An empty or disconnected supplied travel graph is missing geography for you to"
        ));
        assert!(system.contains("current-scene prose, canon descriptions, local exits"));
        assert!(system.contains("Never invent a blocker from prose"));
        assert!(system.contains("Set `directionality` explicitly to exactly"));
        assert!(system.contains("creates a return transition only for"));
        assert!(system.contains("refusal."));
        assert!(!system.contains("return `travel_unavailable_reason`"));
        assert!(system.ends_with("Return JSON only."));
        assert_eq!(
            render_location_generator_user(
                "scene",
                "world",
                "refs",
                "recent",
                "{\"request\":true}",
            ),
            "## Current Scene\nscene\n\n## Canon World Context\nworld\n\n## Entity Refs\nrefs\n\n## Recent Anti-Repeat Keys\nrecent\n\n## Generation Request JSON\n{\"request\":true}\n\nGenerate the structured location/situation now. Return JSON only."
        );
    }
}
