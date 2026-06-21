//! The NPC sub-agent JSON contract.
//!
//! Python origin: `agents.py` `NPC_SCHEMA` and `_norm_npc`:
//! ```python
//! NPC_SCHEMA = {
//!     "type": "object",
//!     "properties": {
//!         "reasoning": {"type": "string"},
//!         "speech": {"type": "string"},
//!         "action": {"type": "string"},
//!         "claims": {"type": "array", "items": {"type": "string"}},
//!     },
//!     "required": ["reasoning", "speech", "action", "claims"],
//! }
//!
//! def _norm_npc(out: dict) -> dict:
//!     if not isinstance(out, dict):
//!         out = {}
//!     return {
//!         "reasoning": _text(out.get("reasoning")),
//!         "speech": _text(out.get("speech")),
//!         "action": _text(out.get("action")),
//!         "claims": _claims(out.get("claims")),
//!     }
//! ```
//! Field names and order match the schema exactly. All four keys are required and
//! always present after normalization (`_norm_npc` coerces missing fields to empty
//! string / empty list).

use serde::{Deserialize, Serialize};

/// Normalized NPC sub-agent response — the `{reasoning, speech, action, claims}`
/// contract. Mirrors the dict produced by `agents._norm_npc`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcResponse {
    /// The NPC's private reasoning (thinking channel).
    #[serde(default)]
    pub reasoning: String,
    /// What the NPC says aloud.
    #[serde(default)]
    pub speech: String,
    /// The NPC's visible action.
    #[serde(default)]
    pub action: String,
    /// Discrete claims the NPC asserts (each a non-empty string after coercion).
    #[serde(default)]
    pub claims: Vec<String>,
}
