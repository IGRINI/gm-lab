//! The SSE event envelope emitted by `run_turn` and streamed by the server.
//!
//! Python origin: `orchestrator.py`
//! ```python
//! def ev(kind, agent, data=None, sid=None):
//!     return {"kind": kind, "agent": agent, "data": data, "sid": sid}
//! ```
//! Serialized in `server.py` via `"data: " + json.dumps(ev, ensure_ascii=False) + "\n\n"`.
//!
//! All four keys ALWAYS serialize, in this exact order: `kind`, `agent`, `data`,
//! `sid`. `agent` and `sid` may be null; `data` may be null (Python `None`) or any
//! JSON value (string, object, array, number, bool).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The SSE event envelope `{kind, agent, data, sid}`.
///
/// Field order matters for byte-for-byte wire fidelity: with serde_json's
/// `preserve_order` feature the output keys appear in struct-declaration order,
/// matching Python's dict insertion order in `ev()`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Event kind. See [`event_kind`] for the full set of values.
    pub kind: String,
    /// Display agent label (e.g. `"ГМ"`, an NPC label, `"scene_sync"`). `None`
    /// serializes as JSON `null`.
    pub agent: Option<String>,
    /// Event payload. `None` serializes as JSON `null` (Python `data=None`);
    /// otherwise an arbitrary JSON value.
    pub data: Value,
    /// Stream id used to tie deltas to their finalized row. `None` serializes as
    /// JSON `null`.
    pub sid: Option<String>,
}

impl Event {
    /// Construct an event, mirroring Python `ev(kind, agent, data=None, sid=None)`.
    pub fn new(
        kind: impl Into<String>,
        agent: Option<String>,
        data: impl Into<Value>,
        sid: Option<String>,
    ) -> Self {
        Event {
            kind: kind.into(),
            agent,
            data: data.into(),
            sid,
        }
    }

    /// Convenience for `ev(kind, agent)` with `data=None, sid=None`.
    pub fn bare(kind: impl Into<String>, agent: Option<String>) -> Self {
        Event {
            kind: kind.into(),
            agent,
            data: Value::Null,
            sid: None,
        }
    }
}

/// The complete set of SSE event `kind` strings the server emits on `/turn`.
///
/// Extracted from every `ev("...")` call site in `orchestrator.py`, plus the
/// terminal `done` frame pushed in `server.py` (`push({"kind": "done"})`) and the
/// non-terminal `error` frame. See PORT_PLAN.md §5.2 for the ordering contracts
/// the frontend depends on.
///
/// `delta.data.channel` is one of `"gm_thinking"`, `"gm_narration"`, `"npc_speech"`.
/// `gm_tool_call` / `tool_result` are suppressed for `ask_player`. The
/// `scene_update` event uses agent `"scene_sync"` for auto-applied deltas.
pub mod event_kind {
    /// Echo of the player's submitted action.
    pub const PLAYER: &str = "player";
    /// Streaming text chunk; `data.channel` selects the target row.
    pub const DELTA: &str = "delta";
    /// Finalized GM reasoning/thinking row.
    pub const GM_THINKING: &str = "gm_thinking";
    /// Finalized GM player-facing narration row.
    pub const GM_NARRATION: &str = "gm_narration";
    /// Per-step usage/meta info.
    pub const META: &str = "meta";
    /// Aggregate usage/meta for the whole turn.
    pub const META_TOTAL: &str = "meta_total";
    /// A GM tool call is about to run (suppressed for `ask_player`).
    pub const GM_TOOL_CALL: &str = "gm_tool_call";
    /// Result of a GM tool call — full text, not the model-facing compact text
    /// (suppressed for `ask_player`).
    pub const TOOL_RESULT: &str = "tool_result";
    /// Result of a `tool_search` call.
    pub const TOOL_SEARCH: &str = "tool_search";
    /// Quick-reply player options (from the `ask_player` tool).
    pub const PLAYER_OPTIONS: &str = "player_options";
    /// A resolved dice roll.
    pub const DICE: &str = "dice";
    /// A world fact surfaced to the player.
    pub const WORLD_FACT: &str = "world_fact";
    /// A world state-record update.
    pub const WORLD_STATE_UPDATE: &str = "world_state_update";
    /// A world-state query result.
    pub const WORLD_QUERY: &str = "world_query";
    /// Selected NPC profile/mechanics fields.
    pub const NPC_PROFILE: &str = "npc_profile";
    /// World clock advanced.
    pub const TIME: &str = "time";
    /// Player character sheet changed.
    pub const PLAYER_CHARACTER_UPDATE: &str = "player_character_update";
    /// Offscreen NPC whereabouts changed.
    pub const NPC_WHEREABOUTS: &str = "npc_whereabouts";
    /// Scene replaced/updated (agent `"scene_sync"` for auto-applied deltas).
    pub const SCENE_UPDATE: &str = "scene_update";
    /// An NPC sub-agent turn is starting.
    pub const NPC_START: &str = "npc_start";
    /// NPC sub-agent recent-history context.
    pub const NPC_HISTORY: &str = "npc_history";
    /// NPC sub-agent reasoning/thinking.
    pub const NPC_THINKING: &str = "npc_thinking";
    /// NPC sub-agent speech/action (`data = {speech, action, claims, npc_id}`).
    pub const NPC_SPEECH: &str = "npc_speech";
    /// GM rejected/corrected an NPC response.
    pub const GM_REJECT: &str = "gm_reject";
    /// Non-terminal error frame.
    pub const ERROR: &str = "error";
    /// Terminal frame — push-only, never appended to the transcript.
    pub const DONE: &str = "done";

    /// All event kinds emitted by `run_turn` plus the server-pushed `done`.
    /// Ordered as: the 25 distinct `ev(...)` kinds (alphabetical, matching the
    /// source extraction) followed by the server-only terminal `done`.
    pub const ALL: &[&str] = &[
        DELTA,
        DICE,
        ERROR,
        GM_NARRATION,
        GM_REJECT,
        GM_THINKING,
        GM_TOOL_CALL,
        META,
        META_TOTAL,
        NPC_HISTORY,
        NPC_PROFILE,
        NPC_SPEECH,
        NPC_START,
        NPC_THINKING,
        NPC_WHEREABOUTS,
        PLAYER,
        PLAYER_CHARACTER_UPDATE,
        PLAYER_OPTIONS,
        SCENE_UPDATE,
        TIME,
        TOOL_RESULT,
        TOOL_SEARCH,
        WORLD_FACT,
        WORLD_QUERY,
        WORLD_STATE_UPDATE,
        DONE,
    ];

    /// Valid values for `delta.data.channel`.
    pub const DELTA_CHANNELS: &[&str] = &[GM_THINKING, GM_NARRATION, NPC_SPEECH];
}
