//! Tool-call and tool-result shapes shared across the agents/orchestrator boundary.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The two-channel result of running a GM tool.
///
/// Python origin: `orchestrator.py`
/// ```python
/// @dataclass(frozen=True)
/// class ToolExecutionResult:
///     full: str
///     model: str
///     terminal: bool = False
/// ```
/// `full` is the player-/UI-facing text (JSON without the reminder); `model` is
/// the model-facing compact text (typically with a trailing `<system-reminder>`).
/// `terminal` marks a result that ends the tool loop (e.g. `ask_player`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolExecutionResult {
    /// Full text channel — surfaced to the player/UI as `tool_result` data.
    pub full: String,
    /// Model-facing channel — compact text fed back into the GM message history.
    pub model: String,
    /// Whether this result terminates the tool loop. Defaults to `false`, matching
    /// the Python dataclass default.
    #[serde(default)]
    pub terminal: bool,
}

impl ToolExecutionResult {
    /// Construct with `terminal = false` (the common case).
    pub fn new(full: impl Into<String>, model: impl Into<String>) -> Self {
        ToolExecutionResult {
            full: full.into(),
            model: model.into(),
            terminal: false,
        }
    }

    /// Construct with an explicit `terminal` flag.
    pub fn with_terminal(full: impl Into<String>, model: impl Into<String>, terminal: bool) -> Self {
        ToolExecutionResult {
            full: full.into(),
            model: model.into(),
            terminal,
        }
    }
}

/// A parsed GM tool call.
///
/// Python origin: `llm_client.py` `_parse_tool_calls`:
/// ```python
/// calls.append({"name": fn.get("name"), "arguments": args or {}, "id": tc.get("id", "")})
/// ```
/// `arguments` is the decoded object (the OpenAI wire form sends it as a JSON
/// string; it is parsed into a map before reaching this shape). Field order
/// matches the Python dict: `name`, `arguments`, `id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedCall {
    /// Tool name.
    pub name: String,
    /// Decoded tool arguments. Defaults to an empty object (Python `args or {}`).
    #[serde(default)]
    pub arguments: Map<String, Value>,
    /// Provider-assigned call id (empty string when absent — Python `tc.get("id", "")`).
    #[serde(default)]
    pub id: String,
}

impl ParsedCall {
    /// Construct a parsed call with the given name, arguments, and id.
    pub fn new(name: impl Into<String>, arguments: Map<String, Value>, id: impl Into<String>) -> Self {
        ParsedCall {
            name: name.into(),
            arguments,
            id: id.into(),
        }
    }
}
