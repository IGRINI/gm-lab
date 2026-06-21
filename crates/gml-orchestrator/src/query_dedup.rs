//! World-fact delivery de-duplication ported from `orchestrator.py`
//! (`_filter_new_fact_payload`, `_fact_source_identity`, `_fact_text_segments`).

use serde_json::{Map, Value};
use std::collections::BTreeSet;

use crate::helpers::{clean_text, short_hash};
use crate::session::Session;

/// `_fact_source_identity(source)`.
fn fact_source_identity(source: &Value) -> String {
    let obj = match source {
        Value::Object(m) => m,
        _ => return "unknown_source".to_string(),
    };
    for key in ["doc_id", "source"] {
        let value = clean_text(obj.get(key).unwrap_or(&Value::Null));
        if !value.is_empty() {
            return value.to_lowercase();
        }
    }
    let metadata = match obj.get("metadata") {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    for key in ["record_id", "fact_id", "npc_id", "scene_id", "item_id", "seq"] {
        let value = clean_text(metadata.get(key).unwrap_or(&Value::Null));
        if !value.is_empty() {
            return format!("{key}:{}", value.to_lowercase());
        }
    }
    let kind = clean_text(obj.get("kind").unwrap_or(&Value::Null)).to_lowercase();
    if kind.is_empty() {
        "unknown_source".to_string()
    } else {
        kind
    }
}

/// `_fact_text_segments(text)` — parse `[n] known|unconfirmed: ...` segments.
fn fact_text_segments(text: &str) -> std::collections::BTreeMap<i64, String> {
    let mut segments = std::collections::BTreeMap::new();
    let cleaned = clean_text(&Value::String(text.to_string()));
    let re = regex::Regex::new(
        r"(?s)(\[(\d+)\]\s+(?:known|unconfirmed):\s.*?)(?=\s+\[\d+\]\s+(?:known|unconfirmed):|$)",
    )
    .unwrap();
    for cap in re.captures_iter(&cleaned) {
        let index: i64 = match cap.get(2).and_then(|m| m.as_str().parse().ok()) {
            Some(i) => i,
            None => continue,
        };
        let seg = cap.get(1).map(|m| m.as_str().trim().to_string()).unwrap_or_default();
        segments.insert(index, seg);
    }
    segments
}

/// `_already_delivered_fact_text()`.
fn already_delivered_fact_text() -> String {
    "No new matching world-fact sources. Previous matching sources were already \
delivered in the active conversation context; after GM history compaction \
this delivery cache resets."
        .to_string()
}

/// `_filter_new_fact_payload(session, scope_key, payload, query)` -> (payload, delivered).
pub fn filter_new_fact_payload(
    session: &mut Session,
    scope_key: &str,
    payload: Value,
    query: &str,
) -> (Value, i64) {
    let text = clean_text(payload.get("text").unwrap_or(&Value::Null));
    let sources: Vec<Value> = match payload.get("sources") {
        Some(Value::Array(a)) => a.iter().filter(|s| s.is_object()).cloned().collect(),
        _ => Vec::new(),
    };
    if text.is_empty() && sources.is_empty() {
        return (payload, 0);
    }

    if !sources.is_empty() {
        let segments = fact_text_segments(&text);
        let mut fresh_sources: Vec<Value> = Vec::new();
        let mut fresh_segments: Vec<String> = Vec::new();
        let mut selected_keys: BTreeSet<String> = BTreeSet::new();
        let mut delivered = 0;
        {
            let seen = session.query_seen_set(scope_key);
            for source in &sources {
                let number = source.get("n").and_then(|v| v.as_i64()).unwrap_or(0);
                let segment = segments.get(&number).cloned().unwrap_or_default();
                let key_text = if segment.is_empty() { text.clone() } else { segment.clone() };
                let key = format!(
                    "fact_source:{}:{}",
                    fact_source_identity(source),
                    short_hash(&key_text)
                );
                if seen.contains(&key) || selected_keys.contains(&key) {
                    delivered += 1;
                    continue;
                }
                fresh_sources.push(source.clone());
                if !segment.is_empty() {
                    fresh_segments.push(segment);
                }
                selected_keys.insert(key);
            }
        }
        {
            let seen = session.query_seen_set(scope_key);
            for k in &selected_keys {
                seen.insert(k.clone());
            }
        }
        if !fresh_sources.is_empty() {
            let mut out = match payload {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            out.insert("sources".to_string(), Value::Array(fresh_sources));
            if !fresh_segments.is_empty() {
                out.insert("text".to_string(), Value::String(fresh_segments.join(" ")));
            }
            if delivered > 0 {
                out.insert("already_delivered".to_string(), Value::from(delivered));
            }
            return (Value::Object(out), delivered);
        }
        let mut out = match payload {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let total = if delivered > 0 { delivered } else { sources.len() as i64 };
        out.insert("status".to_string(), Value::String("already_delivered".to_string()));
        out.insert("text".to_string(), Value::String(already_delivered_fact_text()));
        out.insert("sources".to_string(), Value::Array(Vec::new()));
        out.insert("already_delivered".to_string(), Value::from(total));
        return (Value::Object(out), total);
    }

    let status = clean_text(payload.get("status").unwrap_or(&Value::Null));
    let query_key = if status == "unknown" {
        short_hash(query)
    } else {
        String::new()
    };
    let status_label = if status.is_empty() { "unknown".to_string() } else { status };
    let key = format!(
        "fact_payload:{status_label}:{query_key}:{}",
        short_hash(&text)
    );
    {
        let seen = session.query_seen_set(scope_key);
        if seen.contains(&key) {
            let mut out = match payload {
                Value::Object(m) => m,
                _ => Map::new(),
            };
            out.insert("status".to_string(), Value::String("already_delivered".to_string()));
            out.insert("text".to_string(), Value::String(already_delivered_fact_text()));
            out.insert("sources".to_string(), Value::Array(Vec::new()));
            out.insert("already_delivered".to_string(), Value::from(1));
            return (Value::Object(out), 1);
        }
        seen.insert(key);
    }
    (payload, 0)
}
