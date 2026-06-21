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
use gml_world::{World, WorldEvent};

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

/// `_OBS_CAP = 12`.
pub const OBS_CAP: usize = 12;
/// `_COMMIT_BLOCKS = 8`.
pub const COMMIT_BLOCKS: usize = 8;
/// `config.EVENTS_CAP = 400`.
pub const EVENTS_CAP: usize = 400;
/// `config.GM_RUMORS_CAP = 80`.
pub const RUMORS_CAP: usize = 80;

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
    pub speech: String,
    pub action: String,
    pub claims: Vec<String>,
    pub witnesses: BTreeSet<String>,
    pub user_message: Option<Value>,
    pub assistant_message: Option<Value>,
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
    pub turn_player_event: Option<usize>, // index into `events`

    /// Compaction thresholds (the Rust home for the `config.*` globals Python
    /// reads at call time in `_maybe_compact*` / `context_usage`). Not persisted.
    pub compaction: CompactionThresholds,
}

impl Session {
    /// `Session(client, world=None)`. The default world is the default story.
    /// Uses the default (mock) NPC client factory.
    pub fn new(client: Arc<dyn Backend>) -> Self {
        let world = World::from_seed(&gml_stories::default_story_seed());
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
                let _ = self.world.set_npc_presence(npc_id, requested, "", true, true, "", "");
            }
        }
        if let Some(Value::Object(wb)) = data.get("whereabouts") {
            let _ = self.world.set_npc_whereabouts(
                npc_id,
                wb.get("location_id").and_then(Value::as_str).unwrap_or(""),
                wb.get("location_name").and_then(Value::as_str).unwrap_or(""),
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
        for state in self.npc_client_state.values_mut() {
            state.model = model.to_string();
        }
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
            + turn_total.get("secs").and_then(|v| v.as_f64()).unwrap_or(0.0);
        usage.insert("secs".to_string(), json!(crate::compact::round2(secs)));
        let peak_now = usage.get("peak_context").and_then(|v| v.as_i64()).unwrap_or(0);
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
        let summary = self.npc_summaries.get(npc_id).map(|s| s.trim()).unwrap_or("");
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

    /// `record_public(actor, kind, speech, action)`.
    pub fn record_public(&mut self, actor: &str, kind: &str, speech: &str, action: &str) {
        let seq = self.next_seq();
        let witnesses = self.present();
        self.events.push(WorldEvent {
            seq,
            turn: self.turn,
            actor: actor.to_string(),
            kind: kind.to_string(),
            speech: speech.to_string(),
            action: action.to_string(),
            witnesses,
        });
    }

    /// `record_player_for(npc_id)`.
    pub fn record_player_for(&mut self, npc_id: &str) -> BTreeSet<String> {
        let mut witnesses: BTreeSet<String> = BTreeSet::new();
        witnesses.insert("player".to_string());
        witnesses.insert(npc_id.to_string());
        match self.turn_player_event {
            None => {
                let seq = self.next_seq();
                let ev = WorldEvent {
                    seq,
                    turn: self.turn,
                    actor: "player".to_string(),
                    kind: "speech".to_string(),
                    speech: self.last_player_action.clone(),
                    action: String::new(),
                    witnesses: witnesses.clone(),
                };
                self.events.push(ev);
                self.turn_player_event = Some(self.events.len() - 1);
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
        if speech.is_empty() && action.is_empty() {
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
        let mut items: Vec<(i64, String)> = Vec::new();
        for e in &self.events {
            if e.seq <= seen || !e.witnesses.contains(npc_id) || e.actor == npc_id {
                continue;
            }
            if e.actor == "player" && e.turn == self.turn {
                continue;
            }
            items.push((e.seq, self.render_event(e)));
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
                items.push((d.seq, self.render_npc(k, &d.speech, &d.action)));
            }
        }
        items.sort_by_key(|x| x.0);
        let lines: Vec<String> = items.into_iter().map(|(_, r)| r).filter(|r| !r.is_empty()).collect();
        let tail = if lines.len() > OBS_CAP {
            &lines[lines.len() - OBS_CAP..]
        } else {
            &lines[..]
        };
        tail.join("\n")
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
        self.render_npc(&e.actor, &e.speech, &e.action)
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

    fn npc_name(&self, npc_id: &str) -> String {
        self.world
            .npcs
            .get(npc_id)
            .map(|n| n.name.clone())
            .unwrap_or_else(|| npc_id.to_string())
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
        // Collect pending into a deterministic order (BTreeMap iterates sorted by key —
        // Python dict iterates insertion order; pending is keyed by npc_id and the
        // ordering only affects event-list ordering, which is re-sorted by seq below).
        let pending: Vec<(String, PendingDraft)> =
            self.pending.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        for (npc_id, d) in pending {
            if d.speech.is_empty() && d.action.is_empty() {
                continue;
            }
            let witnesses = if d.witnesses.is_empty() {
                self.present()
            } else {
                d.witnesses.clone()
            };
            self.events.push(WorldEvent {
                seq: d.seq,
                turn: self.turn,
                actor: npc_id.clone(),
                kind: if !d.speech.is_empty() { "speech" } else { "action" }.to_string(),
                speech: d.speech.clone(),
                action: d.action.clone(),
                witnesses: witnesses.clone(),
            });
            let mut block = format!(
                "Я сказал: {}; сделал: {}",
                if d.speech.is_empty() { "—" } else { &d.speech },
                if d.action.is_empty() { "—" } else { &d.action }
            );
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
