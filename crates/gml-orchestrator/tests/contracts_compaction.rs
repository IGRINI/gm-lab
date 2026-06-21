//! Contract tests for GM + NPC history compaction, ported from the compaction
//! blocks of `gm-lab/test_contracts.py`:
//!   - the query-cache-reset-after-GM-compaction block (Python ≈ 1369-1396),
//!   - the GM `_msg_text_for_summary` PLAYER-ACTION strip vs NPC plain `_msg_text`
//!     asymmetry (Python `_msg_text_for_summary` usage ≈ 337-341),
//!   - the trigger conditions (token gate AND boundary gate) for both GM and NPC,
//!   - keep-last-N verbatim + old/prior-summary summarized,
//!   - COMPACT_INPUT_CHARS clip BY CHARS,
//!   - reset_world_query_cache fires AFTER GM compaction only (not NPC).
//!
//! The project owner explicitly requires that compaction/compression never break,
//! so these are the load-bearing tests.
//!
//! Python forces the thresholds by monkeypatching `config.GM_HISTORY_TOKENS = 1`
//! / `config.GM_KEEP_TURNS = 1` (and NPC equivalents). The Rust home for those
//! call-time globals is `session.compaction` (a `CompactionThresholds`, defaulting
//! to the production `config` defaults). Lowering them on a test session is the
//! exact equivalent of the Python monkeypatch — production `Config` defaults are
//! never touched.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Map, Value};

use gml_llm::backend::{
    Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
};
use gml_llm::{mock_stats, MockClient};
use gml_orchestrator::compact::{maybe_compact, maybe_compact_npc, msg_text, msg_text_for_summary};
use gml_orchestrator::worldstate::{apply_world_state_batch, query_world_state};
use gml_orchestrator::Session;

/// A backend whose `summarize` returns a fixed marker and whose NPC-history
/// compaction `chat` returns a fixed marker — the analogue of Python's
/// `QueryCacheCompactClient` (and the NPC compaction client).
struct MarkerCompactClient {
    summary: String,
    npc_summary: String,
}

impl MarkerCompactClient {
    fn new() -> Self {
        MarkerCompactClient {
            summary: "compact summary".to_string(),
            npc_summary: "npc compact summary".to_string(),
        }
    }
}

#[async_trait]
impl Backend for MarkerCompactClient {
    fn model(&self) -> String {
        "mock".to_string()
    }
    fn set_model(&self, _model: &str) {}
    async fn list_models(&self) -> Vec<Value> {
        vec![]
    }
    async fn chat(
        &self,
        _messages: &Value,
        _tools: Option<&Value>,
        _think: Option<bool>,
        _reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        // `_summarize_npc_history` calls `client.chat(...)` and uses `.content`.
        Ok(ChatOutput {
            thinking: String::new(),
            content: self.npc_summary.clone(),
            calls: Vec::new(),
            assistant_msg: json!({"role": "assistant", "content": self.npc_summary}),
        })
    }
    async fn chat_json(
        &self,
        _messages: &Value,
        _schema: &Value,
        _think: Option<bool>,
        _reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        Ok(Map::new())
    }
    async fn summarize(
        &self,
        _text: &str,
        _proper_nouns: &[String],
    ) -> Result<String, BackendError> {
        Ok(self.summary.clone())
    }
    async fn chat_stream(
        &self,
        _messages: &Value,
        _tools: Option<&Value>,
        _think: Option<bool>,
        _reasoning_role: &str,
        _sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        Ok(ChatStreamOutput {
            thinking: String::new(),
            content: String::new(),
            calls: Vec::new(),
            assistant_msg: json!({"role": "assistant", "content": ""}),
            stats: mock_stats(),
        })
    }
    async fn chat_json_stream(
        &self,
        _messages: &Value,
        _schema: &Value,
        _think: Option<bool>,
        _reasoning_role: &str,
        _sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        Ok(JsonStreamOutput {
            data: Map::new(),
            stats: mock_stats(),
        })
    }
}

fn tokio_block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(fut)
}

fn session() -> Session {
    let client: Arc<dyn Backend> = Arc::new(MockClient::new());
    Session::new(client)
}

fn umsg(content: &str) -> Value {
    json!({"role": "user", "content": content})
}
fn amsg(content: &str) -> Value {
    json!({"role": "assistant", "content": content})
}

// =========================================================================
// GM compaction: query-cache reset AFTER GM compaction (Python ≈ 1369-1396)
// =========================================================================

#[test]
fn gm_compaction_resets_world_query_cache_and_keeps_records() {
    let mut s = session();

    // Store a gm-scope record and deliver it once so the world-query cache is
    // populated (Python primes `world_query_seen` via a query before compacting).
    let _ = apply_world_state_batch(
        &mut s,
        &json!({"items": [{
            "type": "fact",
            "text": "QUERY_CACHE_SENTINEL первая улика для проверки выдачи.",
            "scope": "gm",
        }]}),
    );
    let first = query_world_state(&mut s, &json!({"scope": "gm", "query": "QUERY_CACHE_SENTINEL"}));
    assert_eq!(first["status"], "known");
    assert!(!s.world_query_seen.is_empty(), "world_query_seen should be primed");

    // Force the GM compaction thresholds down (Python: config.GM_HISTORY_TOKENS=1,
    // config.GM_KEEP_TURNS=1) and stuff the GM history with >1 user boundary.
    s.compaction.gm_history_tokens = 1;
    s.compaction.gm_keep_turns = 1;
    let client: Arc<dyn Backend> = Arc::new(MarkerCompactClient::new());
    s.client = client.clone();
    s.gm_messages = vec![
        umsg(&"старый ход ".repeat(20)),
        amsg(&"старый ответ ".repeat(20)),
        umsg(&"новый ход ".repeat(20)),
    ];

    assert!(!s.world_query_seen.is_empty());
    tokio_block_on(maybe_compact(&mut s, client.as_ref()));
    // reset_world_query_cache fires AFTER GM compaction.
    assert!(s.world_query_seen.is_empty(), "GM compaction must reset world_query_seen");

    // Summary applied; only the last GM_KEEP_TURNS=1 user-boundary kept verbatim.
    assert_eq!(s.gm_summary, "compact summary");
    assert_eq!(s.gm_messages.len(), 1, "keep-last-1 verbatim");
    assert_eq!(
        s.gm_messages[0]["content"].as_str().unwrap(),
        "новый ход ".repeat(20)
    );

    // The stored gm record survives compaction and is deliverable again.
    let after = query_world_state(&mut s, &json!({"scope": "gm", "query": "QUERY_CACHE_SENTINEL"}));
    assert_eq!(after["status"], "known");
    assert!(serde_json::to_string(&after)
        .unwrap()
        .contains("QUERY_CACHE_SENTINEL первая"));
}

// =========================================================================
// GM compaction trigger conditions: BOTH gates required
// =========================================================================

#[test]
fn gm_compaction_requires_token_gate_and_boundary_gate() {
    // (a) Token gate not met -> no compaction even with many boundaries.
    let mut s = session();
    let client: Arc<dyn Backend> = Arc::new(MarkerCompactClient::new());
    s.client = client.clone();
    s.compaction.gm_history_tokens = 1_000_000; // unreachable
    s.compaction.gm_keep_turns = 1;
    s.gm_messages = vec![umsg("a"), umsg("b"), umsg("c")];
    let before = s.gm_messages.clone();
    tokio_block_on(maybe_compact(&mut s, client.as_ref()));
    assert_eq!(s.gm_messages, before, "no compaction when token gate not met");
    assert_eq!(s.gm_summary, "");

    // (b) Boundary gate not met -> no compaction even past the token threshold.
    // Python: `if len(starts) <= GM_KEEP_TURNS: return`. With keep_turns=3 and
    // only 3 user-boundaries, the gate is NOT exceeded.
    let mut s = session();
    let client: Arc<dyn Backend> = Arc::new(MarkerCompactClient::new());
    s.client = client.clone();
    s.compaction.gm_history_tokens = 1;
    s.compaction.gm_keep_turns = 3;
    s.gm_messages = vec![
        umsg(&"x ".repeat(50)),
        umsg(&"y ".repeat(50)),
        umsg(&"z ".repeat(50)),
    ];
    let before = s.gm_messages.clone();
    tokio_block_on(maybe_compact(&mut s, client.as_ref()));
    assert_eq!(s.gm_messages, before, "no compaction when boundaries <= keep_turns");
    assert_eq!(s.gm_summary, "");
}

// =========================================================================
// GM compaction folds old + prior summary, keeps last N user-boundaries
// =========================================================================

#[test]
fn gm_compaction_keeps_last_n_and_folds_old_plus_prior_summary() {
    let mut s = session();
    let client: Arc<dyn Backend> = Arc::new(MarkerCompactClient::new());
    s.client = client.clone();
    s.compaction.gm_history_tokens = 1;
    s.compaction.gm_keep_turns = 2;
    s.gm_summary = "PRIOR_SUMMARY".to_string();
    // 4 user-boundaries; keep the last 2 verbatim.
    s.gm_messages = vec![
        umsg("U1 first old"),
        amsg("A1 old reply"),
        umsg("U2 second old"),
        amsg("A2 old reply"),
        umsg("U3 keep me"),
        amsg("A3 keep me"),
        umsg("U4 keep me too"),
    ];
    tokio_block_on(maybe_compact(&mut s, client.as_ref()));
    // The prior summary was folded into the base; summary replaced with marker.
    assert_eq!(s.gm_summary, "compact summary");
    // Kept tail starts at the (len-keep_turns)=2nd-from-last user boundary (U3).
    let kept: Vec<&str> = s
        .gm_messages
        .iter()
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(kept, vec!["U3 keep me", "A3 keep me", "U4 keep me too"]);
}

// =========================================================================
// GM uses PLAYER-ACTION-stripped msg_text_for_summary; NPC uses plain msg_text
// (Python asymmetry: `_msg_text_for_summary` vs `_msg_text`)
// =========================================================================

#[test]
fn summary_text_asymmetry_gm_strips_player_action_npc_does_not() {
    let marker = "PLAYER ACTION (latest user input, free roleplay text):";
    let user = json!({
        "role": "user",
        "content": format!("CONTEXT PREAMBLE\n{marker} Я осматриваюсь."),
    });

    // GM summary strips everything up to and including the PLAYER ACTION marker.
    let gm = msg_text_for_summary(&user);
    assert!(gm.contains("Я осматриваюсь."));
    assert!(!gm.contains("CONTEXT PREAMBLE"), "GM summary must strip preamble: {gm}");
    assert!(!gm.contains("PLAYER ACTION"), "GM summary must strip marker: {gm}");

    // NPC summary uses the plain msg_text — preamble and marker are preserved.
    let npc = msg_text(&user);
    assert!(npc.contains("CONTEXT PREAMBLE"));
    assert!(npc.contains("PLAYER ACTION"));
    assert!(npc.contains("Я осматриваюсь."));
}

// =========================================================================
// COMPACT_INPUT_CHARS clip is BY CHARS (Unicode scalars), not bytes
// =========================================================================

#[test]
fn gm_compaction_clips_summarize_input_by_chars() {
    use std::sync::Mutex;

    // A client that records the exact char length of the text it was asked to
    // summarize, so we can assert the clip is by CHARS.
    struct LenRecorder {
        seen_chars: Mutex<usize>,
    }
    #[async_trait]
    impl Backend for LenRecorder {
        fn model(&self) -> String {
            "mock".to_string()
        }
        fn set_model(&self, _m: &str) {}
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
        async fn summarize(
            &self,
            text: &str,
            _pn: &[String],
        ) -> Result<String, BackendError> {
            *self.seen_chars.lock().unwrap() = text.chars().count();
            Ok("ok".to_string())
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

    let mut s = session();
    let recorder = Arc::new(LenRecorder {
        seen_chars: Mutex::new(0),
    });
    let client: Arc<dyn Backend> = recorder.clone();
    s.client = client.clone();
    s.compaction.gm_history_tokens = 1;
    s.compaction.gm_keep_turns = 1;
    s.compaction.compact_input_chars = 100; // clip to 100 CHARS

    // Old content uses multi-byte Cyrillic: 400 chars = 800 bytes. If the clip
    // were by bytes the recorder would see <100 chars; it must see exactly 100.
    let old_user = "ё".repeat(400); // 400 chars, 800 bytes
    s.gm_messages = vec![umsg(&old_user), amsg("a"), umsg("keep")];
    tokio_block_on(maybe_compact(&mut s, client.as_ref()));
    let seen = *recorder.seen_chars.lock().unwrap();
    assert_eq!(seen, 100, "summarize input must be clipped to 100 CHARS, saw {seen}");
}

// =========================================================================
// NPC compaction: trigger gates + does NOT reset world-query cache
// =========================================================================

#[test]
fn npc_compaction_requires_both_gates_and_does_not_reset_world_query_cache() {
    let mut s = session();
    let client: Arc<dyn Backend> = Arc::new(MarkerCompactClient::new());

    // Prime the world-query cache; NPC compaction must NOT touch it.
    s.world_query_seen
        .entry("gm".to_string())
        .or_default()
        .insert("primed".to_string());

    // (a) token gate not met -> no NPC compaction.
    s.compaction.npc_history_tokens = 1_000_000;
    s.compaction.npc_keep_exchanges = 1;
    s.npc_messages.insert(
        "borin".to_string(),
        vec![umsg("a"), amsg("b"), umsg("c"), amsg("d")],
    );
    let before = s.npc_messages.get("borin").cloned().unwrap();
    tokio_block_on(maybe_compact_npc(&mut s, "borin", client.as_ref()));
    assert_eq!(s.npc_messages.get("borin").cloned().unwrap(), before);
    assert!(s.npc_summaries.get("borin").is_none());

    // (b) boundary gate not met -> no NPC compaction.
    s.compaction.npc_history_tokens = 1;
    s.compaction.npc_keep_exchanges = 6;
    let before = s.npc_messages.get("borin").cloned().unwrap();
    tokio_block_on(maybe_compact_npc(&mut s, "borin", client.as_ref()));
    assert_eq!(s.npc_messages.get("borin").cloned().unwrap(), before);

    // (c) both gates met -> NPC compaction fires, keeps last N, summarizes, and
    // STILL does not reset the world-query cache.
    s.compaction.npc_history_tokens = 1;
    s.compaction.npc_keep_exchanges = 1;
    s.npc_messages.insert(
        "borin".to_string(),
        vec![
            umsg(&"u1 ".repeat(20)),
            amsg(&"a1 ".repeat(20)),
            umsg(&"u2 keep ".repeat(20)),
        ],
    );
    tokio_block_on(maybe_compact_npc(&mut s, "borin", client.as_ref()));
    assert_eq!(
        s.npc_summaries.get("borin").map(|x| x.as_str()),
        Some("npc compact summary")
    );
    let kept = s.npc_messages.get("borin").cloned().unwrap();
    assert_eq!(kept.len(), 1, "NPC keep-last-1 verbatim");
    assert_eq!(kept[0]["content"].as_str().unwrap(), "u2 keep ".repeat(20));
    // World-query cache untouched by NPC compaction.
    assert!(
        s.world_query_seen.get("gm").map(|x| x.contains("primed")).unwrap_or(false),
        "NPC compaction must NOT reset world_query_seen"
    );
}

// =========================================================================
// NPC compaction folds prior NPC summary into the base
// =========================================================================

#[test]
fn npc_compaction_folds_prior_npc_summary() {
    let mut s = session();
    let client: Arc<dyn Backend> = Arc::new(MarkerCompactClient::new());
    s.compaction.npc_history_tokens = 1;
    s.compaction.npc_keep_exchanges = 1;
    s.npc_summaries
        .insert("borin".to_string(), "PRIOR_NPC_SUMMARY".to_string());
    s.npc_messages.insert(
        "borin".to_string(),
        vec![umsg("old u"), amsg("old a"), umsg("keep u")],
    );
    tokio_block_on(maybe_compact_npc(&mut s, "borin", client.as_ref()));
    // Prior summary folded; replaced with the compaction marker.
    assert_eq!(
        s.npc_summaries.get("borin").map(|x| x.as_str()),
        Some("npc compact summary")
    );
    let kept = s.npc_messages.get("borin").cloned().unwrap();
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["content"].as_str().unwrap(), "keep u");
}
