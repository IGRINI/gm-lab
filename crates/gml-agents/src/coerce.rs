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
            if *b { "True".to_string() } else { "False".to_string() }
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
        Value::Array(a) => a
            .iter()
            .map(text)
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

/// `_norm_npc(out)` — normalize the NPC sub-agent JSON into the canonical
/// `{reasoning, speech, action, claims}` shape (field order preserved).
pub fn norm_npc(out: &Value) -> Map<String, Value> {
    let obj = out.as_object();
    let get = |k: &str| -> Value {
        obj.and_then(|m| m.get(k)).cloned().unwrap_or(Value::Null)
    };
    let mut m = Map::new();
    m.insert("reasoning".to_string(), Value::String(text(&get("reasoning"))));
    m.insert("speech".to_string(), Value::String(text(&get("speech"))));
    m.insert("action".to_string(), Value::String(text(&get("action"))));
    m.insert(
        "claims".to_string(),
        Value::Array(claims(&get("claims")).into_iter().map(Value::String).collect()),
    );
    m
}
