//! Contract tests for the per-NPC identity / memory lifecycle and debug edits,
//! ported from `gm-lab/test_contracts.py`:
//!   - reset_npc_memory drops npc_messages/summaries/client_state + live client +
//!     commitments + pending, and PINS delivered/shown to the current seq
//!     (Python ≈ 2536-2575),
//!   - reset does not resurface old observations (Python ≈ 2578-2589),
//!   - reset_npc_memory contract: True for any real NPC (incl. commitments-only),
//!     False for unknown/empty id, and no leak into delivered/shown on reject,
//!   - apply_debug_edit presence/visibility guard + reset-only-on-flag
//!     (Python ≈ 2591-2613),
//!   - per-NPC prompt-cache identity round-trip via ensure_npc_client +
//!     set_session_identity (PORT_PLAN §4.5; the orchestrator-layer half of the
//!     thread_id/session_id lifecycle — the codex prompt_cache_key derivation is
//!     covered in gml-codex).

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use gml_llm::backend::{
    Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
};
use gml_llm::{mock_stats, MockClient};
use gml_orchestrator::session::default_client_factory;
use gml_orchestrator::Session;

fn session() -> Session {
    let client: Arc<dyn Backend> = Arc::new(MockClient::new());
    Session::new(client)
}

// =========================================================================
// reset_npc_memory: drops everything for the chosen NPC, pins delivered/shown
// =========================================================================

#[test]
fn reset_npc_memory_drops_state_and_pins_boundaries() {
    let mut s = session();
    s.seq = 9; // current shared-log boundary

    for id in ["borin", "lysa"] {
        s.npc_messages
            .insert(id.to_string(), vec![json!({"role": "user", "content": format!("hi {id}")})]);
        s.npc_summaries.insert(id.to_string(), format!("summary-{id}"));
        s.npc_client_state.insert(
            id.to_string(),
            gml_orchestrator::NpcClientState {
                model: String::new(),
                session_id: String::new(),
                thread_id: format!("thread-{id}"),
            },
        );
        s.npc_clients.insert(id.to_string(), Arc::new(MockClient::new()) as Arc<dyn Backend>);
        s.commitments.insert(id.to_string(), vec![format!("commit-{id}")]);
        s.delivered.insert(id.to_string(), 5);
        s.shown.insert(id.to_string(), 3);
        // pending draft for the NPC
        s.draft(
            id,
            "x",
            "",
            vec![],
            Some(json!({"role": "user", "content": "u"})),
            Some(json!({"role": "assistant", "content": "a"})),
            Some(BTreeSet::from(["player".to_string(), id.to_string()])),
        );
    }

    assert!(s.reset_npc_memory("borin"));
    // All private stores for borin are gone.
    assert!(!s.npc_messages.contains_key("borin"));
    assert!(!s.npc_summaries.contains_key("borin"));
    assert!(!s.npc_client_state.contains_key("borin"));
    assert!(!s.npc_clients.contains_key("borin"));
    assert!(!s.commitments.contains_key("borin"));
    assert!(!s.pending.contains_key("borin"));
    // delivered/shown PINNED to current seq (not deleted) so old events do not
    // resurface as new after reset.
    assert_eq!(s.delivered.get("borin").copied(), Some(s.seq));
    assert_eq!(s.shown.get("borin").copied(), Some(s.seq));

    // lysa is completely untouched.
    assert_eq!(
        s.npc_messages.get("lysa").cloned().unwrap(),
        vec![json!({"role": "user", "content": "hi lysa"})]
    );
    assert_eq!(s.npc_summaries.get("lysa").map(|x| x.as_str()), Some("summary-lysa"));
    assert_eq!(s.npc_client_state.get("lysa").map(|st| st.thread_id.as_str()), Some("thread-lysa"));
    assert!(s.npc_clients.contains_key("lysa"));
    assert_eq!(s.commitments.get("lysa").cloned().unwrap(), vec!["commit-lysa"]);
    assert_eq!(s.delivered.get("lysa").copied(), Some(5));
    assert_eq!(s.shown.get("lysa").copied(), Some(3));
    assert!(s.pending.contains_key("lysa"));
}

#[test]
fn reset_npc_memory_return_contract() {
    let mut s = session();
    // Unknown / empty id -> False, and no leak into delivered/shown.
    assert!(!s.reset_npc_memory("nonexistent"));
    assert!(!s.delivered.contains_key("nonexistent"));
    assert!(!s.shown.contains_key("nonexistent"));
    assert!(!s.reset_npc_memory(""));

    // Any real NPC with no prior memory -> True.
    assert!(Session::new(Arc::new(MockClient::new())).reset_npc_memory("mareth"));

    // A valid NPC with ONLY commitments/pending still mutates -> True.
    let mut co = session();
    co.commitments.insert("borin".to_string(), vec!["block".to_string()]);
    co.draft(
        "borin",
        "y",
        "",
        vec![],
        Some(json!({"role": "user", "content": "u"})),
        Some(json!({"role": "assistant", "content": "a"})),
        Some(BTreeSet::from(["player".to_string(), "borin".to_string()])),
    );
    assert!(co.reset_npc_memory("borin"));
    assert!(!co.commitments.contains_key("borin"));
    assert!(!co.pending.contains_key("borin"));
}

#[test]
fn reset_npc_memory_does_not_resurface_old_observations() {
    let mut s = session();
    s.turn = 1;
    s.last_player_action = "old talk".to_string();
    s.record_player_for("borin");
    s.snapshot_shown("borin");
    s.draft(
        "borin",
        "привет",
        "",
        vec![],
        None,
        None,
        Some(BTreeSet::from(["player".to_string(), "borin".to_string()])),
    );
    s.commit_turn();
    s.turn = 2;
    assert_eq!(s.observations("borin"), "");
    s.reset_npc_memory("borin");
    assert_eq!(s.observations("borin"), "");
}

// =========================================================================
// apply_debug_edit: presence/visibility guard, reset-only-on-flag
// =========================================================================

#[test]
fn apply_debug_edit_presence_visibility_guard_and_reset_flag() {
    let mut s = session();
    s.world.scene.presence.get_mut("borin").unwrap().visible = false;
    s.world.scene.presence.get_mut("borin").unwrap().can_hear = false;

    // Card-only edit (no `present` key) must not flip presence/visibility.
    assert!(s.apply_debug_edit("borin", &json!({"fields": {"persona": "переписанное описание"}})));
    assert!(!s.world.scene.presence["borin"].visible);
    assert!(!s.world.scene.presence["borin"].can_hear);
    assert!(!s.world.npc_can_react("borin"));

    // Same-state present=true (already present) is a guarded no-op for visibility.
    assert!(s.apply_debug_edit("borin", &json!({"present": true})));
    assert!(!s.world.scene.presence["borin"].visible);

    // A genuine present-state CHANGE toggles presence.
    assert!(s.apply_debug_edit("borin", &json!({"present": false})));
    assert!(!s.world.scene.present_npcs.contains("borin"));
    assert!(s.apply_debug_edit("borin", &json!({"present": true})));
    assert!(s.world.scene.present_npcs.contains("borin"));

    // Unknown id is rejected without mutation.
    assert!(!s.apply_debug_edit("not_real", &json!({"fields": {"persona": "x"}})));

    // reset_memory via the edit path clears the chosen NPC's memory.
    s.npc_messages
        .insert("borin".to_string(), vec![json!({"role": "user", "content": "hi"})]);
    assert!(s.apply_debug_edit("borin", &json!({"reset_memory": true})));
    assert!(!s.npc_messages.contains_key("borin"));
}

// =========================================================================
// per-NPC prompt-cache identity round-trip (PORT_PLAN §4.5)
// =========================================================================

/// A backend that honors `set_session_identity` and reports back its ids — the
/// stand-in for the Codex client whose `thread_id` keys the prompt cache.
struct IdentityClient {
    model: Mutex<String>,
    session_id: Mutex<String>,
    thread_id: Mutex<String>,
}

impl IdentityClient {
    fn new(default_thread: &str) -> Self {
        IdentityClient {
            model: Mutex::new("mock".to_string()),
            session_id: Mutex::new(String::new()),
            thread_id: Mutex::new(default_thread.to_string()),
        }
    }
}

#[async_trait]
impl Backend for IdentityClient {
    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }
    fn set_model(&self, m: &str) {
        let m = m.trim();
        if !m.is_empty() {
            *self.model.lock().unwrap() = m.to_string();
        }
    }
    fn set_session_identity(&self, session_id: Option<&str>, thread_id: Option<&str>) {
        // Override only non-empty values (faithful to the codex client).
        if let Some(sid) = session_id {
            if !sid.trim().is_empty() {
                *self.session_id.lock().unwrap() = sid.to_string();
            }
        }
        if let Some(tid) = thread_id {
            if !tid.trim().is_empty() {
                *self.thread_id.lock().unwrap() = tid.to_string();
            }
        }
    }
    fn session_id(&self) -> String {
        self.session_id.lock().unwrap().clone()
    }
    fn thread_id(&self) -> String {
        self.thread_id.lock().unwrap().clone()
    }
    async fn list_models(&self) -> Vec<Value> {
        vec![]
    }
    async fn chat(
        &self,
        _m: &Value,
        _t: Option<&Value>,
        _th: Option<bool>,
        _r: &str,
    ) -> Result<ChatOutput, BackendError> {
        Ok(ChatOutput {
            thinking: String::new(),
            content: String::new(),
            calls: vec![],
            assistant_msg: json!({"role": "assistant", "content": ""}),
        })
    }
    async fn chat_json(
        &self,
        _m: &Value,
        _s: &Value,
        _th: Option<bool>,
        _r: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        Ok(Map::new())
    }
    async fn summarize(&self, _t: &str, _p: &[String]) -> Result<String, BackendError> {
        Ok(String::new())
    }
    async fn chat_stream(
        &self,
        _m: &Value,
        _t: Option<&Value>,
        _th: Option<bool>,
        _r: &str,
        _s: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        Ok(ChatStreamOutput {
            thinking: String::new(),
            content: String::new(),
            calls: vec![],
            assistant_msg: json!({"role": "assistant", "content": ""}),
            stats: mock_stats(),
        })
    }
    async fn chat_json_stream(
        &self,
        _m: &Value,
        _s: &Value,
        _th: Option<bool>,
        _r: &str,
        _sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        Ok(JsonStreamOutput {
            data: Map::new(),
            stats: mock_stats(),
        })
    }
}

#[test]
fn ensure_npc_client_restores_persisted_identity() {
    // Factory builds IdentityClients with a unique default thread per call.
    let counter = Arc::new(Mutex::new(0u32));
    let c2 = counter.clone();
    let factory: gml_orchestrator::ClientFactory = Arc::new(move || {
        let mut n = c2.lock().unwrap();
        *n += 1;
        Arc::new(IdentityClient::new(&format!("fresh-thread-{n}"))) as Arc<dyn Backend>
    });
    let world = gml_world::World::from_seed(&gml_stories::default_story_seed());
    let mut s = Session::with_world(Arc::new(MockClient::new()), world, factory);

    // Pre-seed a persisted identity for borin (as if restored from a snapshot).
    s.npc_client_state.insert(
        "borin".to_string(),
        gml_orchestrator::NpcClientState {
            model: String::new(),
            session_id: "restored-session".to_string(),
            thread_id: "restored-thread".to_string(),
        },
    );

    // First ensure builds the client and restores the persisted identity.
    let client = s.ensure_npc_client("borin").expect("npc client");
    assert_eq!(client.thread_id(), "restored-thread");
    assert_eq!(client.session_id(), "restored-session");
    // remember_npc_client wrote the live ids back into the serializable state.
    assert_eq!(s.npc_client_state["borin"].thread_id, "restored-thread");
    assert_eq!(s.npc_client_state["borin"].session_id, "restored-session");

    // Subsequent ensure returns the SAME live client (stable thread/cache key).
    let again = s.ensure_npc_client("borin").expect("npc client");
    assert_eq!(again.thread_id(), "restored-thread");

    // A different NPC with no persisted identity gets a fresh thread.
    let lysa = s.ensure_npc_client("lysa").expect("npc client");
    assert!(lysa.thread_id().starts_with("fresh-thread-"));
    assert_ne!(lysa.thread_id(), "restored-thread");
}

#[test]
fn reset_npc_memory_drops_live_client_for_fresh_thread() {
    let counter = Arc::new(Mutex::new(0u32));
    let c2 = counter.clone();
    let factory: gml_orchestrator::ClientFactory = Arc::new(move || {
        let mut n = c2.lock().unwrap();
        *n += 1;
        Arc::new(IdentityClient::new(&format!("fresh-thread-{n}"))) as Arc<dyn Backend>
    });
    let world = gml_world::World::from_seed(&gml_stories::default_story_seed());
    let mut s = Session::with_world(Arc::new(MockClient::new()), world, factory);

    let first = s.ensure_npc_client("borin").expect("client").thread_id();
    // reset drops the live client + serialized identity -> next ensure makes a
    // brand-new thread (a fresh prompt_cache_key).
    assert!(s.reset_npc_memory("borin"));
    assert!(!s.npc_clients.contains_key("borin"));
    assert!(!s.npc_client_state.contains_key("borin"));
    let second = s.ensure_npc_client("borin").expect("client").thread_id();
    assert_ne!(first, second, "reset must yield a fresh thread/cache key");
}

#[test]
fn default_factory_is_constructible() {
    // Sanity: the default (mock) NPC client factory builds a usable backend.
    let f = default_client_factory();
    let c = f();
    assert_eq!(c.model(), "mock");
}
