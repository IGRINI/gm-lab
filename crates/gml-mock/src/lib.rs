//! Deterministic connector used by tests and offline demo runs.

mod client;

use std::sync::Arc;

use async_trait::async_trait;
use gml_llm::{
    Backend, ConnectorDescriptor, ConnectorError, ConnectorId, ModelConnector, ModelDescriptor,
};

pub use client::{mock_stats, MockClient};

pub struct MockConnector;

#[async_trait]
impl ModelConnector for MockConnector {
    fn default_model_id(&self) -> String {
        "mock".to_string()
    }

    fn legacy_backend_ids(&self) -> &'static [&'static str] {
        &["mock"]
    }

    fn descriptor(&self) -> ConnectorDescriptor {
        ConnectorDescriptor::new(
            ConnectorId::new("mock").expect("mock connector id is valid"),
            "Mock",
        )
        .expect("static mock descriptor is valid")
    }

    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError> {
        Ok(vec![ModelDescriptor::new("mock", "Mock")?])
    }

    fn create_backend(&self, model_id: &str) -> Arc<dyn Backend> {
        let client = Arc::new(MockClient::new());
        client.set_model(model_id);
        client
    }
}
