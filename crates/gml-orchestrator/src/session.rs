//! `Session` — the party state between turns, ported from `class Session` in
//! `orchestrator.py`.
//!
//! Field set + shapes are pinned to the persistence payload
//! (`tests/reference/persistence/chat_payload.json -> "session"`) so
//! gml-persistence can round-trip them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use serde_json::{json, Map, Value};

use gml_llm::{Backend, MockClient};
use gml_types::NpcBeat;
use gml_world::{MemoryTier, MemoryTruthStatus, MemoryUnit, World, WorldEvent, WorldSpec};

use crate::compact::{empty_usage, usage_from_payload};

/// A synchronous factory for per-NPC clients — the Rust stand-in for Python's
/// module-level `make_client()`. `ensure_npc_client` calls this to spin up a
/// fresh client/thread (so each NPC has its own prompt-cache key).
pub type ClientFactory = std::sync::Arc<dyn Fn() -> Arc<dyn Backend> + Send + Sync>;

/// The default factory builds a [`MockClient`] (matches `GM_BACKEND=mock`).
/// Real backends supply their own factory via [`Session::with_factory`].
pub fn default_client_factory() -> ClientFactory {
    std::sync::Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>)
}

fn compact_location_generator_request(request: &Value) -> Value {
    let mut out = Map::new();
    for key in [
        "purpose",
        "target_place_id",
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
    for key in ["player_observed", "enter_after_commit"] {
        if let Some(value) = request.get(key).filter(|v| !v.is_null()) {
            out.insert(key.to_string(), value.clone());
        }
    }
    insert_clipped_string(&mut out, "request", request.get("request"));
    Value::Object(out)
}

fn compact_location_generator_result(generated: &Value) -> Value {
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
    if let Some(Value::Array(transitions)) = generated.get("transitions") {
        let rows = transitions
            .iter()
            .filter_map(|transition| {
                let row = transition.as_object()?;
                let mut compact = Map::new();
                for key in ["label", "destination_hint", "kind", "risk"] {
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
    pub client_backend: String,
    pub client_model: String,
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

    pub world: World,
    pub gm_messages: Vec<Value>,
    pub gm_summary: String,
    pub npc_messages: BTreeMap<String, Vec<Value>>,
    pub npc_summaries: BTreeMap<String, String>,
    pub loaded_gm_tools: BTreeSet<String>,
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
    pub turn_player_event: Option<usize>, // index into `events`

    /// Compaction thresholds (the Rust home for the `config.*` globals Python
    /// reads at call time in `_maybe_compact*` / `context_usage`). Not persisted.
    pub compaction: CompactionThresholds,
}

impl Session {
    /// `Session(client, world=None)`. The default world is procedural canon.
    /// Uses the default (mock) NPC client factory.
    pub fn new(client: Arc<dyn Backend>) -> Self {
        let world = World::from_worldgen(&WorldSpec::default());
        Self::with_world(client, world, default_client_factory())
    }

    /// `Session(client, world=...)` with an explicit NPC client factory.
    pub fn with_world(
        client: Arc<dyn Backend>,
        world: World,
        npc_client_factory: ClientFactory,
    ) -> Self {
        let client_model = client.model();
        Session {
            client,
            npc_client_factory,
            client_backend: backend_name(),
            client_model,
            client_session_id: String::new(),
            client_thread_id: String::new(),
            npc_clients: HashMap::new(),
            npc_client_state: BTreeMap::new(),
            location_generator_client: None,
            location_generator_client_state: NpcClientState::default(),
            location_generator_anti_repeat: Vec::new(),
            location_generator_messages: Vec::new(),
            world,
            gm_messages: Vec::new(),
            gm_summary: String::new(),
            npc_messages: BTreeMap::new(),
            npc_summaries: BTreeMap::new(),
            loaded_gm_tools: BTreeSet::new(), // set below via configure_loaded_tools
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

    pub fn reset_world_query_cache(&mut self) {
        self.world_query_seen = BTreeMap::new();
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
        } else if !self.client_model.is_empty() {
            self.client_model.clone()
        } else {
            client.model()
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
        } else if !self.client_model.is_empty() {
            self.client_model.clone()
        } else {
            client.model()
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
                self.client_model.clone()
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

    /// `remember_npc_client(npc_id)`.
    pub fn remember_npc_client(&mut self, npc_id: &str) {
        let client = match self.npc_clients.get(npc_id) {
            Some(c) => c.clone(),
            None => return,
        };
        let model = {
            let m = client.model();
            if m.is_empty() {
                self.client_model.clone()
            } else {
                m
            }
        };
        // Capture the real per-NPC session/thread ids so the Codex prompt-cache
        // key survives save/restore (orchestrator.py:2484-2488). Non-Codex
        // backends return "" (their cache is not keyed on a thread id).
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
        if model.is_empty() {
            return;
        }
        self.client_model = model.to_string();
        self.client.set_model(model);
        for client in self.npc_clients.values() {
            client.set_model(model);
        }
        if let Some(client) = &self.location_generator_client {
            client.set_model(model);
        }
        for state in self.npc_client_state.values_mut() {
            state.model = model.to_string();
        }
        self.location_generator_client_state.model = model.to_string();
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
            parts.push(format!("Сжатая память:\n{summary}"));
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
                    rendered.push(format!("Прошлая ситуация для NPC:\n{historical}"));
                } else if role == "assistant" {
                    rendered.push(format!("Ответ {name}:\n{}", content.trim()));
                } else {
                    rendered.push(format!("{role}:\n{}", content.trim()));
                }
            }
            parts.push(format!("Последние сообщения:\n{}", rendered.join("\n\n")));
        }
        if parts.is_empty() {
            "История NPC пока пустая.".to_string()
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
            let mut block = format!("Мой видимый ход: {visible_turn}");
            for c in &d.claims {
                block.push_str(&format!("\n  (опираюсь на: {c})"));
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

/// `config.BACKEND` — the active backend name. For the mock test harness this is
/// "mock"; in production it comes from `GM_BACKEND` (read at process start).
fn backend_name() -> String {
    std::env::var("GM_BACKEND").unwrap_or_else(|_| "codex".to_string())
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
