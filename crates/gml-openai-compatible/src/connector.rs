use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use gml_config::{Config, RuntimeSettings};
use gml_llm::{
    Backend, ConnectorDescriptor, ConnectorError, ConnectorId, ModelConnector, ModelDescriptor,
};

use crate::OpenAICompatClient;

/// Connector for configurable OpenAI-compatible chat-completions endpoints.
pub struct OpenAICompatConnector {
    config: Arc<Config>,
    settings: Arc<RuntimeSettings>,
    default_model: String,
}

impl OpenAICompatConnector {
    pub async fn discover(config: Arc<Config>, settings: Arc<RuntimeSettings>) -> Self {
        let client = OpenAICompatClient::new(config.clone(), settings.clone()).await;
        let default_model = {
            let model = client.model();
            if model.trim().is_empty() {
                "default".to_string()
            } else {
                model
            }
        };
        Self {
            config,
            settings,
            default_model,
        }
    }

    fn id() -> ConnectorId {
        ConnectorId::new("openai-compatible").expect("openai-compatible connector id is valid")
    }
}

#[async_trait]
impl ModelConnector for OpenAICompatConnector {
    fn default_model_id(&self) -> String {
        self.default_model.clone()
    }

    fn legacy_backend_ids(&self) -> &'static [&'static str] {
        &[
            "openai-compatible",
            "openai",
            "openai_compat",
            "llamacpp",
            "llama.cpp",
        ]
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor::new(Self::id(), "OpenAI-compatible")
            .expect("static OpenAI-compatible descriptor is valid")
    }

    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError> {
        let client = OpenAICompatClient::with_model(
            self.config.clone(),
            self.settings.clone(),
            self.default_model.clone(),
        );
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
            models.push(ModelDescriptor::new(
                &self.default_model,
                self.default_model.clone(),
            )?);
        }
        Ok(models)
    }

    fn create_backend(&self, model_id: &str) -> Arc<dyn Backend> {
        Arc::new(OpenAICompatClient::with_model(
            self.config.clone(),
            self.settings.clone(),
            model_id,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    #[tokio::test]
    async fn discover_uses_the_advertised_model_as_default() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 2048];
            let size = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..size]);
            assert!(request.starts_with("GET /v1/models "));
            let body = r#"{"data":[{"id":"advertised-model"}]}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        });

        let mut config = Config::from_env();
        config.model.clear();
        config.api_base = format!("http://{address}");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let settings = RuntimeSettings::new(
            &config,
            std::env::temp_dir().join(format!("gml-openai-discovery-{nonce}.json")),
        );
        let connector = OpenAICompatConnector::discover(Arc::new(config), Arc::new(settings)).await;

        assert_eq!(connector.default_model_id(), "advertised-model");
        server.await.unwrap();
    }
}
