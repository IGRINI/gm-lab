//! gml-llm — model clients: the [`Backend`] trait, the OpenAI-compatible client,
//! the deterministic [`MockClient`], JSON-stream helpers, and the [`make_client`]
//! factory.
//!
//! Faithful port of `gm-lab/llm_client.py` (PORT_PLAN.md §2, §4.5). The real
//! response shape this targets is Qwen3.6 via llama.cpp:
//! - `message.reasoning_content` -> the thinking channel
//! - `message.content`           -> text (empty on a tool call)
//! - `message.tool_calls`        -> `[{id, function:{name, arguments: JSON-STRING}}]`
//! - `usage` / `timings`         -> tokens and speed
//!
//! Thinking is controlled via `chat_template_kwargs.enable_thinking`; NPC JSON is
//! taken as free text (Qwen keeps reasoning separate, so content is not polluted),
//! with a `response_format` fallback.
//!
//! ## Module map
//! - [`backend`] — the [`Backend`] async trait + IO shapes + [`DeltaSink`].
//! - [`openai_compat`] — [`OpenAICompatClient`] (reqwest, `/v1`, SSE, no read timeout).
//! - [`mock`] — [`MockClient`] (deterministic `GM_BACKEND=mock` scenario).
//! - [`json_helpers`] — [`extract_json_string`] / [`json_unescape`] / `_loads` / tool-call parse.
//! - [`parsing`] — `_clean` / `_think` / `_stats` / `_assistant_msg` helpers.
//! - [`identity`] — [`SessionIdentity`] (uuid4 per client, restorable; reused by gml-codex).
//! - [`factory`] — [`make_client`] (backend selection + codex hook).

pub mod backend;
pub mod factory;
pub mod identity;
pub mod json_helpers;
pub mod mock;
pub mod openai_compat;
pub mod parsing;

pub use backend::{
    channel, Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
    NullSink,
};
pub use factory::{make_client, BackendKind, CodexHook};
pub use identity::SessionIdentity;
pub use json_helpers::{extract_json_string, json_unescape, loads_map, loads_value, parse_tool_calls};
pub use mock::MockClient;
pub use openai_compat::{build_payload, OpenAICompatClient};
pub use parsing::{assistant_msg, clean, mock_stats, proper_nouns_line, stats, think};
