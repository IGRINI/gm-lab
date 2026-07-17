//! Small coercion helpers — faithful ports of `_text`, `_as_list`, `_claims`,
//! and `_norm_npc` from `agents.py`.

use serde_json::{Map, Value};

/// `_text(value)` — strip strings; stringify+strip non-strings; `None` -> "".
pub fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        // Python `str(value).strip()` for non-str scalars. We only ever feed
        // this scalars in practice; numbers/bools render like Python's `str`
        // for the value shapes the seed helpers pass.
        Value::Bool(b) => {
            // Python str(True) == "True".
            if *b {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        Value::Number(n) => n.to_string(),
        other => other.to_string().trim().to_string(),
    }
}

/// `_as_list(value)` — `None` -> []; list/tuple -> list; scalar -> `[scalar]`.
pub fn as_list(value: &Value) -> Vec<Value> {
    match value {
        Value::Null => Vec::new(),
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    }
}

/// `_claims(value)` — only from a list; each item via [`text`], drop empties.
pub fn claims(value: &Value) -> Vec<String> {
    match value {
        Value::Array(a) => a.iter().map(text).filter(|s| !s.is_empty()).collect(),
        _ => Vec::new(),
    }
}

/// Normalize the NPC sub-agent JSON into the canonical current shape.
///
/// The visible contract is now an organic `response` plus optional ordered
/// `beats`. `speech`/`action` are retained as derived compatibility fields for
/// the UI and old event plumbing. `reasoning` is populated by the caller from
/// the model's hidden thinking channel when available; the JSON response should
/// not invent it.
pub fn norm_npc(out: &Value) -> Map<String, Value> {
    norm_npc_with_reasoning(out, "")
}

pub fn norm_npc_with_reasoning(out: &Value, reasoning: &str) -> Map<String, Value> {
    let obj = out.as_object();
    let get = |k: &str| -> Value { obj.and_then(|m| m.get(k)).cloned().unwrap_or(Value::Null) };
    let fallback_reasoning = text(&get("reasoning"));
    let reasoning = if reasoning.trim().is_empty() {
        fallback_reasoning
    } else {
        reasoning.trim().to_string()
    };
    let legacy_speech = text(&get("speech"));
    let legacy_action = text(&get("action"));
    let mut response = text(&get("response"));
    if response.is_empty() {
        response = visible_response_from_parts(&legacy_action, &legacy_speech);
    }
    let mut beats = response_beats(&get("beats"));
    if beats.is_empty() {
        if !legacy_action.is_empty() {
            beats.push(beat("action", &legacy_action));
        }
        if !legacy_speech.is_empty() {
            beats.push(beat("speech", &legacy_speech));
        }
    }
    let (action, speech) = derived_action_speech(&beats, &legacy_action, &legacy_speech);
    let mut m = Map::new();
    m.insert("reasoning".to_string(), Value::String(reasoning));
    m.insert("response".to_string(), Value::String(response));
    m.insert("beats".to_string(), Value::Array(beats));
    m.insert("speech".to_string(), Value::String(speech));
    m.insert("action".to_string(), Value::String(action));
    m.insert(
        "claims".to_string(),
        Value::Array(
            claims(&get("claims"))
                .into_iter()
                .map(Value::String)
                .collect(),
        ),
    );
    m
}

fn response_beats(value: &Value) -> Vec<Value> {
    let Value::Array(raw) = value else {
        return Vec::new();
    };
    raw.iter()
        .filter_map(|item| {
            let obj = item.as_object()?;
            let kind = text(obj.get("kind").unwrap_or(&Value::Null)).to_lowercase();
            let text = text(obj.get("text").unwrap_or(&Value::Null));
            if text.is_empty() || !matches!(kind.as_str(), "speech" | "action") {
                return None;
            }
            Some(beat(&kind, &text))
        })
        .collect()
}

fn beat(kind: &str, text: &str) -> Value {
    let mut m = Map::new();
    m.insert("kind".to_string(), Value::String(kind.to_string()));
    m.insert("text".to_string(), Value::String(text.to_string()));
    Value::Object(m)
}

fn derived_action_speech(
    beats: &[Value],
    legacy_action: &str,
    legacy_speech: &str,
) -> (String, String) {
    let mut actions = Vec::new();
    let mut speeches = Vec::new();
    for beat in beats {
        let Some(obj) = beat.as_object() else {
            continue;
        };
        let kind = obj.get("kind").and_then(Value::as_str).unwrap_or("");
        let text = obj.get("text").and_then(Value::as_str).unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        match kind {
            "action" => actions.push(text.to_string()),
            "speech" => speeches.push(text.to_string()),
            _ => {}
        }
    }
    let action = if actions.is_empty() {
        legacy_action.to_string()
    } else {
        actions.join(" ")
    };
    let speech = if speeches.is_empty() {
        legacy_speech.to_string()
    } else {
        speeches.join("\n")
    };
    (action, speech)
}

fn visible_response_from_parts(action: &str, speech: &str) -> String {
    match (action.is_empty(), speech.is_empty()) {
        (true, true) => String::new(),
        (false, true) => action.to_string(),
        (true, false) => speech.to_string(),
        (false, false) => format!("{action}\n{speech}"),
    }
}
