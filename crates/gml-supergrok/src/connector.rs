//! SuperGrok implementation of the provider-neutral connector boundary.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::Value;
use tokio::task::AbortHandle;

use gml_llm::{
    Backend, ConnectorAuthKind, ConnectorAuthMethod, ConnectorAuthStart, ConnectorAuthStatus,
    ConnectorCapability, ConnectorDescriptor, ConnectorError, ConnectorId, ImageGenerationRequest,
    ImageGenerationResult, ModelConnector, ModelDescriptor,
};

use crate::{OAuthError, SuperGrokClient, SuperGrokConfig, SuperGrokOAuth, DEFAULT_MODEL_ID};

pub const SUPERGROK_CONNECTOR_ID: &str = "xai";
pub const SUPERGROK_DEVICE_AUTH_METHOD_ID: &str = "device";

#[derive(Clone)]
struct ModelCache {
    auth_epoch: u64,
    models: Vec<ModelDescriptor>,
}

/// Application-lifetime SuperGrok connector.
///
/// Authentication and model discovery belong to this type. Every history gets
/// a fresh [`SuperGrokClient`] so its session identity and prompt-cache scope
/// cannot leak into another history.
pub struct SuperGrokConnector {
    config: Arc<SuperGrokConfig>,
    http: reqwest::Client,
    oauth: SuperGrokOAuth,
    start_lock: tokio::sync::Mutex<()>,
    auth_flow: Arc<tokio::sync::Mutex<AuthFlowState>>,
    model_cache: RwLock<Option<ModelCache>>,
}

impl SuperGrokConnector {
    pub fn new(config: Arc<SuperGrokConfig>) -> Result<Self, OAuthError> {
        let http = SuperGrokClient::build_http_client()?;
        let oauth = SuperGrokOAuth::with_http(config.clone(), http.clone())?;
        Ok(Self {
            config,
            http,
            oauth,
            start_lock: tokio::sync::Mutex::new(()),
            auth_flow: Arc::new(tokio::sync::Mutex::new(AuthFlowState::default())),
            model_cache: RwLock::new(None),
        })
    }

    pub fn oauth(&self) -> &SuperGrokOAuth {
        &self.oauth
    }

    fn id() -> ConnectorId {
        ConnectorId::new(SUPERGROK_CONNECTOR_ID).expect("static SuperGrok connector id is valid")
    }

    fn fallback_model(&self) -> String {
        let model = self.config.model.trim();
        if model.is_empty() {
            DEFAULT_MODEL_ID.to_string()
        } else {
            model.to_string()
        }
    }

    fn client(&self, model: &str) -> SuperGrokClient {
        let client =
            SuperGrokClient::from_parts(self.config.clone(), self.http.clone(), self.oauth.clone());
        client.set_model(model);
        client
    }

    fn cached_models(&self) -> Vec<ModelDescriptor> {
        let auth_epoch = self.oauth.auth_epoch();
        self.model_cache
            .read()
            .ok()
            .and_then(|cache| {
                cache
                    .as_ref()
                    .filter(|cache| cache.auth_epoch == auth_epoch)
                    .map(|cache| cache.models.clone())
            })
            .unwrap_or_default()
    }

    fn cache_models(&self, auth_epoch: u64, models: &[ModelDescriptor]) {
        let Ok(mut cache) = self.model_cache.write() else {
            return;
        };
        if self.oauth.auth_epoch() == auth_epoch {
            *cache = Some(ModelCache {
                auth_epoch,
                models: models.to_vec(),
            });
        }
    }

    fn clear_model_cache(&self) {
        if let Ok(mut cache) = self.model_cache.write() {
            *cache = None;
        }
    }

    async fn begin_auth_flow(&self) -> Result<ConnectorAuthStart, ConnectorError> {
        // Serializes start/logout so a cancelled device flow cannot recreate a
        // credential after the user logs out.
        let _start_guard = self.start_lock.lock().await;
        let generation = {
            let mut state = self.auth_flow.lock().await;
            state.generation = state.generation.wrapping_add(1);
            let generation = state.generation;
            state.replace_phase(AuthFlowPhase::Starting);
            generation
        };

        let authorization = match self.oauth.start().await {
            Ok(authorization) => authorization,
            Err(error) => {
                let mut state = self.auth_flow.lock().await;
                if state.generation == generation {
                    state.phase = AuthFlowPhase::Failed(error.to_string());
                }
                return Err(ConnectorError::operation(Self::id(), error));
            }
        };
        let challenge = authorization.challenge().clone();
        let oauth = self.oauth.clone();
        let flow = self.auth_flow.clone();
        let task = tokio::spawn(async move {
            let result = oauth.poll(&authorization).await;
            let mut state = flow.lock().await;
            if state.generation == generation {
                state.phase = match result {
                    Ok(_) => AuthFlowPhase::Idle,
                    Err(error) => AuthFlowPhase::Failed(error.to_string()),
                };
            }
        });
        let abort = task.abort_handle();
        drop(task);

        let mut state = self.auth_flow.lock().await;
        if state.generation == generation && matches!(state.phase, AuthFlowPhase::Starting) {
            state.phase = AuthFlowPhase::Polling { abort };
        }

        Ok(ConnectorAuthStart::DeviceCode {
            verification_url: if challenge.verification_uri_complete.trim().is_empty() {
                challenge.verification_uri
            } else {
                challenge.verification_uri_complete
            },
            user_code: challenge.user_code,
            expires_in_seconds: remaining_seconds(challenge.expires_at_ms),
            interval_seconds: challenge.poll_interval_seconds,
        })
    }
}

#[async_trait]
impl ModelConnector for SuperGrokConnector {
    fn default_model_id(&self) -> String {
        self.fallback_model()
    }

    fn legacy_backend_ids(&self) -> &'static [&'static str] {
        &["xai", "supergrok"]
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor::new(Self::id(), "SuperGrok")
            .and_then(|descriptor| {
                descriptor
                    .with_capability(ConnectorCapability::SpeechToText)
                    .with_capability(ConnectorCapability::ImageGeneration)
                    .with_auth_method(ConnectorAuthMethod::new(
                        SUPERGROK_DEVICE_AUTH_METHOD_ID,
                        "xAI Device OAuth",
                        ConnectorAuthKind::DeviceOauth,
                    )?)
            })
            .expect("static SuperGrok descriptor is valid")
    }

    async fn auth_status(&self) -> Result<ConnectorAuthStatus, ConnectorError> {
        let (pending, flow_error) = {
            let state = self.auth_flow.lock().await;
            match &state.phase {
                AuthFlowPhase::Starting => (Some("Starting xAI authorization".to_string()), None),
                AuthFlowPhase::Polling { .. } => (
                    Some("Waiting for xAI device authorization".to_string()),
                    None,
                ),
                AuthFlowPhase::Failed(message) => (None, Some(message.clone())),
                AuthFlowPhase::Idle => (None, None),
            }
        };
        if let Some(message) = pending {
            return Ok(ConnectorAuthStatus::Pending {
                message: Some(message),
            });
        }

        let persistence_error = self
            .oauth
            .ensure_persisted()
            .await
            .err()
            .map(|error| error.to_string());
        let status = self.oauth.status();
        if status.authenticated {
            return if let Some(message) = persistence_error.or(status.error) {
                Ok(ConnectorAuthStatus::Expired {
                    message: Some(message),
                })
            } else {
                Ok(ConnectorAuthStatus::SignedIn {
                    account_label: None,
                })
            };
        }
        let error = flow_error.or(status.error);
        if let Some(message) = error {
            Ok(ConnectorAuthStatus::Expired {
                message: Some(message),
            })
        } else {
            Ok(ConnectorAuthStatus::SignedOut)
        }
    }

    async fn start_auth(
        &self,
        method_id: &str,
        _ui_language: Option<&str>,
    ) -> Result<ConnectorAuthStart, ConnectorError> {
        if method_id != SUPERGROK_DEVICE_AUTH_METHOD_ID {
            return Err(ConnectorError::UnknownAuthMethod {
                connector_id: Self::id(),
                method_id: method_id.to_string(),
            });
        }
        self.begin_auth_flow().await
    }

    async fn logout_auth(&self) -> Result<(), ConnectorError> {
        let _start_guard = self.start_lock.lock().await;
        {
            let mut state = self.auth_flow.lock().await;
            state.generation = state.generation.wrapping_add(1);
            state.replace_phase(AuthFlowPhase::Idle);
        }
        self.oauth
            .logout()
            .await
            .map_err(|error| ConnectorError::operation(Self::id(), error))?;
        self.clear_model_cache();
        Ok(())
    }

    async fn transcribe(
        &self,
        audio: &[u8],
        content_type: &str,
        language: Option<&str>,
    ) -> Result<String, ConnectorError> {
        crate::stt::transcribe(
            &self.config,
            &self.http,
            &self.oauth,
            audio,
            content_type,
            language,
        )
        .await
    }

    async fn generate_images(
        &self,
        request: &ImageGenerationRequest,
    ) -> Result<ImageGenerationResult, ConnectorError> {
        crate::image::generate(&self.config, &self.http, &self.oauth, request).await
    }

    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError> {
        let fallback = self.fallback_model();
        let client = self.client(&fallback);
        let auth_epoch = self.oauth.auth_epoch();
        let mut models = Vec::new();
        let mut seen = HashSet::new();
        let values = match client.list_models_inner().await {
            Ok(values) => values,
            Err(error) => {
                tracing::warn!(error = %error, "using cached SuperGrok model catalog");
                let cached = self.cached_models();
                if !cached.is_empty() {
                    return Ok(cached);
                }
                return Ok(vec![ModelDescriptor::new(&fallback, fallback.clone())?]);
            }
        };
        if self.oauth.auth_epoch() != auth_epoch {
            let cached = self.cached_models();
            if !cached.is_empty() {
                return Ok(cached);
            }
            return Ok(vec![ModelDescriptor::new(&fallback, fallback.clone())?]);
        }
        for value in values {
            let Some(id) = value
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
            else {
                continue;
            };
            if !seen.insert(id.to_string()) {
                continue;
            }
            let display_name = value
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(id);
            let selectable = value
                .get("supported")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if let Ok(model) = ModelDescriptor::new(id, display_name) {
                models.push(model.with_selectable(selectable));
            }
        }
        if models.is_empty() {
            let cached = self.cached_models();
            if !cached.is_empty() {
                return Ok(cached);
            }
            models.push(ModelDescriptor::new(&fallback, fallback.clone())?);
        } else {
            self.cache_models(auth_epoch, &models);
        }
        Ok(models)
    }

    fn create_backend(&self, model_id: &str) -> Arc<dyn Backend> {
        Arc::new(self.client(model_id))
    }
}

#[derive(Default)]
struct AuthFlowState {
    generation: u64,
    phase: AuthFlowPhase,
}

impl AuthFlowState {
    fn replace_phase(&mut self, phase: AuthFlowPhase) {
        let previous = std::mem::replace(&mut self.phase, phase);
        if let AuthFlowPhase::Polling { abort } = previous {
            abort.abort();
        }
    }
}

#[derive(Default)]
enum AuthFlowPhase {
    #[default]
    Idle,
    Starting,
    Polling {
        abort: AbortHandle,
    },
    Failed(String),
}

fn remaining_seconds(expires_at_ms: i64) -> u64 {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let remaining_ms = expires_at_ms.saturating_sub(now_ms).max(0) as u64;
    remaining_ms.div_ceil(1_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connector() -> SuperGrokConnector {
        let directory = tempfile::tempdir().unwrap();
        let credential_path = directory.path().join("oauth.json");
        SuperGrokConnector::new(Arc::new(SuperGrokConfig::new(credential_path))).unwrap()
    }

    #[test]
    fn descriptor_uses_stable_xai_identity_and_device_oauth() {
        let descriptor = connector().descriptor();
        assert_eq!(descriptor.id.as_str(), SUPERGROK_CONNECTOR_ID);
        assert_eq!(descriptor.auth_methods.len(), 1);
        assert_eq!(
            descriptor.auth_methods[0].id,
            SUPERGROK_DEVICE_AUTH_METHOD_ID
        );
        assert_eq!(
            descriptor.auth_methods[0].kind,
            ConnectorAuthKind::DeviceOauth
        );
        assert_eq!(
            descriptor.capabilities,
            vec![
                ConnectorCapability::SpeechToText,
                ConnectorCapability::ImageGeneration,
            ]
        );
    }

    #[tokio::test]
    async fn signed_out_status_and_logout_need_no_network() {
        let connector = connector();
        assert_eq!(
            connector.auth_status().await.unwrap(),
            ConnectorAuthStatus::SignedOut
        );
        connector.logout_auth().await.unwrap();
        assert_eq!(
            connector.auth_status().await.unwrap(),
            ConnectorAuthStatus::SignedOut
        );
    }

    #[tokio::test]
    async fn unknown_auth_method_is_rejected_before_network() {
        let error = connector().start_auth("browser", None).await.unwrap_err();
        assert!(matches!(error, ConnectorError::UnknownAuthMethod { .. }));
    }

    #[test]
    fn backend_model_changes_inside_xai_connector() {
        let connector = connector();
        let backend = connector.create_backend("grok-next");
        assert_eq!(backend.model(), "grok-next");
        backend.set_model("grok-later");
        assert_eq!(backend.model(), "grok-later");
        assert_eq!(connector.descriptor().id.as_str(), SUPERGROK_CONNECTOR_ID);
    }

    #[test]
    fn current_default_model_is_selectable_on_xai() {
        assert_eq!(connector().default_model_id(), DEFAULT_MODEL_ID);
    }

    #[tokio::test]
    async fn model_cache_cannot_cross_logout_or_be_restored_by_an_old_request() {
        let connector = connector();
        let old_epoch = connector.oauth.auth_epoch();
        let old_models = vec![ModelDescriptor::new("old-model", "Old model").unwrap()];
        connector.cache_models(old_epoch, &old_models);
        assert_eq!(connector.cached_models(), old_models);

        connector.logout_auth().await.unwrap();
        assert!(connector.cached_models().is_empty());

        connector.cache_models(old_epoch, &old_models);
        assert!(connector.cached_models().is_empty());
    }

    #[test]
    fn remaining_lifetime_never_underflows() {
        assert_eq!(remaining_seconds(0), 0);
    }
}
