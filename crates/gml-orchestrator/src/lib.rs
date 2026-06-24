//! gml-orchestrator — the GM turn loop, tool dispatch, NPC sub-agent
//! orchestration, two-phase draft/commit, history compaction, and token
//! accounting.
//!
//! Faithful port of `gm-lab/orchestrator.py` (3377 lines). The Python
//! event-generator (`run_turn` + `yield from` over `_drive` / `_run_tool` /
//! `_ask_npc` / `_sync_scene_delta` / `_generate_pre_tool_prelude`) is flattened
//! into ONE event stream per PORT_PLAN §5.2: helpers take a `&Sink` (an mpsc
//! sender wrapper) and return their value (`ToolExecutionResult` / `String`);
//! [`run_turn`] drains the channel into a `Vec<Event>`.
//!
//! Modules:
//! - [`session`] — the [`Session`] struct (field set pinned to the persistence
//!   payload), event log, draft/commit, observations, usage.
//! - [`turn`] — [`run_turn`], the GM tool-hop loop, tool dispatch, `_ask_npc`.
//! - [`compact`] — token estimation, `_meta`/`_meta_total`, compaction, usage.
//! - [`helpers`] / [`model_text`] — tool-result builders + model-facing text.
//! - [`worldstate`] / [`query_dedup`] — scoped memory/fact tools, legacy
//!   world-state compatibility helpers, and world-fact delivery de-duplication.
//! - [`rag`] — the `gml-world` -> `gml-rag` retrieval seam.

pub mod compact;
pub mod helpers;
pub mod memory_crystals;
pub mod model_text;
pub mod query_dedup;
pub mod rag;
pub mod session;
pub mod session_payload;
pub mod turn;
pub mod worldstate;

pub use session::{ClientFactory, CompactionThresholds, NpcClientState, PendingDraft, Session};
pub use turn::{run_tool_collect, run_turn, run_turn_into};

use serde_json::Value;

/// Python truthiness of a JSON value used as `bool(value)` / `if value:`.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i != 0
            } else if let Some(u) = n.as_u64() {
                u != 0
            } else {
                n.as_f64().map(|f| f != 0.0).unwrap_or(false)
            }
        }
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `round(x, ndigits)` — round-half-to-even ("banker's rounding").
pub fn round_half_even(x: f64, ndigits: i32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let pow = 10f64.powi(ndigits);
    let scaled = x * pow;
    let floor = scaled.floor();
    let diff = scaled - floor;
    // Round half-to-even: ties (diff == 0.5) go up only when `floor` is odd.
    let round_up = diff > 0.5 || (diff == 0.5 && (floor as i64) % 2 != 0);
    let rounded = if round_up { floor + 1.0 } else { floor };
    rounded / pow
}
