use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use once_cell::sync::Lazy;
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::SuperGrokConfig;

static CREDENTIAL_LOCK: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));

const CREDENTIAL_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CREDENTIAL_SAVE_ATTEMPTS: usize = 3;
const CREDENTIAL_SAVE_RETRY_DELAY: Duration = Duration::from_millis(50);
const DEFAULT_DEVICE_POLL_INTERVAL_SECONDS: u64 = 5;
const AUTH_SESSION_ENABLED: u64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("SuperGrok is not connected")]
    NotAuthenticated,
    #[error("SuperGrok OAuth credential is invalid: {0}")]
    InvalidCredential(String),
    #[error("invalid xAI OAuth endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("SuperGrok OAuth transport failed: {0}")]
    Transport(String),
    #[error("SuperGrok OAuth storage failed: {0}")]
    Storage(String),
    #[error("SuperGrok authorization expired; start login again")]
    DeviceCodeExpired,
    #[error("SuperGrok authorization was denied")]
    AccessDenied,
    #[error("SuperGrok subscription does not permit xAI API access: {0}")]
    TierDenied(String),
    #[error("SuperGrok login is required again: {0}")]
    ReauthenticationRequired(String),
    #[error("SuperGrok OAuth failed ({status}): {message}")]
    Provider {
        status: u16,
        code: String,
        message: String,
    },
}

/// Plain connector-owned credential stored in the application's config data.
/// Debug output is deliberately redacted even though the file itself is not
/// encrypted.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthCredential {
    #[serde(default = "credential_version")]
    pub version: u32,
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub id_token: String,
    #[serde(default = "default_token_type")]
    pub token_type: String,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    #[serde(default)]
    pub token_endpoint: String,
}

impl fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthCredential")
            .field("version", &self.version)
            .field("access_token", &"[redacted]")
            .field("refresh_token", &"[redacted]")
            .field("id_token", &"[redacted]")
            .field("token_type", &self.token_type)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("token_endpoint", &self.token_endpoint)
            .finish()
    }
}

impl OAuthCredential {
    fn validate(mut self) -> Result<Self, OAuthError> {
        if self.version != credential_version() {
            return Err(OAuthError::InvalidCredential(format!(
                "unsupported version {}",
                self.version
            )));
        }
        if self.access_token.trim().is_empty() {
            return Err(OAuthError::InvalidCredential(
                "access_token is missing".to_string(),
            ));
        }
        if self.refresh_token.trim().is_empty() {
            return Err(OAuthError::InvalidCredential(
                "refresh_token is missing".to_string(),
            ));
        }
        if !self.token_endpoint.trim().is_empty() {
            validate_xai_https_endpoint(&self.token_endpoint, "token endpoint")?;
        }
        self.access_token = self.access_token.trim().to_string();
        self.refresh_token = self.refresh_token.trim().to_string();
        self.id_token = self.id_token.trim().to_string();
        self.token_type = self.token_type.trim().to_string();
        self.token_endpoint = self.token_endpoint.trim().to_string();
        Ok(self)
    }

    fn effective_expiry_ms(&self) -> Option<i64> {
        self.expires_at_ms
            .or_else(|| jwt_expiry_ms(&self.access_token))
    }

    fn needs_refresh(&self, margin: Duration) -> bool {
        self.effective_expiry_ms()
            .is_some_and(|expiry| expiry <= now_ms().saturating_add(duration_ms(margin)))
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DeviceChallenge {
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub user_code: String,
    pub expires_at_ms: i64,
    pub poll_interval_seconds: u64,
}

/// Opaque device authorization handle. Its serialized/public view never
/// contains the device code or token endpoint.
#[derive(Clone)]
pub struct DeviceAuthorization {
    challenge: DeviceChallenge,
    device_code: String,
    token_endpoint: String,
    deadline: Instant,
}

impl fmt::Debug for DeviceAuthorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceAuthorization")
            .field("challenge", &self.challenge)
            .field("device_code", &"[redacted]")
            .field("token_endpoint", &self.token_endpoint)
            .finish()
    }
}

impl DeviceAuthorization {
    pub fn challenge(&self) -> &DeviceChallenge {
        &self.challenge
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AuthStatus {
    pub authenticated: bool,
    pub expires_at_ms: Option<i64>,
    pub refresh_required: bool,
    pub error: Option<String>,
}

#[derive(Clone)]
pub struct SuperGrokOAuth {
    config: Arc<SuperGrokConfig>,
    http: Client,
    volatile_credential: Arc<Mutex<Option<VolatileCredential>>>,
    auth_session: Arc<AtomicU64>,
}

#[derive(Clone)]
struct VolatileCredential {
    credential: OAuthCredential,
    stale_disk_refresh_token: Option<String>,
    storage_error: String,
}

struct CredentialCandidate {
    credential: OAuthCredential,
    stale_disk_refresh_token: Option<String>,
    pending_persistence: bool,
}

struct CredentialStoreGuard {
    _in_process: tokio::sync::MutexGuard<'static, ()>,
    lock_file: File,
}

impl Drop for CredentialStoreGuard {
    fn drop(&mut self) {
        let _ = File::unlock(&self.lock_file);
    }
}

impl SuperGrokOAuth {
    pub fn new(config: Arc<SuperGrokConfig>) -> Result<Self, OAuthError> {
        let http = Client::builder()
            .connect_timeout(config.oauth_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| OAuthError::Transport(error.to_string()))?;
        Self::with_http(config, http)
    }

    pub fn with_http(config: Arc<SuperGrokConfig>, http: Client) -> Result<Self, OAuthError> {
        validate_xai_https_endpoint(&config.discovery_url, "discovery URL")?;
        validate_xai_https_endpoint(&config.device_code_url, "device-code URL")?;
        validate_xai_https_endpoint(&config.inference_base_url, "inference base URL")?;
        if config.client_id.trim().is_empty() {
            return Err(OAuthError::InvalidCredential(
                "OAuth client_id is empty".to_string(),
            ));
        }
        Ok(Self {
            config,
            http,
            volatile_credential: Arc::new(Mutex::new(None)),
            auth_session: Arc::new(AtomicU64::new(AUTH_SESSION_ENABLED)),
        })
    }

    pub fn credential_path(&self) -> &Path {
        &self.config.credential_path
    }

    pub fn status(&self) -> AuthStatus {
        if !self.auth_requests_enabled() {
            return AuthStatus {
                authenticated: false,
                expires_at_ms: None,
                refresh_required: false,
                error: None,
            };
        }
        match self.current_credential() {
            Ok(candidate) => AuthStatus {
                authenticated: true,
                expires_at_ms: candidate.credential.effective_expiry_ms(),
                refresh_required: candidate
                    .credential
                    .needs_refresh(self.config.refresh_margin),
                error: self.volatile_storage_error(),
            },
            Err(OAuthError::NotAuthenticated) => AuthStatus {
                authenticated: false,
                expires_at_ms: None,
                refresh_required: false,
                error: None,
            },
            Err(error) => AuthStatus {
                authenticated: false,
                expires_at_ms: None,
                refresh_required: false,
                error: Some(error.to_string()),
            },
        }
    }

    /// Start RFC 8628 device authorization. The caller may show the returned
    /// challenge immediately and keep the opaque handle for [`Self::poll`].
    pub async fn start(&self) -> Result<DeviceAuthorization, OAuthError> {
        let discovery = self.discovery().await?;
        let response = self
            .http
            .post(&self.config.device_code_url)
            .timeout(self.config.oauth_timeout)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("client_id", self.config.client_id.as_str()),
                ("scope", self.config.scope.as_str()),
            ])
            .send()
            .await
            .map_err(|error| OAuthError::Transport(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| OAuthError::Transport(error.to_string()))?;
        if !status.is_success() {
            return Err(provider_error(status, &body));
        }
        parse_device_authorization(&body, &discovery.token_endpoint)
    }

    /// Poll until the user approves, denies, or the device code expires. A
    /// successful result is atomically persisted before it is returned.
    pub async fn poll(
        &self,
        authorization: &DeviceAuthorization,
    ) -> Result<OAuthCredential, OAuthError> {
        let mut interval =
            Duration::from_secs(authorization.challenge.poll_interval_seconds.max(1));
        loop {
            if Instant::now() >= authorization.deadline {
                return Err(OAuthError::DeviceCodeExpired);
            }
            let response = self
                .http
                .post(&authorization.token_endpoint)
                .timeout(self.config.oauth_timeout)
                .header(reqwest::header::ACCEPT, "application/json")
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", self.config.client_id.as_str()),
                    ("device_code", authorization.device_code.as_str()),
                ])
                .send()
                .await
                .map_err(|error| OAuthError::Transport(error.to_string()))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .map_err(|error| OAuthError::Transport(error.to_string()))?;
            if status.is_success() {
                let credential = parse_token_response(&body, &authorization.token_endpoint, None)?;
                let _guard = self.lock_credential_store().await?;
                let stale_disk_refresh_token = load_credential(&self.config.credential_path)
                    .ok()
                    .map(|stored| stored.refresh_token);
                let _ = self
                    .persist_or_remember(&credential, stale_disk_refresh_token)
                    .await;
                self.rotate_auth_session(true);
                return Ok(credential);
            }
            match oauth_error_code(&body).as_deref() {
                Some("authorization_pending") => {}
                Some("slow_down") => {
                    interval = interval.saturating_add(Duration::from_secs(5));
                }
                Some("expired_token") => return Err(OAuthError::DeviceCodeExpired),
                Some("access_denied") => return Err(OAuthError::AccessDenied),
                _ => return Err(provider_error(status, &body)),
            }
            let remaining = authorization
                .deadline
                .saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(OAuthError::DeviceCodeExpired);
            }
            tokio::time::sleep(interval.min(remaining)).await;
        }
    }

    pub async fn login(&self) -> Result<(DeviceChallenge, OAuthCredential), OAuthError> {
        let authorization = self.start().await?;
        let challenge = authorization.challenge.clone();
        let credential = self.poll(&authorization).await?;
        Ok((challenge, credential))
    }

    pub async fn ensure_fresh(&self, force: bool) -> Result<OAuthCredential, OAuthError> {
        self.begin_request()?;
        if !force {
            let candidate = self.current_credential()?;
            if !candidate.pending_persistence
                && !candidate
                    .credential
                    .needs_refresh(self.config.refresh_margin)
            {
                return Ok(candidate.credential);
            }
        }
        self.refresh_locked(force, None).await
    }

    /// Retry a previously failed atomic write without forcing a new token
    /// rotation. Authentication status calls use this to heal transient disk
    /// failures and surface persistent ones.
    pub async fn ensure_persisted(&self) -> Result<(), OAuthError> {
        if self
            .volatile_credential
            .lock()
            .expect("volatile credential lock")
            .is_none()
        {
            return Ok(());
        }
        let _guard = self.lock_credential_store().await?;
        let candidate = self.current_credential()?;
        if !candidate.pending_persistence {
            return Ok(());
        }
        self.persist_or_remember(&candidate.credential, candidate.stale_disk_refresh_token)
            .await
    }

    /// Recover from a 401 without rotating a refresh token that another
    /// concurrent request has already exchanged.
    pub async fn refresh_after_unauthorized(
        &self,
        rejected_access_token: &str,
    ) -> Result<OAuthCredential, OAuthError> {
        self.begin_request()?;
        self.refresh_locked(true, Some(rejected_access_token.trim()))
            .await
    }

    pub async fn logout(&self) -> Result<(), OAuthError> {
        self.rotate_auth_session(false);
        let _guard = match self.lock_credential_store().await {
            Ok(guard) => guard,
            Err(error) => {
                self.rotate_auth_session(true);
                return Err(error);
            }
        };
        if let Err(error) = remove_credential(&self.config.credential_path) {
            self.rotate_auth_session(true);
            return Err(error);
        }
        self.clear_volatile_credential();
        Ok(())
    }

    pub(crate) fn auth_epoch(&self) -> u64 {
        self.auth_session.load(Ordering::Acquire)
    }

    pub(crate) fn begin_request(&self) -> Result<u64, OAuthError> {
        let epoch = self.auth_epoch();
        if epoch & AUTH_SESSION_ENABLED == 0 {
            Err(OAuthError::NotAuthenticated)
        } else {
            Ok(epoch)
        }
    }

    pub(crate) fn request_epoch_is_current(&self, epoch: u64) -> bool {
        epoch & AUTH_SESSION_ENABLED != 0 && self.auth_epoch() == epoch
    }

    fn auth_requests_enabled(&self) -> bool {
        self.auth_epoch() & AUTH_SESSION_ENABLED != 0
    }

    fn rotate_auth_session(&self, enabled: bool) -> u64 {
        let enabled_bit = u64::from(enabled);
        loop {
            let current = self.auth_epoch();
            let generation = (current >> 1).wrapping_add(1);
            let next = (generation << 1) | enabled_bit;
            if self
                .auth_session
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return next;
            }
        }
    }

    async fn refresh_locked(
        &self,
        force: bool,
        rejected_access_token: Option<&str>,
    ) -> Result<OAuthCredential, OAuthError> {
        let _guard = self.lock_credential_store().await?;
        let candidate = self.current_credential()?;
        let current = candidate.credential;
        let mut stale_disk_refresh_token = candidate.stale_disk_refresh_token;

        if candidate.pending_persistence
            && self
                .persist_or_remember(&current, stale_disk_refresh_token.clone())
                .await
                .is_ok()
        {
            stale_disk_refresh_token = Some(current.refresh_token.clone());
        }

        if can_reuse_after_unauthorized(&current, rejected_access_token, self.config.refresh_margin)
        {
            return Ok(current);
        }
        if !force && !current.needs_refresh(self.config.refresh_margin) {
            return Ok(current);
        }

        let token_endpoint = if current.token_endpoint.trim().is_empty() {
            self.discovery().await?.token_endpoint
        } else {
            validate_xai_https_endpoint(&current.token_endpoint, "token endpoint")?;
            current.token_endpoint.clone()
        };
        let response = self
            .http
            .post(&token_endpoint)
            .timeout(self.config.oauth_timeout)
            .header(reqwest::header::ACCEPT, "application/json")
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", self.config.client_id.as_str()),
                ("refresh_token", current.refresh_token.as_str()),
            ])
            .send()
            .await
            .map_err(|error| OAuthError::Transport(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| OAuthError::Transport(error.to_string()))?;
        if !status.is_success() {
            let invalid_refresh_token = refresh_token_is_invalid(&body);
            let error = refresh_error(status, &body);
            if invalid_refresh_token {
                let _ = remove_credential(&self.config.credential_path);
                self.clear_volatile_credential();
            }
            return Err(error);
        }

        let refreshed = parse_token_response(&body, &token_endpoint, Some(&current))?;
        let _ = self
            .persist_or_remember(&refreshed, stale_disk_refresh_token)
            .await;
        Ok(refreshed)
    }

    async fn lock_credential_store(&self) -> Result<CredentialStoreGuard, OAuthError> {
        let in_process = CREDENTIAL_LOCK.lock().await;
        let lock_path = credential_lock_path(&self.config.credential_path);
        if let Some(parent) = lock_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| OAuthError::Storage(error.to_string()))?;
        }
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| OAuthError::Storage(error.to_string()))?;
        let wait_timeout = self
            .config
            .oauth_timeout
            .saturating_mul(2)
            .max(Duration::from_secs(10));
        let deadline = Instant::now() + wait_timeout;
        loop {
            match lock_file.try_lock() {
                Ok(()) => {
                    return Ok(CredentialStoreGuard {
                        _in_process: in_process,
                        lock_file,
                    })
                }
                Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                    tokio::time::sleep(CREDENTIAL_LOCK_POLL_INTERVAL).await;
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(OAuthError::Storage(
                        "timed out waiting for the SuperGrok credential lock".to_string(),
                    ))
                }
                Err(TryLockError::Error(error)) => {
                    return Err(OAuthError::Storage(error.to_string()))
                }
            }
        }
    }

    fn current_credential(&self) -> Result<CredentialCandidate, OAuthError> {
        let disk = load_credential(&self.config.credential_path);
        let mut volatile = self
            .volatile_credential
            .lock()
            .expect("volatile credential lock");
        if let Some(pending) = volatile.as_ref().cloned() {
            match disk {
                Ok(stored) if stored.refresh_token == pending.credential.refresh_token => {
                    *volatile = None;
                    let refresh_token = stored.refresh_token.clone();
                    return Ok(CredentialCandidate {
                        credential: stored,
                        stale_disk_refresh_token: Some(refresh_token),
                        pending_persistence: false,
                    });
                }
                Ok(stored)
                    if pending.stale_disk_refresh_token.as_deref()
                        == Some(stored.refresh_token.as_str()) =>
                {
                    return Ok(CredentialCandidate {
                        credential: pending.credential,
                        stale_disk_refresh_token: pending.stale_disk_refresh_token,
                        pending_persistence: true,
                    });
                }
                Ok(stored) => {
                    *volatile = None;
                    let refresh_token = stored.refresh_token.clone();
                    return Ok(CredentialCandidate {
                        credential: stored,
                        stale_disk_refresh_token: Some(refresh_token),
                        pending_persistence: false,
                    });
                }
                Err(_) => {
                    return Ok(CredentialCandidate {
                        credential: pending.credential,
                        stale_disk_refresh_token: pending.stale_disk_refresh_token,
                        pending_persistence: true,
                    });
                }
            }
        }
        disk.map(|credential| CredentialCandidate {
            stale_disk_refresh_token: Some(credential.refresh_token.clone()),
            credential,
            pending_persistence: false,
        })
    }

    async fn persist_or_remember(
        &self,
        credential: &OAuthCredential,
        stale_disk_refresh_token: Option<String>,
    ) -> Result<(), OAuthError> {
        let mut last_error = None;
        for attempt in 0..CREDENTIAL_SAVE_ATTEMPTS {
            match save_credential_atomic(&self.config.credential_path, credential) {
                Ok(()) => {
                    self.clear_volatile_credential();
                    return Ok(());
                }
                Err(error) => last_error = Some(error.to_string()),
            }
            if attempt + 1 < CREDENTIAL_SAVE_ATTEMPTS {
                tokio::time::sleep(CREDENTIAL_SAVE_RETRY_DELAY).await;
            }
        }
        let storage_error =
            last_error.unwrap_or_else(|| "credential persistence failed".to_string());
        *self
            .volatile_credential
            .lock()
            .expect("volatile credential lock") = Some(VolatileCredential {
            credential: credential.clone(),
            stale_disk_refresh_token,
            storage_error: storage_error.clone(),
        });
        Err(OAuthError::Storage(storage_error))
    }

    fn clear_volatile_credential(&self) {
        *self
            .volatile_credential
            .lock()
            .expect("volatile credential lock") = None;
    }

    fn volatile_storage_error(&self) -> Option<String> {
        self.volatile_credential
            .lock()
            .expect("volatile credential lock")
            .as_ref()
            .map(|credential| {
                format!(
                    "credential is usable only until this app exits: {}",
                    credential.storage_error
                )
            })
    }

    async fn discovery(&self) -> Result<Discovery, OAuthError> {
        let response = self
            .http
            .get(&self.config.discovery_url)
            .timeout(self.config.oauth_timeout)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|error| OAuthError::Transport(error.to_string()))?;
        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|error| OAuthError::Transport(error.to_string()))?;
        if !status.is_success() {
            return Err(provider_error(status, &body));
        }
        parse_discovery(&body)
    }
}

#[derive(Debug)]
struct Discovery {
    #[allow(dead_code)]
    authorization_endpoint: String,
    token_endpoint: String,
}

fn parse_discovery(body: &str) -> Result<Discovery, OAuthError> {
    let value: Value = serde_json::from_str(body).map_err(|error| {
        OAuthError::InvalidCredential(format!("invalid discovery JSON: {error}"))
    })?;
    let authorization_endpoint = required_string(&value, "authorization_endpoint")?;
    let token_endpoint = required_string(&value, "token_endpoint")?;
    validate_xai_https_endpoint(&authorization_endpoint, "authorization endpoint")?;
    validate_xai_https_endpoint(&token_endpoint, "token endpoint")?;
    Ok(Discovery {
        authorization_endpoint,
        token_endpoint,
    })
}

fn parse_device_authorization(
    body: &str,
    token_endpoint: &str,
) -> Result<DeviceAuthorization, OAuthError> {
    let value: Value = serde_json::from_str(body).map_err(|error| {
        OAuthError::InvalidCredential(format!("invalid device-code JSON: {error}"))
    })?;
    let device_code = required_string(&value, "device_code")?;
    let user_code = required_string(&value, "user_code")?;
    let verification_uri = required_string(&value, "verification_uri")?;
    let verification_uri_complete = value
        .get("verification_uri_complete")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&verification_uri)
        .to_string();
    validate_xai_https_browser_url(&verification_uri, "verification URI")?;
    validate_xai_https_browser_url(&verification_uri_complete, "complete verification URI")?;
    let expires_in = positive_u64(&value, "expires_in")?;
    let interval = value
        .get("interval")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_DEVICE_POLL_INTERVAL_SECONDS);
    validate_xai_https_endpoint(token_endpoint, "token endpoint")?;
    Ok(DeviceAuthorization {
        challenge: DeviceChallenge {
            verification_uri,
            verification_uri_complete,
            user_code,
            expires_at_ms: now_ms().saturating_add(seconds_ms(expires_in)),
            poll_interval_seconds: interval,
        },
        device_code,
        token_endpoint: token_endpoint.to_string(),
        deadline: Instant::now() + Duration::from_secs(expires_in),
    })
}

fn parse_token_response(
    body: &str,
    token_endpoint: &str,
    previous: Option<&OAuthCredential>,
) -> Result<OAuthCredential, OAuthError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| OAuthError::InvalidCredential(format!("invalid token JSON: {error}")))?;
    let access_token = required_string(&value, "access_token")?;
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.trim().is_empty())
        .map(str::to_string)
        .or_else(|| previous.map(|credential| credential.refresh_token.clone()))
        .ok_or_else(|| OAuthError::InvalidCredential("refresh_token is missing".to_string()))?;
    let expires_at_ms = value
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|seconds| now_ms().saturating_add(seconds_ms(seconds)))
        .or_else(|| jwt_expiry_ms(&access_token));
    OAuthCredential {
        version: credential_version(),
        access_token,
        refresh_token,
        id_token: value
            .get("id_token")
            .and_then(Value::as_str)
            .filter(|token| !token.trim().is_empty())
            .map(str::to_string)
            .or_else(|| previous.map(|credential| credential.id_token.clone()))
            .unwrap_or_default(),
        token_type: value
            .get("token_type")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("Bearer")
            .to_string(),
        expires_at_ms,
        token_endpoint: token_endpoint.to_string(),
    }
    .validate()
}

fn load_credential(path: &Path) -> Result<OAuthCredential, OAuthError> {
    let body = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(OAuthError::NotAuthenticated)
        }
        Err(error) => return Err(OAuthError::Storage(error.to_string())),
    };
    serde_json::from_str::<OAuthCredential>(&body)
        .map_err(|error| OAuthError::InvalidCredential(error.to_string()))?
        .validate()
}

fn save_credential_atomic(path: &Path, credential: &OAuthCredential) -> Result<(), OAuthError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent).map_err(|error| OAuthError::Storage(error.to_string()))?;
    }
    let directory = parent.unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| OAuthError::Storage(error.to_string()))?;
    let body =
        serde_json::to_vec(credential).map_err(|error| OAuthError::Storage(error.to_string()))?;
    temporary
        .write_all(&body)
        .and_then(|_| temporary.flush())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| OAuthError::Storage(error.to_string()))?;
    temporary
        .persist(path)
        .map_err(|error| OAuthError::Storage(error.error.to_string()))?;
    Ok(())
}

fn remove_credential(path: &Path) -> Result<(), OAuthError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(OAuthError::Storage(error.to_string())),
    }
}

fn credential_lock_path(path: &Path) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(".lock");
    PathBuf::from(value)
}

fn validate_xai_https_endpoint(value: &str, field: &str) -> Result<Url, OAuthError> {
    let url = validate_xai_https_url(value, field)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(OAuthError::InvalidEndpoint(format!(
            "{field} must not contain a query or fragment"
        )));
    }
    Ok(url)
}

fn validate_xai_https_browser_url(value: &str, field: &str) -> Result<Url, OAuthError> {
    validate_xai_https_url(value, field)
}

fn validate_xai_https_url(value: &str, field: &str) -> Result<Url, OAuthError> {
    let url = Url::parse(value)
        .map_err(|error| OAuthError::InvalidEndpoint(format!("{field}: {error}")))?;
    if url.scheme() != "https" {
        return Err(OAuthError::InvalidEndpoint(format!(
            "{field} must use HTTPS"
        )));
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    if host != "x.ai" && !host.ends_with(".x.ai") {
        return Err(OAuthError::InvalidEndpoint(format!(
            "{field} host must be x.ai"
        )));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(OAuthError::InvalidEndpoint(format!(
            "{field} must not contain user information"
        )));
    }
    if url.port().is_some_and(|port| port != 443) {
        return Err(OAuthError::InvalidEndpoint(format!(
            "{field} must use the default HTTPS port"
        )));
    }
    Ok(url)
}

fn refresh_error(status: StatusCode, body: &str) -> OAuthError {
    let message = oauth_error_message(body);
    let code = oauth_error_code(body).unwrap_or_default();
    if matches!(code.as_str(), "invalid_grant" | "invalid_token") {
        return OAuthError::ReauthenticationRequired(message);
    }
    if status == StatusCode::FORBIDDEN {
        return OAuthError::TierDenied(message);
    }
    provider_error(status, body)
}

fn refresh_token_is_invalid(body: &str) -> bool {
    oauth_error_code(body).is_some_and(|code| code == "invalid_grant")
}

fn can_reuse_after_unauthorized(
    current: &OAuthCredential,
    rejected_access_token: Option<&str>,
    refresh_margin: Duration,
) -> bool {
    rejected_access_token.is_some_and(|rejected| {
        !rejected.is_empty()
            && current.access_token != rejected
            && !current.needs_refresh(refresh_margin)
    })
}

fn provider_error(status: StatusCode, body: &str) -> OAuthError {
    OAuthError::Provider {
        status: status.as_u16(),
        code: oauth_error_code(body).unwrap_or_default(),
        message: oauth_error_message(body),
    }
}

fn oauth_error_code(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body).ok().and_then(|value| {
        value
            .get("error")
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn oauth_error_message(body: &str) -> String {
    let parsed = serde_json::from_str::<Value>(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| {
            value
                .get("error_description")
                .and_then(Value::as_str)
                .or_else(|| value.get("message").and_then(Value::as_str))
                .or_else(|| value.get("error").and_then(Value::as_str))
        })
        .unwrap_or(body);
    let compact = message.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.chars().take(1_000).collect()
}

fn required_string(value: &Value, key: &str) -> Result<String, OAuthError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| OAuthError::InvalidCredential(format!("{key} is missing")))
}

fn positive_u64(value: &Value, key: &str) -> Result<u64, OAuthError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| OAuthError::InvalidCredential(format!("{key} must be positive")))
}

fn jwt_expiry_ms(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let seconds = value.get("exp")?.as_i64()?;
    seconds.checked_mul(1_000)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

fn duration_ms(duration: Duration) -> i64 {
    duration.as_millis().min(i64::MAX as u128) as i64
}

fn seconds_ms(seconds: u64) -> i64 {
    seconds.saturating_mul(1_000).min(i64::MAX as u64) as i64
}

const fn credential_version() -> u32 {
    1
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn credential(token: &str) -> OAuthCredential {
        OAuthCredential {
            version: 1,
            access_token: token.to_string(),
            refresh_token: "refresh".to_string(),
            id_token: String::new(),
            token_type: "Bearer".to_string(),
            expires_at_ms: Some(now_ms() + 60_000),
            token_endpoint: "https://auth.x.ai/oauth2/token".to_string(),
        }
    }

    #[test]
    fn endpoint_pinning_accepts_only_xai_https() {
        assert!(validate_xai_https_url("https://auth.x.ai/oauth2/token", "token").is_ok());
        assert!(validate_xai_https_url("https://x.ai/token", "token").is_ok());
        assert!(validate_xai_https_url("http://auth.x.ai/token", "token").is_err());
        assert!(validate_xai_https_url("https://x.ai.evil.test/token", "token").is_err());
        assert!(validate_xai_https_url("https://x.ai@evil.test/token", "token").is_err());
    }

    #[test]
    fn machine_endpoints_reject_ambiguous_url_components() {
        assert!(validate_xai_https_endpoint("https://auth.x.ai/token?next=evil", "token").is_err());
        assert!(validate_xai_https_endpoint("https://auth.x.ai/token#fragment", "token").is_err());
        assert!(validate_xai_https_endpoint("https://user@auth.x.ai/token", "token").is_err());
        assert!(validate_xai_https_endpoint("https://auth.x.ai:8443/token", "token").is_err());
        assert!(validate_xai_https_browser_url(
            "https://auth.x.ai/device?user_code=ABCD",
            "verification"
        )
        .is_ok());
    }

    #[test]
    fn discovery_requires_and_pins_endpoints() {
        let parsed = parse_discovery(
            r#"{"authorization_endpoint":"https://auth.x.ai/oauth2/authorize","token_endpoint":"https://auth.x.ai/oauth2/token"}"#,
        )
        .unwrap();
        assert_eq!(parsed.token_endpoint, "https://auth.x.ai/oauth2/token");
        assert!(parse_discovery(
            r#"{"authorization_endpoint":"https://auth.x.ai/oauth2/authorize","token_endpoint":"https://evil.test/token"}"#
        )
        .is_err());
    }

    #[test]
    fn device_public_challenge_does_not_expose_device_code() {
        let authorization = parse_device_authorization(
            r#"{"device_code":"secret-device","user_code":"ABCD","verification_uri":"https://auth.x.ai/device","verification_uri_complete":"https://auth.x.ai/device?code=ABCD","expires_in":600,"interval":5}"#,
            "https://auth.x.ai/oauth2/token",
        )
        .unwrap();
        let serialized = serde_json::to_string(authorization.challenge()).unwrap();
        assert!(!serialized.contains("secret-device"));
        assert!(serialized.contains("ABCD"));
    }

    #[test]
    fn device_poll_interval_defaults_to_rfc_value() {
        let authorization = parse_device_authorization(
            r#"{"device_code":"secret-device","user_code":"ABCD","verification_uri":"https://auth.x.ai/device","expires_in":600}"#,
            "https://auth.x.ai/oauth2/token",
        )
        .unwrap();
        assert_eq!(
            authorization.challenge().poll_interval_seconds,
            DEFAULT_DEVICE_POLL_INTERVAL_SECONDS
        );
    }

    #[test]
    fn device_challenge_rejects_non_xai_verification_url() {
        assert!(parse_device_authorization(
            r#"{"device_code":"secret-device","user_code":"ABCD","verification_uri":"https://evil.test/device","expires_in":600,"interval":5}"#,
            "https://auth.x.ai/oauth2/token",
        )
        .is_err());
    }

    #[test]
    fn refresh_preserves_rotating_token_when_omitted() {
        let previous = credential("old-access");
        let refreshed = parse_token_response(
            r#"{"access_token":"new-access","expires_in":900}"#,
            "https://auth.x.ai/oauth2/token",
            Some(&previous),
        )
        .unwrap();
        assert_eq!(refreshed.refresh_token, "refresh");
        assert_eq!(refreshed.access_token, "new-access");
    }

    #[test]
    fn token_response_normalizes_credential_fields() {
        let parsed = parse_token_response(
            r#"{"access_token":" access ","refresh_token":" refresh ","id_token":" id ","token_type":" Bearer "}"#,
            "https://auth.x.ai/oauth2/token",
            None,
        )
        .unwrap();
        assert_eq!(parsed.access_token, "access");
        assert_eq!(parsed.refresh_token, "refresh");
        assert_eq!(parsed.id_token, "id");
        assert_eq!(parsed.token_type, "Bearer");
    }

    #[test]
    fn atomic_store_round_trips_and_replaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connectors/xai/auth.json");
        save_credential_atomic(&path, &credential("first")).unwrap();
        assert_eq!(load_credential(&path).unwrap().access_token, "first");
        save_credential_atomic(&path, &credential("second")).unwrap();
        assert_eq!(load_credential(&path).unwrap().access_token, "second");
    }

    #[tokio::test]
    async fn transient_storage_failure_is_retried_without_token_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("blocked-auth-path");
        std::fs::create_dir(&path).unwrap();
        let oauth = SuperGrokOAuth::new(Arc::new(SuperGrokConfig::new(&path))).unwrap();
        let expected = credential("rotated-access");

        assert!(oauth.persist_or_remember(&expected, None).await.is_err());
        let pending = oauth.status();
        assert!(pending.authenticated);
        assert!(pending.error.is_some());

        std::fs::remove_dir(&path).unwrap();
        oauth.ensure_persisted().await.unwrap();
        assert_eq!(
            load_credential(&path).unwrap().access_token,
            "rotated-access"
        );
        assert!(oauth.status().error.is_none());
    }

    #[test]
    fn credential_store_lock_serializes_independent_file_handles() {
        let dir = tempfile::tempdir().unwrap();
        let path = credential_lock_path(&dir.path().join("auth.json"));
        let first = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .unwrap();
        first.try_lock().unwrap();
        assert!(matches!(second.try_lock(), Err(TryLockError::WouldBlock)));
        File::unlock(&first).unwrap();
        second.try_lock().unwrap();
        File::unlock(&second).unwrap();
    }

    #[test]
    fn jwt_expiry_is_used_when_explicit_expiry_is_absent() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&json!({"exp": 12345})).unwrap());
        let token = format!("header.{payload}.signature");
        assert_eq!(jwt_expiry_ms(&token), Some(12_345_000));
    }

    #[test]
    fn refresh_error_classifies_relogin_and_tier_denial() {
        assert!(matches!(
            refresh_error(StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#),
            OAuthError::ReauthenticationRequired(_)
        ));
        assert!(matches!(
            refresh_error(StatusCode::FORBIDDEN, r#"{"error":"forbidden"}"#),
            OAuthError::TierDenied(_)
        ));
        assert!(matches!(
            refresh_error(StatusCode::FORBIDDEN, r#"{"error":"invalid_grant"}"#),
            OAuthError::ReauthenticationRequired(_)
        ));
        assert!(matches!(
            refresh_error(StatusCode::BAD_REQUEST, r#"{"error":"invalid_request"}"#),
            OAuthError::Provider { .. }
        ));
        assert!(refresh_token_is_invalid(r#"{"error":"invalid_grant"}"#));
        assert!(!refresh_token_is_invalid(r#"{"error":"invalid_token"}"#));
        assert!(!refresh_token_is_invalid(r#"{"error":"invalid_request"}"#));
    }

    #[test]
    fn unauthorized_retry_reuses_a_concurrently_rotated_token() {
        let mut current = credential("new-access");
        current.expires_at_ms = Some(now_ms() + 3_600_000);
        assert!(can_reuse_after_unauthorized(
            &current,
            Some("rejected-access"),
            Duration::from_secs(120)
        ));
        assert!(!can_reuse_after_unauthorized(
            &current,
            Some("new-access"),
            Duration::from_secs(120)
        ));
    }
}
