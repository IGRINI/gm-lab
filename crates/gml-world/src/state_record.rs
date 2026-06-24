//! State-record constants, coercion helpers, canonical hashing, and the
//! actor-safe RAG document type — ports of the corresponding world.py code.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

use crate::helpers::{actor_key, as_list, as_str};
use crate::model::StateRecord;

pub const STATE_RECORD_KINDS: [&str; 5] = ["fact", "rumor", "npc_memory", "relationship", "goal"];
pub const STATE_RECORD_SCOPES: [&str; 5] = ["public", "gm", "owner", "subject", "participants"];

/// `STATE_RECORD_SCOPE_ALIASES` (private/npc->owner, shared/participant(s)).
pub fn scope_alias(raw: &str) -> Option<&'static str> {
    match raw {
        "private" => Some("owner"),
        "npc" => Some("owner"),
        "shared" => Some("participants"),
        "participant" => Some("participants"),
        _ => None,
    }
}

/// `STATE_DEBUG_ACTORS = {"debug", "system"}`.
pub fn is_debug_actor(actor: &str) -> bool {
    actor == "debug" || actor == "system"
}

/// `STATE_GM_ACTORS = {"gm", *STATE_DEBUG_ACTORS}`.
pub fn is_gm_actor(actor: &str) -> bool {
    actor == "gm" || is_debug_actor(actor)
}

/// `_state_record_kind(value)`.
pub fn state_record_kind(value: &str) -> String {
    let raw = crate::helpers::safe_id(value, "fact");
    if STATE_RECORD_KINDS.contains(&raw.as_str()) {
        raw
    } else {
        "fact".to_string()
    }
}

/// `_state_record_scope(value)`.
pub fn state_record_scope(value: &str) -> String {
    let mut raw = crate::helpers::safe_id(value, "public");
    if let Some(aliased) = scope_alias(&raw) {
        raw = aliased.to_string();
    }
    if STATE_RECORD_SCOPES.contains(&raw.as_str()) {
        raw
    } else {
        "public".to_string()
    }
}

/// `_state_record_tags(value)` — dedup, preserve order, drop empties.
pub fn state_record_tags(value: &Value) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();
    for item in as_list(value) {
        let tag = as_str(&item);
        if !tag.is_empty() && !seen.contains(&tag) {
            seen.insert(tag.clone());
            out.push(tag);
        }
    }
    out
}

/// `_state_record_aliases(value)` — same as tags.
pub fn state_record_aliases(value: &Value) -> Vec<String> {
    state_record_tags(value)
}

/// `_state_record_participants(value)` — actor_key of each non-empty tag.
pub fn state_record_participants(value: &Value) -> Vec<String> {
    state_record_tags(value)
        .iter()
        .map(|t| actor_key(t))
        .filter(|t| !t.is_empty())
        .collect()
}

/// `_state_record_metadata(value)` — dict with stringified keys, skipping None
/// keys (JSON keys are always strings, so this is effectively a passthrough of
/// a dict).
pub fn state_record_metadata(value: &Value) -> Map<String, Value> {
    match value {
        Value::Object(m) => m.clone(),
        _ => Map::new(),
    }
}

/// `_state_record_active(value, default)`.
pub fn state_record_active(value: &Value, default: bool) -> bool {
    match value {
        Value::Bool(b) => *b,
        Value::Null => default,
        Value::String(s) => {
            let raw = s.trim().to_lowercase();
            if matches!(raw.as_str(), "1" | "true" | "yes" | "on" | "active") {
                true
            } else if matches!(raw.as_str(), "0" | "false" | "no" | "off" | "inactive") {
                false
            } else {
                // bool(str) in Python: non-empty string is truthy.
                !s.is_empty()
            }
        }
        Value::Number(n) => {
            // bool(number): 0 -> False, else True.
            n.as_f64().map(|f| f != 0.0).unwrap_or(true)
        }
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `state_record_hash(record)` — canonical JSON (sort_keys, ensure_ascii=False,
/// separators=(',',':'), default=str) then sha256 hexdigest.
pub fn state_record_hash(record: &StateRecord) -> String {
    let mut payload: Map<String, Value> = Map::new();
    payload.insert("id".to_string(), Value::String(record.record_id.clone()));
    payload.insert(
        "kind".to_string(),
        Value::String(state_record_kind(&record.kind)),
    );
    payload.insert(
        "text".to_string(),
        Value::String(as_str_value(&record.text)),
    );
    payload.insert(
        "scope".to_string(),
        Value::String(state_record_scope(&record.scope)),
    );
    payload.insert("active".to_string(), Value::Bool(record.active));
    payload.insert(
        "owner".to_string(),
        Value::String(as_str_value(&record.owner)),
    );
    payload.insert(
        "subject".to_string(),
        Value::String(as_str_value(&record.subject)),
    );
    let status = {
        let s = as_str_value(&record.status);
        if s.is_empty() {
            "known".to_string()
        } else {
            s
        }
    };
    payload.insert("status".to_string(), Value::String(status));
    payload.insert("tags".to_string(), to_str_array(&record.tags));
    payload.insert(
        "entity_id".to_string(),
        Value::String(as_str_value(&record.entity_id)),
    );
    payload.insert(
        "source_npc".to_string(),
        Value::String(as_str_value(&record.source_npc)),
    );
    payload.insert(
        "location_id".to_string(),
        Value::String(as_str_value(&record.location_id)),
    );
    payload.insert(
        "location_name".to_string(),
        Value::String(as_str_value(&record.location_name)),
    );
    payload.insert(
        "region_id".to_string(),
        Value::String(as_str_value(&record.region_id)),
    );
    payload.insert(
        "region_name".to_string(),
        Value::String(as_str_value(&record.region_name)),
    );
    payload.insert(
        "scene_id".to_string(),
        Value::String(as_str_value(&record.scene_id)),
    );
    payload.insert(
        "importance".to_string(),
        Value::String(as_str_value(&record.importance)),
    );
    payload.insert("aliases".to_string(), to_str_array(&record.aliases));
    payload.insert(
        "metadata".to_string(),
        Value::Object(record.metadata.clone()),
    );
    if !record.participants.is_empty() {
        payload.insert(
            "participants".to_string(),
            to_str_array(&record.participants),
        );
    }

    let canonical = canonical_json(&Value::Object(payload));
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    hex(&digest)
}

/// `_as_str` applied to an already-owned string field for the hash payload.
fn as_str_value(s: &str) -> String {
    s.trim().to_string()
}

fn to_str_array(items: &[String]) -> Value {
    Value::Array(items.iter().map(|s| Value::String(s.clone())).collect())
}

/// Serialize a JSON value with `sort_keys=True`, `separators=(',',':')`,
/// `ensure_ascii=False` (raw UTF-8) — recursively sorting object keys.
pub fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_json_string(s, out),
        Value::Array(a) => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            // sort_keys=True sorts by the (unicode) key.
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(k, out);
                out.push(':');
                write_canonical(&m[*k], out);
            }
            out.push('}');
        }
    }
}

/// JSON string escaping matching Python `json.dumps(ensure_ascii=False)`:
/// escapes `"`, `\`, control chars (`\b \t \n \f \r` shortforms, else `\uXXXX`),
/// leaves all other (non-ASCII) characters raw.
fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0a}' => out.push_str("\\n"),
            '\u{0c}' => out.push_str("\\f"),
            '\u{0d}' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// `_anchor_label` re-export path used by state_record_documents.
pub use crate::helpers::anchor_label;

/// Actor-safe RAG corpus document produced by `gml-world`.
///
/// `gml-rag` owns the retrieval implementation and has an equivalent boundary
/// type. The world crate keeps this local shape to avoid depending on the
/// retrieval crate from the domain model; the orchestrator converts between the
/// two at the integration boundary.
#[derive(Clone, Debug, PartialEq)]
pub struct RagDocument {
    pub doc_id: String,
    pub kind: String,
    pub text: String,
    pub status: String,
    pub source: String,
    pub visibility: String,
    pub tags: Vec<String>,
    pub metadata: Map<String, Value>,
}

impl RagDocument {
    /// Builder mirroring the Python `RagDocument(...)` constructor defaults
    /// (`status="known"`, `source=""`, `visibility="player"`, `tags=()`,
    /// `metadata={}`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        doc_id: String,
        kind: String,
        text: String,
        status: String,
        source: String,
        visibility: String,
        tags: Vec<String>,
        metadata: Map<String, Value>,
    ) -> Self {
        RagDocument {
            doc_id,
            kind,
            text,
            status,
            source,
            visibility,
            tags,
            metadata,
        }
    }

    /// `contextual_text()` — the RAG-facing block format.
    pub fn contextual_text(&self) -> String {
        let mut meta = vec![
            "RPG world memory block.".to_string(),
            format!("Kind: {}.", self.kind),
            format!("Status: {}.", self.status),
        ];
        if !self.source.is_empty() {
            meta.push(format!("Source: {}.", self.source));
        }
        if !self.tags.is_empty() {
            meta.push(format!("Tags: {}.", self.tags.join(", ")));
        }
        format!("{}\nText: {}", meta.join("\n"), self.text.trim())
    }
}

/// `_state_record_visible_to(record, actor_id)` — static visibility gate.
pub fn state_record_visible_to(record: &StateRecord, actor_id: &str) -> bool {
    let actor = actor_key(if actor_id.is_empty() {
        "player"
    } else {
        actor_id
    });
    let scope = state_record_scope(&record.scope);
    let owner = actor_key(&record.owner);
    let subject = actor_key(&record.subject);
    let participants: BTreeSet<String> = record
        .participants
        .iter()
        .map(|p| actor_key(p))
        .filter(|p| !p.is_empty())
        .collect();
    if scope == "public" {
        return true;
    }
    if is_gm_actor(&actor) {
        return true;
    }
    match scope.as_str() {
        "gm" => is_gm_actor(&actor),
        "owner" => !owner.is_empty() && actor == owner,
        "subject" => !subject.is_empty() && actor == subject,
        "participants" => {
            (!owner.is_empty() && actor == owner)
                || (!subject.is_empty() && actor == subject)
                || participants.contains(&actor)
        }
        _ => false,
    }
}
