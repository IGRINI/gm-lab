//! Remaining compact-payload + model-facing renderers ported from
//! `orchestrator.py` (world_state_update, world_query, npc_profile, time,
//! player_character, whereabouts, presence, scene, ask_npc) plus the
//! `ask_player` player-options builder and tool-arg normalization.

use serde_json::{Map, Value};

use gml_world::World;

use crate::helpers::{
    clean_text, clip_text, compact_sources, drop_empty, is_empty_value, kv, kv_str, plain_lines,
    row_summary, scalar_text,
};

fn get<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

fn obj_get<'a>(m: &'a Map<String, Value>, key: &str) -> &'a Value {
    m.get(key).unwrap_or(&Value::Null)
}

// =========================================================================
// _compact_whereabouts / presence / scene
// =========================================================================

pub fn compact_whereabouts_payload(payload: &Value) -> Value {
    let mut out = Map::new();
    if let Value::Object(m) = payload {
        for key in ["npc_id", "name", "present", "current_scene", "whereabouts"] {
            if let Some(v) = m.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

pub fn compact_presence_payload(payload: &Value) -> Value {
    let mut out = Map::new();
    if let Value::Object(m) = payload {
        for key in ["npc_id", "name", "present", "scene", "whereabouts"] {
            if let Some(v) = m.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

fn compact_scene_item(item: &Value) -> Value {
    let mut out = Map::new();
    if let Value::Object(m) = item {
        for key in ["item_id", "name", "visible", "portable"] {
            if let Some(v) = m.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

fn compact_scene_exit(exit_: &Value) -> Value {
    let mut out = Map::new();
    if let Value::Object(m) = exit_ {
        for key in ["exit_id", "name", "destination", "visible", "blocked_by"] {
            if let Some(v) = m.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    Value::Object(out)
}

pub fn compact_scene_payload(payload: &Value) -> Value {
    let mut out = Map::new();
    if let Value::Object(m) = payload {
        for key in [
            "scene_id",
            "location_id",
            "title",
            "present_npcs",
            "constraints",
            "tension",
            "dropped_present_npcs",
            "repair_hint",
        ] {
            if let Some(v) = m.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    let items: Vec<Value> = match get(payload, "items") {
        Value::Array(a) => a
            .iter()
            .filter(|i| i.is_object())
            .map(compact_scene_item)
            .collect(),
        _ => Vec::new(),
    };
    let exits: Vec<Value> = match get(payload, "exits") {
        Value::Array(a) => a
            .iter()
            .filter(|e| e.is_object())
            .map(compact_scene_exit)
            .collect(),
        _ => Vec::new(),
    };
    if !items.is_empty() {
        out.insert("items".to_string(), Value::Array(items));
    }
    if !exits.is_empty() {
        out.insert("exits".to_string(), Value::Array(exits));
    }
    Value::Object(out)
}

// =========================================================================
// _compact_ask_npc
// =========================================================================

pub fn compact_ask_npc_payload(payload: &Value) -> Value {
    let mut out = Map::new();
    if let Value::Object(m) = payload {
        for key in ["npc_id", "npc_label", "speech_ru", "action_ru"] {
            if let Some(v) = m.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    out.insert("already_emitted".to_string(), Value::Bool(true));
    out.insert(
        "final_narration_rule".to_string(),
        Value::String(
            "Do not rewrite, retell, paraphrase, embellish, or mention this NPC's \
speech/action/body/emotion again. Final narration may add only non-NPC \
scene consequences; output empty if none. For another named NPC reaction, \
call ask_npc for that NPC."
                .to_string(),
        ),
    );
    Value::Object(out)
}

// =========================================================================
// _compact_world_state_update
// =========================================================================

pub fn compact_world_state_update_payload(payload: &Value) -> Value {
    let mut applied = Vec::new();
    if let Value::Array(rows) = get(payload, "applied") {
        for row in rows {
            let m = match row {
                Value::Object(m) => m,
                _ => continue,
            };
            let mut compact = Map::new();
            for (source_key, target_key) in [
                ("index", "i"),
                ("op", "op"),
                ("type", "type"),
                ("id", "id"),
                ("npc_id", "npc_id"),
                ("target", "target"),
                ("entity_id", "entity_id"),
                ("source_npc", "source_npc"),
                ("participants", "participants"),
                ("known_name", "known_name"),
                ("location_id", "location_id"),
                ("location_name", "location_name"),
                ("region_id", "region_id"),
                ("region_name", "region_name"),
                ("scene_id", "scene_id"),
                ("importance", "importance"),
                ("aliases", "aliases"),
                ("scope", "scope"),
                ("mode", "mode"),
                ("hash", "hash"),
                ("status", "status"),
            ] {
                if let Some(v) = m.get(source_key) {
                    compact.insert(target_key.to_string(), v.clone());
                }
            }
            applied.push(drop_empty(&Value::Object(compact)));
        }
    }
    let mut errors = Vec::new();
    if let Value::Array(rows) = get(payload, "errors") {
        for row in rows {
            let m = match row {
                Value::Object(m) => m,
                _ => continue,
            };
            let mut e = Map::new();
            // Map index->i, keep the rest.
            for (source_key, target_key) in [
                ("index", "i"),
                ("op", "op"),
                ("type", "type"),
                ("id", "id"),
                ("npc_id", "npc_id"),
                ("target", "target"),
                ("entity_id", "entity_id"),
                ("source_npc", "source_npc"),
                ("participants", "participants"),
                ("known_name", "known_name"),
                ("location_id", "location_id"),
                ("location_name", "location_name"),
                ("region_id", "region_id"),
                ("region_name", "region_name"),
                ("scene_id", "scene_id"),
                ("importance", "importance"),
                ("aliases", "aliases"),
                ("scope", "scope"),
                ("existing_id", "existing_id"),
                ("existing_hash", "existing_hash"),
                ("expected_hash", "expected_hash"),
                ("actual_hash", "actual_hash"),
                ("status", "status"),
                ("error", "error"),
            ] {
                e.insert(target_key.to_string(), obj_get(m, source_key).clone());
            }
            errors.push(drop_empty(&Value::Object(e)));
        }
    }
    let mut out = Map::new();
    out.insert(
        "ok".to_string(),
        Value::Bool(get(payload, "ok").as_bool().unwrap_or(false)),
    );
    out.insert("applied".to_string(), Value::Array(applied));
    out.insert("errors".to_string(), Value::Array(errors));
    drop_empty(&Value::Object(out))
}

pub fn model_world_state_update_text(payload: &Value) -> String {
    let compact = compact_world_state_update_payload(payload);
    let mut lines = vec![kv("ok", get(&compact, "ok"))];
    if let Value::Array(applied) = get(&compact, "applied") {
        if !applied.is_empty() {
            lines.push("applied:".to_string());
            for row in applied {
                let summary = row_summary(
                    row,
                    &[
                        "i",
                        "status",
                        "op",
                        "type",
                        "id",
                        "hash",
                        "scope",
                        "npc_id",
                        "target",
                        "participants",
                        "entity_id",
                        "source_npc",
                        "known_name",
                        "location_id",
                        "region_id",
                        "scene_id",
                    ],
                );
                if !summary.is_empty() {
                    lines.push(format!("- {summary}"));
                }
            }
        }
    }
    if let Value::Array(errors) = get(&compact, "errors") {
        if !errors.is_empty() {
            lines.push("not stored:".to_string());
            for row in errors {
                let summary = row_summary(
                    row,
                    &[
                        "i",
                        "status",
                        "op",
                        "type",
                        "id",
                        "existing_id",
                        "existing_hash",
                        "expected_hash",
                        "actual_hash",
                        "scope",
                        "npc_id",
                        "target",
                        "participants",
                        "entity_id",
                        "error",
                    ],
                );
                if !summary.is_empty() {
                    lines.push(format!("- {summary}"));
                }
            }
        }
    }
    plain_lines("WORLD STATE WRITE", &lines)
}

// =========================================================================
// _compact_world_query
// =========================================================================

pub fn compact_world_query_payload(payload: &Value) -> Value {
    let mut rows = Vec::new();
    if let Value::Array(results) = get(payload, "results") {
        for row in results {
            let m = match row {
                Value::Object(m) => m,
                _ => continue,
            };
            let mut r = Map::new();
            for key in [
                "kind",
                "id",
                "npc_id",
                "target",
                "entity_id",
                "source_npc",
                "participants",
                "known_name",
                "location_id",
                "location_name",
                "region_id",
                "region_name",
                "scene_id",
                "importance",
                "aliases",
                "memory_id",
                "tier",
                "owner_scope",
                "truth_status",
                "injection_state",
                "visibility_scopes",
                "source_event_ids",
                "source_memory_ids",
                "source_state_record_ids",
                "consumed_by",
            ] {
                r.insert(key.to_string(), obj_get(m, key).clone());
            }
            // scope = scope or visibility
            let scope = if !is_empty_value(obj_get(m, "scope")) {
                obj_get(m, "scope").clone()
            } else {
                obj_get(m, "visibility").clone()
            };
            r.insert("scope".to_string(), scope);
            r.insert("status".to_string(), obj_get(m, "status").clone());
            r.insert("hash".to_string(), obj_get(m, "hash").clone());
            r.insert(
                "text".to_string(),
                Value::String(clip_text(obj_get(m, "text"), 500)),
            );
            rows.push(drop_empty(&Value::Object(r)));
        }
    }
    let mut out = Map::new();
    out.insert("scope".to_string(), get(payload, "scope").clone());
    out.insert("status".to_string(), get(payload, "status").clone());
    out.insert(
        "text".to_string(),
        Value::String(clip_text(get(payload, "text"), 500)),
    );
    out.insert("results".to_string(), Value::Array(rows));
    out.insert(
        "sources".to_string(),
        Value::Array(compact_sources(get(payload, "sources"), 3)),
    );
    out.insert(
        "already_delivered".to_string(),
        get(payload, "already_delivered").clone(),
    );
    out.insert("error".to_string(), get(payload, "error").clone());
    drop_empty(&Value::Object(out))
}

pub fn model_world_query_text(payload: &Value) -> String {
    let compact = compact_world_query_payload(payload);
    let mut lines = vec![
        kv("status", get(&compact, "status")),
        kv_str("text", &clip_text(get(&compact, "text"), 500)),
        kv("already_delivered", get(&compact, "already_delivered")),
    ];
    let results = match get(&compact, "results") {
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    };
    lines.push(kv("found", &Value::from(results.len())));
    if !results.is_empty() {
        lines.push("results:".to_string());
        for row in &results {
            let mut summary = row_summary(
                row,
                &[
                    "kind",
                    "id",
                    "hash",
                    "scope",
                    "npc_id",
                    "target",
                    "entity_id",
                    "source_npc",
                    "participants",
                    "known_name",
                    "location_id",
                    "location_name",
                    "region_id",
                    "scene_id",
                    "importance",
                    "memory_id",
                    "tier",
                    "owner_scope",
                    "truth_status",
                    "injection_state",
                    "source_event_ids",
                    "source_memory_ids",
                    "consumed_by",
                    "status",
                ],
            );
            let text = clip_text(get(row, "text"), 500);
            if !text.is_empty() {
                summary = if summary.is_empty() {
                    format!("text={text}")
                } else {
                    format!("{summary} text={text}")
                };
            }
            if !summary.is_empty() {
                lines.push(format!("- {summary}"));
            }
        }
    }
    plain_lines("WORLD STATE QUERY", &lines)
}

// =========================================================================
// _compact_npc_profile
// =========================================================================

pub fn compact_npc_profile_payload(payload: &Value) -> Value {
    let mut profile = Map::new();
    if let Value::Object(p) = get(payload, "profile") {
        for (key, value) in p {
            match value {
                Value::String(_) => {
                    profile.insert(key.clone(), Value::String(clip_text(value, 500)));
                }
                _ => {
                    profile.insert(key.clone(), value.clone());
                }
            }
        }
    }
    let mut out = Map::new();
    out.insert("status".to_string(), get(payload, "status").clone());
    out.insert("npc_id".to_string(), get(payload, "npc_id").clone());
    out.insert("label".to_string(), get(payload, "label").clone());
    out.insert("preset".to_string(), get(payload, "preset").clone());
    out.insert(
        "card_revision".to_string(),
        get(payload, "card_revision").clone(),
    );
    out.insert("profile".to_string(), Value::Object(profile));
    out.insert(
        "ignored_fields".to_string(),
        get(payload, "ignored_fields").clone(),
    );
    out.insert("error".to_string(), get(payload, "error").clone());
    drop_empty(&Value::Object(out))
}

pub fn model_npc_profile_text(payload: &Value) -> String {
    let compact = compact_npc_profile_payload(payload);
    let mut lines = vec![
        kv("status", get(&compact, "status")),
        kv("npc", get(&compact, "npc_id")),
        kv("label", get(&compact, "label")),
        kv("revision", get(&compact, "card_revision")),
    ];
    if let Value::Object(profile) = get(&compact, "profile") {
        if !profile.is_empty() {
            lines.push("fields:".to_string());
            for (key, value) in profile {
                let value_text = scalar_text(value);
                if !value_text.is_empty() {
                    lines.push(format!("- {key}: {value_text}"));
                }
            }
        }
    }
    if let Value::Array(ignored) = get(&compact, "ignored_fields") {
        if !ignored.is_empty() {
            lines.push(kv("ignored", &Value::Array(ignored.clone())));
        }
    }
    let err = get(&compact, "error");
    if !is_empty_value(err) {
        lines.push(kv("error", err));
    }
    plain_lines("NPC PROFILE", &lines)
}

// =========================================================================
// _compact_time
// =========================================================================

pub fn compact_time_payload(payload: &Value) -> Value {
    let current = match get(payload, "current") {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    let mut cur = Map::new();
    for key in [
        "absolute_minutes",
        "current_date_label",
        "day_number",
        "time_of_day",
    ] {
        cur.insert(
            key.to_string(),
            current.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    let mut out = Map::new();
    out.insert("ok".to_string(), get(payload, "ok").clone());
    out.insert(
        "elapsed_minutes".to_string(),
        get(payload, "elapsed_minutes").clone(),
    );
    out.insert("summary".to_string(), get(payload, "summary").clone());
    out.insert("current".to_string(), Value::Object(cur));
    out.insert("error".to_string(), get(payload, "error").clone());
    drop_empty(&Value::Object(out))
}

pub fn model_time_text(payload: &Value) -> String {
    let compact = compact_time_payload(payload);
    let current = match get(&compact, "current") {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    let elapsed = if compact.get("elapsed_minutes").is_some() {
        format!(
            "{} min",
            scalar_text(obj_get(&compact_obj(&compact), "elapsed_minutes"))
        )
    } else {
        String::new()
    };
    let now = [
        scalar_text(current.get("current_date_label").unwrap_or(&Value::Null)),
        scalar_text(current.get("time_of_day").unwrap_or(&Value::Null)),
    ]
    .iter()
    .filter(|s| !s.is_empty())
    .cloned()
    .collect::<Vec<_>>()
    .join(", ");
    let lines = vec![
        kv("ok", get(&compact, "ok")),
        kv_str("elapsed", &elapsed),
        kv_str("now", &now),
        kv(
            "absolute_minutes",
            current.get("absolute_minutes").unwrap_or(&Value::Null),
        ),
        kv("summary", get(&compact, "summary")),
        kv("error", get(&compact, "error")),
    ];
    plain_lines("TIME", &lines)
}

fn compact_obj(v: &Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    }
}

// =========================================================================
// _compact_player_character_update
// =========================================================================

pub fn compact_player_character_update_payload(payload: &Value) -> Value {
    let mut out = Map::new();
    out.insert("ok".to_string(), get(payload, "ok").clone());
    out.insert("updated".to_string(), get(payload, "updated").clone());
    out.insert(
        "card_revision".to_string(),
        get(payload, "card_revision").clone(),
    );
    out.insert(
        "reason".to_string(),
        Value::String(clip_text(get(payload, "reason"), 180)),
    );
    out.insert("error".to_string(), get(payload, "error").clone());
    drop_empty(&Value::Object(out))
}

pub fn model_player_character_update_text(payload: &Value) -> String {
    let compact = compact_player_character_update_payload(payload);
    plain_lines(
        "PLAYER CHARACTER UPDATE",
        &[
            kv("ok", get(&compact, "ok")),
            kv("updated", get(&compact, "updated")),
            kv("revision", get(&compact, "card_revision")),
            kv("error", get(&compact, "error")),
        ],
    )
}

// =========================================================================
// _model_whereabouts / presence / scene
// =========================================================================

pub fn model_whereabouts_text(payload: &Value) -> String {
    let compact = compact_whereabouts_payload(payload);
    let whereabouts = match get(&compact, "whereabouts") {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    let location = {
        let ln = whereabouts
            .get("location_name")
            .cloned()
            .unwrap_or(Value::Null);
        if !is_empty_value(&ln) {
            ln
        } else {
            whereabouts
                .get("location_id")
                .cloned()
                .unwrap_or(Value::Null)
        }
    };
    let lines = vec![
        kv("npc", get(&compact, "npc_id")),
        kv("label", get(&compact, "name")),
        kv("present", get(&compact, "present")),
        kv("status", whereabouts.get("status").unwrap_or(&Value::Null)),
        kv("location", &location),
        kv_str(
            "details",
            &clip_text(whereabouts.get("details").unwrap_or(&Value::Null), 300),
        ),
    ];
    plain_lines("NPC WHEREABOUTS", &lines)
}

pub fn model_presence_text(payload: &Value) -> String {
    let compact = compact_presence_payload(payload);
    let whereabouts = match get(&compact, "whereabouts") {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    };
    let lines = vec![
        kv("npc", get(&compact, "npc_id")),
        kv("label", get(&compact, "name")),
        kv("present", get(&compact, "present")),
        kv("scene", get(&compact, "scene")),
        kv(
            "whereabouts",
            whereabouts.get("status").unwrap_or(&Value::Null),
        ),
    ];
    plain_lines("NPC PRESENCE", &lines)
}

pub fn model_scene_text(payload: &Value) -> String {
    let compact = compact_scene_payload(payload);
    let mut lines = vec![
        kv("scene_id", get(&compact, "scene_id")),
        kv("location_id", get(&compact, "location_id")),
        kv("title", get(&compact, "title")),
        kv("present_npcs", get(&compact, "present_npcs")),
        kv(
            "dropped_present_npcs",
            get(&compact, "dropped_present_npcs"),
        ),
        kv("repair_hint", get(&compact, "repair_hint")),
    ];
    if let Value::Array(items) = get(&compact, "items") {
        if !items.is_empty() {
            lines.push("items:".to_string());
            for item in items {
                let summary = row_summary(item, &["item_id", "name", "visible", "portable"]);
                if !summary.is_empty() {
                    lines.push(format!("- {summary}"));
                }
            }
        }
    }
    if let Value::Array(exits) = get(&compact, "exits") {
        if !exits.is_empty() {
            lines.push("exits:".to_string());
            for exit_ in exits {
                let summary = row_summary(
                    exit_,
                    &["exit_id", "name", "destination", "visible", "blocked_by"],
                );
                if !summary.is_empty() {
                    lines.push(format!("- {summary}"));
                }
            }
        }
    }
    plain_lines("SCENE SAVED", &lines)
}

pub fn model_ask_npc_text(payload: &Value) -> String {
    let compact = compact_ask_npc_payload(payload);
    let label = {
        let l = clean_text(get(&compact, "npc_label"));
        if l.is_empty() {
            clean_text(get(&compact, "npc_id"))
        } else {
            l
        }
    };
    let npc_id = clean_text(get(&compact, "npc_id"));
    let npc_line = if !label.is_empty() && !npc_id.is_empty() && label != npc_id {
        format!("{label} ({npc_id})")
    } else {
        label
    };
    plain_lines(
        "NPC RESULT",
        &[
            kv_str("npc", &npc_line),
            kv("speech", get(&compact, "speech_ru")),
            kv("action", get(&compact, "action_ru")),
            "already_emitted: yes".to_string(),
            "final_narration: only new non-NPC consequences; ask_npc for another named NPC reaction."
                .to_string(),
        ],
    )
}

// =========================================================================
// ask_player player-options payload
// =========================================================================

/// `_player_options_payload(args)` -> (payload, error).
pub fn player_options_payload(args: &Value) -> (Value, String) {
    let question = {
        let q = clip_text(get(args, "question"), 180);
        if q.is_empty() {
            "Что ты делаешь дальше?".to_string()
        } else {
            q
        }
    };
    let raw_options = match get(args, "options") {
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    };
    let mut options = Vec::new();
    for row in &raw_options {
        let m = match row {
            Value::Object(m) => m,
            _ => continue,
        };
        let label = clip_text(m.get("label").unwrap_or(&Value::Null), 80);
        let message = clip_text(m.get("message").unwrap_or(&Value::Null), 700);
        if !label.is_empty() && !message.is_empty() {
            let mut o = Map::new();
            o.insert("label".to_string(), Value::String(label));
            o.insert("message".to_string(), Value::String(message));
            options.push(Value::Object(o));
        }
    }
    if options.len() < 4 {
        return (
            Value::Object(Map::new()),
            "ask_player requires at least 4 options with label and message".to_string(),
        );
    }
    options.truncate(8);
    let mut out = Map::new();
    out.insert("question".to_string(), Value::String(question));
    out.insert("options".to_string(), Value::Array(options));
    (Value::Object(out), String::new())
}

// =========================================================================
// tool-arg normalization (_normalize_tool_calls / _compact_tool_value)
// =========================================================================

const OMIT: &str = "\u{0}__OMIT__\u{0}";

fn is_omit(v: &Value) -> bool {
    matches!(v, Value::String(s) if s == OMIT)
}

fn schema_types(schema: &Value) -> Vec<String> {
    match schema.get("type") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a.iter().map(scalar_text).collect(),
        _ => Vec::new(),
    }
}

/// `_compact_tool_value(schema, value, required)`.
fn compact_tool_value(schema: &Value, value: &Value, required: bool) -> Value {
    if value.is_null() {
        return if required {
            Value::Null
        } else {
            Value::String(OMIT.to_string())
        };
    }
    let schema_obj = match schema {
        Value::Object(_) => schema,
        _ => return value.clone(),
    };
    let props = schema_obj.get("properties");
    let types = schema_types(schema_obj);
    let is_object = types.iter().any(|t| t == "object") || matches!(props, Some(Value::Object(_)));

    if is_object {
        let value_obj = match value {
            Value::Object(m) => m,
            _ => return value.clone(),
        };
        let props_map = match props {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        if props_map.is_empty() {
            let clean = drop_empty(value);
            return if required || !is_empty_value(&clean) {
                clean
            } else {
                Value::String(OMIT.to_string())
            };
        }
        let required_keys: Vec<String> = match schema_obj.get("required") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        };
        let mut out = Map::new();
        for (key, prop_schema) in &props_map {
            if !value_obj.contains_key(key) {
                continue;
            }
            let child = compact_tool_value(
                prop_schema,
                value_obj.get(key).unwrap(),
                required_keys.contains(key),
            );
            if !is_omit(&child) {
                out.insert(key.clone(), child);
            }
        }
        return if required || !out.is_empty() {
            Value::Object(out)
        } else {
            Value::String(OMIT.to_string())
        };
    }

    if types.iter().any(|t| t == "array") {
        let value_arr = match value {
            Value::Array(a) => a,
            _ => return value.clone(),
        };
        let item_schema = match schema_obj.get("items") {
            Some(Value::Object(_)) => schema_obj.get("items"),
            _ => None,
        };
        let mut out = Vec::new();
        for item in value_arr {
            let child = match item_schema {
                Some(s) => compact_tool_value(s, item, true),
                None => item.clone(),
            };
            if !is_omit(&child) && !is_empty_value(&child) {
                out.push(child);
            }
        }
        return Value::Array(out);
    }

    value.clone()
}

/// `_normalize_update_world_state_args(value)`.
fn normalize_update_world_state_args(value: &Value) -> Value {
    let items = match value.get("items") {
        Some(Value::Array(a)) => a,
        _ => return Value::Object(Map::new()),
    };
    let mut clean_items = Vec::new();
    for item in items {
        let m = match item {
            Value::Object(m) => m,
            _ => {
                clean_items.push(item.clone());
                continue;
            }
        };
        let mut clean_item = Map::new();
        for (key, child) in m {
            if key == "type" || key == "text" || !is_empty_value(child) {
                clean_item.insert(key.clone(), child.clone());
            }
        }
        clean_items.push(Value::Object(clean_item));
    }
    let mut out = Map::new();
    out.insert("items".to_string(), Value::Array(clean_items));
    Value::Object(out)
}

/// `_normalize_ask_player_args(value)`.
fn normalize_ask_player_args(value: &Value) -> Value {
    let mut out = Map::new();
    let question = clean_text(get(value, "question"));
    if !question.is_empty() {
        out.insert("question".to_string(), Value::String(question));
    }
    let mut options = Vec::new();
    if let Value::Array(raw) = get(value, "options") {
        for item in raw {
            let m = match item {
                Value::Object(m) => m,
                _ => continue,
            };
            let label = clean_text(m.get("label").unwrap_or(&Value::Null));
            let message = clean_text(m.get("message").unwrap_or(&Value::Null));
            if !label.is_empty() && !message.is_empty() {
                let mut o = Map::new();
                o.insert("label".to_string(), Value::String(label));
                o.insert("message".to_string(), Value::String(message));
                options.push(Value::Object(o));
            }
        }
    }
    if !options.is_empty() {
        out.insert("options".to_string(), Value::Array(options));
    }
    Value::Object(out)
}

/// `_tool_parameters_schema(world, name)` — looks up the static GM tool catalog.
fn tool_parameters_schema(name: &str) -> Option<Value> {
    let catalog = gml_agents::gm_tool_catalog();
    let tool = catalog.get(name)?;
    let function = tool.get("function")?;
    let schema = function.get("parameters")?;
    if schema.is_object() {
        Some(schema.clone())
    } else {
        None
    }
}

/// `_normalize_tool_args(name, args, parameters_schema)`.
pub fn normalize_tool_args(name: &str, args: &Value) -> Value {
    let args_obj = match args {
        Value::Object(_) => args,
        _ => return Value::Object(Map::new()),
    };
    let schema = match tool_parameters_schema(name) {
        Some(s) => s,
        None => return args_obj.clone(),
    };
    let normalized = compact_tool_value(&schema, args_obj, true);
    let mut normalized = match normalized {
        Value::Object(_) => normalized,
        _ => return Value::Object(Map::new()),
    };
    if name == "update_world_state" {
        normalized = normalize_update_world_state_args(&normalized);
    } else if name == "ask_player" {
        normalized = normalize_ask_player_args(&normalized);
    }
    match normalized {
        Value::Object(_) => normalized,
        _ => Value::Object(Map::new()),
    }
}

// =========================================================================
// scene-delta application helper (used by _sync_scene_delta)
// =========================================================================

/// Apply one scene-delta move to the world; returns the player-facing payload if
/// the NPC resolves, else None (Python `except KeyError: continue`).
pub fn apply_scene_move(world: &mut World, move_: &Value) -> Option<Value> {
    let m = match move_ {
        Value::Object(m) => m,
        _ => return None,
    };
    let npc_id = m.get("npc_id").and_then(Value::as_str).unwrap_or("");
    let present = m.get("present").map(crate::truthy).unwrap_or(false);
    let location = m.get("location").and_then(Value::as_str).unwrap_or("");
    let visible = m.get("visible").map(crate::truthy).unwrap_or(true);
    let can_hear = m.get("can_hear").map(crate::truthy).unwrap_or(true);
    let activity = m.get("activity").and_then(Value::as_str).unwrap_or("");
    let attitude = m.get("attitude").and_then(Value::as_str).unwrap_or("");
    world
        .set_npc_presence(
            npc_id, present, location, visible, can_hear, activity, attitude,
        )
        .ok()
}
