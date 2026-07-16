//! Provider-neutral model contracts shared by the application core and
//! connector crates. Provider transport, authentication, model catalog and
//! cache policies intentionally live outside this crate.
//!
//! ## Module map
//! - [`backend`] — the [`Backend`] async trait + IO shapes + [`DeltaSink`].
//! - [`connector`] — provider-neutral connector contracts and registry.
//! - [`json_helpers`] — [`extract_json_string`] / [`json_unescape`] / `_loads` / tool-call parse.
//! - [`parsing`] — `_clean` / `_think` / `_stats` / `_assistant_msg` helpers.
//! - [`identity`] — [`SessionIdentity`] (uuid4 per client, restorable; reused by gml-codex).

pub mod backend;
pub mod connector;
pub mod identity;
pub mod json_helpers;
pub mod parsing;
pub mod response_language;

pub use backend::{
    channel, Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
    NullSink,
};
pub use connector::{
    ConnectorAuthKind, ConnectorAuthMethod, ConnectorAuthStart, ConnectorAuthStatus,
    ConnectorCapability, ConnectorDescriptor, ConnectorError, ConnectorId, ConnectorRegistry,
    ModelBinding, ModelConnector, ModelDescriptor,
};
pub use identity::SessionIdentity;
pub use json_helpers::{
    extract_json_string, json_unescape, loads_map, loads_value, parse_tool_calls,
};
pub use parsing::{assistant_msg, clean, proper_nouns_line, stats, think};
pub use response_language::{
    messages_with_response_language, ResponseLanguageBackend, ResponseLanguageSource,
};
