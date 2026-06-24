//! Dedicated location / travel-situation generator.
//!
//! This is deliberately separate from the GM role. The GM decides *when* a new
//! place or road situation is needed; this generator only drafts bounded,
//! structured location content that the orchestrator can validate/commit into
//! canon.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError};
use gml_types::Role;
use gml_world::World;

const LOCATION_GENERATOR_SYSTEM: &str = r#"You are the GM-Lab location generator, a specialist content author called by the
Game Master. The GM decides when generation is needed; you draft one bounded,
structured place or travel situation for the engine to validate and commit.

## Priorities
1. Canon fidelity: use supplied names, ids, factions, routes, time, and geography.
2. Player-visible honesty: visible_summary and description contain only what a
   character could notice, infer locally, or learn immediately.
3. Playable affordances: include things the player can touch, ask about, follow,
   search, avoid, negotiate with, or use as leverage.
4. Anti-repeat: reuse neither recent anti_repeat_key values nor their names,
   motifs, weather, threat shapes, loot shapes, or social setups unless the
   request explicitly asks for repetition inside the same larger location.

## Visibility
Write in Russian. Keep hidden truth in hidden_summary, hidden_clues, knows_more,
and memory_note. Visible fields may foreshadow by traces, rumors, witnesses, or
physical evidence, but they must not explain secret causes, future threats, or
offscreen actors as facts.

## Shape
Generate exactly one bounded location, room, road stop, city point, village point,
dungeon point, or travel situation. Return compact, concrete fields: a name, kind,
short visible summary, useful description, 3-6 features, 2-5 choices, optional
sensory details, optional consequences, and 0-4 transitions. Transitions are only
real exits or next steps, with plausible time_cost_minutes and risk when known.

## Road Situations
For travel_situation, honor route_time_minutes, elapsed_minutes,
remaining_minutes, situation_type, rarity, and road_risk. Place the situation at
the elapsed point of the journey, not automatically at the destination. Guarded
roads skew toward patrols, tolls, delays, witnesses, commerce, signs, controlled
trouble, or lawful complications. Dangerous roads can produce harsher events.

## JSON Object Shape
Return a single JSON object like this. Keep the same field names. Omit optional
fields only when they add no useful signal.

{
  "name": "Короткое русское название места",
  "kind": "room | local_place | city_point | village_point | dungeon_point | road_stop | travel_situation",
  "visible_summary": "1 short Russian sentence with only visible/player-safe facts",
  "description": "1 compact Russian paragraph of concrete visible details",
  "hidden_summary": "GM-only secret cause or backstage truth, if any",
  "features": ["3-6 concrete interactable details"],
  "sensory_details": ["optional smell/sound/light/texture details"],
  "choices": ["2-5 natural player actions this place supports"],
  "consequences": ["optional likely consequences or pressures"],
  "hidden_clues": ["optional clues the GM can reveal through play"],
  "knows_more": ["optional NPC/group/place that can reveal more"],
  "transitions": [
    {
      "label": "visible exit/action label",
      "destination_hint": "where it plausibly leads",
      "kind": "door | road | path | stairs | corridor | clue_followup | other",
      "time_cost_minutes": 5,
      "risk": "none | low | medium | high"
    }
  ],
  "anti_repeat_key": "short-lowercase-motif-key",
  "memory_note": "one compact GM memory note, if this place matters later"
}

Return JSON only."#;

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
    let user = format!(
        "## Current Scene\n{scene}\n\n## Canon World Context\n{canon_world}\n\n## Entity Refs\n{entity_refs}\n\n## Recent Anti-Repeat Keys\n{recent}\n\n## Generation Request JSON\n{request_json}\n\nGenerate the structured location/situation now. Return JSON only."
    );
    let mut messages = vec![json!({"role": "system", "content": LOCATION_GENERATOR_SYSTEM})];
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
        .chat_json(&messages, &Value::Null, Some(true), Role::Location.as_str())
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
