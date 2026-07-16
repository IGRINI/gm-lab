//! Dedicated significant-NPC / character generator.
//!
//! This is deliberately separate from the GM role. The GM decides *when* a new
//! significant character is needed and passes a qualitative brief; this
//! generator only drafts one bounded, fully-realized NPC card that the
//! orchestrator can validate/commit into canon. Background extras stay GM-voiced
//! narration and never reach this generator.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError};
use gml_types::Role;
use gml_world::World;

const CHARACTER_GENERATOR_SYSTEM: &str = r#"You are the GM-Lab NPC generator, a specialist content author called by the
Game Master. The GM decides when a significant character is needed and hands you a
qualitative brief; you draft one bounded, fully-realized NPC for the engine to
validate and commit into canon.

## Priorities
1. Canon fidelity: honor supplied names, ids, factions, places, and the world's
   genre and tone. Never translate or transliterate a canon name — reuse it exactly.
2. Player-visible honesty: name, public_label, role, and appearance carry only
   what a character could notice on sight; the person's truth surfaces through play.
3. Calibrated capability: read the Player Character Sheet and the requested power
   tier, then set every mechanics number relative to THAT player — no default hero,
   no fixed stat block.
4. Living friction: give real goals, an agenda, values, and boundaries that can
   resist or complicate the player instead of bland agreeableness.

## Anti-Repeat
Reuse neither recent anti_repeat_key values nor their names, archetypes, voices,
motifs, or social setups. Do NOT use the fixated names Elara / Элара,
Elias Thorne / Элиас Торн, or Kael / Каэль, nor their close variants.

## Visibility
Write every player-visible field in Russian. Keep GM-only truth in secret,
knowledge, and memory_note. Visible fields may hint at depth through manner,
appearance, or reputation, but must not state secrets, hidden goals, or offscreen
facts as plain truth.

## Shape
Generate exactly one significant NPC: a name, pronouns, short role, a public_label
the player sees before acquaintance, concrete appearance, a 1-2 sentence persona,
personality/values/habits/pressure_response/boundaries, a voice hint, 1-3 goals, a
present-moment agenda, an attitude to the player from -2 to 2, and what they know.
Include mechanics ONLY when this NPC may fight or be rolled against — then calibrate
every number to the player sheet and power tier; otherwise omit mechanics entirely.

## JSON Object Shape
Return a single JSON object like this. Keep the same field names. Omit optional
fields only when they add no useful signal.

{
  "name": "Имя и прозвище персонажа (RU, канон-имена не переводить)",
  "pronouns": "М | Ж",
  "role": "короткая роль или профессия (RU)",
  "public_label": "как игрок видит персонажа до знакомства (RU)",
  "age": "возраст или возрастная категория (RU)",
  "physical_type": "телосложение, вид, размер (RU)",
  "distinctive_features": "1-2 приметы, по которым его узнают (RU)",
  "persona": "1-2 предложения: суть и манера (RU)",
  "personality": "черты характера (RU)",
  "values": "во что верит, что защищает (RU)",
  "habits": "заметные привычки (RU)",
  "pressure_response": "как ведёт себя под давлением (RU)",
  "boundaries": "чего не сделает даже под нажимом (RU)",
  "voice": "подсказка стиля речи для озвучки (RU)",
  "goals": ["1-3 цели персонажа (RU)"],
  "agenda": "чем занят прямо сейчас в сцене (RU)",
  "attitude_to_player": 0,
  "knowledge": "что персонаж знает по теме сцены/сюжета (RU, GM-only)",
  "secret": "GM-only тайна — никогда в видимых полях (RU)",
  "mechanics": {
    "abilities": {"STR": 9, "DEX": 14, "CON": 11, "INT": 12, "WIS": 15, "CHA": 10},
    "skills": {"Скрытность": 4, "Проницательность": 3},
    "ac": 12,
    "hp": {"current": 16, "max": 16},
    "speed": "30 футов",
    "senses": "обычное зрение",
    "languages": "Общий"
  },
  "anti_repeat_key": "short-lowercase-motif-key",
  "memory_note": "компактная GM-заметка, если персонаж важен позже (RU)"
}

Return JSON only."#;

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
    let user = format!(
        "## Current Scene\n{scene}\n\n## Canon World Context\n{canon_world}\n\n## Entity Refs\n{entity_refs}\n\n## Player Character Sheet\n{player_sheet}\n\n## Existing NPC Roster\n{roster}\n\n## Recent Anti-Repeat Keys\n{recent}\n\n## Generation Request JSON\n{request_json}\n\nGenerate the structured NPC now. Return JSON only."
    );
    let mut messages = vec![json!({"role": "system", "content": CHARACTER_GENERATOR_SYSTEM})];
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
