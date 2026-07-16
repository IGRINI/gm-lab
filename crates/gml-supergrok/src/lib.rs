//! xAI SuperGrok connector.
//!
//! The crate owns every xAI-specific concern: device OAuth, rotating refresh
//! tokens, the Responses wire protocol, prompt-cache headers, and SSE parsing.
//! Game history and tool execution remain in the caller through
//! [`gml_llm::Backend`].

mod client;
mod config;
mod connector;
mod oauth;
mod protocol;
mod stream;
mod stt;

pub use client::SuperGrokClient;
pub use config::{
    SuperGrokConfig, DEFAULT_CLIENT_ID, DEFAULT_DEVICE_CODE_URL, DEFAULT_DISCOVERY_URL,
    DEFAULT_INFERENCE_BASE_URL, DEFAULT_MODEL_ID, DEFAULT_SCOPE, DEFAULT_STT_LANGUAGE,
    DEFAULT_STT_MAX_AUDIO_BYTES,
};
pub use connector::{SuperGrokConnector, SUPERGROK_CONNECTOR_ID, SUPERGROK_DEVICE_AUTH_METHOD_ID};
pub use oauth::{
    AuthStatus, DeviceAuthorization, DeviceChallenge, OAuthCredential, OAuthError, SuperGrokOAuth,
};
