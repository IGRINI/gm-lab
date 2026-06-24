//! Dedicated world-architect agent.
//!
//! This is not the in-game GM and not the location generator. It is a separate
//! planning chat that helps the player author a reusable world bible. The only
//! mutating surface it has is a draft tool call; saving the draft creates a
//! standalone world, not a running campaign.

use serde_json::{json, Map, Value};

use gml_llm::{Backend, BackendError, NullSink};
use gml_types::Role;

const WORLD_ARCHITECT_SYSTEM: &str = r#"You are the GM-Lab world architect, a specialist AI that helps a user create a
coherent reusable world bible before any story is created.

You are not the in-game GM. Do not narrate play, do not resolve player actions,
and do not create a running scene, player role, starting quest, or starting
location. Your job is to ask useful questions, clarify preferences, and draft
the world-level canon that later constrains the GM and location generator.

Write in Russian. Keep the conversation practical and concrete. If the user is
underspecified, ask 3-6 focused questions instead of inventing everything. If
there is enough information, summarize the direction and call draft_world_bible.

## What a complete world bible must cover
- Core promise: what kind of story this world makes possible.
- Genre, tone, world size, population scale, inspirations, and explicit
  anti-inspirations.
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
- World tensions: reusable conflicts that can support later stories without
  defining a specific start.
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
  "world_size": "Континент с несколькими королевствами, духами дорог и дальними землями за картой",
  "population": "Десятки миллионов жителей: люди, духи мест, малые народы и редкие призванные чужаки",
  "public_premise": "Имя, клятва и долг имеют силу закона и магии.",
  "world_lore": {
    "name": "Порог Второго Неба",
    "public_premise": "...",
    "hidden_premise": "...",
    "world_size": "...",
    "population": "...",
    "dogmas": ["..."],
    "world_laws": ["..."],
    "inhabitants": ["..."],
    "creatures": ["..."],
    "power_sources": ["..."],
    "technologies": ["..."],
    "taboos": ["..."],
    "conflicts": ["..."],
    "inspirations": ["..."],
    "regions": ["..."],
    "power_centers": ["..."],
    "religions": ["..."],
    "gods": ["..."],
    "cultures": ["..."],
    "history": ["..."],
    "economy": ["..."],
    "daily_life": ["..."],
    "story_hooks": ["..."],
    "hidden_secrets": ["..."],
    "location_rules": ["..."],
    "prohibited_elements": ["..."],
    "open_questions": ["..."]
  }
}"#;

pub struct WorldArchitectOutput {
    pub reply: String,
    pub draft: Option<Value>,
    pub user_msg: Value,
    pub assistant_history_msg: Value,
    pub assistant_msg: Value,
    pub calls: Vec<Value>,
    /// Cleaned reasoning text from the model call (for the debug view).
    pub thinking: String,
    /// Normalized `_meta` stats for the call (cached_tokens, prompt_eval_count,
    /// eval_count, durations) — drives the architect token-usage readout.
    pub stats: Map<String, Value>,
    /// The exact messages array sent to the model (for the debug view).
    pub request_messages: Value,
}

pub fn world_architect_messages(history: &[Value], draft: &Value, user_text: &str) -> Vec<Value> {
    let user_msg = world_architect_user_message(draft, user_text);
    world_architect_messages_with_user(history, user_msg)
}

pub fn world_architect_user_message(draft: &Value, user_text: &str) -> Value {
    let draft_json = serde_json::to_string(draft).unwrap_or_else(|_| "null".to_string());
    json!({
        "role": "user",
        "content": format!(
            "## Current Draft JSON\n{draft_json}\n\n## User Message\n{}\n\nAnswer now. Ask questions if needed; call draft_world_bible if the draft should be updated.",
            user_text.trim()
        )
    })
}

fn world_architect_messages_with_user(history: &[Value], user_msg: Value) -> Vec<Value> {
    let mut messages = vec![json!({"role": "system", "content": WORLD_ARCHITECT_SYSTEM})];
    messages.extend(history.iter().filter_map(history_message));
    messages.push(user_msg);
    messages
}

pub fn world_architect_tools() -> Vec<Value> {
    vec![json!({
        "type": "function",
        "function": {
            "name": "draft_world_bible",
            "description": "Create or update the structured reusable world bible draft.",
            "parameters": {
                "type": "object",
                "additionalProperties": true,
                "properties": {
                    "title": {"type": "string", "description": "World title."},
                    "genre": {"type": "string", "description": "Short genre label, e.g. fantasy isekai or machine postapocalypse."},
                    "tone": {"type": "string", "description": "Short tone label."},
                    "world_size": {"type": "string", "description": "Descriptive setting size, not a start scope."},
                    "population": {"type": "string", "description": "Approximate population scale and diversity."},
                    "public_premise": {"type": "string", "description": "Player-safe world premise without a starting quest."},
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
    let user_msg = world_architect_user_message(draft, user_text);
    let messages = Value::Array(world_architect_messages_with_user(
        history,
        user_msg.clone(),
    ));
    let tools = Value::Array(world_architect_tools());
    // Use the streaming path (deltas discarded) so the call's usage `stats` come
    // back for the architect token-usage readout; the architect UI is request/
    // response, not streamed, so a NullSink is fine.
    let mut sink = NullSink;
    let output = client
        .chat_stream(
            &messages,
            Some(&tools),
            Some(true),
            Role::Gm.as_str(),
            &mut sink,
        )
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
            .map(|title| format!("Собрал черновик мира «{title}». Его можно править дальше или сохранить как отдельный мир."))
            .unwrap_or_else(|| "Черновик мира обновлён. Его можно править дальше или сохранить как отдельный мир.".to_string())
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
    let assistant_history_msg = json!({"role": "assistant", "content": reply});
    Ok(WorldArchitectOutput {
        reply,
        draft: draft_call,
        user_msg,
        assistant_history_msg,
        assistant_msg: output.assistant_msg,
        calls,
        thinking: output.thinking,
        stats: output.stats,
        request_messages: messages,
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
