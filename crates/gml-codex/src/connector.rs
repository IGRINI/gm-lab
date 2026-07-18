//! Codex implementation of the provider-neutral connector boundary.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use gml_config::{Config, RuntimeSettings};
use gml_llm::{
    Backend, ConnectorAuthKind, ConnectorAuthMethod, ConnectorAuthStart, ConnectorAuthStatus,
    ConnectorDescriptor, ConnectorError, ConnectorId, ModelConnector, ModelDescriptor,
};

use crate::{auth_status, revoke_credential, run_oauth, CodexClient};

pub struct CodexConnector {
    config: Arc<Config>,
    settings: Arc<RuntimeSettings>,
    http: reqwest::Client,
}

impl CodexConnector {
    pub fn new(config: Arc<Config>, settings: Arc<RuntimeSettings>) -> Self {
        Self {
            config,
            settings,
            http: reqwest::Client::new(),
        }
    }

    fn id() -> ConnectorId {
        ConnectorId::new("codex").expect("codex connector id is valid")
    }

    fn fallback_model(&self) -> String {
        if !self.config.codex_model.trim().is_empty() {
            self.config.codex_model.trim().to_string()
        } else if !self.config.model.trim().is_empty() {
            self.config.model.trim().to_string()
        } else {
            "gpt-5.4".to_string()
        }
    }
}

#[async_trait]
impl ModelConnector for CodexConnector {
    fn default_model_id(&self) -> String {
        self.fallback_model()
    }

    fn legacy_backend_ids(&self) -> &'static [&'static str] {
        &["codex", "codex-oauth"]
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor::new(Self::id(), "Codex")
            .and_then(|descriptor| {
                descriptor.with_auth_method(ConnectorAuthMethod::new(
                    "chatgpt",
                    "ChatGPT OAuth",
                    ConnectorAuthKind::BrowserOauth,
                )?)
            })
            .expect("static Codex descriptor is valid")
    }

    async fn auth_status(&self) -> Result<ConnectorAuthStatus, ConnectorError> {
        let status = auth_status();
        if status
            .get("authenticated")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let account_label = status
                .get("account_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            Ok(ConnectorAuthStatus::SignedIn { account_label })
        } else {
            Ok(ConnectorAuthStatus::SignedOut)
        }
    }

    async fn start_auth(
        &self,
        method_id: &str,
        ui_language: Option<&str>,
    ) -> Result<ConnectorAuthStart, ConnectorError> {
        if method_id != "chatgpt" {
            return Err(ConnectorError::UnknownAuthMethod {
                connector_id: Self::id(),
                method_id: method_id.to_string(),
            });
        }
        run_oauth(&self.http, &self.config, ui_language)
            .await
            .map_err(|error| ConnectorError::operation(Self::id(), error))?;
        Ok(ConnectorAuthStart::Complete)
    }

    async fn logout_auth(&self) -> Result<(), ConnectorError> {
        revoke_credential(&self.http, &self.config)
            .await
            .map_err(|error| ConnectorError::operation(Self::id(), error))
    }

    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError> {
        let client = CodexClient::new(self.config.clone(), self.settings.clone());
        let mut models = Vec::new();
        for value in Backend::list_models(&client).await {
            let Some(id) = value.get("id").and_then(Value::as_str) else {
                continue;
            };
            let display_name = value.get("name").and_then(Value::as_str).unwrap_or(id);
            let selectable = value
                .get("supported")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            if let Ok(model) = ModelDescriptor::new(id, display_name) {
                models.push(model.with_selectable(selectable));
            }
        }
        if models.is_empty() {
            let fallback = self.fallback_model();
            models.push(ModelDescriptor::new(&fallback, fallback.clone())?);
        }
        Ok(models)
    }

    fn create_backend(&self, model_id: &str) -> Arc<dyn Backend> {
        let client = Arc::new(CodexClient::new(self.config.clone(), self.settings.clone()));
        client.set_model(model_id);
        client
    }
}
