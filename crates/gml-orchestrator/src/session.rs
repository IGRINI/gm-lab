//! `Session` — the party state between turns, ported from `class Session` in
//! `orchestrator.py`.
//!
//! Field set + shapes are pinned to the persistence payload
//! (`tests/reference/persistence/chat_payload.json -> "session"`) so
//! gml-persistence can round-trip them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::{json, Map, Value};

use gml_llm::{Backend, ConnectorId, ModelBinding, SessionIdentity};
use gml_types::NpcBeat;
use gml_world::{MemoryTier, MemoryTruthStatus, MemoryUnit, World, WorldEvent, WorldSpec};

use crate::compact::{empty_usage, usage_from_payload};

/// A synchronous factory for per-NPC clients — the Rust stand-in for Python's
/// module-level `make_client()`. `ensure_npc_client` calls this to spin up a
/// fresh client/thread (so each NPC has its own prompt-cache key).
pub type ClientFactory = std::sync::Arc<dyn Fn() -> Arc<dyn Backend> + Send + Sync>;

/// Compatibility factory for callers that do not need independent child cache
/// scopes. Production histories receive a connector-owned factory through
/// [`Session::new_bound`] or [`Session::with_world_binding`].
pub fn default_client_factory(client: Arc<dyn Backend>) -> ClientFactory {
    std::sync::Arc::new(move || client.clone())
}

fn fresh_branch_client(
    factory: &ClientFactory,
    model: &str,
    had_provider_identity: bool,
) -> Arc<dyn Backend> {
    let client = factory();
    if !model.trim().is_empty() {
        client.set_model(model);
    }
    if had_provider_identity || !client.session_id().is_empty() || !client.thread_id().is_empty() {
        let identity = SessionIdentity::new();
        client.set_session_identity(
            Some(identity.session_id().as_str()),
            Some(identity.thread_id().as_str()),
        );
    }
    client
}

fn rotated_branch_client_state(
    factory: &ClientFactory,
    state: &NpcClientState,
    fallback_model: &str,
) -> NpcClientState {
    let model = if state.model.trim().is_empty() {
        fallback_model.to_string()
    } else {
        state.model.clone()
    };
    let client = fresh_branch_client(
        factory,
        &model,
        !state.session_id.is_empty() || !state.thread_id.is_empty(),
    );
    NpcClientState {
        model,
        session_id: client.session_id(),
        thread_id: client.thread_id(),
    }
}

fn compact_location_generator_request(request: &Value) -> Value {
    if request.get("purpose").and_then(Value::as_str) == Some("travel_route") {
        return compact_travel_route_request(request);
    }

    let mut out = Map::new();
    for key in [
        "purpose",
        "target_place_id",
        "entry_from_place_id",
        "parent_place_id",
        "route_transition_id",
        "situation_type",
        "rarity",
        "road_risk",
    ] {
        insert_clipped_string(&mut out, key, request.get(key));
    }
    for key in ["elapsed_minutes", "remaining_minutes", "route_time_minutes"] {
        if let Some(value) = request.get(key).filter(|v| !v.is_null()) {
            out.insert(key.to_string(), value.clone());
        }
    }
    for key in [
        "player_observed",
        "enter_after_commit",
        "requires_entry_transition",
    ] {
        if let Some(value) = request.get(key).filter(|v| !v.is_null()) {
            out.insert(key.to_string(), value.clone());
        }
    }
    insert_clipped_string(&mut out, "request", request.get("request"));
    Value::Object(out)
}

fn compact_location_generator_result(generated: &Value) -> Value {
    if let Some(compact) = compact_travel_route_result(generated) {
        return compact;
    }

    let mut out = Map::new();
    for key in [
        "name",
        "kind",
        "visible_summary",
        "description",
        "hidden_summary",
        "anti_repeat_key",
        "memory_note",
    ] {
        insert_clipped_string(&mut out, key, generated.get(key));
    }
    for key in [
        "features",
        "sensory_details",
        "choices",
        "consequences",
        "hidden_clues",
        "knows_more",
    ] {
        insert_clipped_string_list(&mut out, key, generated.get(key));
    }
    if let Some(entry) = generated.get("entry_transition").and_then(Value::as_object) {
        let mut compact = Map::new();
        for key in ["label", "return_label", "directionality", "kind", "risk"] {
            insert_clipped_string(&mut compact, key, entry.get(key));
        }
        if let Some(value) = entry.get("time_cost_minutes") {
            compact.insert("time_cost_minutes".to_string(), value.clone());
        }
        if !compact.is_empty() {
            out.insert("entry_transition".to_string(), Value::Object(compact));
        }
    }
    if let Some(Value::Array(transitions)) = generated.get("transitions") {
        let rows = transitions
            .iter()
            .filter_map(|transition| {
                let row = transition.as_object()?;
                let mut compact = Map::new();
                for key in [
                    "label",
                    "destination_hint",
                    "directionality",
                    "kind",
                    "risk",
                ] {
                    insert_clipped_string(&mut compact, key, row.get(key));
                }
                if let Some(value) = row.get("time_cost_minutes") {
                    compact.insert("time_cost_minutes".to_string(), value.clone());
                }
                Some(Value::Object(compact))
            })
            .take(6)
            .collect::<Vec<_>>();
        if !rows.is_empty() {
            out.insert("transitions".to_string(), Value::Array(rows));
        }
    }
    Value::Object(out)
}

fn compact_travel_route_request(request: &Value) -> Value {
    let mut out = Map::new();
    out.insert(
        "purpose".to_string(),
        Value::String("travel_route".to_string()),
    );

    insert_exact_string(
        &mut out,
        "origin_place_id",
        request
            .pointer("/origin/place_id")
            .or_else(|| request.get("origin_place_id")),
    );
    insert_exact_string(
        &mut out,
        "destination_place_id",
        request
            .pointer("/destination/place_id")
            .or_else(|| request.get("destination_place_id")),
    );
    insert_exact_string(
        &mut out,
        "requested_network_id",
        request.get("requested_network_id"),
    );

    Value::Object(out)
}

fn compact_travel_route_result(generated: &Value) -> Option<Value> {
    let geography = generated.get("travel_geography")?.as_object()?;

    let mut out = Map::new();
    for key in ["name", "kind", "anti_repeat_key"] {
        insert_clipped_string(&mut out, key, generated.get(key));
    }

    let compact = compact_travel_geography(geography);
    if !compact.is_empty() {
        out.insert("travel_geography".to_string(), Value::Object(compact));
    }

    Some(Value::Object(out))
}

fn compact_travel_geography(geography: &Map<String, Value>) -> Map<String, Value> {
    const NETWORK_CAP: usize = 8;
    const ANCHOR_CAP: usize = 24;
    const ACCESS_CAP: usize = 24;
    const LINK_CAP: usize = 32;

    let mut out = Map::new();
    insert_compact_travel_rows(
        &mut out,
        "networks",
        geography.get("networks"),
        NETWORK_CAP,
        |row| {
            compact_travel_row(
                row,
                &["network_id", "scope_id", "blocked_by"],
                &["default_for_normal_travel", "passable"],
                &[],
                &[],
            )
        },
    );
    insert_compact_travel_rows(
        &mut out,
        "anchors",
        geography.get("anchors"),
        ANCHOR_CAP,
        |row| {
            compact_travel_row(
                row,
                &["anchor_id", "network_id", "blocked_by"],
                &["passable"],
                &[],
                &[],
            )
        },
    );
    insert_compact_travel_rows(
        &mut out,
        "accesses",
        geography.get("accesses"),
        ACCESS_CAP,
        |row| {
            compact_travel_row(
                row,
                &["access_id", "place_id", "anchor_id", "blocked_by"],
                &["passable"],
                &[],
                &["required_fact_ids"],
            )
        },
    );
    insert_compact_travel_rows(&mut out, "links", geography.get("links"), LINK_CAP, |row| {
        compact_travel_row(
            row,
            &["link_id", "anchor_a", "anchor_b", "risk", "blocked_by"],
            &["passable"],
            &["time_cost_minutes"],
            &["required_fact_ids"],
        )
    });
    out
}

fn insert_compact_travel_rows<F>(
    out: &mut Map<String, Value>,
    key: &str,
    value: Option<&Value>,
    cap: usize,
    compact_row: F,
) where
    F: Fn(&Map<String, Value>) -> Map<String, Value>,
{
    let Some(Value::Array(rows)) = value else {
        return;
    };
    let compact = rows
        .iter()
        .filter_map(Value::as_object)
        .map(compact_row)
        .filter(|row| !row.is_empty())
        .take(cap)
        .map(Value::Object)
        .collect::<Vec<_>>();
    if !compact.is_empty() {
        out.insert(key.to_string(), Value::Array(compact));
    }
}

fn compact_travel_row(
    row: &Map<String, Value>,
    string_keys: &[&str],
    boolean_keys: &[&str],
    integer_keys: &[&str],
    string_list_keys: &[&str],
) -> Map<String, Value> {
    let mut compact = Map::new();
    for key in string_keys {
        insert_exact_string(&mut compact, key, row.get(*key));
    }
    for key in boolean_keys {
        if let Some(value) = row.get(*key).filter(|value| value.is_boolean()) {
            compact.insert((*key).to_string(), value.clone());
        }
    }
    for key in integer_keys {
        if let Some(value) = row
            .get(*key)
            .filter(|value| value.is_i64() || value.is_u64())
        {
            compact.insert((*key).to_string(), value.clone());
        }
    }
    for key in string_list_keys {
        insert_exact_string_list(&mut compact, key, row.get(*key));
    }
    compact
}

fn insert_exact_string(out: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    let Some(value @ Value::String(text)) = value else {
        return;
    };
    if !text.trim().is_empty() {
        out.insert(key.to_string(), value.clone());
    }
}

fn insert_exact_string_list(out: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    const FACT_ID_CAP: usize = 16;

    let Some(Value::Array(items)) = value else {
        return;
    };
    let items = items
        .iter()
        .filter(|item| item.as_str().is_some_and(|text| !text.trim().is_empty()))
        .take(FACT_ID_CAP)
        .cloned()
        .collect::<Vec<_>>();
    if !items.is_empty() {
        out.insert(key.to_string(), Value::Array(items));
    }
}

#[cfg(test)]
mod location_generator_history_tests {
    use super::{compact_location_generator_request, compact_location_generator_result};
    use serde_json::{json, Value};

    #[test]
    fn travel_route_request_keeps_only_explicit_route_identity() {
        let request = json!({
            "purpose": "travel_route",
            "origin": {"place_id": "market_square", "name": "Market Square"},
            "destination": {"place_id": "west_alley", "name": "West Alley"},
            "origin_place_id": "stale_compat_origin",
            "destination_place_id": "stale_compat_destination",
            "requested_network_id": "greyhaven_public_streets",
            "existing_travel_geography": {"links": [{"link_id": "large_context"}]},
            "request": "Infer a route from names",
            "unrelated": "drop me"
        });

        assert_eq!(
            compact_location_generator_request(&request),
            json!({
                "purpose": "travel_route",
                "origin_place_id": "market_square",
                "destination_place_id": "west_alley",
                "requested_network_id": "greyhaven_public_streets"
            })
        );
    }

    #[test]
    fn travel_route_request_accepts_explicit_top_level_id_contract() {
        let request = json!({
            "purpose": "travel_route",
            "origin_place_id": "dock_gate",
            "destination_place_id": "old_shop"
        });

        assert_eq!(
            compact_location_generator_request(&request),
            json!({
                "purpose": "travel_route",
                "origin_place_id": "dock_gate",
                "destination_place_id": "old_shop"
            })
        );
    }

    #[test]
    fn travel_route_result_keeps_compact_canonical_geography() {
        let exact_long_id = "n".repeat(700);
        let mut links = (0..40)
            .map(|index| {
                json!({
                    "link_id": format!("link_{index}"),
                    "anchor_a": "market_surface",
                    "anchor_b": "west_surface",
                    "time_cost_minutes": 24,
                    "risk": "low",
                    "passable": true,
                    "blocked_by": "",
                    "required_fact_ids": ["gate_open"],
                    "label": "not canonical travel data"
                })
            })
            .collect::<Vec<_>>();
        links[0]["link_id"] = Value::String(exact_long_id.clone());
        let generated = json!({
            "name": "Market to west route",
            "kind": "travel_route",
            "anti_repeat_key": "market-west-route",
            "visible_summary": "not needed in route history",
            "travel_geography": {
                "networks": [{
                    "network_id": "greyhaven_public_streets",
                    "scope_id": "greyhaven",
                    "default_for_normal_travel": true,
                    "passable": true,
                    "blocked_by": "",
                    "display_name": "drop me"
                }],
                "anchors": [
                    {"anchor_id": "market_surface", "network_id": "greyhaven_public_streets", "passable": true},
                    {"anchor_id": "west_surface", "network_id": "greyhaven_public_streets", "passable": true}
                ],
                "accesses": [{
                    "access_id": "shop_to_market",
                    "place_id": "known_shop",
                    "anchor_id": "market_surface",
                    "passable": true,
                    "required_fact_ids": ["shop_known"],
                    "description": "drop me"
                }],
                "links": links,
                "unknown_rows": [{"id": "drop me"}]
            }
        });

        let compact = compact_location_generator_result(&generated);
        assert_eq!(
            compact["travel_geography"]["links"]
                .as_array()
                .unwrap()
                .len(),
            32
        );
        assert_eq!(
            compact["travel_geography"]["links"][0]["link_id"],
            Value::String(exact_long_id)
        );
        assert_eq!(
            compact["travel_geography"]["networks"][0],
            json!({
                "network_id": "greyhaven_public_streets",
                "scope_id": "greyhaven",
                "default_for_normal_travel": true,
                "passable": true
            })
        );
        assert!(compact.get("visible_summary").is_none());
        assert!(compact["travel_geography"].get("unknown_rows").is_none());
        assert!(compact["travel_geography"]["links"][0]
            .get("label")
            .is_none());
    }

    #[test]
    fn free_form_unavailability_does_not_override_canonical_geography() {
        let generated = json!({
            "kind": "travel_route",
            "travel_unavailable_reason": "The west gate is canonically sealed.",
            "travel_geography": {
                "links": [{
                    "link_id": "contradictory_link",
                    "anchor_a": "a",
                    "anchor_b": "b",
                    "time_cost_minutes": 5,
                    "risk": "none",
                    "passable": true
                }]
            }
        });

        assert_eq!(
            compact_location_generator_result(&generated),
            json!({
                "kind": "travel_route",
                "travel_geography": {
                    "links": [{
                        "link_id": "contradictory_link",
                        "anchor_a": "a",
                        "anchor_b": "b",
                        "time_cost_minutes": 5,
                        "risk": "none",
                        "passable": true
                    }]
                }
            })
        );
    }

    #[test]
    fn ordinary_location_compaction_is_unchanged() {
        let request = json!({
            "purpose": "local_place",
            "target_place_id": "kitchen",
            "parent_place_id": "inn",
            "request": "A working kitchen",
            "unknown": "drop me"
        });
        assert_eq!(
            compact_location_generator_request(&request),
            json!({
                "purpose": "local_place",
                "target_place_id": "kitchen",
                "parent_place_id": "inn",
                "request": "A working kitchen"
            })
        );

        let generated = json!({
            "name": "Kitchen",
            "kind": "room",
            "visible_summary": "A smoky kitchen.",
            "features": ["hearth"],
            "transitions": [{
                "label": "Back door",
                "destination_hint": "yard",
                "directionality": "bidirectional",
                "kind": "door",
                "risk": "none",
                "time_cost_minutes": 1,
                "extra": "drop me"
            }]
        });
        assert_eq!(
            compact_location_generator_result(&generated),
            json!({
                "name": "Kitchen",
                "kind": "room",
                "visible_summary": "A smoky kitchen.",
                "features": ["hearth"],
                "transitions": [{
                    "label": "Back door",
                    "destination_hint": "yard",
                    "directionality": "bidirectional",
                    "kind": "door",
                    "risk": "none",
                    "time_cost_minutes": 1
                }]
            })
        );
    }
}

fn insert_clipped_string(out: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    let text = value.as_str().unwrap_or("").trim();
    if text.is_empty() {
        return;
    }
    out.insert(key.to_string(), json!(clip_location_text(text)));
}

fn insert_clipped_string_list(out: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    let Some(Value::Array(items)) = value else {
        return;
    };
    let rows = items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(clip_location_text)
        .take(8)
        .map(Value::String)
        .collect::<Vec<_>>();
    if !rows.is_empty() {
        out.insert(key.to_string(), Value::Array(rows));
    }
}

fn clip_location_text(text: &str) -> String {
    text.chars().take(LOCATION_GENERATOR_TEXT_CHARS).collect()
}

/// Character-generator request compaction — the NPC analogue of
/// [`compact_location_generator_request`]. Keeps the qualitative brief fields the
/// generator's own rolling history needs for anti-repeat; drops nothing sensitive
/// because this history feeds ONLY the generator's private thread.
fn compact_character_generator_request(request: &Value) -> Value {
    let mut out = Map::new();
    for key in ["role", "name", "appearance", "power_tier", "place_id"] {
        insert_clipped_string(&mut out, key, request.get(key));
    }
    if let Some(value) = request.get("present").filter(|v| !v.is_null()) {
        out.insert("present".to_string(), value.clone());
    }
    insert_clipped_string(&mut out, "request", request.get("request"));
    Value::Object(out)
}

/// Character-generator result compaction — the NPC analogue of
/// [`compact_location_generator_result`]. Retains the identity/motif fields that
/// anti-repeat keys on (name, role, persona, voice, agenda, anti_repeat_key) plus
/// the GM-only note; this history never leaves the generator's private thread.
fn compact_character_generator_result(generated: &Value) -> Value {
    let mut out = Map::new();
    for key in [
        "name",
        "pronouns",
        "role",
        "public_label",
        "persona",
        "voice",
        "agenda",
        "knowledge",
        "secret",
        "anti_repeat_key",
        "memory_note",
    ] {
        insert_clipped_string(&mut out, key, generated.get(key));
    }
    insert_clipped_string_list(&mut out, "goals", generated.get("goals"));
    if let Some(value) = generated.get("attitude_to_player").filter(|v| !v.is_null()) {
        out.insert("attitude_to_player".to_string(), value.clone());
    }
    Value::Object(out)
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string())
}

/// `_OBS_CAP = 12`.
pub const OBS_CAP: usize = 12;
/// `_COMMIT_BLOCKS = 8`.
pub const COMMIT_BLOCKS: usize = 8;
/// `config.EVENTS_CAP = 400`.
pub const EVENTS_CAP: usize = 400;
/// `config.GM_RUMORS_CAP = 80`.
pub const RUMORS_CAP: usize = 80;
const OBS_DIGEST_LINES: usize = 4;
const OBS_LINE_CHARS: usize = 220;
const LOCATION_GENERATOR_HISTORY_MESSAGES: usize = 12;
const LOCATION_GENERATOR_TEXT_CHARS: usize = 600;
const CHARACTER_GENERATOR_HISTORY_MESSAGES: usize = 12;

/// Compaction thresholds — the Rust home for the `config.GM_HISTORY_TOKENS` /
/// `config.GM_KEEP_TURNS` / `config.NPC_HISTORY_TOKENS` /
/// `config.NPC_KEEP_EXCHANGES` / `config.GM_COMPACT_INPUT_CHARS` globals that
/// `orchestrator.py` reads **at call time** inside `_maybe_compact`,
/// `_maybe_compact_npc`, `_summarize_npc_history`, and `context_usage`.
///
/// Python keeps these on the `config` module and contract tests monkeypatch them
/// to force compaction without touching production defaults. We mirror that by
/// holding them on the [`Session`] (default = production defaults), so tests can
/// lower them on a session exactly like the Python tests lower `config.*` — the
/// production `Config` defaults are never mutated.
#[derive(Clone, Copy, Debug)]
pub struct CompactionThresholds {
    /// `config.GM_HISTORY_TOKENS` (default 100000).
    pub gm_history_tokens: i64,
    /// `config.GM_KEEP_TURNS` (default 3).
    pub gm_keep_turns: i64,
    /// `config.NPC_HISTORY_TOKENS` (default 64000).
    pub npc_history_tokens: i64,
    /// `config.NPC_KEEP_EXCHANGES` (default 6).
    pub npc_keep_exchanges: i64,
    /// `config.GM_COMPACT_INPUT_CHARS` (default 12000).
    pub compact_input_chars: i64,
}

impl Default for CompactionThresholds {
    fn default() -> Self {
        CompactionThresholds {
            gm_history_tokens: 100_000,
            gm_keep_turns: 3,
            npc_history_tokens: 64_000,
            npc_keep_exchanges: 6,
            compact_input_chars: 12_000,
        }
    }
}

impl CompactionThresholds {
    /// Build from the live [`gml_config::Config`] so env overrides
    /// (`GM_HISTORY_TOKENS`, `NPC_HISTORY_TOKENS`, `GM_KEEP_TURNS`,
    /// `NPC_KEEP_EXCHANGES`, `GM_COMPACT_INPUT_CHARS`) take effect in production —
    /// mirroring Python's call-time `config.*` reads. The `Default` impl above is
    /// the production-default fallback used by tests and config-less construction.
    pub fn from_config(cfg: &gml_config::Config) -> Self {
        CompactionThresholds {
            gm_history_tokens: cfg.gm_history_tokens,
            gm_keep_turns: cfg.gm_keep_turns,
            npc_history_tokens: cfg.npc_history_tokens,
            npc_keep_exchanges: cfg.npc_keep_exchanges,
            compact_input_chars: cfg.compact_input_chars,
        }
    }
}

/// Serializable per-NPC client identity (`npc_client_state[id]`).
#[derive(Clone, Debug, Default)]
pub struct NpcClientState {
    pub model: String,
    pub session_id: String,
    pub thread_id: String,
}

/// A provisional NPC draft held in `session.pending[npc_id]` until commit.
#[derive(Clone, Debug)]
pub struct PendingDraft {
    pub seq: i64,
    pub time_minutes: i64,
    pub response: String,
    pub beats: Vec<NpcBeat>,
    pub speech: String,
    pub action: String,
    pub claims: Vec<String>,
    pub witnesses: BTreeSet<String>,
    pub user_message: Option<Value>,
    pub assistant_message: Option<Value>,
}

#[derive(Clone, Debug)]
struct ObservationItem {
    seq: i64,
    actor_label: String,
    text: String,
}

fn is_observable_room_event(event: &WorldEvent) -> bool {
    if event.actor == "gm" {
        return false;
    }
    !matches!(event.kind.as_str(), "dice" | "tool" | "meta")
}

fn legacy_beats(speech: &str, action: &str) -> Vec<NpcBeat> {
    let mut beats = Vec::new();
    if !action.trim().is_empty() {
        beats.push(NpcBeat {
            kind: "action".to_string(),
            text: action.to_string(),
        });
    }
    if !speech.trim().is_empty() {
        beats.push(NpcBeat {
            kind: "speech".to_string(),
            text: speech.to_string(),
        });
    }
    beats
}

fn visible_turn_from_parts(action: &str, speech: &str) -> String {
    [action, speech]
        .into_iter()
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `class Session`.
pub struct Session {
    pub client: Arc<dyn Backend>,
    /// Factory for per-NPC clients (Python `make_client`). Not persisted.
    pub npc_client_factory: ClientFactory,
    /// The connector is fixed for the lifetime of this history. Only the model
    /// part may change between turns.
    model_binding: ModelBinding,
    pub client_session_id: String,
    pub client_thread_id: String,

    /// Live per-NPC clients (not persisted). Rebuilt lazily.
    pub npc_clients: HashMap<String, Arc<dyn Backend>>,
    /// Serializable per-NPC client identities.
    pub npc_client_state: BTreeMap<String, NpcClientState>,
    /// Dedicated location/situation generator client. It deliberately has its
    /// own thread/cache identity instead of sharing the GM conversation.
    pub location_generator_client: Option<Arc<dyn Backend>>,
    pub location_generator_client_state: NpcClientState,
    pub location_generator_anti_repeat: Vec<String>,
    pub location_generator_messages: Vec<Value>,
    /// Dedicated significant-NPC / character generator client. Like the location
    /// generator it keeps its OWN thread/cache identity instead of sharing the GM
    /// conversation, so its prompt cache is separate.
    pub character_generator_client: Option<Arc<dyn Backend>>,
    pub character_generator_client_state: NpcClientState,
    pub character_generator_anti_repeat: Vec<String>,
    pub character_generator_messages: Vec<Value>,
    /// Armed by a `duplicate_candidates` gate result; `retry=true` bypasses the
    /// dedup gate ONLY while armed. Keeps a confused GM from stamping duplicates
    /// by sending `retry` on a fresh request. Not persisted (turn-scoped UX).
    pub character_generator_retry_armed: bool,

    pub world: World,
    pub gm_messages: Vec<Value>,
    pub gm_summary: String,
    pub npc_messages: BTreeMap<String, Vec<Value>>,
    pub npc_summaries: BTreeMap<String, String>,
    pub loaded_gm_tools: BTreeSet<String>,
    /// Turn index at which each executed tool was last run (updated in the
    /// `run_tool` dispatch for EVERY executed tool). Feeds the compaction-time
    /// prune of stale SEARCHED/loaded tools; persisted so the staleness signal
    /// survives save/restore. Legacy payloads without it load as an empty map
    /// (no records => nothing is pruned until the map starts filling).
    pub tool_last_used: BTreeMap<String, i64>,
    /// Turn index at which each SEARCHED tool was admitted into
    /// `loaded_gm_tools` (recorded where `load_tool_schema` / `invoke_loaded_tool`
    /// load a tool schema). Combined with `tool_last_used`, a non-initial tool is
    /// pruned only when BOTH signals are older than the retained window.
    pub tool_loaded_turn: BTreeMap<String, i64>,
    pub world_query_seen: BTreeMap<String, BTreeSet<String>>,
    pub run_usage: Map<String, Value>,
    pub last_player_action: String,
    pub sid_counter: i64,
    pub turn_time_advances: Vec<Value>,

    pub events: Vec<WorldEvent>,
    pub seq: i64,
    pub turn: i64,
    pub delivered: BTreeMap<String, i64>,
    pub shown: BTreeMap<String, i64>,
    pub pending: BTreeMap<String, PendingDraft>,
    pub commitments: BTreeMap<String, Vec<String>>,
    pub npc_last_contact_minutes: BTreeMap<String, i64>,
    /// Last player-options state recorded into the GM WORLD SNAPSHOT / toggle
    /// notice stream. `None` until the first snapshot is injected. A mid-session
    /// change vs. this value appends a one-line toggle notice (persisted).
    pub snapshot_options_state: Option<bool>,
    /// Per-NPC card revision already injected into that NPC's history (§7). A
    /// bump of `Npc.card_revision` past this value appends a fresh NPC CARD
    /// UPDATED message. Persisted like `npc_last_contact_minutes`.
    pub npc_injected_card_revision: BTreeMap<String, i64>,
    pub turn_player_event: Option<usize>, // index into `events`

    /// Compaction thresholds (the Rust home for the `config.*` globals Python
    /// reads at call time in `_maybe_compact*` / `context_usage`). Not persisted.
    pub compaction: CompactionThresholds,
}

impl Session {
    /// `Session(client, world=None)`. The default world is procedural canon.
    /// Reuses the supplied backend for compatibility. Production code uses
    /// [`Session::new_bound`] so each child client comes from the connector.
    pub fn new(client: Arc<dyn Backend>) -> Self {
        let world = World::from_worldgen(&WorldSpec::default());
        let factory = default_client_factory(client.clone());
        Self::with_world(client, world, factory)
    }

    pub fn new_bound(
        client: Arc<dyn Backend>,
        npc_client_factory: ClientFactory,
        model_binding: ModelBinding,
    ) -> Self {
        let world = World::from_worldgen(&WorldSpec::default());
        Self::with_world_binding(client, world, npc_client_factory, model_binding)
    }

    pub fn model_binding(&self) -> &ModelBinding {
        &self.model_binding
    }

    /// `Session(client, world=...)` with an explicit NPC client factory.
    pub fn with_world(
        client: Arc<dyn Backend>,
        world: World,
        npc_client_factory: ClientFactory,
    ) -> Self {
        let binding = Self::inferred_model_binding(client.as_ref());
        Self::with_world_binding(client, world, npc_client_factory, binding)
    }

    pub(crate) fn inferred_model_binding(client: &dyn Backend) -> ModelBinding {
        let model = client.model();
        let connector_id = ConnectorId::new(client.connector_id())
            .expect("a backend must expose a valid connector id");
        ModelBinding::new(
            connector_id,
            if model.trim().is_empty() {
                "default"
            } else {
                &model
            },
        )
        .expect("the active model must be a valid model id")
    }

    /// Build a session with an explicit connector/model binding. This is the
    /// canonical constructor for persisted and newly-created histories.
    pub fn with_world_binding(
        client: Arc<dyn Backend>,
        world: World,
        npc_client_factory: ClientFactory,
        model_binding: ModelBinding,
    ) -> Self {
        Session {
            client,
            npc_client_factory,
            model_binding,
            client_session_id: String::new(),
            client_thread_id: String::new(),
            npc_clients: HashMap::new(),
            npc_client_state: BTreeMap::new(),
            location_generator_client: None,
            location_generator_client_state: NpcClientState::default(),
            location_generator_anti_repeat: Vec::new(),
            location_generator_messages: Vec::new(),
            character_generator_client: None,
            character_generator_client_state: NpcClientState::default(),
            character_generator_anti_repeat: Vec::new(),
            character_generator_messages: Vec::new(),
            character_generator_retry_armed: false,
            world,
            gm_messages: Vec::new(),
            gm_summary: String::new(),
            npc_messages: BTreeMap::new(),
            npc_summaries: BTreeMap::new(),
            loaded_gm_tools: BTreeSet::new(), // set below via configure_loaded_tools
            tool_last_used: BTreeMap::new(),
            tool_loaded_turn: BTreeMap::new(),
            world_query_seen: BTreeMap::new(),
            run_usage: empty_usage(),
            last_player_action: String::new(),
            sid_counter: 0,
            turn_time_advances: Vec::new(),
            events: Vec::new(),
            seq: 0,
            turn: 0,
            delivered: BTreeMap::new(),
            shown: BTreeMap::new(),
            pending: BTreeMap::new(),
            commitments: BTreeMap::new(),
            npc_last_contact_minutes: BTreeMap::new(),
            snapshot_options_state: None,
            npc_injected_card_revision: BTreeMap::new(),
            turn_player_event: None,
            compaction: CompactionThresholds::default(),
        }
    }

    /// Initialize `loaded_gm_tools` (Python: `agents.initial_gm_tool_names()`).
    /// Called by `run_turn` lazily? No — Python sets it in `__init__` with the
    /// default (`include_player_options_tool=False`). We mirror that, but the
    /// canonical initial set is recomputed against the actual setting at use.
    pub fn ensure_initial_tools(&mut self, include_player_options_tool: bool) {
        if self.loaded_gm_tools.is_empty() {
            self.loaded_gm_tools = gml_agents::initial_gm_tool_names(include_player_options_tool);
        }
    }

    /// Give a newly forked history independent provider conversation and cache
    /// scopes while preserving its model and gameplay state. This must be
    /// applied both to the fork snapshot and to every inherited checkpoint so
    /// a later rewind cannot restore identities owned by the source history.
    pub fn rotate_provider_identities_for_branch(&mut self) {
        for npc_id in self.npc_clients.keys().cloned().collect::<Vec<_>>() {
            self.remember_npc_client(&npc_id);
        }
        self.remember_location_generator_client();
        self.remember_character_generator_client();

        let factory = self.npc_client_factory.clone();
        let model = self.model_binding.model_id().to_string();
        let had_main_identity = !self.client_session_id.is_empty()
            || !self.client_thread_id.is_empty()
            || !self.client.session_id().is_empty()
            || !self.client.thread_id().is_empty();
        let client = fresh_branch_client(&factory, &model, had_main_identity);
        self.client_session_id = client.session_id();
        self.client_thread_id = client.thread_id();
        self.client = client;

        self.npc_clients.clear();
        for state in self.npc_client_state.values_mut() {
            *state = rotated_branch_client_state(&factory, state, &model);
        }

        self.location_generator_client = None;
        if !self.location_generator_client_state.model.is_empty()
            || !self.location_generator_client_state.session_id.is_empty()
            || !self.location_generator_client_state.thread_id.is_empty()
        {
            self.location_generator_client_state = rotated_branch_client_state(
                &factory,
                &self.location_generator_client_state,
                &model,
            );
        }

        self.character_generator_client = None;
        if !self.character_generator_client_state.model.is_empty()
            || !self.character_generator_client_state.session_id.is_empty()
            || !self.character_generator_client_state.thread_id.is_empty()
        {
            self.character_generator_client_state = rotated_branch_client_state(
                &factory,
                &self.character_generator_client_state,
                &model,
            );
        }
    }

    pub fn reset_world_query_cache(&mut self) {
        self.world_query_seen = BTreeMap::new();
    }

    /// Record that `name` was executed this turn (the staleness "last used"
    /// signal for the compaction-time prune of searched tools). Called from the
    /// `run_tool` dispatch for EVERY executed tool.
    pub fn mark_tool_used(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        self.tool_last_used.insert(name.to_string(), self.turn);
    }

    /// Record that `name` was admitted into `loaded_gm_tools` this turn (the
    /// "recently loaded" signal). Called where `load_tool_schema` /
    /// `invoke_loaded_tool` load a tool schema.
    pub fn mark_tool_loaded(&mut self, name: &str) {
        if name.is_empty() {
            return;
        }
        self.loaded_gm_tools.insert(name.to_string());
        self.tool_loaded_turn.insert(name.to_string(), self.turn);
    }

    /// Prune SEARCHED/loaded tools that went stale, called from `maybe_compact`
    /// AFTER the retained history was rebuilt around a fresh snapshot (the GM
    /// prompt cache resets at compaction anyway). A tool is dropped from
    /// `loaded_gm_tools` only when ALL of:
    ///   - it is NOT in the INITIAL default set (those are never pruned);
    ///   - it HAS at least one staleness record (a legacy tool with no
    ///     `tool_last_used` / `tool_loaded_turn` entry is kept — no record can
    ///     prove it stale);
    ///   - every record it does have is OLDER than `oldest_retained_turn` (the
    ///     first turn still inside the retained keep-window). A record at or past
    ///     that turn means the tool was used/loaded recently, so it is kept.
    pub fn prune_stale_loaded_tools(&mut self, oldest_retained_turn: i64) {
        // The superset (player options included) is the "initial" set: ask_player
        // and the other defaults must never be pruned regardless of the toggle.
        let initial = gml_agents::initial_gm_tool_names(true);
        let mut drop: Vec<String> = Vec::new();
        for name in &self.loaded_gm_tools {
            if initial.contains(name) {
                continue;
            }
            let last = self.tool_last_used.get(name).copied();
            let loaded = self.tool_loaded_turn.get(name).copied();
            if last.is_none() && loaded.is_none() {
                // No record at all — a legacy/native-loaded tool. Keep it.
                continue;
            }
            let last_stale = last.is_none_or(|t| t < oldest_retained_turn);
            let loaded_stale = loaded.is_none_or(|t| t < oldest_retained_turn);
            if last_stale && loaded_stale {
                drop.push(name.clone());
            }
        }
        for name in drop {
            self.loaded_gm_tools.remove(&name);
            self.tool_last_used.remove(&name);
            self.tool_loaded_turn.remove(&name);
        }
    }

    /// `_query_seen_set(session, scope_key)` — get-or-create the per-scope set.
    pub fn query_seen_set(&mut self, scope_key: &str) -> &mut BTreeSet<String> {
        self.world_query_seen
            .entry(scope_key.to_string())
            .or_default()
    }

    /// `ensure_npc_client(npc_id)` — get-or-create the per-NPC client/thread.
    pub fn ensure_npc_client(&mut self, npc_id: &str) -> Option<Arc<dyn Backend>> {
        if let Some(c) = self.npc_clients.get(npc_id) {
            return Some(c.clone());
        }
        let client: Arc<dyn Backend> = (self.npc_client_factory)();
        let state = self.npc_client_state.entry(npc_id.to_string()).or_default();
        let model = if !state.model.is_empty() {
            state.model.clone()
        } else {
            self.model_binding.model_id().to_string()
        };
        if !model.is_empty() {
            client.set_model(&model);
        }
        client.set_session_identity(
            Some(state.session_id.as_str()),
            Some(state.thread_id.as_str()),
        );
        self.npc_clients.insert(npc_id.to_string(), client.clone());
        self.remember_npc_client(npc_id);
        Some(client)
    }

    pub fn ensure_location_generator_client(&mut self) -> Arc<dyn Backend> {
        if let Some(client) = &self.location_generator_client {
            return client.clone();
        }
        let client: Arc<dyn Backend> = (self.npc_client_factory)();
        let state = &mut self.location_generator_client_state;
        let model = if !state.model.is_empty() {
            state.model.clone()
        } else {
            self.model_binding.model_id().to_string()
        };
        if !model.is_empty() {
            client.set_model(&model);
        }
        client.set_session_identity(
            Some(state.session_id.as_str()),
            Some(state.thread_id.as_str()),
        );
        self.location_generator_client = Some(client.clone());
        self.remember_location_generator_client();
        client
    }

    pub fn remember_location_generator_client(&mut self) {
        let Some(client) = &self.location_generator_client else {
            return;
        };
        let model = {
            let m = client.model();
            if m.is_empty() {
                self.model_binding.model_id().to_string()
            } else {
                m
            }
        };
        self.location_generator_client_state = NpcClientState {
            model,
            session_id: client.session_id(),
            thread_id: client.thread_id(),
        };
    }

    pub fn note_location_anti_repeat_key(&mut self, key: &str) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        self.location_generator_anti_repeat
            .retain(|existing| existing != key);
        self.location_generator_anti_repeat.push(key.to_string());
        const MAX_GENERATOR_KEYS: usize = 24;
        if self.location_generator_anti_repeat.len() > MAX_GENERATOR_KEYS {
            let drop = self.location_generator_anti_repeat.len() - MAX_GENERATOR_KEYS;
            self.location_generator_anti_repeat.drain(0..drop);
        }
    }

    pub fn record_location_generator_exchange(&mut self, request: &Value, generated: &Value) {
        let request_summary = compact_location_generator_request(request);
        let generated_summary = compact_location_generator_result(generated);
        self.location_generator_messages.push(json!({
            "role": "user",
            "content": format!(
                "PREVIOUS LOCATION GENERATION REQUEST:\n{}",
                compact_json(&request_summary)
            ),
        }));
        self.location_generator_messages.push(json!({
            "role": "assistant",
            "content": format!(
                "PREVIOUS LOCATION GENERATION RESULT:\n{}",
                compact_json(&generated_summary)
            ),
        }));
        if self.location_generator_messages.len() > LOCATION_GENERATOR_HISTORY_MESSAGES {
            let drop = self.location_generator_messages.len() - LOCATION_GENERATOR_HISTORY_MESSAGES;
            self.location_generator_messages.drain(0..drop);
        }
    }

    /// Get-or-create the dedicated character generator client — mirrors
    /// [`Session::ensure_location_generator_client`] exactly: same NPC factory,
    /// own thread/cache identity restored from the persisted state so its prompt
    /// cache is separate from the GM conversation and survives save/restore.
    pub fn ensure_character_generator_client(&mut self) -> Arc<dyn Backend> {
        if let Some(client) = &self.character_generator_client {
            return client.clone();
        }
        let client: Arc<dyn Backend> = (self.npc_client_factory)();
        let state = &mut self.character_generator_client_state;
        let model = if !state.model.is_empty() {
            state.model.clone()
        } else {
            self.model_binding.model_id().to_string()
        };
        if !model.is_empty() {
            client.set_model(&model);
        }
        client.set_session_identity(
            Some(state.session_id.as_str()),
            Some(state.thread_id.as_str()),
        );
        self.character_generator_client = Some(client.clone());
        self.remember_character_generator_client();
        client
    }

    pub fn remember_character_generator_client(&mut self) {
        let Some(client) = &self.character_generator_client else {
            return;
        };
        let model = {
            let m = client.model();
            if m.is_empty() {
                self.model_binding.model_id().to_string()
            } else {
                m
            }
        };
        self.character_generator_client_state = NpcClientState {
            model,
            session_id: client.session_id(),
            thread_id: client.thread_id(),
        };
    }

    pub fn note_character_anti_repeat_key(&mut self, key: &str) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        self.character_generator_anti_repeat
            .retain(|existing| existing != key);
        self.character_generator_anti_repeat.push(key.to_string());
        const MAX_GENERATOR_KEYS: usize = 24;
        if self.character_generator_anti_repeat.len() > MAX_GENERATOR_KEYS {
            let drop = self.character_generator_anti_repeat.len() - MAX_GENERATOR_KEYS;
            self.character_generator_anti_repeat.drain(0..drop);
        }
    }

    pub fn record_character_generator_exchange(&mut self, request: &Value, generated: &Value) {
        let request_summary = compact_character_generator_request(request);
        let generated_summary = compact_character_generator_result(generated);
        self.character_generator_messages.push(json!({
            "role": "user",
            "content": format!(
                "PREVIOUS NPC GENERATION REQUEST:\n{}",
                compact_json(&request_summary)
            ),
        }));
        self.character_generator_messages.push(json!({
            "role": "assistant",
            "content": format!(
                "PREVIOUS NPC GENERATION RESULT:\n{}",
                compact_json(&generated_summary)
            ),
        }));
        if self.character_generator_messages.len() > CHARACTER_GENERATOR_HISTORY_MESSAGES {
            let drop =
                self.character_generator_messages.len() - CHARACTER_GENERATOR_HISTORY_MESSAGES;
            self.character_generator_messages.drain(0..drop);
        }
    }

    /// `remember_npc_client(npc_id)`.
    pub fn remember_npc_client(&mut self, npc_id: &str) {
        let client = match self.npc_clients.get(npc_id) {
            Some(c) => c.clone(),
            None => return,
        };
        let model = {
            let m = client.model();
            if m.is_empty() {
                self.model_binding.model_id().to_string()
            } else {
                m
            }
        };
        // Capture the connector's per-NPC cache identity so it survives
        // save/restore. Backends without cache identity return empty strings.
        self.npc_client_state.insert(
            npc_id.to_string(),
            NpcClientState {
                model,
                session_id: client.session_id(),
                thread_id: client.thread_id(),
            },
        );
    }

    /// `reset_npc_memory(npc_id)` — returns true for any real NPC.
    pub fn reset_npc_memory(&mut self, npc_id: &str) -> bool {
        if npc_id.is_empty() || !self.world.npcs.contains_key(npc_id) {
            return false;
        }
        self.npc_messages.remove(npc_id);
        self.npc_summaries.remove(npc_id);
        self.npc_client_state.remove(npc_id);
        self.npc_clients.remove(npc_id);
        self.commitments.remove(npc_id);
        self.pending.remove(npc_id);
        self.npc_last_contact_minutes.remove(npc_id);
        self.npc_injected_card_revision.remove(npc_id);
        let owner_scope = format!("actor:{}", npc_id.trim().to_lowercase());
        self.world
            .world_canon
            .memory
            .units
            .retain(|_, unit| unit.owner_scope != owner_scope);
        self.delivered.insert(npc_id.to_string(), self.seq);
        self.shown.insert(npc_id.to_string(), self.seq);
        true
    }

    /// `apply_debug_edit(npc_id, data)`.
    pub fn apply_debug_edit(&mut self, npc_id: &str, data: &Value) -> bool {
        if npc_id.is_empty() || !self.world.npcs.contains_key(npc_id) {
            return false;
        }
        let data = match data {
            Value::Object(_) => data,
            _ => &Value::Object(Map::new()),
        };
        if let Some(Value::Object(_)) = data.get("fields") {
            self.world.update_npc(npc_id, data.get("fields").unwrap());
        }
        if let Some(present) = data.get("present") {
            let requested = crate::truthy(present);
            let currently = self.world.scene.present_npcs.contains(npc_id);
            if requested != currently {
                let _ = self
                    .world
                    .set_npc_presence(npc_id, requested, "", true, true, "", "");
            }
        }
        if let Some(Value::Object(wb)) = data.get("whereabouts") {
            let _ = self.world.set_npc_whereabouts(
                npc_id,
                wb.get("location_id").and_then(Value::as_str).unwrap_or(""),
                wb.get("location_name")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                wb.get("status").and_then(Value::as_str).unwrap_or(""),
                wb.get("details").and_then(Value::as_str).unwrap_or(""),
                "",
            );
        }
        if data.get("reset_memory").map(crate::truthy).unwrap_or(false) {
            self.reset_npc_memory(npc_id);
        }
        true
    }

    pub fn set_model_for_all_clients(&mut self, model: &str) {
        let model = model.trim();
        if model.is_empty() || self.model_binding.model_id() == model {
            return;
        }
        self.model_binding = self
            .model_binding
            .with_model(model)
            .expect("a non-empty backend model must be a valid model id");
        self.client.set_model(model);
        self.client_session_id = self.client.session_id();
        self.client_thread_id = self.client.thread_id();
        for client in self.npc_clients.values() {
            client.set_model(model);
        }
        if let Some(client) = &self.location_generator_client {
            client.set_model(model);
        }
        if let Some(client) = &self.character_generator_client {
            client.set_model(model);
        }
        for state in self.npc_client_state.values_mut() {
            state.model = model.to_string();
            state.session_id.clear();
            state.thread_id.clear();
        }
        self.location_generator_client_state = NpcClientState {
            model: model.to_string(),
            ..NpcClientState::default()
        };
        self.character_generator_client_state = NpcClientState {
            model: model.to_string(),
            ..NpcClientState::default()
        };

        let npc_ids = self.npc_clients.keys().cloned().collect::<Vec<_>>();
        for npc_id in npc_ids {
            self.remember_npc_client(&npc_id);
        }
        self.remember_location_generator_client();
        self.remember_character_generator_client();
    }

    pub fn set_run_usage(&mut self, usage: &Value) {
        self.run_usage = usage_from_payload(usage);
    }

    /// `add_turn_usage(turn_total)`.
    pub fn add_turn_usage(&mut self, turn_total: &Value) -> Value {
        let mut usage = usage_from_payload(&Value::Object(self.run_usage.clone()));
        let tt_i = |key: &str| turn_total.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
        let calls = match turn_total.get("calls") {
            Some(Value::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        bump(&mut usage, "turns", 1);
        bump(&mut usage, "calls", calls.len() as i64);
        bump(&mut usage, "in", tt_i("in"));
        bump(&mut usage, "out", tt_i("out"));
        bump(&mut usage, "cached", tt_i("cached"));
        bump(&mut usage, "tokens", tt_i("tokens"));
        let secs = usage.get("secs").and_then(|v| v.as_f64()).unwrap_or(0.0)
            + turn_total
                .get("secs")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
        usage.insert("secs".to_string(), json!(crate::compact::round2(secs)));
        let peak_now = usage
            .get("peak_context")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        usage.insert(
            "peak_context".to_string(),
            json!(peak_now.max(tt_i("peak_context"))),
        );
        for call in &calls {
            let scope = call.get("scope").and_then(Value::as_str).unwrap_or("npc");
            let tokens = call.get("in").and_then(|v| v.as_i64()).unwrap_or(0)
                + call.get("out").and_then(|v| v.as_i64()).unwrap_or(0);
            match scope {
                "gm" => {
                    bump(&mut usage, "gm_calls", 1);
                    bump(&mut usage, "gm_tokens", tokens);
                }
                "other" => {
                    bump(&mut usage, "other_calls", 1);
                    bump(&mut usage, "other_tokens", tokens);
                }
                _ => {
                    bump(&mut usage, "npc_calls", 1);
                    bump(&mut usage, "npc_tokens", tokens);
                }
            }
        }
        self.run_usage = usage.clone();
        Value::Object(usage)
    }

    /// Remove one previously-accounted empty failed turn before replacing it
    /// with a resumed attempt.
    ///
    /// This is deliberately strict: only a `meta_total` with no model/tool
    /// calls and zero token/context counters is reversible. The caller should
    /// perform this operation on a staged session so a second failed attempt can
    /// still be discarded atomically. On rejection the usage map is unchanged.
    pub fn remove_empty_failed_turn_usage(&mut self, failed_turn_total: &Value) -> bool {
        let Some(total) = failed_turn_total.as_object() else {
            return false;
        };
        if !matches!(total.get("calls"), Some(Value::Array(calls)) if calls.is_empty())
            || ["in", "out", "cached", "tokens", "peak_context"]
                .into_iter()
                .any(|key| total.get(key).and_then(Value::as_i64) != Some(0))
        {
            return false;
        }

        let Some(failed_secs) = total.get("secs").and_then(Value::as_f64) else {
            return false;
        };
        let Some(turns) = self.run_usage.get("turns").and_then(Value::as_i64) else {
            return false;
        };
        let Some(run_secs) = self.run_usage.get("secs").and_then(Value::as_f64) else {
            return false;
        };
        if turns <= 0
            || !failed_secs.is_finite()
            || failed_secs < 0.0
            || !run_secs.is_finite()
            || run_secs < failed_secs
        {
            return false;
        }

        let remaining_secs = crate::compact::round2(run_secs - failed_secs);
        self.run_usage.insert("turns".to_string(), json!(turns - 1));
        self.run_usage.insert(
            "secs".to_string(),
            json!(if remaining_secs == 0.0 {
                0.0
            } else {
                remaining_secs
            }),
        );
        true
    }

    /// `npc_history_text(npc_id, max_messages=8)`.
    pub fn npc_history_text(&self, npc_id: &str, max_messages: usize) -> String {
        let name = self
            .world
            .npcs
            .get(npc_id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| npc_id.to_string());
        let mut parts: Vec<String> = Vec::new();
        let summary = self
            .npc_summaries
            .get(npc_id)
            .map(|s| s.trim())
            .unwrap_or("");
        if !summary.is_empty() {
            parts.push(format!("Compressed memory:\n{summary}"));
        }
        let history = self.npc_messages.get(npc_id).cloned().unwrap_or_default();
        let tail = if history.len() > max_messages {
            &history[history.len() - max_messages..]
        } else {
            &history[..]
        };
        if !tail.is_empty() {
            let mut rendered = Vec::new();
            for msg in tail {
                let role = msg.get("role").and_then(Value::as_str).unwrap_or("?");
                let content = msg.get("content").and_then(Value::as_str).unwrap_or("");
                if role == "user" {
                    let historical = content.trim().replacen(
                        "CURRENT SITUATION (what's happening now, what you react to):",
                        "PREVIOUS NPC SITUATION (historical; do not treat as current):",
                        1,
                    );
                    rendered.push(format!("Previous situation for NPC:\n{historical}"));
                } else if role == "assistant" {
                    rendered.push(format!("Response from {name}:\n{}", content.trim()));
                } else {
                    rendered.push(format!("{role}:\n{}", content.trim()));
                }
            }
            parts.push(format!("Recent messages:\n{}", rendered.join("\n\n")));
        }
        if parts.is_empty() {
            "NPC history is empty.".to_string()
        } else {
            parts.join("\n\n")
        }
    }

    pub fn next_sid(&mut self) -> String {
        self.sid_counter += 1;
        format!("s{}", self.sid_counter)
    }

    pub fn next_seq(&mut self) -> i64 {
        self.seq += 1;
        self.seq
    }

    fn present(&self) -> BTreeSet<String> {
        self.world.present_witnesses()
    }

    pub fn current_game_minutes(&self) -> i64 {
        std::cmp::max(
            0,
            std::cmp::max(
                self.world.time.absolute_minutes,
                self.world.world_canon.clock_minutes,
            ),
        )
    }

    pub fn npc_last_contact_text(&self, npc_id: &str) -> String {
        match self.npc_last_contact_minutes.get(npc_id).copied() {
            Some(last) => {
                let elapsed = std::cmp::max(0, self.current_game_minutes() - last);
                if elapsed == 0 {
                    "Last direct contact with the player was just now.".to_string()
                } else {
                    format!(
                        "Last direct contact with the player was {} ago in world time.",
                        format_duration(elapsed, &self.world.time)
                    )
                }
            }
            None => {
                "No previous direct contact with the player is recorded for this NPC.".to_string()
            }
        }
    }

    pub fn mark_npc_contact(&mut self, npc_id: &str) {
        if !npc_id.is_empty() && self.world.npcs.contains_key(npc_id) {
            self.npc_last_contact_minutes
                .insert(npc_id.to_string(), self.current_game_minutes());
        }
    }

    /// Snapshot-once for the NPC sub-agent (GM_CONTEXT_TZ §7): guarantee the
    /// NPC's history opens with its card, and append a one-line NPC CARD UPDATED
    /// notice whenever `Npc.card_revision` moved past the last injected revision.
    /// Call once per `ask_npc`, AFTER `maybe_compact_npc` (so a compaction that
    /// dropped the card re-injects a fresh one) and BEFORE the request is built.
    /// Append-only and idempotent: no persisted message is ever rewritten.
    pub fn ensure_npc_card_injected(&mut self, npc_id: &str) {
        let (rev, card_msg, update_msg) = match self.world.npcs.get(npc_id) {
            Some(npc) => (
                npc.card_revision,
                gml_agents::npc_card_message(npc),
                gml_agents::npc_card_update_message(npc),
            ),
            None => return,
        };
        let has_card = self
            .npc_messages
            .get(npc_id)
            .map(|h| gml_agents::npc_messages_have_card(h))
            .unwrap_or(false);
        if !has_card {
            // First contact, compaction re-inject, or legacy migration: the card
            // becomes history[0] so the model reads it before any exchange.
            self.npc_messages
                .entry(npc_id.to_string())
                .or_default()
                .insert(0, card_msg);
            self.npc_injected_card_revision
                .insert(npc_id.to_string(), rev);
        } else if self.npc_injected_card_revision.get(npc_id).copied() != Some(rev) {
            // The card was edited since it was injected: append a fresh notice.
            self.npc_messages
                .entry(npc_id.to_string())
                .or_default()
                .push(update_msg);
            self.npc_injected_card_revision
                .insert(npc_id.to_string(), rev);
        }
    }

    /// NPC ids the player contacted within the last game day (deterministic
    /// input to the dynamic roster, §3.4). Falls back to all recorded contacts
    /// when the calendar defines no positive day length.
    pub fn recent_contact_ids(&self) -> BTreeSet<String> {
        let now = self.current_game_minutes();
        let day = self.world.time.minutes_per_hour * self.world.time.hours_per_day;
        self.npc_last_contact_minutes
            .iter()
            .filter(|(_, &last)| day <= 0 || now - last <= day)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// `record_public(actor, kind, speech, action)`.
    pub fn record_public(&mut self, actor: &str, kind: &str, speech: &str, action: &str) {
        let seq = self.next_seq();
        let witnesses = self.present();
        self.events.push(WorldEvent {
            seq,
            turn: self.turn,
            time_minutes: self.current_game_minutes(),
            actor: actor.to_string(),
            kind: kind.to_string(),
            response: String::new(),
            beats: Vec::new(),
            speech: speech.to_string(),
            action: action.to_string(),
            witnesses,
        });
        if let Some(event) = self.events.last().cloned() {
            self.record_event_memory(&event);
        }
    }

    /// `record_player_for(npc_id)`.
    pub fn record_player_for(&mut self, npc_id: &str) -> BTreeSet<String> {
        self.record_player_for_with_room_witnesses(npc_id, true)
    }

    /// Record the player's current turn as witnessed by the addressed NPC.
    ///
    /// Direct `ask_npc` exchanges are private by default: bystanders may notice
    /// body language through the GM narration, but they must not receive the
    /// exact player wording as durable actor memory unless the exchange was
    /// explicitly public/audible.
    pub fn record_player_for_direct(&mut self, npc_id: &str) -> BTreeSet<String> {
        self.record_player_for_with_room_witnesses(npc_id, false)
    }

    fn record_player_for_with_room_witnesses(
        &mut self,
        npc_id: &str,
        include_room: bool,
    ) -> BTreeSet<String> {
        let mut witnesses = if include_room {
            self.present()
        } else {
            BTreeSet::new()
        };
        witnesses.insert("player".to_string());
        witnesses.insert(npc_id.to_string());
        match self.turn_player_event {
            None => {
                let seq = self.next_seq();
                let ev = WorldEvent {
                    seq,
                    turn: self.turn,
                    time_minutes: self.current_game_minutes(),
                    actor: "player".to_string(),
                    kind: "speech".to_string(),
                    response: String::new(),
                    beats: Vec::new(),
                    speech: self.last_player_action.clone(),
                    action: String::new(),
                    witnesses: witnesses.clone(),
                };
                self.events.push(ev);
                self.turn_player_event = Some(self.events.len() - 1);
                if let Some(event) = self.events.last().cloned() {
                    self.record_event_memory(&event);
                }
                witnesses
            }
            Some(idx) => {
                let ev = &mut self.events[idx];
                for w in &witnesses {
                    ev.witnesses.insert(w.clone());
                }
                ev.witnesses.clone()
            }
        }
    }

    /// `draft(npc_id, speech, action, claims, user_message, assistant_message, witnesses)`.
    #[allow(clippy::too_many_arguments)]
    pub fn draft(
        &mut self,
        npc_id: &str,
        speech: &str,
        action: &str,
        claims: Vec<String>,
        user_message: Option<Value>,
        assistant_message: Option<Value>,
        witnesses: Option<BTreeSet<String>>,
    ) {
        self.draft_with_response(
            npc_id,
            &visible_turn_from_parts(action, speech),
            legacy_beats(speech, action),
            speech,
            action,
            claims,
            user_message,
            assistant_message,
            witnesses,
        );
    }

    /// `draft_with_response(...)` stores the current organic NPC output while
    /// retaining derived speech/action fields for legacy rendering and rumor text.
    #[allow(clippy::too_many_arguments)]
    pub fn draft_with_response(
        &mut self,
        npc_id: &str,
        response: &str,
        beats: Vec<NpcBeat>,
        speech: &str,
        action: &str,
        claims: Vec<String>,
        user_message: Option<Value>,
        assistant_message: Option<Value>,
        witnesses: Option<BTreeSet<String>>,
    ) {
        if response.is_empty() && speech.is_empty() && action.is_empty() {
            self.pending.remove(npc_id);
            return;
        }
        let prev = self.pending.get(npc_id).cloned();
        let seq = match &prev {
            Some(p) => p.seq,
            None => self.next_seq(),
        };
        let event_witnesses = witnesses
            .or_else(|| prev.as_ref().map(|p| p.witnesses.clone()))
            .unwrap_or_else(|| self.present());
        self.pending.insert(
            npc_id.to_string(),
            PendingDraft {
                seq,
                time_minutes: self.current_game_minutes(),
                response: response.to_string(),
                beats,
                speech: speech.to_string(),
                action: action.to_string(),
                claims,
                witnesses: event_witnesses,
                user_message,
                assistant_message,
            },
        );
    }

    pub fn snapshot_shown(&mut self, npc_id: &str) {
        self.shown.insert(npc_id.to_string(), self.seq);
    }

    /// `observations(npc_id)`.
    pub fn observations(&self, npc_id: &str) -> String {
        let seen = self.delivered.get(npc_id).copied().unwrap_or(0);
        let mut items: Vec<ObservationItem> = Vec::new();
        let access = self.world.memory_access_for_actor(npc_id);
        for unit in self.world.world_canon.memory.units.values() {
            if !unit.owner_scope.starts_with("place:")
                || !unit.injection_state.is_default_visible()
                || !unit.is_visible_to(&access)
            {
                continue;
            }
            let Some(seq) = memory_source_event_seq(unit) else {
                continue;
            };
            if seq <= seen {
                continue;
            }
            let Some(event) = self.events.iter().find(|event| event.seq == seq) else {
                continue;
            };
            if event.actor == npc_id
                || !event.witnesses.contains(npc_id)
                || !is_observable_room_event(event)
                || (event.actor == "player" && event.turn == self.turn)
            {
                continue;
            }
            let text = unit
                .summary
                .strip_prefix("Room note: ")
                .unwrap_or(unit.summary.as_str())
                .trim()
                .to_string();
            if !text.is_empty() {
                items.push(ObservationItem {
                    seq,
                    actor_label: self.event_actor_label(event),
                    text,
                });
            }
        }
        let present = self.present();
        for (k, d) in &self.pending {
            if k != npc_id && d.seq > seen {
                let witnesses = if d.witnesses.is_empty() {
                    &present
                } else {
                    &d.witnesses
                };
                if !witnesses.contains(npc_id) {
                    continue;
                }
                let text = self.render_npc_turn(k, &d.response, &d.speech, &d.action);
                if !text.is_empty() {
                    items.push(ObservationItem {
                        seq: d.seq,
                        actor_label: self.npc_name(k),
                        text,
                    });
                }
            }
        }
        self.render_observation_digest(items)
    }

    fn render_event(&self, e: &WorldEvent) -> String {
        if e.actor == "player" {
            if !e.speech.is_empty() && !e.action.is_empty() {
                return format!("Player: «{}» [{}]", e.speech, e.action);
            }
            if !e.speech.is_empty() {
                return format!("Player: «{}»", e.speech);
            }
            return if !e.action.is_empty() {
                format!("[{}]", e.action)
            } else {
                String::new()
            };
        }
        if e.kind == "dice" {
            return format!("(roll) {}", e.action);
        }
        self.render_npc_turn(&e.actor, &e.response, &e.speech, &e.action)
    }

    fn render_npc(&self, npc_id: &str, speech: &str, action: &str) -> String {
        if speech.is_empty() && action.is_empty() {
            return String::new();
        }
        let name = self.npc_name(npc_id);
        let sp = if speech.is_empty() {
            String::new()
        } else {
            format!("«{speech}»")
        };
        let ac = if action.is_empty() {
            String::new()
        } else {
            format!(" [{action}]")
        };
        format!("{name}: {sp}{ac}").trim().to_string()
    }

    fn render_npc_turn(&self, npc_id: &str, response: &str, speech: &str, action: &str) -> String {
        let response = response.trim();
        if !response.is_empty() {
            return format!("{}: {response}", self.npc_name(npc_id));
        }
        self.render_npc(npc_id, speech, action)
    }

    fn npc_name(&self, npc_id: &str) -> String {
        self.world
            .npcs
            .get(npc_id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| npc_id.to_string())
    }

    fn event_actor_label(&self, e: &WorldEvent) -> String {
        if e.actor == "player" {
            "Player".to_string()
        } else if e.kind == "dice" {
            "Roll".to_string()
        } else {
            self.npc_name(&e.actor)
        }
    }

    fn render_observation_digest(&self, mut items: Vec<ObservationItem>) -> String {
        items.sort_by_key(|x| x.seq);
        if items.is_empty() {
            return String::new();
        }
        let total = items.len();
        let start = total.saturating_sub(OBS_CAP);
        let tail = &items[start..];
        let folded = total.saturating_sub(tail.len());

        let mut by_actor: BTreeMap<String, Vec<&ObservationItem>> = BTreeMap::new();
        for item in tail {
            by_actor
                .entry(item.actor_label.clone())
                .or_default()
                .push(item);
        }

        let mut out = vec![format!(
            "Compact room note: {} observable beat(s) since you were last caught up.",
            total
        )];
        for (actor, actor_items) in by_actor {
            let actor_total = actor_items.len();
            let latest: Vec<&ObservationItem> = actor_items
                .iter()
                .rev()
                .take(OBS_DIGEST_LINES)
                .rev()
                .copied()
                .collect();
            let latest_text = latest
                .iter()
                .map(|item| clip_chars(&item.text, OBS_LINE_CHARS))
                .collect::<Vec<_>>()
                .join(" / ");
            if actor_total == 1 {
                out.push(format!("- {actor}: {latest_text}"));
            } else {
                out.push(format!(
                    "- {actor}: {actor_total} observable beat(s); latest: {latest_text}"
                ));
            }
        }
        if folded > 0 {
            out.push(format!(
                "Earlier observable beats folded into this note: {folded}."
            ));
        }
        out.join("\n")
    }

    /// `commit_text(npc_id)`.
    pub fn commit_text(&self, npc_id: &str) -> String {
        let blocks = self.commitments.get(npc_id).cloned().unwrap_or_default();
        let tail = if blocks.len() > COMMIT_BLOCKS {
            &blocks[blocks.len() - COMMIT_BLOCKS..]
        } else {
            &blocks[..]
        };
        tail.join("\n")
    }

    /// `commit_turn()`.
    pub fn commit_turn(&mut self) {
        self.commit_turn_without_memory_consolidation();
        self.world.auto_consolidate_memory();
    }

    /// Commit pending NPC/world beats while leaving memory crystal
    /// consolidation to the caller. The live turn loop uses this so semantic
    /// LLM compaction can run asynchronously after all raw memories are present.
    pub fn commit_turn_without_memory_consolidation(&mut self) {
        // Collect pending into a deterministic order (BTreeMap iterates sorted by key —
        // Python dict iterates insertion order; pending is keyed by npc_id and the
        // ordering only affects event-list ordering, which is re-sorted by seq below).
        let pending: Vec<(String, PendingDraft)> = self
            .pending
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (npc_id, d) in pending {
            if d.response.is_empty() && d.speech.is_empty() && d.action.is_empty() {
                continue;
            }
            let witnesses = if d.witnesses.is_empty() {
                self.present()
            } else {
                d.witnesses.clone()
            };
            let event = WorldEvent {
                seq: d.seq,
                turn: self.turn,
                time_minutes: d.time_minutes,
                actor: npc_id.clone(),
                kind: if !d.speech.is_empty() {
                    "speech"
                } else {
                    "action"
                }
                .to_string(),
                response: d.response.clone(),
                beats: d.beats.clone(),
                speech: d.speech.clone(),
                action: d.action.clone(),
                witnesses: witnesses.clone(),
            };
            self.events.push(event.clone());
            self.record_event_memory(&event);
            self.world
                .record_npc_claims(d.seq, self.turn, &npc_id, &d.claims, &witnesses);
            let visible_turn = if d.response.trim().is_empty() {
                [d.action.as_str(), d.speech.as_str()]
                    .into_iter()
                    .filter(|text| !text.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                d.response.clone()
            };
            let mut block = format!("My visible turn: {visible_turn}");
            for c in &d.claims {
                block.push_str(&format!("\n  (relied on: {c})"));
            }
            let entry = self.commitments.entry(npc_id.clone()).or_default();
            entry.push(block);
            if entry.len() > COMMIT_BLOCKS {
                let start = entry.len() - COMMIT_BLOCKS;
                entry.drain(0..start);
            }
            if let (Some(um), Some(am)) = (&d.user_message, &d.assistant_message) {
                self.npc_messages
                    .entry(npc_id.clone())
                    .or_default()
                    .extend([um.clone(), am.clone()]);
            }
            self.world
                .record_rumor(d.seq, self.turn, &npc_id, &d.speech, witnesses, RUMORS_CAP);
            let shown = self
                .shown
                .get(&npc_id)
                .copied()
                .or_else(|| self.delivered.get(&npc_id).copied())
                .unwrap_or(0);
            self.delivered.insert(npc_id.clone(), shown);
            self.remember_npc_client(&npc_id);
        }
        self.events.sort_by_key(|e| e.seq);
        if self.events.len() > EVENTS_CAP {
            let start = self.events.len() - EVENTS_CAP;
            self.events.drain(0..start);
        }
        self.pending.clear();
        self.shown.clear();
    }

    fn record_event_memory(&mut self, event: &WorldEvent) {
        if event.kind == "dice" || event.actor == "gm" {
            return;
        }
        let summary = self.render_event(event);
        if summary.is_empty() {
            return;
        }

        let place_id = self.current_place_id();
        let source_id = format!("world_event_{}", event.seq);
        let mut actor_ids = vec![event.actor.clone()];
        for witness in &event.witnesses {
            if self.world.npcs.contains_key(witness) && !actor_ids.contains(witness) {
                actor_ids.push(witness.clone());
            }
        }

        let details = format!(
            "Scene event seq {}; turn {}; actor {}; kind {}; witnesses: {}",
            event.seq,
            event.turn,
            event.actor,
            event.kind,
            event
                .witnesses
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );

        for witness in &event.witnesses {
            if !self.world.npcs.contains_key(witness) {
                continue;
            }
            let mut unit = MemoryUnit {
                tier: MemoryTier::Raw,
                owner_scope: format!("actor:{witness}"),
                summary: format!("Observed in scene: {summary}"),
                details: details.clone(),
                source_event_ids: vec![source_id.clone()],
                time_start: event.time_minutes,
                time_end: event.time_minutes,
                actor_ids: actor_ids.clone(),
                topic_tags: vec![event.kind.clone(), "scene_observation".to_string()],
                truth_status: MemoryTruthStatus::Actual,
                created_by: "scene_observer".to_string(),
                ..Default::default()
            };
            if !place_id.is_empty() {
                unit.place_ids = vec![place_id.clone()];
            }
            self.world.add_memory_unit(unit);
        }
        let room_visible = self
            .canonical_room_witnesses(&place_id)
            .is_subset(&event.witnesses);
        if !place_id.is_empty() && room_visible {
            self.world.add_memory_unit(MemoryUnit {
                tier: MemoryTier::Raw,
                owner_scope: format!("place:{place_id}"),
                visibility_scopes: vec![format!("place:{place_id}")],
                summary: format!("Room note: {summary}"),
                details: details.clone(),
                source_event_ids: vec![source_id.clone()],
                time_start: event.time_minutes,
                time_end: event.time_minutes,
                place_ids: vec![place_id.clone()],
                actor_ids: actor_ids.clone(),
                topic_tags: vec![event.kind.clone(), "room_note".to_string()],
                truth_status: MemoryTruthStatus::Actual,
                created_by: "room_observer".to_string(),
                ..Default::default()
            });
        }
        if event.actor == "player" || event.witnesses.contains("player") {
            let mut unit = MemoryUnit {
                tier: MemoryTier::Raw,
                owner_scope: "player".to_string(),
                visibility_scopes: vec!["player".to_string()],
                summary: format!("Player memory: {summary}"),
                details,
                source_event_ids: vec![source_id],
                time_start: event.time_minutes,
                time_end: event.time_minutes,
                actor_ids,
                topic_tags: vec![event.kind.clone(), "player_memory".to_string()],
                truth_status: MemoryTruthStatus::Actual,
                created_by: "player_observer".to_string(),
                ..Default::default()
            };
            if !place_id.is_empty() {
                unit.place_ids = vec![place_id];
            }
            self.world.add_memory_unit(unit);
        }
    }

    fn current_place_id(&self) -> String {
        if !self.world.world_canon.player_place_id.is_empty() {
            return self.world.world_canon.player_place_id.clone();
        }
        self.world.scene.location_id.clone()
    }

    fn canonical_room_witnesses(&self, place_id: &str) -> BTreeSet<String> {
        let actors_at_place = self.world.world_canon.actors_at(place_id);
        if actors_at_place.is_empty() {
            return self.present();
        }
        let mut witnesses = BTreeSet::new();
        witnesses.insert("player".to_string());
        for actor in actors_at_place {
            witnesses.insert(actor.actor_id.clone());
        }
        witnesses
    }
}

/// Add `delta` to the integer counter `key` in `usage`.
fn bump(usage: &mut Map<String, Value>, key: &str, delta: i64) {
    let cur = usage.get(key).and_then(|v| v.as_i64()).unwrap_or(0);
    usage.insert(key.to_string(), json!(cur + delta));
}

fn clip_chars(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let keep = limit.saturating_sub(3);
    let mut out: String = text.chars().take(keep).collect();
    out.push_str("...");
    out
}

fn memory_source_event_seq(unit: &MemoryUnit) -> Option<i64> {
    unit.source_event_ids.iter().find_map(|source| {
        source
            .strip_prefix("world_event_")
            .and_then(|raw| raw.parse::<i64>().ok())
    })
}

fn format_duration(minutes: i64, time: &gml_world::WorldTime) -> String {
    let minutes_per_hour = time.minutes_per_hour.max(1);
    let hours_per_day = time.hours_per_day.max(1);
    let minutes_per_day = minutes_per_hour * hours_per_day;
    let days = minutes / minutes_per_day;
    let rem_after_days = minutes % minutes_per_day;
    let hours = rem_after_days / minutes_per_hour;
    let mins = rem_after_days % minutes_per_hour;

    if days > 0 {
        if hours > 0 {
            format!("{days} day(s) {hours} hour(s)")
        } else {
            format!("{days} day(s)")
        }
    } else if hours > 0 {
        if mins > 0 {
            format!("{hours} hour(s) {mins} minute(s)")
        } else {
            format!("{hours} hour(s)")
        }
    } else {
        format!("{mins} minute(s)")
    }
}
