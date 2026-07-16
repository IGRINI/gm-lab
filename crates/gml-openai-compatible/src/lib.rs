//! OpenAI-compatible model connector and wire protocol implementation.

mod client;
mod connector;

pub use client::{build_payload, OpenAICompatClient};
pub use connector::OpenAICompatConnector;
