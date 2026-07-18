//! Provider-neutral connector contracts and registry.
//!
//! A connector is selected once when a history is created. Its model may be
//! changed later, but only inside that connector. The legacy [`crate::Backend`]
//! remains the runtime inference surface; connectors construct compatible
//! backend instances after the application has validated a [`ModelBinding`].

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use serde::{de, Deserialize, Deserializer, Serialize};

use crate::{Backend, ResponseLanguageBackend, ResponseLanguageSource};

const MAX_CONNECTOR_ID_CHARS: usize = 64;
const MAX_MODEL_ID_CHARS: usize = 256;
const MAX_LABEL_CHARS: usize = 120;

/// Stable connector identifier persisted with a history, for example `codex`
/// or `xai`.
///
/// IDs are deliberately path-safe because connector-owned files may later be
/// placed under an application-data directory named after this value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ConnectorId(String);

impl ConnectorId {
    /// Create a normalized, validated connector id.
    pub fn new(value: impl Into<String>) -> Result<Self, ConnectorError> {
        let raw = value.into();
        let value = raw.trim();
        if value.is_empty() {
            return Err(ConnectorError::InvalidConnectorId {
                value: raw,
                reason: "connector id is required".to_string(),
            });
        }
        if value.chars().count() > MAX_CONNECTOR_ID_CHARS {
            return Err(ConnectorError::InvalidConnectorId {
                value: raw,
                reason: format!("connector id exceeds {MAX_CONNECTOR_ID_CHARS} characters"),
            });
        }
        if !value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            return Err(ConnectorError::InvalidConnectorId {
                value: raw,
                reason: "connector id must start with a lowercase ASCII letter or digit"
                    .to_string(),
            });
        }
        if !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        }) {
            return Err(ConnectorError::InvalidConnectorId {
                value: raw,
                reason:
                    "connector id may contain only lowercase ASCII letters, digits, '-' and '_'"
                        .to_string(),
            });
        }
        Ok(Self(value.to_string()))
    }

    /// Borrow the canonical id.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConnectorId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ConnectorId {
    type Err = ConnectorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ConnectorId {
    type Error = ConnectorError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ConnectorId> for String {
    fn from(value: ConnectorId) -> Self {
        value.0
    }
}

impl<'de> Deserialize<'de> for ConnectorId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// Immutable connector choice plus the model currently selected inside it.
///
/// The fields are private on purpose: existing histories change model through
/// [`Self::with_model`], which cannot replace their connector id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "ModelBindingWire")]
pub struct ModelBinding {
    connector_id: ConnectorId,
    model_id: String,
}

#[derive(Deserialize)]
struct ModelBindingWire {
    connector_id: ConnectorId,
    model_id: String,
}

impl TryFrom<ModelBindingWire> for ModelBinding {
    type Error = ConnectorError;

    fn try_from(value: ModelBindingWire) -> Result<Self, Self::Error> {
        Self::new(value.connector_id, value.model_id)
    }
}

impl ModelBinding {
    /// Bind a history to one connector and one of its models.
    pub fn new(
        connector_id: ConnectorId,
        model_id: impl Into<String>,
    ) -> Result<Self, ConnectorError> {
        Ok(Self {
            connector_id,
            model_id: validate_model_id(model_id.into())?,
        })
    }

    /// Connector permanently assigned to this history.
    pub fn connector_id(&self) -> &ConnectorId {
        &self.connector_id
    }

    /// Current model inside the assigned connector.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Return a binding for another model without allowing a connector change.
    pub fn with_model(&self, model_id: impl Into<String>) -> Result<Self, ConnectorError> {
        Self::new(self.connector_id.clone(), model_id)
    }
}

/// Authentication mechanism offered by a connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorAuthKind {
    BrowserOauth,
    DeviceOauth,
    ApiKey,
}

/// One user-selectable authentication method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorAuthMethod {
    pub id: String,
    pub display_name: String,
    pub kind: ConnectorAuthKind,
}

impl ConnectorAuthMethod {
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        kind: ConnectorAuthKind,
    ) -> Result<Self, ConnectorError> {
        let id = validate_local_id(id.into(), "auth method id")?;
        let display_name = validate_label(display_name.into(), "auth method display name")?;
        Ok(Self {
            id,
            display_name,
            kind,
        })
    }
}

/// Optional provider-owned features exposed through the connector boundary.
///
/// Capabilities are descriptive as well as executable: callers can hide an
/// unavailable action without probing it, while the trait default still
/// rejects unsupported calls defensively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorCapability {
    SpeechToText,
    ImageGeneration,
}

/// Provider-neutral request for one batch of generated images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenerationRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub count: u32,
}

/// One generated image returned as owned bytes so provider URLs never leak
/// into persisted application state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub media_type: String,
}

/// Completed provider image generation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageGenerationResult {
    pub model: String,
    pub images: Vec<GeneratedImage>,
}

/// Static connector metadata. It is snapshotted when registered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorDescriptor {
    pub id: ConnectorId,
    pub display_name: String,
    #[serde(default)]
    pub auth_methods: Vec<ConnectorAuthMethod>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<ConnectorCapability>,
}

impl ConnectorDescriptor {
    pub fn new(id: ConnectorId, display_name: impl Into<String>) -> Result<Self, ConnectorError> {
        Ok(Self {
            id,
            display_name: validate_label(display_name.into(), "connector display name")?,
            auth_methods: Vec::new(),
            capabilities: Vec::new(),
        })
    }

    pub fn with_auth_method(mut self, method: ConnectorAuthMethod) -> Result<Self, ConnectorError> {
        if self.auth_methods.iter().any(|known| known.id == method.id) {
            return Err(ConnectorError::InvalidDescriptor {
                connector_id: self.id.clone(),
                reason: format!("duplicate auth method id: {}", method.id),
            });
        }
        self.auth_methods.push(method);
        Ok(self)
    }

    pub fn with_capability(mut self, capability: ConnectorCapability) -> Self {
        if !self.capabilities.contains(&capability) {
            self.capabilities.push(capability);
        }
        self
    }
}

/// Current connector authentication state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectorAuthStatus {
    NotRequired,
    SignedOut,
    Pending {
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    SignedIn {
        #[serde(skip_serializing_if = "Option::is_none")]
        account_label: Option<String>,
    },
    Expired {
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

/// User action returned after an OAuth flow has been started.
///
/// The connector owns the pending flow and exposes progress through
/// [`ConnectorAuthStatus::Pending`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConnectorAuthStart {
    Browser {
        authorization_url: String,
    },
    DeviceCode {
        verification_url: String,
        user_code: String,
        expires_in_seconds: u64,
        interval_seconds: u64,
    },
    Complete,
}

/// One model advertised by a connector.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub display_name: String,
    #[serde(default = "default_true")]
    pub selectable: bool,
}

impl ModelDescriptor {
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Result<Self, ConnectorError> {
        Ok(Self {
            id: validate_model_id(id.into())?,
            display_name: validate_label(display_name.into(), "model display name")?,
            selectable: true,
        })
    }

    pub fn with_selectable(mut self, selectable: bool) -> Self {
        self.selectable = selectable;
        self
    }
}

const fn default_true() -> bool {
    true
}

/// Connector and registry failures that are safe to surface at application
/// boundaries without exposing a concrete transport implementation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectorError {
    #[error("invalid connector id '{value}': {reason}")]
    InvalidConnectorId { value: String, reason: String },
    #[error("invalid model id '{value}': {reason}")]
    InvalidModelId { value: String, reason: String },
    #[error("invalid {field}: {reason}")]
    InvalidField { field: String, reason: String },
    #[error("invalid connector descriptor for '{connector_id}': {reason}")]
    InvalidDescriptor {
        connector_id: ConnectorId,
        reason: String,
    },
    #[error("connector already registered: {connector_id}")]
    DuplicateConnector { connector_id: ConnectorId },
    #[error("unknown connector: {connector_id}")]
    UnknownConnector { connector_id: ConnectorId },
    #[error("unknown auth method '{method_id}' for connector '{connector_id}'")]
    UnknownAuthMethod {
        connector_id: ConnectorId,
        method_id: String,
    },
    #[error("duplicate model '{model_id}' in connector '{connector_id}' catalog")]
    DuplicateModel {
        connector_id: ConnectorId,
        model_id: String,
    },
    #[error("unknown model '{model_id}' for connector '{connector_id}'")]
    UnknownModel {
        connector_id: ConnectorId,
        model_id: String,
    },
    #[error("model '{model_id}' is not selectable for connector '{connector_id}'")]
    ModelUnavailable {
        connector_id: ConnectorId,
        model_id: String,
    },
    #[error("connector '{connector_id}' does not support {operation}")]
    UnsupportedOperation {
        connector_id: ConnectorId,
        operation: String,
    },
    #[error("connector '{connector_id}': {message}")]
    Operation {
        connector_id: ConnectorId,
        message: String,
    },
    #[error("connector '{connector_id}' {operation} failed (HTTP {status}): {message}")]
    HttpOperation {
        connector_id: ConnectorId,
        operation: String,
        status: u16,
        message: String,
    },
}

impl ConnectorError {
    pub fn operation(connector_id: ConnectorId, message: impl fmt::Display) -> Self {
        Self::Operation {
            connector_id,
            message: message.to_string(),
        }
    }

    pub fn http_operation(
        connector_id: ConnectorId,
        operation: impl Into<String>,
        status: u16,
        message: impl fmt::Display,
    ) -> Self {
        Self::HttpOperation {
            connector_id,
            operation: operation.into(),
            status,
            message: message.to_string(),
        }
    }

    /// Provider HTTP status suitable for an application boundary, when the
    /// connector received one.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::HttpOperation { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// Provider-owned behavior. The application core owns histories and tool
/// execution; a connector owns authentication, its model catalog, and creation
/// of provider-specific [`Backend`] instances.
#[async_trait]
pub trait ModelConnector: Send + Sync {
    fn descriptor(&self) -> ConnectorDescriptor;

    /// Connector-owned model used for brand-new histories when the caller did
    /// not explicitly choose one.
    fn default_model_id(&self) -> String;

    /// Pre-connector backend names owned by this connector. The registry uses
    /// them only for history migration; aliases never affect newly stored ids.
    fn legacy_backend_ids(&self) -> &'static [&'static str] {
        &[]
    }

    async fn auth_status(&self) -> Result<ConnectorAuthStatus, ConnectorError> {
        Ok(ConnectorAuthStatus::NotRequired)
    }

    async fn start_auth(
        &self,
        _method_id: &str,
        _ui_language: Option<&str>,
    ) -> Result<ConnectorAuthStart, ConnectorError> {
        let connector_id = self.descriptor().id;
        Err(ConnectorError::UnsupportedOperation {
            connector_id,
            operation: "authentication".to_string(),
        })
    }

    async fn logout_auth(&self) -> Result<(), ConnectorError> {
        let connector_id = self.descriptor().id;
        Err(ConnectorError::UnsupportedOperation {
            connector_id,
            operation: "logout".to_string(),
        })
    }

    /// Transcribe one complete audio clip. Language is a formatting hint, not
    /// a requirement that the provider restrict recognition to that language.
    async fn transcribe(
        &self,
        _audio: &[u8],
        _content_type: &str,
        _language: Option<&str>,
    ) -> Result<String, ConnectorError> {
        let connector_id = self.descriptor().id;
        Err(ConnectorError::UnsupportedOperation {
            connector_id,
            operation: "speech-to-text".to_string(),
        })
    }

    async fn generate_images(
        &self,
        _request: &ImageGenerationRequest,
    ) -> Result<ImageGenerationResult, ConnectorError> {
        let connector_id = self.descriptor().id;
        Err(ConnectorError::UnsupportedOperation {
            connector_id,
            operation: "image generation".to_string(),
        })
    }

    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError>;

    /// Construct a backend synchronously for lazy NPC/generator factories.
    /// Model availability must be checked with [`ConnectorRegistry::validate_binding`]
    /// before the history is started or its model is changed.
    fn create_backend(&self, model_id: &str) -> Arc<dyn Backend>;
}

struct RegisteredConnector {
    descriptor: ConnectorDescriptor,
    default_model_id: String,
    legacy_backend_ids: Vec<String>,
    connector: Arc<dyn ModelConnector>,
}

/// Process-wide, thread-safe connector catalog.
///
/// Registry locks are never held across connector code or `.await` points.
#[derive(Default)]
pub struct ConnectorRegistry {
    connectors: RwLock<HashMap<ConnectorId, RegisteredConnector>>,
    response_language_source: Option<Arc<dyn ResponseLanguageSource>>,
}

impl fmt::Debug for ConnectorRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectorRegistry")
            .field("len", &self.len())
            .finish()
    }
}

impl ConnectorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a registry that applies one application-owned response language
    /// policy to every backend created by every connector.
    pub fn with_response_language_source<S>(source: S) -> Self
    where
        S: ResponseLanguageSource + 'static,
    {
        Self {
            connectors: RwLock::new(HashMap::new()),
            response_language_source: Some(Arc::new(source)),
        }
    }

    /// Register one application-lifetime connector. Replacing an existing id is
    /// rejected so histories cannot silently switch implementation.
    pub fn register(&self, connector: Arc<dyn ModelConnector>) -> Result<(), ConnectorError> {
        let descriptor = connector.descriptor();
        validate_connector_descriptor(&descriptor)?;
        let connector_id = descriptor.id.clone();
        let default_model_id = validate_model_id(connector.default_model_id())
            .map_err(|error| descriptor_error(&connector_id, error))?;
        let legacy_backend_ids =
            normalize_legacy_backend_ids(&connector_id, connector.legacy_backend_ids())?;
        let mut connectors = self
            .connectors
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if connectors.contains_key(&connector_id) {
            return Err(ConnectorError::DuplicateConnector { connector_id });
        }
        if connectors.values().any(|entry| {
            entry
                .legacy_backend_ids
                .iter()
                .any(|alias| alias == connector_id.as_str())
        }) {
            return Err(ConnectorError::InvalidDescriptor {
                connector_id: connector_id.clone(),
                reason: "connector id is already registered as a legacy backend id".to_string(),
            });
        }
        if let Some(alias) = legacy_backend_ids.iter().find(|alias| {
            connectors.values().any(|entry| {
                entry.descriptor.id.as_str() == alias.as_str()
                    || entry.legacy_backend_ids.contains(alias)
            })
        }) {
            return Err(ConnectorError::InvalidDescriptor {
                connector_id,
                reason: format!("legacy backend id '{alias}' is already registered"),
            });
        }
        connectors.insert(
            connector_id,
            RegisteredConnector {
                descriptor,
                default_model_id,
                legacy_backend_ids,
                connector,
            },
        );
        Ok(())
    }

    /// Resolve a pre-connector backend name through aliases declared by the
    /// connector that owns that migration.
    pub fn resolve_legacy_backend(&self, backend: &str) -> Option<ConnectorId> {
        let backend = backend.trim().to_ascii_lowercase();
        if backend.is_empty() {
            return None;
        }
        self.connectors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .find(|entry| {
                entry.descriptor.id.as_str() == backend
                    || entry
                        .legacy_backend_ids
                        .iter()
                        .any(|alias| alias == &backend)
            })
            .map(|entry| entry.descriptor.id.clone())
    }

    /// Binding for a brand-new history, with the model selected by the
    /// connector rather than by application-core conditionals.
    pub fn default_binding(&self, id: &ConnectorId) -> Result<ModelBinding, ConnectorError> {
        let connectors = self
            .connectors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = connectors
            .get(id)
            .ok_or_else(|| ConnectorError::UnknownConnector {
                connector_id: id.clone(),
            })?;
        ModelBinding::new(id.clone(), entry.default_model_id.clone())
    }

    /// Stable descriptor snapshot, sorted by id for deterministic APIs/tests.
    pub fn descriptors(&self) -> Vec<ConnectorDescriptor> {
        let connectors = self
            .connectors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut descriptors = connectors
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect::<Vec<_>>();
        descriptors.sort_by(|left, right| left.id.cmp(&right.id));
        descriptors
    }

    pub fn connector(&self, id: &ConnectorId) -> Option<Arc<dyn ModelConnector>> {
        self.connectors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .map(|entry| entry.connector.clone())
    }

    pub fn len(&self) -> usize {
        self.connectors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub async fn auth_status(
        &self,
        id: &ConnectorId,
    ) -> Result<ConnectorAuthStatus, ConnectorError> {
        self.require_connector(id)?.auth_status().await
    }

    pub async fn start_auth(
        &self,
        id: &ConnectorId,
        method_id: &str,
        ui_language: Option<&str>,
    ) -> Result<ConnectorAuthStart, ConnectorError> {
        let method_id = method_id.trim();
        let (connector, descriptor) = self.require_entry(id)?;
        if !descriptor
            .auth_methods
            .iter()
            .any(|method| method.id == method_id)
        {
            return Err(ConnectorError::UnknownAuthMethod {
                connector_id: id.clone(),
                method_id: method_id.to_string(),
            });
        }
        connector.start_auth(method_id, ui_language).await
    }

    pub async fn logout_auth(&self, id: &ConnectorId) -> Result<(), ConnectorError> {
        self.require_connector(id)?.logout_auth().await
    }

    pub async fn list_models(
        &self,
        id: &ConnectorId,
    ) -> Result<Vec<ModelDescriptor>, ConnectorError> {
        let connector = self.require_connector(id)?;
        let models = connector.list_models().await?;
        validate_model_catalog(id, &models)?;
        Ok(models)
    }

    /// Route speech-to-text through the connector assigned to the current
    /// history. No registry lock is held while audio is uploaded.
    pub async fn transcribe(
        &self,
        id: &ConnectorId,
        audio: &[u8],
        content_type: &str,
        language: Option<&str>,
    ) -> Result<String, ConnectorError> {
        let (connector, descriptor) = self.require_entry(id)?;
        if !descriptor
            .capabilities
            .contains(&ConnectorCapability::SpeechToText)
        {
            return Err(ConnectorError::UnsupportedOperation {
                connector_id: id.clone(),
                operation: "speech-to-text".to_string(),
            });
        }
        connector.transcribe(audio, content_type, language).await
    }

    /// Route image generation through a capable connector without exposing its
    /// authentication or transport details to the application server.
    pub async fn generate_images(
        &self,
        id: &ConnectorId,
        request: &ImageGenerationRequest,
    ) -> Result<ImageGenerationResult, ConnectorError> {
        let (connector, descriptor) = self.require_entry(id)?;
        if !descriptor
            .capabilities
            .contains(&ConnectorCapability::ImageGeneration)
        {
            return Err(ConnectorError::UnsupportedOperation {
                connector_id: id.clone(),
                operation: "image generation".to_string(),
            });
        }
        connector.generate_images(request).await
    }

    /// Confirm that the connector exists and the model is currently selectable.
    pub async fn validate_binding(
        &self,
        binding: &ModelBinding,
    ) -> Result<ModelDescriptor, ConnectorError> {
        let models = self.list_models(binding.connector_id()).await?;
        let model = models
            .into_iter()
            .find(|model| model.id == binding.model_id())
            .ok_or_else(|| ConnectorError::UnknownModel {
                connector_id: binding.connector_id().clone(),
                model_id: binding.model_id().to_string(),
            })?;
        if !model.selectable {
            return Err(ConnectorError::ModelUnavailable {
                connector_id: binding.connector_id().clone(),
                model_id: binding.model_id().to_string(),
            });
        }
        Ok(model)
    }

    /// Validate a model switch while mechanically preserving the history's
    /// connector assignment.
    pub async fn validate_model_change(
        &self,
        current: &ModelBinding,
        model_id: &str,
    ) -> Result<ModelBinding, ConnectorError> {
        let candidate = current.with_model(model_id)?;
        self.validate_binding(&candidate).await?;
        Ok(candidate)
    }

    /// Synchronously build a backend after startup/model-change validation.
    pub fn create_backend(
        &self,
        binding: &ModelBinding,
    ) -> Result<Arc<dyn Backend>, ConnectorError> {
        let connector = self.require_connector(binding.connector_id())?;
        let backend = connector.create_backend(binding.model_id());
        match &self.response_language_source {
            Some(source) => Ok(Arc::new(ResponseLanguageBackend::new(
                backend,
                source.clone(),
            ))),
            None => Ok(backend),
        }
    }

    fn require_connector(
        &self,
        id: &ConnectorId,
    ) -> Result<Arc<dyn ModelConnector>, ConnectorError> {
        self.connector(id)
            .ok_or_else(|| ConnectorError::UnknownConnector {
                connector_id: id.clone(),
            })
    }

    fn require_entry(
        &self,
        id: &ConnectorId,
    ) -> Result<(Arc<dyn ModelConnector>, ConnectorDescriptor), ConnectorError> {
        self.connectors
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .map(|entry| (entry.connector.clone(), entry.descriptor.clone()))
            .ok_or_else(|| ConnectorError::UnknownConnector {
                connector_id: id.clone(),
            })
    }
}

fn validate_connector_descriptor(descriptor: &ConnectorDescriptor) -> Result<(), ConnectorError> {
    validate_label(descriptor.display_name.clone(), "connector display name")
        .map_err(|error| descriptor_error(&descriptor.id, error))?;
    let mut method_ids = HashSet::new();
    for method in &descriptor.auth_methods {
        let normalized = validate_local_id(method.id.clone(), "auth method id")
            .map_err(|error| descriptor_error(&descriptor.id, error))?;
        if normalized != method.id {
            return Err(ConnectorError::InvalidDescriptor {
                connector_id: descriptor.id.clone(),
                reason: format!("auth method id must be canonical: {}", method.id),
            });
        }
        validate_label(method.display_name.clone(), "auth method display name")
            .map_err(|error| descriptor_error(&descriptor.id, error))?;
        if !method_ids.insert(method.id.clone()) {
            return Err(ConnectorError::InvalidDescriptor {
                connector_id: descriptor.id.clone(),
                reason: format!("duplicate auth method id: {}", method.id),
            });
        }
    }
    let mut capabilities = HashSet::new();
    for capability in &descriptor.capabilities {
        if !capabilities.insert(*capability) {
            return Err(ConnectorError::InvalidDescriptor {
                connector_id: descriptor.id.clone(),
                reason: format!("duplicate connector capability: {capability:?}"),
            });
        }
    }
    Ok(())
}

fn normalize_legacy_backend_ids(
    connector_id: &ConnectorId,
    aliases: &[&str],
) -> Result<Vec<String>, ConnectorError> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for alias in aliases {
        let alias = alias.trim().to_ascii_lowercase();
        if alias.is_empty()
            || alias.len() > MAX_CONNECTOR_ID_CHARS
            || !alias.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.')
            })
        {
            return Err(ConnectorError::InvalidDescriptor {
                connector_id: connector_id.clone(),
                reason: format!("invalid legacy backend id: {alias}"),
            });
        }
        if alias == connector_id.as_str() {
            continue;
        }
        if !seen.insert(alias.clone()) {
            return Err(ConnectorError::InvalidDescriptor {
                connector_id: connector_id.clone(),
                reason: format!("duplicate legacy backend id: {alias}"),
            });
        }
        normalized.push(alias);
    }
    Ok(normalized)
}

fn validate_model_catalog(
    connector_id: &ConnectorId,
    models: &[ModelDescriptor],
) -> Result<(), ConnectorError> {
    let mut model_ids = HashSet::new();
    for model in models {
        let normalized = validate_model_id(model.id.clone())?;
        if normalized != model.id {
            return Err(ConnectorError::Operation {
                connector_id: connector_id.clone(),
                message: format!("model id must be canonical: {}", model.id),
            });
        }
        validate_label(model.display_name.clone(), "model display name")
            .map_err(|error| descriptor_error(connector_id, error))?;
        if !model_ids.insert(model.id.clone()) {
            return Err(ConnectorError::DuplicateModel {
                connector_id: connector_id.clone(),
                model_id: model.id.clone(),
            });
        }
    }
    Ok(())
}

fn descriptor_error(connector_id: &ConnectorId, error: ConnectorError) -> ConnectorError {
    ConnectorError::InvalidDescriptor {
        connector_id: connector_id.clone(),
        reason: error.to_string(),
    }
}

fn validate_model_id(raw: String) -> Result<String, ConnectorError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(ConnectorError::InvalidModelId {
            value: raw,
            reason: "model id is required".to_string(),
        });
    }
    if value.chars().count() > MAX_MODEL_ID_CHARS {
        return Err(ConnectorError::InvalidModelId {
            value: raw,
            reason: format!("model id exceeds {MAX_MODEL_ID_CHARS} characters"),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ConnectorError::InvalidModelId {
            value: raw,
            reason: "model id may not contain control characters".to_string(),
        });
    }
    Ok(value.to_string())
}

fn validate_local_id(raw: String, field: &str) -> Result<String, ConnectorError> {
    ConnectorId::new(raw.clone())
        .map(|id| id.0)
        .map_err(|error| ConnectorError::InvalidField {
            field: field.to_string(),
            reason: error.to_string(),
        })
}

fn validate_label(raw: String, field: &str) -> Result<String, ConnectorError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(ConnectorError::InvalidField {
            field: field.to_string(),
            reason: "value is required".to_string(),
        });
    }
    if value.chars().count() > MAX_LABEL_CHARS {
        return Err(ConnectorError::InvalidField {
            field: field.to_string(),
            reason: format!("value exceeds {MAX_LABEL_CHARS} characters"),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(ConnectorError::InvalidField {
            field: field.to_string(),
            reason: "value may not contain control characters".to_string(),
        });
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    use serde_json::{Map, Value};

    use super::*;
    use crate::{BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput};

    struct TestBackend {
        model: Mutex<String>,
    }

    impl TestBackend {
        fn new() -> Self {
            Self {
                model: Mutex::new("test".to_string()),
            }
        }
    }

    #[async_trait]
    impl Backend for TestBackend {
        fn model(&self) -> String {
            self.model.lock().expect("model lock").clone()
        }

        fn set_model(&self, model: &str) {
            let model = model.trim();
            if !model.is_empty() {
                *self.model.lock().expect("model lock") = model.to_string();
            }
        }

        async fn list_models(&self) -> Vec<Value> {
            vec![]
        }

        async fn chat(
            &self,
            _messages: &Value,
            _tools: Option<&Value>,
            _think: Option<bool>,
            _reasoning_role: &str,
        ) -> Result<ChatOutput, BackendError> {
            Err(BackendError::new("not used by connector registry tests"))
        }

        async fn chat_json(
            &self,
            _messages: &Value,
            _think: Option<bool>,
            _reasoning_role: &str,
        ) -> Result<Map<String, Value>, BackendError> {
            Err(BackendError::new("not used by connector registry tests"))
        }

        async fn summarize(
            &self,
            _text: &str,
            _proper_nouns: &[String],
        ) -> Result<String, BackendError> {
            Err(BackendError::new("not used by connector registry tests"))
        }

        async fn chat_stream(
            &self,
            _messages: &Value,
            _tools: Option<&Value>,
            _think: Option<bool>,
            _reasoning_role: &str,
            _sink: &mut (dyn DeltaSink + Send),
        ) -> Result<ChatStreamOutput, BackendError> {
            Err(BackendError::new("not used by connector registry tests"))
        }

        async fn chat_json_stream(
            &self,
            _messages: &Value,
            _think: Option<bool>,
            _reasoning_role: &str,
            _sink: &mut (dyn DeltaSink + Send),
        ) -> Result<JsonStreamOutput, BackendError> {
            Err(BackendError::new("not used by connector registry tests"))
        }
    }

    struct TestConnector {
        descriptor: ConnectorDescriptor,
        models: Vec<ModelDescriptor>,
        create_count: AtomicUsize,
    }

    impl TestConnector {
        fn new(id: &str, models: &[(&str, bool)]) -> Self {
            let descriptor =
                ConnectorDescriptor::new(ConnectorId::new(id).unwrap(), format!("Connector {id}"))
                    .unwrap()
                    .with_capability(ConnectorCapability::SpeechToText)
                    .with_auth_method(
                        ConnectorAuthMethod::new(
                            "device",
                            "Device OAuth",
                            ConnectorAuthKind::DeviceOauth,
                        )
                        .unwrap(),
                    )
                    .unwrap();
            Self {
                descriptor,
                models: models
                    .iter()
                    .map(|(id, selectable)| {
                        ModelDescriptor::new(*id, format!("Model {id}"))
                            .unwrap()
                            .with_selectable(*selectable)
                    })
                    .collect(),
                create_count: AtomicUsize::new(0),
            }
        }
    }

    struct DefaultAuthConnector(TestConnector);

    struct LegacyConnector(TestConnector);

    #[async_trait]
    impl ModelConnector for LegacyConnector {
        fn descriptor(&self) -> ConnectorDescriptor {
            self.0.descriptor()
        }

        fn default_model_id(&self) -> String {
            self.0.default_model_id()
        }

        fn legacy_backend_ids(&self) -> &'static [&'static str] {
            &["old-provider"]
        }

        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError> {
            self.0.list_models().await
        }

        fn create_backend(&self, model_id: &str) -> Arc<dyn Backend> {
            self.0.create_backend(model_id)
        }
    }

    #[async_trait]
    impl ModelConnector for DefaultAuthConnector {
        fn descriptor(&self) -> ConnectorDescriptor {
            let mut descriptor = self.0.descriptor.clone();
            descriptor.auth_methods.clear();
            descriptor
        }

        fn default_model_id(&self) -> String {
            self.0.default_model_id()
        }

        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError> {
            Ok(self.0.models.clone())
        }

        fn create_backend(&self, model_id: &str) -> Arc<dyn Backend> {
            self.0.create_backend(model_id)
        }
    }

    #[async_trait]
    impl ModelConnector for TestConnector {
        fn descriptor(&self) -> ConnectorDescriptor {
            self.descriptor.clone()
        }

        fn default_model_id(&self) -> String {
            self.models
                .first()
                .map(|model| model.id.clone())
                .unwrap_or_else(|| "default".to_string())
        }

        async fn auth_status(&self) -> Result<ConnectorAuthStatus, ConnectorError> {
            Ok(ConnectorAuthStatus::SignedOut)
        }

        async fn start_auth(
            &self,
            _method_id: &str,
            _ui_language: Option<&str>,
        ) -> Result<ConnectorAuthStart, ConnectorError> {
            Ok(ConnectorAuthStart::DeviceCode {
                verification_url: "https://example.test/device".to_string(),
                user_code: "ABCD-EFGH".to_string(),
                expires_in_seconds: 600,
                interval_seconds: 5,
            })
        }

        async fn logout_auth(&self) -> Result<(), ConnectorError> {
            Ok(())
        }

        async fn transcribe(
            &self,
            audio: &[u8],
            content_type: &str,
            language: Option<&str>,
        ) -> Result<String, ConnectorError> {
            Ok(format!(
                "{}:{}:{}",
                audio.len(),
                content_type,
                language.unwrap_or_default()
            ))
        }

        async fn generate_images(
            &self,
            request: &ImageGenerationRequest,
        ) -> Result<ImageGenerationResult, ConnectorError> {
            Ok(ImageGenerationResult {
                model: request
                    .model
                    .clone()
                    .unwrap_or_else(|| "image-default".to_string()),
                images: vec![GeneratedImage {
                    bytes: b"image".to_vec(),
                    media_type: "image/png".to_string(),
                }],
            })
        }

        async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ConnectorError> {
            Ok(self.models.clone())
        }

        fn create_backend(&self, model_id: &str) -> Arc<dyn Backend> {
            self.create_count.fetch_add(1, Ordering::Relaxed);
            let backend = TestBackend::new();
            backend.set_model(model_id);
            Arc::new(backend)
        }
    }

    #[test]
    fn connector_ids_are_stable_and_path_safe() {
        assert_eq!(ConnectorId::new(" codex ").unwrap().as_str(), "codex");
        for invalid in ["", "Codex", "x.ai", "../xai", "xai/oauth"] {
            assert!(ConnectorId::new(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn binding_round_trips_and_model_change_preserves_connector() {
        let binding = ModelBinding::new(ConnectorId::new("codex").unwrap(), "gpt-5.6").unwrap();
        let changed = binding.with_model("gpt-5.6-mini").unwrap();
        assert_eq!(changed.connector_id(), binding.connector_id());
        assert_eq!(changed.model_id(), "gpt-5.6-mini");

        let encoded = serde_json::to_string(&changed).unwrap();
        assert_eq!(
            encoded,
            r#"{"connector_id":"codex","model_id":"gpt-5.6-mini"}"#
        );
        assert_eq!(
            serde_json::from_str::<ModelBinding>(&encoded).unwrap(),
            changed
        );
        assert!(
            serde_json::from_str::<ModelBinding>(r#"{"connector_id":"codex","model_id":""}"#)
                .is_err()
        );
    }

    #[test]
    fn legacy_backend_resolution_is_connector_owned() {
        let registry = ConnectorRegistry::new();
        registry
            .register(Arc::new(LegacyConnector(TestConnector::new(
                "modern-provider",
                &[("model", true)],
            ))))
            .unwrap();

        assert_eq!(
            registry
                .resolve_legacy_backend(" OLD-PROVIDER ")
                .unwrap()
                .as_str(),
            "modern-provider"
        );
        assert_eq!(
            registry
                .resolve_legacy_backend("modern-provider")
                .unwrap()
                .as_str(),
            "modern-provider"
        );
        assert_eq!(
            registry
                .default_binding(&ConnectorId::new("modern-provider").unwrap())
                .unwrap()
                .model_id(),
            "model"
        );
        assert!(registry.resolve_legacy_backend("unknown").is_none());
        assert!(matches!(
            registry.register(Arc::new(TestConnector::new(
                "old-provider",
                &[("model", true)],
            ))),
            Err(ConnectorError::InvalidDescriptor { .. })
        ));
    }

    #[test]
    fn concurrent_registration_is_safe_and_descriptors_are_sorted() {
        let registry = Arc::new(ConnectorRegistry::new());
        let handles = (0..16)
            .map(|index| {
                let registry = registry.clone();
                std::thread::spawn(move || {
                    registry
                        .register(Arc::new(TestConnector::new(
                            &format!("connector-{index:02}"),
                            &[("model", true)],
                        )))
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(registry.len(), 16);
        let ids = registry
            .descriptors()
            .into_iter()
            .map(|descriptor| descriptor.id.to_string())
            .collect::<Vec<_>>();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
    }

    #[test]
    fn duplicate_registration_is_rejected_without_replacement() {
        let registry = ConnectorRegistry::new();
        registry
            .register(Arc::new(TestConnector::new("codex", &[("a", true)])))
            .unwrap();
        let error = registry
            .register(Arc::new(TestConnector::new("codex", &[("b", true)])))
            .unwrap_err();
        assert!(matches!(error, ConnectorError::DuplicateConnector { .. }));
        assert_eq!(registry.len(), 1);
    }

    #[tokio::test]
    async fn binding_validation_checks_connector_catalog_and_availability() {
        let registry = ConnectorRegistry::new();
        registry
            .register(Arc::new(TestConnector::new(
                "xai",
                &[("grok-4", true), ("grok-preview", false)],
            )))
            .unwrap();
        let binding = ModelBinding::new(ConnectorId::new("xai").unwrap(), "grok-4").unwrap();
        assert_eq!(
            registry.validate_binding(&binding).await.unwrap().id,
            "grok-4"
        );
        let changed = registry
            .validate_model_change(&binding, "grok-preview")
            .await
            .unwrap_err();
        assert!(matches!(changed, ConnectorError::ModelUnavailable { .. }));
        let unknown = registry
            .validate_model_change(&binding, "missing")
            .await
            .unwrap_err();
        assert!(matches!(unknown, ConnectorError::UnknownModel { .. }));
    }

    #[tokio::test]
    async fn auth_and_backend_calls_are_routed_without_holding_registry_locks() {
        let registry = ConnectorRegistry::new();
        registry
            .register(Arc::new(TestConnector::new("xai", &[("grok-4", true)])))
            .unwrap();
        let connector_id = ConnectorId::new("xai").unwrap();
        assert_eq!(
            registry.auth_status(&connector_id).await.unwrap(),
            ConnectorAuthStatus::SignedOut
        );
        assert!(matches!(
            registry
                .start_auth(&connector_id, "device", Some("en"))
                .await
                .unwrap(),
            ConnectorAuthStart::DeviceCode { .. }
        ));
        assert!(matches!(
            registry.start_auth(&connector_id, "browser", None).await,
            Err(ConnectorError::UnknownAuthMethod { .. })
        ));
        registry.logout_auth(&connector_id).await.unwrap();

        assert_eq!(
            registry
                .transcribe(&connector_id, b"audio", "audio/webm", Some("ru"))
                .await
                .unwrap(),
            "5:audio/webm:ru"
        );

        let binding = ModelBinding::new(connector_id, "grok-4").unwrap();
        let backend = registry.create_backend(&binding).unwrap();
        assert_eq!(backend.model(), "grok-4");
    }

    #[tokio::test]
    async fn default_auth_contract_is_explicitly_not_required_and_not_logoutable() {
        let registry = ConnectorRegistry::new();
        registry
            .register(Arc::new(DefaultAuthConnector(TestConnector::new(
                "mock",
                &[("mock", true)],
            ))))
            .unwrap();
        let connector_id = ConnectorId::new("mock").unwrap();
        assert_eq!(
            registry.auth_status(&connector_id).await.unwrap(),
            ConnectorAuthStatus::NotRequired
        );
        assert!(matches!(
            registry.logout_auth(&connector_id).await,
            Err(ConnectorError::UnsupportedOperation { .. })
        ));
    }

    #[tokio::test]
    async fn duplicate_catalog_entries_are_rejected() {
        let registry = ConnectorRegistry::new();
        registry
            .register(Arc::new(TestConnector::new(
                "xai",
                &[("grok-4", true), ("grok-4", true)],
            )))
            .unwrap();
        let connector_id = ConnectorId::new("xai").unwrap();
        assert!(matches!(
            registry.list_models(&connector_id).await,
            Err(ConnectorError::DuplicateModel { .. })
        ));
    }

    #[tokio::test]
    async fn registry_rejects_unadvertised_speech_to_text() {
        let registry = ConnectorRegistry::new();
        let mut connector = TestConnector::new("mock", &[("mock", true)]);
        connector.descriptor.capabilities.clear();
        registry.register(Arc::new(connector)).unwrap();
        let connector_id = ConnectorId::new("mock").unwrap();
        assert!(matches!(
            registry
                .transcribe(&connector_id, b"audio", "audio/webm", None)
                .await,
            Err(ConnectorError::UnsupportedOperation { .. })
        ));
    }

    #[tokio::test]
    async fn image_generation_requires_capability_and_routes_owned_bytes() {
        let request = ImageGenerationRequest {
            prompt: "test".to_string(),
            model: Some("grok-imagine-image".to_string()),
            width: Some(1024),
            height: Some(1024),
            count: 1,
        };
        let unadvertised = ConnectorRegistry::new();
        unadvertised
            .register(Arc::new(TestConnector::new(
                "without-images",
                &[("model", true)],
            )))
            .unwrap();
        assert!(matches!(
            unadvertised
                .generate_images(&ConnectorId::new("without-images").unwrap(), &request)
                .await,
            Err(ConnectorError::UnsupportedOperation { .. })
        ));

        let registry = ConnectorRegistry::new();
        let mut connector = TestConnector::new("xai", &[("grok-4", true)]);
        connector
            .descriptor
            .capabilities
            .push(ConnectorCapability::ImageGeneration);
        registry.register(Arc::new(connector)).unwrap();
        let connector_id = ConnectorId::new("xai").unwrap();
        let generated = registry
            .generate_images(&connector_id, &request)
            .await
            .unwrap();
        assert_eq!(generated.model, "grok-imagine-image");
        assert_eq!(generated.images[0].bytes, b"image");
        assert_eq!(generated.images[0].media_type, "image/png");
    }

    #[test]
    fn http_operation_preserves_status_without_losing_context() {
        let error = ConnectorError::http_operation(
            ConnectorId::new("xai").unwrap(),
            "speech-to-text",
            429,
            "rate limited",
        );
        assert_eq!(error.http_status(), Some(429));
        assert!(error.to_string().contains("speech-to-text"));
    }
}
