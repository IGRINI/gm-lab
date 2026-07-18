//! gml-codex — Codex ChatGPT OAuth backend (PKCE auth flow + Responses API
//! client).
//!
//! Faithful port of `gm-lab/codex_oauth.py` and `gm-lab/codex_client.py`
//! (PORT_PLAN.md §2, §3.2, §4.1, §4.5). Two layers:
//!
//! - [`oauth`] — the browser PKCE OAuth flow (loopback callback server on
//!   `127.0.0.1:1455`, fallback `1457`, 300s timeout), local JSON credential
//!   storage with `GM_CODEX_CREDENTIAL_PATH` override, near-expiry refresh, and
//!   token revocation. Verbatim Russian success/failure HTML.
//! - [`responses`] — the pure request/response transforms:
//!   [`responses::split_messages_for_responses`] (join ALL system messages into
//!   one `instructions` string with `"\n\n"` — byte-identical for Codex cache
//!   reuse), strict-schema tool conversion, and output/tool-call extraction.
//! - [`stream`] — the SSE event accumulators (`response.output_text.delta`,
//!   reasoning deltas, `function_call` accumulation, `response.completed`).
//! - [`client`] — [`CodexClient`], which implements [`gml_llm::Backend`].
//! - [`install_id`] — the persisted per-install `x-codex-installation-id`
//!   (PORT_PLAN risk #9 decisive default: persist, not per-process).
//!
//! ## HTTP / impersonation
//!
//! The Codex Responses backend uses plain TLS (`reqwest`) with header spoofing
//! only — it does NOT use JA3/curl_cffi impersonation today (the only
//! impersonation case in TaleShift is STT in `gml-audio`). If Codex later adds JA3
//! checks, switch this crate to an impersonating client (PORT_PLAN §1.3).

pub mod client;
pub mod connector;
pub mod install_id;
pub mod oauth;
pub mod responses;
pub mod stream;

pub use client::{usage_stats, CodexClient};
pub use connector::CodexConnector;
pub use install_id::installation_id;
pub use oauth::{
    account_id_from_tokens, auth_status, authorize_url, code_challenge, credential_path,
    decode_jwt_claims, delete_credential, ensure_fresh_credential, expires_at, is_near_expiry,
    load_credential, random_url_token, refresh_credential, revoke_credential, run_oauth,
    save_credential, CodexCredential, OAuthError,
};
pub use responses::{
    convert_tool_for_responses, extract_output_text, extract_tool_calls, nullable_schema,
    split_messages_for_responses, strict_schema_for_responses,
};
pub use stream::{StreamAccumulator, StreamResult};
