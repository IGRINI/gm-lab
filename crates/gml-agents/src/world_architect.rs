//! Dedicated world-architect agent.
//!
//! This is not the in-game GM and not the location generator. It is a separate
//! planning chat that helps the player author a world bible before a procedural
//! campaign is created. The only mutating surface it has is a draft tool call;
//! committing that draft into an actual campaign remains the `/chats`
//! procedural create path.

use serde_json::{json, Value};

use gml_llm::{Backend, BackendError};
use gml_types::Role;

const WORLD_ARCHITECT_SYSTEM: &str = r#"You are the GM-Lab world architect, a specialist AI that helps a user create a
coherent playable world bible before the live GM starts a campaign.

You are not the in-game GM. Do not narrate play, do not resolve player actions,
and do not create a running scene. Your job is to ask useful questions, clarify
preferences, and draft the world-level canon that later constrains the GM and
location generator.

Write in Russian. Keep the conversation practical and concrete. If the user is
underspecified, ask 3-6 focused questions instead of inventing everything. If
there is enough information, summarize the direction and call draft_world_bible.

## What a complete world bible must cover
- Core promise: what kind of story this world makes possible.
- Genre, tone, scale, inspirations, and explicit anti-inspirations.
- Reality laws: magic, technology, divinity, ecology, death, travel, and limits.
- Religions/creeds, gods/spirits/forces, heresies, rituals, taboos, afterlife.
- History: ancient origin, major breaks, recent causes; avoid one-cause history.
- Geography: macro regions, roads, borders, dangerous zones, climate pressures.
- Peoples/cultures: species, classes, languages, customs, law, education, food.
- Power: rulers, factions, institutions, armies, guilds, corporations, councils.
- Economy: scarcity, trade, resources, money, production, transport, debt.
- Daily life: what common people know, fear, celebrate, punish, and want.
- Creatures/anomalies: what can exist here and why it belongs.
- Hidden GM truths and secrets that must not leak to the player directly.
- Location generation rules: what future cities, villages, rooms, dungeons,
  roads, and situations are allowed to contain.
- Prohibited elements: things that should not appear without a special reason.
- Story hooks: tensions that can produce a living campaign start.
- Open questions: what still needs user choice later.

## Tool use
Use draft_world_bible only when you have a coherent draft or a meaningful update
to the previous draft. The tool is the source of structured truth for the UI.
Do not put hidden secrets into player-facing fields.

Example draft shape:
{
  "title": "Порог Второго Неба",
  "genre": "fantasy isekai",
  "tone": "tense hopeful",
  "scale": "region",
  "story_brief": "Ты приходишь в мир, где имя и клятва весят больше стали...",
  "public_intro": "Местные знают: старые договоры с духами снова дают трещину...",
  "world_lore": {
    "name": "Порог Второго Неба",
    "public_premise": "...",
    "hidden_premise": "...",
    "dogmas": ["..."],
    "world_laws": ["..."],
    "regions": ["..."],
    "power_centers": ["..."],
    "religions": ["..."],
    "gods": ["..."],
    "cultures": ["..."],
    "history": ["..."],
    "economy": ["..."],
    "daily_life": ["..."],
    "hidden_secrets": ["..."],
    "location_rules": ["..."],
    "prohibited_elements": ["..."]
  }
}"#;

pub struct WorldArchitectOutput {
    pub reply: String,
    pub draft: Option<Value>,
    pub assistant_msg: Value,
    pub calls: Vec<Value>,
}

pub fn world_architect_messages(history: &[Value], draft: &Value, user_text: &str) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": WORLD_ARCHITECT_SYSTEM})];
    messages.extend(history.iter().filter_map(history_message).take(30));
    let draft_json = serde_json::to_string(draft).unwrap_or_else(|_| "null".to_string());
    messages.push(json!({
        "role": "user",
        "content": format!(
            "## Current Draft JSON\n{draft_json}\n\n## User Message\n{}\n\nAnswer now. Ask questions if needed; call draft_world_bible if the draft should be updated.",
            user_text.trim()
        )
    }));
    messages
}

pub fn world_architect_tools() -> Vec<Value> {
    vec![json!({
        "type": "function",
        "function": {
            "name": "draft_world_bible",
            "description": "Create or update the structured world bible draft that can later be committed into a procedural campaign.",
            "parameters": {
                "type": "object",
                "additionalProperties": true,
                "properties": {
                    "title": {"type": "string", "description": "Player-facing campaign/world title."},
                    "genre": {"type": "string", "description": "Short genre label, e.g. fantasy isekai or machine postapocalypse."},
                    "tone": {"type": "string", "description": "Short tone label."},
                    "scale": {"type": "string", "description": "Starting scope: village, town, city, outpost, region, kingdom, world."},
                    "story_brief": {"type": "string", "description": "Short player-facing start brief: where they are, what happened, why it matters."},
                    "public_intro": {"type": "string", "description": "Player-safe world premise."},
                    "world_lore": {
                        "type": "object",
                        "additionalProperties": true,
                        "description": "Structured canon world bible. Include public and hidden fields, lists of rules, faiths, regions, powers, cultures, history, economy, daily life, secrets, generation rules, and prohibited elements."
                    },
                    "open_questions": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Questions still worth asking the user later."
                    }
                }
            }
        }
    })]
}

pub async fn world_architect_turn(
    client: &dyn Backend,
    history: &[Value],
    draft: &Value,
    user_text: &str,
) -> Result<WorldArchitectOutput, BackendError> {
    let messages = Value::Array(world_architect_messages(history, draft, user_text));
    let tools = Value::Array(world_architect_tools());
    let output = client
        .chat(&messages, Some(&tools), Some(true), Role::Gm.as_str())
        .await?;
    let draft_call = output
        .calls
        .iter()
        .rev()
        .find(|call| call.name == "draft_world_bible")
        .map(|call| Value::Object(call.arguments.clone()));
    let reply = if output.content.trim().is_empty() {
        draft_call
            .as_ref()
            .and_then(draft_title)
            .map(|title| format!("Собрал черновик мира «{title}». Его можно править дальше или создать по нему историю."))
            .unwrap_or_else(|| "Черновик мира обновлён. Его можно править дальше или создать по нему историю.".to_string())
    } else {
        output.content.trim().to_string()
    };
    let calls = output
        .calls
        .iter()
        .map(|call| {
            json!({
                "name": call.name,
                "arguments": call.arguments,
                "id": call.id,
            })
        })
        .collect();
    Ok(WorldArchitectOutput {
        reply,
        draft: draft_call,
        assistant_msg: output.assistant_msg,
        calls,
    })
}

fn history_message(message: &Value) -> Option<Value> {
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

fn draft_title(value: &Value) -> Option<String> {
    let object = value.as_object()?;
    let title = object
        .get("title")
        .or_else(|| object.get("world_lore").and_then(|lore| lore.get("name")))
        .and_then(Value::as_str)?
        .trim();
    if title.is_empty() {
        None
    } else {
        Some(title.to_string())
    }
}
