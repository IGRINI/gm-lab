//! Pure helper functions ported from the top of `orchestrator.py`:
//! tool-result builders, payload compaction, model-facing text renderers,
//! visibility/scope coercion, and small text/json utilities.
//!
//! Every function here is a faithful port; the Python origin is named per item.

use serde_json::{Map, Value};
use std::collections::BTreeSet;

use gml_prompts::{render_prompt, PromptId};
use gml_types::ToolExecutionResult;
use gml_world::World;

// =========================================================================
// JSON compaction + system reminder
// =========================================================================

/// `_json_compact(data)` — `json.dumps(data, ensure_ascii=False, separators=(",",":"))`.
/// serde_json's default (with `preserve_order`) is exactly compact + raw UTF-8.
pub fn json_compact(data: &Value) -> String {
    serde_json::to_string(data).expect("json_compact serialize")
}

pub const SYSTEM_REMINDER_OPEN: &str = "<system-reminder>";
pub const SYSTEM_REMINDER_CLOSE: &str = "</system-reminder>";

/// `_system_reminder(text)` — collapse whitespace, strip, wrap in tags. Empty -> "".
pub fn system_reminder(text: &str) -> String {
    let collapsed = collapse_ws(text);
    if collapsed.is_empty() {
        return String::new();
    }
    format!("{SYSTEM_REMINDER_OPEN}\n{collapsed}\n{SYSTEM_REMINDER_CLOSE}")
}

/// `re.sub(r"\s+", " ", text).strip()`.
pub fn collapse_ws(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_ws = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(ch);
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

/// `_VISIBLE_CONTINUATION_REMINDER`.
pub const VISIBLE_CONTINUATION_REMINDER: &str = gml_prompts::VISIBLE_CONTINUATION_REMINDER;

/// `_VISIBLE_CONTINUATION_REMINDER`.
pub fn visible_continuation_reminder() -> &'static str {
    VISIBLE_CONTINUATION_REMINDER
}

/// The static `_TOOL_REMINDERS` map. Returns the reminder for a tool name (or "").
pub fn tool_reminder(name: &str) -> &'static str {
    gml_prompts::tool_reminder(name)
}

// =========================================================================
// ToolExecutionResult helpers
// =========================================================================

/// `_tool_result(full, model=None, reminder=None, terminal=False)`.
pub fn tool_result(
    full: &str,
    model: Option<&str>,
    reminder: Option<&str>,
    terminal: bool,
) -> ToolExecutionResult {
    let full = full.to_string();
    let mut model_text = model.unwrap_or(&full).to_string();
    let reminder_text = system_reminder(reminder.unwrap_or(""));
    if !reminder_text.is_empty() {
        model_text = join_nonempty_nn(&[model_text.trim_end(), &reminder_text]);
    }
    ToolExecutionResult::with_terminal(full, model_text, terminal)
}

/// `_with_model_reminder(result, reminder)` — append a `<system-reminder>` to the
/// model channel (the full channel is unchanged).
pub fn with_model_reminder(result: ToolExecutionResult, reminder: &str) -> ToolExecutionResult {
    let reminder_text = system_reminder(reminder);
    if reminder_text.is_empty() {
        return result;
    }
    let model_text = join_nonempty_nn(&[result.model.trim_end(), &reminder_text]);
    ToolExecutionResult::with_terminal(result.full, model_text, result.terminal)
}

/// `_tool_error(tool, message, full=None, code="", **fields)`.
pub fn tool_error(
    tool: &str,
    message: &str,
    full: Option<&str>,
    code: &str,
    extra: &[(&str, Value)],
) -> ToolExecutionResult {
    let mut payload = Map::new();
    payload.insert("ok".to_string(), Value::Bool(false));
    payload.insert("status".to_string(), Value::String("error".to_string()));
    payload.insert("tool".to_string(), Value::String(tool.to_string()));
    payload.insert("error".to_string(), Value::String(message.to_string()));
    if !code.is_empty() {
        payload.insert("code".to_string(), Value::String(code.to_string()));
    }
    for (key, value) in extra {
        if !is_empty_value(value) {
            payload.insert((*key).to_string(), value.clone());
        }
    }
    let full_text = full
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("(tool error: {message})"));
    tool_result(&full_text, Some(&model_error_text(&payload)), None, false)
}

/// `"\n\n".join(part for part in parts if part)`.
fn join_nonempty_nn(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n\n")
}

// =========================================================================
// Scalar / kv text rendering
// =========================================================================

/// `_clean_text(value)`.
pub fn clean_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.trim().to_string(),
        other => other.to_string().trim().to_string(),
    }
}

/// `_clean_list(value)` — strings only, stripped, non-empty.
pub fn clean_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(a) => a.iter().map(clean_text).filter(|s| !s.is_empty()).collect(),
        _ => Vec::new(),
    }
}

/// `_is_empty_value(value)` — None | "" | [] | {}.
pub fn is_empty_value(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

/// `_scalar_text(value)`.
pub fn scalar_text(value: &Value) -> String {
    match value {
        Value::Bool(b) => {
            if *b {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        Value::Null => String::new(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.trim().to_string(),
        Value::Array(a) => a
            .iter()
            .map(scalar_text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(o) => o
            .iter()
            .filter_map(|(k, child)| {
                let ct = scalar_text(child);
                if ct.is_empty() {
                    None
                } else {
                    Some(format!("{k} {ct}"))
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
    }
}

/// `_kv(label, value)` — `"{label}: {text}"` or "" when text empty.
pub fn kv(label: &str, value: &Value) -> String {
    let text = scalar_text(value);
    if text.is_empty() {
        String::new()
    } else {
        format!("{label}: {text}")
    }
}

/// `_kv` for a string value.
pub fn kv_str(label: &str, value: &str) -> String {
    kv(label, &Value::String(value.to_string()))
}

/// `_margin_text(value)` — signed int, fallback to scalar_text.
pub fn margin_text(value: &Value) -> String {
    if value.is_null() {
        return String::new();
    }
    if let Some(i) = value.as_i64() {
        return format!("{i:+}");
    }
    if let Some(f) = value.as_f64() {
        return format!("{:+}", f as i64);
    }
    scalar_text(value)
}

/// `_plain_lines(title, *lines)` — title + non-empty lines, joined by '\n'.
pub fn plain_lines(title: &str, lines: &[String]) -> String {
    let mut out = vec![title.to_string()];
    out.extend(lines.iter().filter(|l| !l.is_empty()).cloned());
    out.join("\n")
}

/// `_row_summary(row, keys)` — `"k=v"` for non-empty scalar values, space-joined.
pub fn row_summary(row: &Value, keys: &[&str]) -> String {
    let obj = match row {
        Value::Object(m) => m,
        _ => return String::new(),
    };
    let mut parts = Vec::new();
    for key in keys {
        if let Some(v) = obj.get(*key) {
            let text = scalar_text(v);
            if !text.is_empty() {
                parts.push(format!("{key}={text}"));
            }
        }
    }
    parts.join(" ")
}

/// `_clip_text(value, limit=700)` — clip to `limit` CHARS, rstrip + "..." suffix.
pub fn clip_text(value: &Value, limit: usize) -> String {
    let text = clean_text(value);
    if text.chars().count() <= limit {
        return text;
    }
    let clipped: String = text.chars().take(limit).collect();
    format!("{}...", clipped.trim_end())
}

/// `_model_error_text(payload)`.
pub fn model_error_text(payload: &Map<String, Value>) -> String {
    let mut lines = vec![
        kv("tool", payload.get("tool").unwrap_or(&Value::Null)),
        kv("code", payload.get("code").unwrap_or(&Value::Null)),
        kv("message", payload.get("error").unwrap_or(&Value::Null)),
    ];
    for key in ["npc_id", "npc_label", "whereabouts"] {
        let line = kv(key, payload.get(key).unwrap_or(&Value::Null));
        if !line.is_empty() {
            lines.push(line);
        }
    }
    plain_lines("ERROR", &lines)
}

// =========================================================================
// drop_empty / value normalization
// =========================================================================

/// `_drop_empty(value)` — recursively drop None/""/[]/{} entries.
pub fn drop_empty(value: &Value) -> Value {
    match value {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, child) in m {
                let clean = drop_empty(child);
                if !is_empty_value(&clean) {
                    out.insert(k.clone(), clean);
                }
            }
            Value::Object(out)
        }
        Value::Array(a) => {
            let mut out = Vec::new();
            for child in a {
                let clean = drop_empty(child);
                if !is_empty_value(&clean) {
                    out.push(clean);
                }
            }
            Value::Array(out)
        }
        other => other.clone(),
    }
}

// =========================================================================
// Compact payload helpers (for model-facing tool text)
// =========================================================================

/// `_compact_sources(sources, limit=3)`.
pub fn compact_sources(sources: &Value, limit: usize) -> Vec<Value> {
    let mut out = Vec::new();
    if let Value::Array(a) = sources {
        for source in a.iter().take(limit) {
            let m = match source {
                Value::Object(m) => m,
                _ => continue,
            };
            let mut row = Map::new();
            for key in ["n", "kind", "status", "source"] {
                if let Some(v) = m.get(key) {
                    row.insert(key.to_string(), v.clone());
                }
            }
            if !row.is_empty() {
                out.push(Value::Object(row));
            }
        }
    }
    out
}

fn get<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

/// `_compact_world_fact_payload(payload)`.
pub fn compact_world_fact_payload(payload: &Value) -> Value {
    let mut out = Map::new();
    out.insert(
        "status".to_string(),
        match payload.get("status") {
            Some(v) if !v.is_null() => v.clone(),
            _ => Value::String("unknown".to_string()),
        },
    );
    out.insert(
        "text".to_string(),
        match payload.get("text") {
            Some(v) if !v.is_null() => v.clone(),
            _ => Value::String(String::new()),
        },
    );
    let sources = compact_sources(get(payload, "sources"), 3);
    if !sources.is_empty() {
        out.insert("sources".to_string(), Value::Array(sources));
    }
    if let Some(ad) = payload.get("already_delivered") {
        if !ad.is_null() && ad != &Value::Bool(false) && ad != &Value::from(0) {
            out.insert("already_delivered".to_string(), ad.clone());
        }
    }
    Value::Object(out)
}

/// `_compact_tool_search_payload`.
pub fn compact_tool_search_payload(payload: &Value) -> Value {
    let matches = get(payload, "matches");
    let missing = get(payload, "missing");
    let mut out = Map::new();
    let matches_arr = match matches {
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    };
    let missing_arr = match missing {
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    };
    out.insert("matches".to_string(), Value::Array(matches_arr.clone()));
    out.insert("missing".to_string(), Value::Array(missing_arr));
    if matches_arr.is_empty() {
        if let Some(msg) = payload.get("message") {
            if !msg.is_null() {
                out.insert("message".to_string(), msg.clone());
            }
        }
    }
    Value::Object(out)
}

/// `_compact_roll_payload`.
pub fn compact_roll_payload(payload: &Value) -> Value {
    let mut m = Map::new();
    for key in [
        "ok",
        "notation",
        "roll_kind",
        "target_kind",
        "target_number",
        "total",
        "grade",
        "margin",
        "natural",
    ] {
        m.insert(key.to_string(), get(payload, key).clone());
    }
    drop_empty(&Value::Object(m))
}

// =========================================================================
// Model-facing renderers
// =========================================================================

/// `_model_player_options_text(payload)`.
pub fn model_player_options_text(payload: &Value) -> String {
    let count = match payload.get("options") {
        Some(Value::Array(a)) => a.len(),
        _ => 0,
    };
    let next = render_prompt(PromptId::OrchestratorPlayerOptionsNext, ())
        .expect("embedded player-options next-step prompt must render");
    plain_lines(
        "PLAYER OPTIONS",
        &[
            kv_str("status", "buttons shown to player"),
            kv("shown", &Value::from(count)),
            next,
        ],
    )
}

/// `_model_tool_search_text(payload)`.
pub fn model_tool_search_text(payload: &Value) -> String {
    let matches = match get(payload, "matches") {
        Value::Array(a) if !a.is_empty() => Value::Array(
            a.iter()
                .filter_map(|row| {
                    row.get("name")
                        .and_then(Value::as_str)
                        .map(|name| Value::String(name.to_string()))
                })
                .collect(),
        ),
        _ => Value::String("none".to_string()),
    };
    let missing = match get(payload, "missing") {
        Value::Array(a) if !a.is_empty() => Value::Array(a.clone()),
        _ => Value::String("none".to_string()),
    };
    let next = get(payload, "next")
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            render_prompt(PromptId::OrchestratorToolSearchDefaultNext, ())
                .expect("embedded tool-search default next-step prompt must render")
        });
    plain_lines(
        "TOOL SEARCH",
        &[
            kv("matches", &matches),
            kv("missing", &missing),
            kv_str("next", &next),
        ],
    )
}

pub fn model_load_tool_schema_text(payload: &Value) -> String {
    let loaded_schema = get(payload, "loaded_schema");
    let already_loaded = match get(payload, "already_loaded") {
        Value::Array(a) if !a.is_empty() => Value::Array(a.clone()),
        _ => Value::String("none".to_string()),
    };
    let missing = match get(payload, "missing") {
        Value::Array(a) if !a.is_empty() => Value::Array(a.clone()),
        _ => Value::String("none".to_string()),
    };
    let next = get(payload, "next")
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            render_prompt(PromptId::OrchestratorLoadToolSchemaDefaultNext, ())
                .expect("embedded load-tool-schema default next-step prompt must render")
        });
    let invoke_tool = get(payload, "invoke_tool")
        .as_str()
        .unwrap_or("invoke_loaded_tool");
    let schema = get(payload, "schema");
    let schema_line = if schema.is_null() {
        String::new()
    } else {
        format!("schema: {}", json_compact(schema))
    };
    plain_lines(
        "LOAD TOOL SCHEMA",
        &[
            kv("status", get(payload, "status")),
            kv("loaded_schema", loaded_schema),
            kv_str("invoke_tool", invoke_tool),
            kv("already_loaded", &already_loaded),
            kv("missing", &missing),
            schema_line,
            kv_str("next", &next),
        ],
    )
}

/// `_model_roll_text(payload)`.
pub fn model_roll_text(payload: &Value) -> String {
    let compact = compact_roll_payload(payload);
    let mut parts = Vec::new();
    if let Some(total) = compact.get("total") {
        parts.push(format!("total {}", scalar_text(total)));
    }
    if let Some(grade) = compact.get("grade") {
        let g = scalar_text(grade);
        if !g.is_empty() {
            parts.push(g);
        }
    }
    if let Some(margin) = compact.get("margin") {
        parts.push(format!("margin {}", margin_text(margin)));
    }
    if let Some(natural) = compact.get("natural") {
        parts.push(format!("natural {}", scalar_text(natural)));
    }
    format!("RESULT: {}.", parts.join(", "))
}

/// `_model_world_fact_text(payload)`.
pub fn model_world_fact_text(payload: &Value) -> String {
    let compact = compact_world_fact_payload(payload);
    let mut lines = vec![
        kv("status", get(&compact, "status")),
        kv_str("text", &clip_text(get(&compact, "text"), 700)),
        kv("already_delivered", get(&compact, "already_delivered")),
    ];
    let mut sources = Vec::new();
    if let Value::Array(a) = get(&compact, "sources") {
        for source in a {
            let summary = row_summary(source, &["n", "kind", "status", "source"]);
            if !summary.is_empty() {
                sources.push(format!("- {summary}"));
            }
        }
    }
    if !sources.is_empty() {
        lines.push("sources:".to_string());
        lines.extend(sources);
    }
    plain_lines("WORLD FACT", &lines)
}

/// `_visibility(value, default)`.
pub fn visibility(value: &Value, default: &str) -> String {
    let mut raw = clean_text(value).to_lowercase().replace('-', "_");
    raw = match raw.as_str() {
        "public" | "player_safe" => "player".to_string(),
        "player_private" | "private_player" | "participants" | "participant" => {
            "shared".to_string()
        }
        "truth" | "gm_truth" => "gm".to_string(),
        "private" | "npc_private" => "npc".to_string(),
        _ => raw,
    };
    if matches!(raw.as_str(), "player" | "gm" | "npc" | "shared") {
        raw
    } else {
        default.to_string()
    }
}

// =========================================================================
// NPC tool payload player-facing relabel
// =========================================================================

/// `_player_facing_payload(world, payload)` — relabel `name` to the player label.
pub fn player_facing_payload(world: &World, payload: &Value) -> Value {
    let obj = match payload {
        Value::Object(m) => m,
        _ => return payload.clone(),
    };
    let npc_id = match obj.get("npc_id").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => id,
        _ => return payload.clone(),
    };
    let label = world.npc_player_label(npc_id, "player");
    let current_name = obj.get("name").and_then(Value::as_str).unwrap_or("");
    if label.is_empty() || label == current_name {
        return payload.clone();
    }
    let mut out = obj.clone();
    out.insert("name".to_string(), Value::String(label));
    Value::Object(out)
}

// =========================================================================
// hashing
// =========================================================================

/// `_short_hash(value)` — sha1 hexdigest, first 16 chars; "" for empty input.
pub fn short_hash(value: &str) -> String {
    use sha1::{Digest, Sha1};
    let text = value.trim();
    if text.is_empty() {
        return String::new();
    }
    let mut hasher = Sha1::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex.chars().take(16).collect()
}

/// Convenience: dedup a list of strings preserving order.
pub fn dedup_preserve(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    for item in items {
        if !item.is_empty() && !seen.contains(&item) {
            seen.insert(item.clone());
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn model_routing_guidance_keeps_legacy_text() {
        assert_eq!(
            model_player_options_text(&json!({ "options": [{}, {}] })),
            "PLAYER OPTIONS\nstatus: buttons shown to player\nshown: 2\nnext: write the final player-facing narration now, then stop; do not call ask_player again."
        );
        assert_eq!(
            model_tool_search_text(&json!({})),
            "TOOL SEARCH\nmatches: none\nmissing: none\nnext: call load_tool_schema with one exact match.name"
        );
        assert_eq!(
            model_load_tool_schema_text(&json!({})),
            "LOAD TOOL SCHEMA\ninvoke_tool: invoke_loaded_tool\nalready_loaded: none\nmissing: none\nnext: call invoke_loaded_tool with the loaded schema"
        );
    }
}
