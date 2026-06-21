//! `make_client` factory — selects the backend by configuration.
//!
//! Python:
//! ```python
//! def make_client():
//!     if config.BACKEND == "mock":
//!         return MockClient()
//!     if config.BACKEND == "codex":
//!         from codex_client import CodexClient
//!         return CodexClient()
//!     return OpenAICompatClient()
//! ```
//!
//! The `CodexClient` is built in the `gml-codex` crate. To avoid a dependency
//! edge `gml-llm -> gml-codex` (which would be a cycle, since `gml-codex`
//! depends on `gml-llm` for the [`Backend`](crate::Backend) trait), the codex
//! branch is handled via a caller-supplied **hook**: `gml-server` (or the app)
//! passes a constructor that returns a `dyn Backend`. This is the "expose a
//! hook/enum so gml-server can wire codex without gml-llm depending on
//! gml-codex" requirement from the build spec.

use std::sync::Arc;

use gml_config::{Config, RuntimeSettings};

use crate::backend::{Backend, BackendError};
use crate::mock::MockClient;
use crate::openai_compat::OpenAICompatClient;

/// The kind of backend selected by configuration. Mirrors the three branches of
/// Python `make_client`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// `config.BACKEND == "mock"` -> [`MockClient`].
    Mock,
    /// `config.BACKEND == "codex"` -> Codex client (built by the hook).
    Codex,
    /// Anything else -> [`OpenAICompatClient`].
    OpenAiCompat,
}

impl BackendKind {
    /// Classify a `config.BACKEND` string.
    pub fn from_backend(backend: &str) -> Self {
        match backend {
            "mock" => BackendKind::Mock,
            "codex" => BackendKind::Codex,
            _ => BackendKind::OpenAiCompat,
        }
    }
}

/// A constructor hook for the Codex backend, supplied by `gml-server`/`gml-app`.
///
/// Receives the shared [`Config`] / [`RuntimeSettings`] and returns a boxed
/// [`Backend`] (or an error). This lets the codex implementation live in
/// `gml-codex` while `make_client` stays in `gml-llm`.
pub type CodexHook = dyn Fn(Arc<Config>, Arc<RuntimeSettings>) -> Result<Arc<dyn Backend>, BackendError>
    + Send
    + Sync;

/// `make_client()` — build the configured backend.
///
/// - `mock`  -> [`MockClient`]
/// - `codex` -> `codex_hook(cfg, settings)` (must be supplied; otherwise an
///   error is returned — Python imports `CodexClient` lazily, which would fail
///   at runtime if absent)
/// - else    -> [`OpenAICompatClient`] (auto-detects the model via GET /v1/models)
///
/// `codex_hook` is `Option<&CodexHook>` so callers that never use the codex
/// backend (e.g. contract tests) can pass `None`.
pub async fn make_client(
    cfg: Arc<Config>,
    settings: Arc<RuntimeSettings>,
    codex_hook: Option<&CodexHook>,
) -> Result<Arc<dyn Backend>, BackendError> {
    match BackendKind::from_backend(&cfg.backend) {
        BackendKind::Mock => Ok(Arc::new(MockClient::new())),
        BackendKind::Codex => match codex_hook {
            Some(hook) => hook(cfg, settings),
            None => Err(BackendError::new(
                "codex backend selected but no codex constructor hook was provided",
            )),
        },
        BackendKind::OpenAiCompat => {
            Ok(Arc::new(OpenAICompatClient::new(cfg, settings).await))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_kind_classification() {
        assert_eq!(BackendKind::from_backend("mock"), BackendKind::Mock);
        assert_eq!(BackendKind::from_backend("codex"), BackendKind::Codex);
        assert_eq!(BackendKind::from_backend("llamacpp"), BackendKind::OpenAiCompat);
        assert_eq!(BackendKind::from_backend(""), BackendKind::OpenAiCompat);
        assert_eq!(BackendKind::from_backend("openai"), BackendKind::OpenAiCompat);
    }

    #[tokio::test]
    async fn make_client_mock() {
        let mut cfg = Config::from_env();
        cfg.backend = "mock".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_llm_factory_test_settings.json"),
        ));
        let client = make_client(cfg, settings, None).await.unwrap();
        assert_eq!(client.model(), "mock");
    }

    #[tokio::test]
    async fn make_client_codex_without_hook_errors() {
        let mut cfg = Config::from_env();
        cfg.backend = "codex".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_llm_factory_test_settings2.json"),
        ));
        let res = make_client(cfg, settings, None).await;
        match res {
            Err(e) => assert!(e.to_string().contains("codex")),
            Ok(_) => panic!("expected codex-without-hook to error"),
        }
    }

    #[tokio::test]
    async fn make_client_codex_with_hook() {
        let mut cfg = Config::from_env();
        cfg.backend = "codex".to_string();
        let cfg = Arc::new(cfg);
        let settings = Arc::new(RuntimeSettings::new(
            &cfg,
            std::env::temp_dir().join("gml_llm_factory_test_settings3.json"),
        ));
        let hook: Box<CodexHook> =
            Box::new(|_c, _s| Ok(Arc::new(MockClient::new()) as Arc<dyn Backend>));
        let client = make_client(cfg, settings, Some(hook.as_ref())).await.unwrap();
        // hook returned a MockClient stand-in
        assert_eq!(client.model(), "mock");
    }
}
