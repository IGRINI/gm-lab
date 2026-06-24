//! Coercion helpers — faithful ports of the module-level `_*` functions in
//! world.py (`_safe_id`, `_as_list`, `_as_dict`, `_as_str`, `_as_joined_str`,
//! `_as_int_or_none`, `_match_words`, `_actor_key`, `_anchor_label`).
//!
//! Inputs come from loosely-typed JSON (seed/tool args), so most take a
//! `serde_json::Value`.

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeSet;

static SAFE_ID_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^a-zA-Z0-9_]+").unwrap());
static SAFE_ID_TRIM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^_+|_+$").unwrap());
static INT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-?\d+").unwrap());
// _match_words tokenizer: [a-zа-яё0-9]+ (unicode-aware via regex crate).
static WORD_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[a-zа-яё0-9]+").unwrap());

/// `_safe_id(raw, fallback)` — lowercase, replace `[^a-zA-Z0-9_]+` with `_`,
/// strip leading/trailing `_`, fall back when empty.
pub fn safe_id(raw: &str, fallback: &str) -> String {
    let lowered = raw.trim().to_lowercase();
    let replaced = SAFE_ID_RE.replace_all(&lowered, "_");
    let trimmed = SAFE_ID_TRIM_RE.replace_all(&replaced, "");
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.into_owned()
    }
}

/// `_as_str(value)` — `""` for None, else `str(value).strip()`.
///
/// Python's `str()` of a JSON-derived value: strings as-is, numbers/bools via
/// Python repr. world.py only ever feeds strings/None through `_as_str` in
/// practice, but we mirror the broad behaviour for ints/floats/bools.
pub fn as_str(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        Value::Bool(b) => {
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

/// `_as_str` for an already-owned Rust string-ish, trimming.
pub fn str_strip(s: &str) -> String {
    s.trim().to_string()
}

/// `_as_list(value)` — None -> []; list -> itself; tuple -> list; else [value].
pub fn as_list(value: &Value) -> Vec<Value> {
    match value {
        Value::Null => Vec::new(),
        Value::Array(a) => a.clone(),
        other => vec![other.clone()],
    }
}

/// `_as_dict(value)` — dict(value) if dict, else {}.
pub fn as_dict(value: &Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    }
}

/// `_as_joined_str(value)` — for a list, join non-empty `_as_str`s with ", ";
/// otherwise `_as_str`.
pub fn as_joined_str(value: &Value) -> String {
    match value {
        Value::Array(items) => items
            .iter()
            .map(as_str)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        other => as_str(other),
    }
}

/// `_as_int_or_none(value)` — int/integral-float/first-int-in-string; bools
/// and everything else -> None.
pub fn as_int_or_none(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(_) => None,
        Value::Null => None,
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 {
                    Some(f as i64)
                } else {
                    None
                }
            } else {
                None
            }
        }
        Value::String(s) => INT_RE.find(s).and_then(|m| m.as_str().parse::<i64>().ok()),
        _ => None,
    }
}

/// `_match_words(text)` — lowercase + tokenize via `[a-zа-яё0-9]+`.
pub fn match_words(text: &str) -> BTreeSet<String> {
    let lowered = text.to_lowercase();
    WORD_RE
        .find_iter(&lowered)
        .map(|m| m.as_str().to_string())
        .collect()
}

/// `_actor_key(value)` — `_as_str(value).lower()`.
pub fn actor_key(value: &str) -> String {
    value.trim().to_lowercase()
}

/// `_anchor_label(name, identifier)`.
pub fn anchor_label(name: &str, identifier: &str) -> String {
    let name = name.trim();
    let identifier = identifier.trim();
    if !name.is_empty() && !identifier.is_empty() && name != identifier {
        format!("{name} ({identifier})")
    } else if !name.is_empty() {
        name.to_string()
    } else {
        identifier.to_string()
    }
}

/// Convenience: pull a string field from a JSON object, returning `_as_str`.
pub fn get_str(obj: &serde_json::Map<String, Value>, key: &str) -> String {
    obj.get(key).map(as_str).unwrap_or_default()
}
