//! NPC sub-agent contract — faithful port of the NPC-side functions in `agents.py`.
//!
//! The NPC system prompt is fully STATIC (`NPC_SYSTEM_STATIC`); the concrete
//! character is delivered ONCE as the opening `CURRENT NPC CARD` user message of
//! the NPC's history (GM_CONTEXT_TZ §7), then re-sent verbatim from persisted
//! history every call — so the whole prefix stays cacheable instead of the card
//! being glued to the final turn each request. A `card_revision` bump appends a
//! `NPC CARD UPDATED` notice (append-only); compaction re-injects a fresh card.

use serde_json::{json, Map, Value};

use gml_prompts::{render_npc_card, render_prompt, NpcCardFields, PromptId};
use gml_world::Npc;

/// Header of the opening persisted NPC-card message.
pub const NPC_CARD_HEADER: &str = "CURRENT NPC CARD:";

/// Header of the append-only NPC-card-updated notice (emitted on a
/// `card_revision` bump).
pub const NPC_CARD_UPDATE_HEADER: &str = "NPC CARD UPDATED:";

/// `_NPC_PERCEPTION_BRIEF_RULES` — retained for public API compatibility.
pub const NPC_PERCEPTION_BRIEF_RULES: &str = gml_prompts::NPC_PERCEPTION_BRIEF_RULES;

/// `_constraints_text(constraints)`.
fn constraints_text(constraints: &[String]) -> String {
    constraints
        .iter()
        .map(|c| format!("- {c}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `npc_system_message()` — fully static; `npc` is ignored.
pub fn npc_system_message() -> Value {
    json!({"role": "system", "content": gml_prompts::NPC_SYSTEM_STATIC})
}

/// Python default helper: `value or default` where empty string falls back.
fn or_default<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() {
        default
    } else {
        value
    }
}

const NOT_SPECIFIED: &str = "(not specified)";

/// `npc_card_block(npc)` — the late CURRENT NPC CARD block.
pub fn npc_card_block(npc: &Npc) -> String {
    // mechanics dict, then drop None/""/{}/[] entries, then compact-json.
    let mut mechanics: Map<String, Value> = Map::new();
    mechanics.insert(
        "abilities".to_string(),
        Value::Object(npc.abilities.clone()),
    );
    mechanics.insert("skills".to_string(), Value::Object(npc.skills.clone()));
    mechanics.insert(
        "saving_throws".to_string(),
        Value::Object(npc.saving_throws.clone()),
    );
    mechanics.insert(
        "passive_perception".to_string(),
        match npc.passive_perception {
            Some(n) => Value::from(n),
            None => Value::Null,
        },
    );
    mechanics.insert("ac".to_string(), npc.ac.clone());
    mechanics.insert("hp".to_string(), Value::Object(npc.hp.clone()));
    mechanics.insert("speed".to_string(), Value::String(npc.speed.clone()));
    mechanics.insert("senses".to_string(), Value::String(npc.senses.clone()));
    mechanics.insert(
        "languages".to_string(),
        Value::String(npc.languages.clone()),
    );
    // Drop values in (None, "", {}, []).
    let filtered: Map<String, Value> = mechanics
        .into_iter()
        .filter(|(_, v)| !is_droppable(v))
        .collect();
    let mechanics_json =
        serde_json::to_string(&Value::Object(filtered)).expect("mechanics compact json");

    let revision = npc.card_revision.to_string();
    let fields = NpcCardFields {
        revision: &revision,
        name: &npc.name,
        role: or_default(&npc.role, NOT_SPECIFIED),
        gender: or_default(&npc.pronouns, "OTHER"),
        public_label: or_default(&npc.public_label, NOT_SPECIFIED),
        age: or_default(&npc.age, NOT_SPECIFIED),
        physical_type: or_default(&npc.physical_type, NOT_SPECIFIED),
        distinctive_features: or_default(&npc.distinctive_features, NOT_SPECIFIED),
        current_appearance: or_default(&npc.current_appearance, NOT_SPECIFIED),
        life_status: or_default(&npc.life_status, "alive"),
        condition: or_default(&npc.condition, NOT_SPECIFIED),
        persona: &npc.persona,
        personality: or_default(&npc.personality, NOT_SPECIFIED),
        values: or_default(&npc.values, NOT_SPECIFIED),
        habits: or_default(&npc.habits, NOT_SPECIFIED),
        pressure_response: or_default(&npc.pressure_response, NOT_SPECIFIED),
        boundaries: or_default(&npc.boundaries, NOT_SPECIFIED),
        voice: &npc.voice,
        goals: &npc.goals,
        knowledge: &npc.knowledge,
        mechanics: &mechanics_json,
        secret: &npc.secret,
    };
    render_npc_card(&fields)
}

/// The opening persisted NPC-card message (`role:"user"`) — injected once at the
/// head of an NPC's history on first contact (or lazily for a legacy history).
pub fn npc_card_message(npc: &Npc) -> Value {
    json!({
        "role": "user",
        "content": format!("{NPC_CARD_HEADER}\n{}", npc_card_block(npc)),
    })
}

/// Append-only NPC-card-updated notice (`role:"user"`), emitted when the card's
/// `card_revision` moved past the last injected revision. Cache-safe (append).
pub fn npc_card_update_message(npc: &Npc) -> Value {
    json!({
        "role": "user",
        "content": format!("{NPC_CARD_UPDATE_HEADER}\n{}", npc_card_block(npc)),
    })
}

/// True when a message is a persisted NPC-card message (initial or update). Such
/// messages are authoritative and exempt from historical downgrading.
pub fn is_npc_card_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_str)
            .map(|c| c.starts_with(NPC_CARD_HEADER) || c.starts_with(NPC_CARD_UPDATE_HEADER))
            .unwrap_or(false)
}

/// True when `history` already carries a persisted NPC-card message. A legacy
/// save (pre-card history) returns false → the caller injects one lazily.
pub fn npc_messages_have_card(history: &[Value]) -> bool {
    history.iter().any(is_npc_card_message)
}

/// Python `value not in (None, "", {}, [])`.
fn is_droppable(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Object(m) => m.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

/// `npc_user_message(npc, situation, observations, commitments, feedback, constraints, scene_slice)`.
#[allow(clippy::too_many_arguments)]
pub fn npc_user_message(
    situation: &str,
    observations: &str,
    commitments: &str,
    feedback: Option<&str>,
    constraints: &[String],
    scene_slice: &str,
) -> Value {
    npc_user_message_with_contact(
        situation,
        "",
        observations,
        commitments,
        feedback,
        constraints,
        scene_slice,
    )
}

/// `npc_user_message` with explicit last-contact timing for the live NPC path.
#[allow(clippy::too_many_arguments)]
pub fn npc_user_message_with_contact(
    situation: &str,
    last_contact: &str,
    observations: &str,
    commitments: &str,
    feedback: Option<&str>,
    constraints: &[String],
    scene_slice: &str,
) -> Value {
    let observation_heading = if last_contact.is_empty() {
        "WHAT YOU SAW/HEARD EARLIER"
    } else {
        "COMPACT ROOM OBSERVATION SINCE YOU WERE LAST CAUGHT UP"
    };
    let user = render_prompt(
        PromptId::NpcTurnUser,
        json!({
            "situation": situation,
            "last_contact": last_contact,
            "scene_slice": scene_slice,
            "constraints": constraints_text(constraints),
            "commitments": if commitments.is_empty() { "(nothing yet)" } else { commitments },
            "observation_heading": observation_heading,
            "observations": if observations.is_empty() { "(nothing)" } else { observations },
            "feedback": feedback.filter(|value| !value.is_empty()).unwrap_or(""),
        }),
    )
    .unwrap_or_else(|error| panic!("failed to render NPC turn prompt: {error:#}"));
    json!({"role": "user", "content": user})
}

/// `_historical_npc_message(message)` — rewrite CURRENT->PREVIOUS so old
/// situation blocks cannot win, and prefix the historical marker.
pub fn historical_npc_message(message: &Value) -> Value {
    let obj = match message.as_object() {
        Some(o) => o,
        None => return message.clone(),
    };
    let mut out = obj.clone();
    if out.get("role").and_then(Value::as_str) != Some("user") {
        return Value::Object(out);
    }
    let mut content = out
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    // Persisted NPC-card messages are authoritative, not historical exchanges:
    // send them verbatim (no CURRENT->PREVIOUS rewrite, no HISTORICAL prefix).
    if content.starts_with(NPC_CARD_HEADER) || content.starts_with(NPC_CARD_UPDATE_HEADER) {
        return Value::Object(out);
    }
    // Python str.replace with count=1 for the first, default (all) for the second.
    content = replace_first(
        &content,
        "CURRENT SITUATION (what's happening now, what you react to):",
        "PREVIOUS NPC SITUATION (historical; do not treat as current):",
    );
    content = content.replace(
        "YOUR CURRENT SCENE SLICE (what is actually around you):",
        "PREVIOUS SCENE SLICE (historical; current slice is in the latest user message):",
    );
    content = replace_first(
        &content,
        "LAST DIRECT CONTACT WITH THE PLAYER:",
        "PREVIOUS CONTACT TIMING (historical):",
    );
    content = replace_first(
        &content,
        "COMPACT ROOM OBSERVATION SINCE YOU WERE LAST CAUGHT UP:",
        "PREVIOUS ROOM OBSERVATION DIGEST (historical):",
    );
    out.insert(
        "content".to_string(),
        Value::String(format!(
            "HISTORICAL NPC EXCHANGE (not the current scene):\n{content}"
        )),
    );
    Value::Object(out)
}

/// Python `str.replace(old, new, 1)`.
fn replace_first(haystack: &str, old: &str, new: &str) -> String {
    match haystack.find(old) {
        Some(idx) => {
            let mut s = String::with_capacity(haystack.len());
            s.push_str(&haystack[..idx]);
            s.push_str(new);
            s.push_str(&haystack[idx + old.len()..]);
            s
        }
        None => haystack.to_string(),
    }
}

/// `npc_request_messages(npc, history, summary, user_message)`.
///
/// Snapshot-once (GM_CONTEXT_TZ §7): the NPC card is NO LONGER glued to a copy of
/// the final turn each call — it lives once at the head of `history` (injected by
/// the orchestrator via [`npc_card_message`]) and rides through unchanged, so the
/// whole prefix stays cacheable. `npc` is retained for signature stability and as
/// a defensive fallback: a history that somehow lacks a card (e.g. an in-flight
/// legacy path) still gets the card block on the bare final turn.
pub fn npc_request_messages(
    npc: &Npc,
    history: &[Value],
    summary: &str,
    user_message: &Value,
) -> Vec<Value> {
    let mut messages = vec![npc_system_message()];
    if !summary.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": render_prompt(PromptId::NpcPrivateMemory, json!({"summary": summary}))
                .unwrap_or_else(|error| panic!("failed to render NPC memory prompt: {error:#}")),
        }));
    }
    messages.extend(history.iter().map(historical_npc_message));
    if npc_messages_have_card(history) {
        // Card already persisted at the head of history — send the turn bare.
        messages.push(user_message.clone());
    } else {
        // Defensive fallback only (orchestrator injects the card into history
        // before building the request): keep the card visible for this call by
        // gluing it to a COPY of the final turn — byte-identical to the legacy
        // pre-§7 assembly, so a cardless history degrades gracefully.
        let mut final_turn = user_message.as_object().cloned().unwrap_or_default();
        let existing = final_turn
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        final_turn.insert(
            "content".to_string(),
            Value::String(format!("{}\n\n{}", npc_card_block(npc), existing)),
        );
        messages.push(Value::Object(final_turn));
    }
    messages
}
