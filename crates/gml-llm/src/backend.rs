//! The [`Backend`] trait — the client interface the orchestrator drives.
//!
//! Faithful port of the duck-typed client surface that `orchestrator.py` calls
//! on whatever `make_client()` returns (`OpenAICompatClient`, `MockClient`, or
//! `CodexClient`). It is an `async_trait` so that `gml-codex` and `MockClient`
//! can both implement it and the orchestrator can hold a `dyn Backend`.
//!
//! ## Message / tool / schema shapes
//!
//! Python passes plain dicts/lists around (`messages`, `tools`, `schema`,
//! `response_format`). We keep them as [`serde_json::Value`] so the exact JSON
//! shapes the agents layer assembles are preserved byte-for-byte (key order via
//! the `preserve_order` serde feature). The orchestrator owns the construction
//! of these values; the backend only forwards/serializes them.
//!
//! ## Streaming
//!
//! Python's streaming methods are generators: they `yield (channel, delta)` for
//! each chunk and `return` a final tuple. Rust async generators are unstable, so
//! we model the `yield` as a [`DeltaSink`] callback the caller supplies, and the
//! `return` value as the method's return type. This is the same `(sender,
//! return-value)` shape PORT_PLAN §5.2 mandates for the orchestrator refactor.

use async_trait::async_trait;
use serde_json::{Map, Value};

use gml_types::ParsedCall;

/// The delta channel of a streamed chunk — the first element of the Python
/// `yield ("thinking"|"content", delta)` / `yield ("content", delta)` tuples.
pub mod channel {
    /// Reasoning/thinking delta (`reasoning_content`).
    pub const THINKING: &str = "thinking";
    /// Player-/JSON-facing content delta.
    pub const CONTENT: &str = "content";
}

/// A sink for streamed deltas — the Rust stand-in for a generator `yield`.
///
/// Each call corresponds to one `yield (channel, delta)` in the Python source,
/// in the same order. The orchestrator supplies an implementation that forwards
/// deltas onto its event stream.
pub trait DeltaSink {
    /// Emit one `(channel, delta)` pair. `channel` is one of [`channel::THINKING`]
    /// / [`channel::CONTENT`].
    fn emit(&mut self, channel: &str, delta: &str);
}

/// Blanket impl so any `FnMut(&str, &str)` can be used as a [`DeltaSink`].
impl<F: FnMut(&str, &str)> DeltaSink for F {
    fn emit(&mut self, channel: &str, delta: &str) {
        self(channel, delta)
    }
}

/// A no-op sink — discards deltas. Useful when only the final value is wanted.
pub struct NullSink;

impl DeltaSink for NullSink {
    fn emit(&mut self, _channel: &str, _delta: &str) {}
}

/// Return value of [`Backend::chat`] — Python `(thinking, content, calls,
/// assistant_msg)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatOutput {
    /// Cleaned reasoning text (`_think(reasoning_content)`).
    pub thinking: String,
    /// Cleaned player-facing content (`_clean(content)`).
    pub content: String,
    /// Parsed tool calls (`_parse_tool_calls`).
    pub calls: Vec<ParsedCall>,
    /// The assistant message to append to history (`_assistant_msg`), as a JSON
    /// object (`{role, content[, tool_calls]}`).
    pub assistant_msg: Value,
}

/// Return value of [`Backend::chat_stream`] — Python `(thinking, content, calls,
/// assistant_msg, stats)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ChatStreamOutput {
    /// Cleaned reasoning text.
    pub thinking: String,
    /// Cleaned player-facing content.
    pub content: String,
    /// Parsed tool calls.
    pub calls: Vec<ParsedCall>,
    /// The assistant message to append to history.
    pub assistant_msg: Value,
    /// Normalized `_meta` stats for the call.
    pub stats: Map<String, Value>,
}

/// Return value of [`Backend::chat_json_stream`] — Python `(parsed_dict, stats)`.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonStreamOutput {
    /// The parsed JSON object (`_loads` of the streamed content).
    pub data: Map<String, Value>,
    /// Normalized `_meta` stats for the call.
    pub stats: Map<String, Value>,
}

/// Error returned by backend network calls. Wraps a message; the orchestrator
/// surfaces failures as non-terminal `error` events, so a string carrier is
/// sufficient and avoids leaking the concrete transport error type across the
/// crate boundary.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct BackendError(pub String);

impl BackendError {
    /// Construct from anything displayable.
    pub fn new(msg: impl std::fmt::Display) -> Self {
        BackendError(msg.to_string())
    }
}

/// The client interface the orchestrator drives. Faithful port of the duck-typed
/// surface of `OpenAICompatClient` / `MockClient` / `CodexClient`.
///
/// All `messages` / `tools` / `schema` / `response_format` are JSON values the
/// agents layer constructs. `think` is `Option<bool>` to mirror Python's
/// tri-state (`think=None` disables the reasoning/sampling block entirely in
/// `_payload`; `chat` passes `think=False`, `chat_json` passes `think=True`).
/// `reasoning_role` is the role string (`config.ROLE_GM` etc.).
#[async_trait]
pub trait Backend: Send + Sync {
    /// The current model id (Python `@property model`).
    fn model(&self) -> String;

    /// `set_model(model)` — set the active model if `model` is non-empty after
    /// trimming.
    fn set_model(&self, model: &str);

    /// `set_session_identity(session_id, thread_id)` — restore the per-client
    /// uuid identity used for prompt-cache keys. Both are optional; an
    /// empty/absent value leaves the corresponding id untouched.
    ///
    /// The OpenAI-compatible and mock backends do not key the cache on a
    /// thread/session id (only Codex does), so they default to a no-op. Codex
    /// overrides this.
    fn set_session_identity(&self, _session_id: Option<&str>, _thread_id: Option<&str>) {}

    /// `list_models()` — available models as `[{id, name, supported}]`.
    async fn list_models(&self) -> Vec<Value>;

    /// `chat(messages, tools, think, reasoning_role)` -> `(thinking, content,
    /// calls, assistant_msg)`.
    async fn chat(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError>;

    /// `chat_json(messages, schema, think, reasoning_role)` -> parsed dict.
    async fn chat_json(
        &self,
        messages: &Value,
        schema: &Value,
        think: Option<bool>,
        reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError>;

    /// `summarize(text, proper_nouns)` -> summary string.
    async fn summarize(
        &self,
        text: &str,
        proper_nouns: &[String],
    ) -> Result<String, BackendError>;

    /// `chat_stream(messages, tools, think, reasoning_role)` — yields deltas via
    /// `sink`, returns the final `(thinking, content, calls, assistant_msg,
    /// stats)`.
    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError>;

    /// `chat_json_stream(messages, schema, think, reasoning_role)` — yields
    /// content deltas via `sink`, returns the final `(parsed_dict, stats)`.
    async fn chat_json_stream(
        &self,
        messages: &Value,
        schema: &Value,
        think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError>;
}
