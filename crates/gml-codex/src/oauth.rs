//! Codex ChatGPT OAuth credential storage and refresh.
//!
//! Faithful port of `gm-lab/codex_oauth.py`. Implements the browser PKCE OAuth
//! flow (loopback callback HTTP server on `127.0.0.1:1455`, fallback `1457`,
//! 300s timeout), local JSON credential storage, near-expiry refresh, and token
//! revocation.
//!
//! Storage path resolution honors `GM_CODEX_CREDENTIAL_PATH` first, then the
//! per-OS app-data directory via the `directories` crate (PORT_PLAN §3.2),
//! falling back to `%APPDATA%`/`~/.config` to match the Python defaults.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use gml_config::Config;

/// `ISSUER` — the OpenAI auth issuer.
pub const ISSUER: &str = "https://auth.openai.com";
/// `SCOPE` — the requested OAuth scope string (verbatim).
pub const SCOPE: &str =
    "openid profile email offline_access api.connectors.read api.connectors.invoke";
/// `DEFAULT_AUTH_PORT`.
pub const DEFAULT_AUTH_PORT: u16 = 1455;
/// `FALLBACK_AUTH_PORT`.
pub const FALLBACK_AUTH_PORT: u16 = 1457;
/// `TOKEN_TIMEOUT_SECS` (token endpoint read timeout).
pub const TOKEN_TIMEOUT_SECS: f64 = 30.0;
/// `OAUTH_TIMEOUT_SECS` (callback wait timeout).
pub const OAUTH_TIMEOUT_SECS: f64 = 300.0;
/// `REFRESH_MARGIN_MS` — refresh when within 5 minutes of expiry.
pub const REFRESH_MARGIN_MS: i64 = 5 * 60 * 1000;

/// `REVOKE_ENDPOINT` — `{ISSUER}/oauth/revoke`.
pub fn revoke_endpoint() -> String {
    format!("{ISSUER}/oauth/revoke")
}

/// An error from the OAuth flow / credential storage.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OAuthError(pub String);

impl OAuthError {
    /// Construct from anything displayable.
    pub fn new(msg: impl std::fmt::Display) -> Self {
        OAuthError(msg.to_string())
    }
}

/// `CodexCredential` dataclass.
///
/// Field order / defaults mirror the Python dataclass; `to_dict`/`from_dict`
/// reproduce the on-disk JSON shape byte-for-byte (`type` key, key order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexCredential {
    /// `access_token`.
    pub access_token: String,
    /// `refresh_token`.
    pub refresh_token: String,
    /// `id_token` (optional).
    pub id_token: Option<String>,
    /// `expires_at` — epoch milliseconds (optional).
    pub expires_at: Option<i64>,
    /// `account_id` (optional).
    pub account_id: Option<String>,
    /// `credential_type` — defaults to `"openai_codex_oauth"`.
    pub credential_type: String,
}

impl CodexCredential {
    /// Default credential type string.
    pub const DEFAULT_TYPE: &'static str = "openai_codex_oauth";

    /// `CodexCredential.from_dict(data)`.
    ///
    /// ```python
    /// access_token=str(data.get("access_token") or ""),
    /// refresh_token=str(data.get("refresh_token") or ""),
    /// id_token=data.get("id_token") if data.get("id_token") else None,
    /// expires_at=_int_or_none(data.get("expires_at")),
    /// account_id=data.get("account_id") if data.get("account_id") else None,
    /// credential_type=str(data.get("type") or data.get("credential_type") or "openai_codex_oauth"),
    /// ```
    pub fn from_dict(data: &Map<String, Value>) -> Self {
        CodexCredential {
            access_token: str_or_empty(data.get("access_token")),
            refresh_token: str_or_empty(data.get("refresh_token")),
            id_token: nonempty_str(data.get("id_token")),
            expires_at: int_or_none(data.get("expires_at")),
            account_id: nonempty_str(data.get("account_id")),
            credential_type: {
                // str(data.get("type") or data.get("credential_type") or "openai_codex_oauth")
                let t = nonempty_str(data.get("type"))
                    .or_else(|| nonempty_str(data.get("credential_type")));
                t.unwrap_or_else(|| Self::DEFAULT_TYPE.to_string())
            },
        }
    }

    /// `CodexCredential.to_dict()` — the on-disk JSON object (key order:
    /// `type, access_token, refresh_token, id_token, expires_at, account_id`).
    pub fn to_dict(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("type".into(), Value::String(self.credential_type.clone()));
        m.insert(
            "access_token".into(),
            Value::String(self.access_token.clone()),
        );
        m.insert(
            "refresh_token".into(),
            Value::String(self.refresh_token.clone()),
        );
        m.insert(
            "id_token".into(),
            self.id_token
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        m.insert(
            "expires_at".into(),
            self.expires_at.map(Value::from).unwrap_or(Value::Null),
        );
        m.insert(
            "account_id".into(),
            self.account_id
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        m
    }
}

/// `credential_path()` — resolve the credential file location.
///
/// Python:
/// ```python
/// override = os.environ.get("GM_CODEX_CREDENTIAL_PATH", "").strip()
/// if override: return Path(override).expanduser()
/// appdata = os.environ.get("APPDATA", "").strip()
/// if appdata: return Path(appdata) / "gm-lab" / "codex-oauth.json"
/// return Path.home() / ".config" / "gm-lab" / "codex-oauth.json"
/// ```
///
/// DEVIATION (PORT_PLAN §3.2): the `directories` crate is preferred for the
/// per-OS app-data location, but to keep byte-compatible with the Python
/// defaults we first honor `GM_CODEX_CREDENTIAL_PATH`, then `%APPDATA%` /
/// `~/.config`. We additionally consult `directories::ProjectDirs` only as the
/// final fallback when neither `APPDATA` nor a home dir is available.
pub fn credential_path() -> PathBuf {
    let override_path = std::env::var("GM_CODEX_CREDENTIAL_PATH")
        .unwrap_or_default()
        .trim()
        .to_string();
    if !override_path.is_empty() {
        return expanduser(&override_path);
    }
    let appdata = std::env::var("APPDATA").unwrap_or_default().trim().to_string();
    if !appdata.is_empty() {
        return PathBuf::from(appdata).join("gm-lab").join("codex-oauth.json");
    }
    if let Some(home) = home_dir() {
        return home.join(".config").join("gm-lab").join("codex-oauth.json");
    }
    // Final fallback: directories crate (covers exotic environments).
    if let Some(dirs) = directories::ProjectDirs::from("", "", "gm-lab") {
        return dirs.config_dir().join("codex-oauth.json");
    }
    PathBuf::from("codex-oauth.json")
}

/// `load_credential()` — read & parse the credential file, or `None` if absent.
///
/// Raises on a non-object JSON or a missing `access_token`.
pub fn load_credential() -> Result<Option<CodexCredential>, OAuthError> {
    let path = credential_path();
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(OAuthError::new)?;
    let data: Value = serde_json::from_str(&text).map_err(OAuthError::new)?;
    let obj = match data {
        Value::Object(m) => m,
        _ => {
            return Err(OAuthError::new("Codex credential file is not a JSON object"));
        }
    };
    let credential = CodexCredential::from_dict(&obj);
    if credential.access_token.is_empty() {
        return Err(OAuthError::new("Codex credential file has no access_token"));
    }
    Ok(Some(credential))
}

/// `save_credential(credential)` — write the credential JSON with
/// `ensure_ascii=False, indent=2` (matching Python `json.dump`).
pub fn save_credential(credential: &CodexCredential) -> Result<(), OAuthError> {
    let path = credential_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(OAuthError::new)?;
    }
    let body = serde_json::to_string_pretty(&Value::Object(credential.to_dict()))
        .map_err(OAuthError::new)?;
    std::fs::write(&path, body).map_err(OAuthError::new)?;
    Ok(())
}

/// `delete_credential()` — remove the credential file, ignoring "not found".
pub fn delete_credential() -> Result<(), OAuthError> {
    match std::fs::remove_file(credential_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(OAuthError::new(e)),
    }
}

/// `auth_status()` — non-secret authentication status for the UI.
///
/// Returns `{authenticated, account_id, expires_at, message}` with the exact
/// Russian/English message strings from the Python source.
pub fn auth_status() -> Map<String, Value> {
    match load_credential() {
        Err(exc) => status_map(
            false,
            Value::Null,
            Value::Null,
            &format!("Codex OAuth credential is invalid: {exc}"),
        ),
        Ok(None) => status_map(false, Value::Null, Value::Null, "Codex OAuth не авторизован"),
        Ok(Some(c)) => status_map(
            true,
            c.account_id.clone().map(Value::String).unwrap_or(Value::Null),
            c.expires_at.map(Value::from).unwrap_or(Value::Null),
            "Codex OAuth авторизован",
        ),
    }
}

fn status_map(authenticated: bool, account_id: Value, expires_at: Value, message: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("authenticated".into(), Value::Bool(authenticated));
    m.insert("account_id".into(), account_id);
    m.insert("expires_at".into(), expires_at);
    m.insert("message".into(), Value::String(message.to_string()));
    m
}

/// `ensure_fresh_credential(http)` — load the credential, refresh it if near
/// expiry, save the refreshed credential, and return it.
///
/// Errors (exact message strings) when not authorized, expired without a
/// refresh token, etc.
pub async fn ensure_fresh_credential(
    http: &reqwest::Client,
    cfg: &Config,
) -> Result<CodexCredential, OAuthError> {
    let credential = load_credential()?
        .ok_or_else(|| OAuthError::new("Codex OAuth не авторизован. Подключи Codex в интерфейсе."))?;
    if is_near_expiry(&credential) {
        if credential.refresh_token.trim().is_empty() {
            return Err(OAuthError::new("Codex OAuth token expired; reconnect Codex."));
        }
        let refreshed = refresh_credential(&credential, http, cfg).await?;
        save_credential(&refreshed)?;
        return Ok(refreshed);
    }
    Ok(credential)
}

/// `refresh_credential(credential, http)` — exchange the refresh token.
pub async fn refresh_credential(
    credential: &CodexCredential,
    http: &reqwest::Client,
    cfg: &Config,
) -> Result<CodexCredential, OAuthError> {
    let form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", credential.refresh_token.clone()),
        ("client_id", cfg.codex_client_id.clone()),
    ];
    let data = post_token(http, &form).await?;
    let access_token = str_or_empty(data.get("access_token"));
    if access_token.is_empty() {
        return Err(OAuthError::new("Codex OAuth refresh response has no access_token"));
    }
    // refresh_token = str(data.get("refresh_token") or credential.refresh_token)
    let refresh_token = {
        let v = str_or_empty(data.get("refresh_token"));
        if v.is_empty() {
            credential.refresh_token.clone()
        } else {
            v
        }
    };
    // id_token = data.get("id_token") or credential.id_token
    let id_token = nonempty_str(data.get("id_token")).or_else(|| credential.id_token.clone());
    let account_id = account_id_from_tokens(id_token.as_deref(), Some(access_token.as_str()))
        .or_else(|| credential.account_id.clone());
    Ok(CodexCredential {
        access_token,
        refresh_token,
        id_token,
        expires_at: expires_at(data.get("expires_in")),
        account_id,
        credential_type: CodexCredential::DEFAULT_TYPE.to_string(),
    })
}

/// `run_oauth(http)` — the full browser PKCE flow. Binds the loopback callback
/// server, opens the browser (or prints the URL), waits for the callback,
/// exchanges the authorization code, and saves the credential.
pub async fn run_oauth(http: &reqwest::Client, cfg: &Config) -> Result<CodexCredential, OAuthError> {
    let pkce_verifier = random_url_token(32);
    let code_challenge = code_challenge(&pkce_verifier);
    let state = random_url_token(32);

    let (server, port) = bind_callback_server(cfg)?;
    let redirect_uri = format!("http://localhost:{port}/auth/callback");
    let auth_url = authorize_url(cfg, &redirect_uri, &code_challenge, &state);

    if cfg.codex_auto_open_browser {
        // open::that maps to ShellExecute / open / xdg-open. Failure is non-fatal;
        // we always print the URL as a fallback (headless/remote).
        let _ = open::that(&auth_url);
    }
    eprintln!("Codex OAuth: open this URL to authorize:\n{auth_url}");

    // Wait for exactly one /auth/callback request, with the 300s timeout.
    let callback = wait_for_callback(server)?;
    if callback.state != state {
        return Err(OAuthError::new("OAuth state mismatch"));
    }

    let form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", callback.code),
        ("redirect_uri", redirect_uri),
        ("client_id", cfg.codex_client_id.clone()),
        ("code_verifier", pkce_verifier),
    ];
    let data = post_token(http, &form).await?;
    let access_token = str_or_empty(data.get("access_token"));
    let refresh_token = str_or_empty(data.get("refresh_token"));
    if access_token.is_empty() {
        return Err(OAuthError::new("Codex OAuth token response has no access_token"));
    }
    let id_token = nonempty_str(data.get("id_token"));
    let credential = CodexCredential {
        account_id: account_id_from_tokens(id_token.as_deref(), Some(access_token.as_str())),
        access_token,
        refresh_token,
        id_token,
        expires_at: expires_at(data.get("expires_in")),
        credential_type: CodexCredential::DEFAULT_TYPE.to_string(),
    };
    save_credential(&credential)?;
    Ok(credential)
}

/// `revoke_credential(http)` — best-effort token revocation, then delete the
/// local credential file regardless of the outcome.
pub async fn revoke_credential(http: &reqwest::Client, cfg: &Config) -> Result<(), OAuthError> {
    let credential = match load_credential()? {
        Some(c) => c,
        None => return Ok(()),
    };
    let refresh = credential.refresh_token.trim().to_string();
    let access = credential.access_token.trim().to_string();
    let token = if !refresh.is_empty() { refresh.clone() } else { access };
    if token.is_empty() {
        delete_credential()?;
        return Ok(());
    }
    let token_type = if !refresh.is_empty() {
        "refresh_token"
    } else {
        "access_token"
    };
    let mut payload = Map::new();
    payload.insert("token".into(), Value::String(token));
    payload.insert("token_type_hint".into(), Value::String(token_type.to_string()));
    if token_type == "refresh_token" {
        payload.insert("client_id".into(), Value::String(cfg.codex_client_id.clone()));
    }
    // try: client.post(REVOKE_ENDPOINT, json=payload, timeout=10) finally: delete
    let _ = http
        .post(revoke_endpoint())
        .timeout(Duration::from_secs(10))
        .json(&Value::Object(payload))
        .send()
        .await;
    delete_credential()?;
    Ok(())
}

// --- loopback callback server -----------------------------------------------

/// The parsed `/auth/callback` result.
struct Callback {
    code: String,
    state: String,
}

/// `_bind_callback_server()` — bind on the preferred port, fall back to 1457.
fn bind_callback_server(cfg: &Config) -> Result<(tiny_http::Server, u16), OAuthError> {
    let preferred: u16 = {
        let p = cfg.codex_auth_port;
        if p > 0 && p <= u16::MAX as i64 {
            p as u16
        } else {
            DEFAULT_AUTH_PORT
        }
    };
    let mut ports = vec![preferred];
    if preferred != FALLBACK_AUTH_PORT {
        ports.push(FALLBACK_AUTH_PORT);
    }
    let mut last_error: Option<String> = None;
    for port in ports {
        match tiny_http::Server::http(("127.0.0.1", port)) {
            Ok(server) => return Ok((server, port)),
            Err(e) => last_error = Some(e.to_string()),
        }
    }
    Err(OAuthError::new(format!(
        "Cannot bind Codex OAuth callback server: {}",
        last_error.unwrap_or_default()
    )))
}

/// Wait for one callback request with the 300s timeout, serve the RU success/
/// failure HTML, and return the parsed `{code, state}` or an error.
fn wait_for_callback(server: tiny_http::Server) -> Result<Callback, OAuthError> {
    let timeout = Duration::from_secs_f64(OAUTH_TIMEOUT_SECS);
    let request = match server.recv_timeout(timeout) {
        Ok(Some(req)) => req,
        Ok(None) => return Err(OAuthError::new("Timed out waiting for Codex OAuth callback")),
        Err(e) => return Err(OAuthError::new(e)),
    };

    let (callback, callback_error) = parse_callback(request.url());
    let ok = callback.is_some() && callback_error.is_none();
    let body = callback_html(ok);
    let encoded = body.into_bytes();
    let header = tiny_http::Header::from_bytes(
        &b"Content-Type"[..],
        &b"text/html; charset=utf-8"[..],
    )
    .map_err(|_| OAuthError::new("invalid header"))?;
    let len = encoded.len();
    let response = tiny_http::Response::from_data(encoded)
        .with_status_code(200)
        .with_header(header)
        .with_header(
            tiny_http::Header::from_bytes(&b"Content-Length"[..], len.to_string().as_bytes())
                .map_err(|_| OAuthError::new("invalid header"))?,
        );
    let _ = request.respond(response);

    if let Some(err) = callback_error {
        return Err(OAuthError::new(err));
    }
    callback.ok_or_else(|| OAuthError::new("Timed out waiting for Codex OAuth callback"))
}

/// `_CallbackHandler.do_GET` parse logic — extract `code`/`state` from the URL.
/// Returns `(Some(callback), None)` on success or `(None, Some(error))` on
/// failure, matching the Python handler's `callback`/`callback_error` attrs.
fn parse_callback(raw_url: &str) -> (Option<Callback>, Option<String>) {
    // urlparse(self.path): split path and query.
    let (path, query) = match raw_url.split_once('?') {
        Some((p, q)) => (p, q),
        None => (raw_url, ""),
    };
    let result: Result<Callback, String> = (|| {
        if path != "/auth/callback" {
            return Err("Unexpected OAuth callback path".to_string());
        }
        let params = parse_qs(query);
        if first(&params, "error").is_some() {
            return Err("Codex OAuth rejected authorization".to_string());
        }
        let code = first(&params, "code").unwrap_or_default().trim().to_string();
        let state = first(&params, "state").unwrap_or_default().trim().to_string();
        if code.is_empty() {
            return Err("OAuth callback has no code".to_string());
        }
        if state.is_empty() {
            return Err("OAuth callback has no state".to_string());
        }
        Ok(Callback { code, state })
    })();
    match result {
        Ok(cb) => (Some(cb), None),
        Err(e) => (None, Some(e)),
    }
}

/// The exact RU success / failure HTML bodies from the Python handler.
fn callback_html(ok: bool) -> String {
    if ok {
        "<!doctype html><meta charset=utf-8><title>GM-Lab Codex</title>\
<body style=\"font-family:system-ui;background:#14161c;color:#e7e9ef\">\
<p>Codex авторизован. Можно закрыть вкладку и вернуться в GM-Lab.</p>"
            .to_string()
    } else {
        "<!doctype html><meta charset=utf-8><title>GM-Lab Codex</title>\
<body style=\"font-family:system-ui;background:#14161c;color:#e7e9ef\">\
<p>Авторизация Codex не удалась. Вернись в GM-Lab и попробуй снова.</p>"
            .to_string()
    }
}

// --- URL / token helpers ----------------------------------------------------

/// `_authorize_url(redirect_uri, code_challenge, state)`.
///
/// Reproduces the param order and `urlencode` quoting from the Python source.
pub fn authorize_url(cfg: &Config, redirect_uri: &str, code_challenge: &str, state: &str) -> String {
    let params: [(&str, &str); 10] = [
        ("response_type", "code"),
        ("client_id", &cfg.codex_client_id),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", state),
        ("originator", &cfg.codex_originator),
    ];
    let query = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{ISSUER}/oauth/authorize?{query}")
}

/// `_post_token(form, http)` — POST `application/x-www-form-urlencoded` to the
/// token endpoint and return the JSON object. Errors on non-2xx or non-object.
async fn post_token(
    http: &reqwest::Client,
    form: &[(&str, String)],
) -> Result<Map<String, Value>, OAuthError> {
    let body = form
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let response = http
        .post(format!("{ISSUER}/oauth/token"))
        .timeout(Duration::from_secs_f64(TOKEN_TIMEOUT_SECS))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(OAuthError::new)?;
    let status = response.status();
    if !status.is_success() {
        return Err(OAuthError::new(format!(
            "Codex OAuth token endpoint failed with status {}",
            status.as_u16()
        )));
    }
    let data: Value = response.json().await.map_err(OAuthError::new)?;
    match data {
        Value::Object(m) => Ok(m),
        _ => Err(OAuthError::new(
            "Codex OAuth token endpoint returned non-object JSON",
        )),
    }
}

/// `_random_url_token(byte_len)` — base64url(random bytes), no padding.
pub fn random_url_token(byte_len: usize) -> String {
    let mut bytes = vec![0u8; byte_len];
    fill_random(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `_code_challenge(verifier)` — base64url(sha256(verifier)), no padding.
pub fn code_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// `_decode_jwt_claims(token)` — base64url-decode the JWT payload segment.
pub fn decode_jwt_claims(token: Option<&str>) -> Option<Map<String, Value>> {
    let token = token?;
    if token.is_empty() {
        return None;
    }
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let mut payload = parts[1].to_string();
    // payload += "=" * (-len(payload) % 4)
    let pad = (4 - (payload.len() % 4)) % 4;
    payload.push_str(&"=".repeat(pad));
    let raw = base64::engine::general_purpose::URL_SAFE
        .decode(payload.as_bytes())
        .ok()?;
    let text = String::from_utf8(raw).ok()?;
    match serde_json::from_str::<Value>(&text).ok()? {
        Value::Object(m) => Some(m),
        _ => None,
    }
}

/// `_account_id_from_tokens(id_token, access_token)` — pull `chatgpt_account_id`
/// from the JWT claims of either token.
pub fn account_id_from_tokens(id_token: Option<&str>, access_token: Option<&str>) -> Option<String> {
    for token in [id_token, access_token] {
        let claims = match decode_jwt_claims(token) {
            Some(c) => c,
            None => continue,
        };
        // claims.get("chatgpt_account_id")
        //   or (claims.get("https://api.openai.com/auth") or {}).get("chatgpt_account_id")
        //   or claims.get("https://api.openai.com/auth.chatgpt_account_id")
        let account_id = claims
            .get("chatgpt_account_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .or_else(|| {
                claims
                    .get("https://api.openai.com/auth")
                    .and_then(|v| v.as_object())
                    .and_then(|o| o.get("chatgpt_account_id"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            })
            .or_else(|| {
                claims
                    .get("https://api.openai.com/auth.chatgpt_account_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string())
            });
        if let Some(id) = account_id {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

/// `_expires_at(expires_in)` — `now_ms + seconds*1000`, or `None`.
pub fn expires_at(expires_in: Option<&Value>) -> Option<i64> {
    let seconds = int_or_none(expires_in)?;
    Some(now_ms() + seconds * 1000)
}

/// `_is_near_expiry(credential)`.
pub fn is_near_expiry(credential: &CodexCredential) -> bool {
    match credential.expires_at {
        None => false,
        Some(exp) => exp <= now_ms() + REFRESH_MARGIN_MS,
    }
}

// --- tiny coercion / platform helpers ---------------------------------------

/// `int(time.time() * 1000)`.
fn now_ms() -> i64 {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    (dur.as_secs_f64() * 1000.0) as i64
}

/// `_int_or_none(value)` — coerce ints / numeric strings, `None` on empty/None.
fn int_or_none(value: Option<&Value>) -> Option<i64> {
    match value {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => {
            if s.is_empty() {
                None
            } else {
                s.trim().parse::<i64>().ok().or_else(|| {
                    // Python int() does not parse "1.0"; mimic by failing.
                    None
                })
            }
        }
        Some(Value::Bool(b)) => Some(if *b { 1 } else { 0 }),
        _ => None,
    }
}

/// `str(data.get(key) or "")`.
fn str_or_empty(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(other) => {
            // `or ""` — falsy values (false, 0, [], {}) become "".
            if is_falsy(other) {
                String::new()
            } else {
                value_to_py_str(other)
            }
        }
    }
}

/// `data.get(key) if data.get(key) else None` for a string-ish value.
fn nonempty_str(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn is_falsy(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::Bool(b) => !b,
        Value::Number(n) => n.as_f64().map(|f| f == 0.0).unwrap_or(false),
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

fn value_to_py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Number(n) => n.to_string(),
        _ => v.to_string(),
    }
}

/// `Path(...).expanduser()` — expand a leading `~`.
fn expanduser(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~") {
        if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') {
            if let Some(home) = home_dir() {
                let rest = rest.trim_start_matches(['/', '\\']);
                return if rest.is_empty() {
                    home
                } else {
                    home.join(rest)
                };
            }
        }
    }
    PathBuf::from(path)
}

fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
}

/// Fill `buf` with cryptographically-random bytes (Python `secrets.token_bytes`).
fn fill_random(buf: &mut [u8]) {
    // uuid v4 uses the OS RNG (getrandom); derive bytes from successive uuids.
    let mut i = 0;
    while i < buf.len() {
        let bytes = *uuid::Uuid::new_v4().as_bytes();
        let take = (buf.len() - i).min(bytes.len());
        buf[i..i + take].copy_from_slice(&bytes[..take]);
        i += take;
    }
}

/// `urllib.parse.quote_plus`-equivalent for `urlencode` (form / query encoding).
/// `urlencode` uses `quote_via=quote_plus` by default, so spaces become `+`.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `urllib.parse.parse_qs(query)` — minimal form parser returning key -> values.
fn parse_qs(query: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = match pair.split_once('=') {
            Some((k, v)) => (k, v),
            None => (pair, ""),
        };
        let key = urldecode(k);
        let val = urldecode(v);
        // parse_qs drops empty values by default (keep_blank_values=False).
        if val.is_empty() {
            continue;
        }
        out.push((key, val));
    }
    out
}

fn first<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

fn urldecode(s: &str) -> String {
    // form-decode: '+' -> ' ', then percent-decode.
    let replaced = s.replace('+', " ");
    let bytes = replaced.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_known_vector() {
        // RFC 7636 Appendix B test vector.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = code_challenge(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn pkce_verifier_is_url_safe_no_pad() {
        let v = random_url_token(32);
        assert!(!v.contains('='));
        assert!(!v.contains('+'));
        assert!(!v.contains('/'));
        // 32 bytes base64url no-pad -> 43 chars.
        assert_eq!(v.chars().count(), 43);
        // distinct across calls
        assert_ne!(random_url_token(32), random_url_token(32));
    }

    #[test]
    fn challenge_is_base64url_no_pad_of_sha256() {
        let v = "test-verifier";
        let c = code_challenge(v);
        assert!(!c.contains('='));
        // sha256 -> 32 bytes -> 43 base64url chars
        assert_eq!(c.chars().count(), 43);
    }

    #[test]
    fn credential_to_dict_key_order_and_type_key() {
        let c = CodexCredential {
            access_token: "atk".into(),
            refresh_token: "rtk".into(),
            id_token: Some("idt".into()),
            expires_at: Some(123),
            account_id: Some("acc".into()),
            credential_type: CodexCredential::DEFAULT_TYPE.into(),
        };
        let s = serde_json::to_string(&Value::Object(c.to_dict())).unwrap();
        assert_eq!(
            s,
            r#"{"type":"openai_codex_oauth","access_token":"atk","refresh_token":"rtk","id_token":"idt","expires_at":123,"account_id":"acc"}"#
        );
    }

    #[test]
    fn credential_roundtrip_from_dict() {
        let json = r#"{"type":"openai_codex_oauth","access_token":"a","refresh_token":"r","id_token":null,"expires_at":null,"account_id":null}"#;
        let v: Value = serde_json::from_str(json).unwrap();
        let c = CodexCredential::from_dict(v.as_object().unwrap());
        assert_eq!(c.access_token, "a");
        assert_eq!(c.refresh_token, "r");
        assert_eq!(c.id_token, None);
        assert_eq!(c.expires_at, None);
        assert_eq!(c.account_id, None);
        assert_eq!(c.credential_type, "openai_codex_oauth");
    }

    #[test]
    fn from_dict_credential_type_fallback() {
        // type/credential_type absent -> default.
        let v: Value = serde_json::json!({"access_token": "x", "refresh_token": "y"});
        let c = CodexCredential::from_dict(v.as_object().unwrap());
        assert_eq!(c.credential_type, "openai_codex_oauth");
        // credential_type key used when type absent
        let v2: Value = serde_json::json!({"access_token": "x", "credential_type": "custom"});
        let c2 = CodexCredential::from_dict(v2.as_object().unwrap());
        assert_eq!(c2.credential_type, "custom");
    }

    #[test]
    fn account_id_from_jwt_top_level_claim() {
        // header.payload.signature with payload {"chatgpt_account_id":"acc-1"}
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"chatgpt_account_id":"acc-1"}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(account_id_from_tokens(Some(&token), None), Some("acc-1".to_string()));
    }

    #[test]
    fn account_id_from_jwt_nested_claim() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"nested"}}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(account_id_from_tokens(None, Some(&token)), Some("nested".to_string()));
    }

    #[test]
    fn account_id_none_when_absent() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"sub":"x"}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(account_id_from_tokens(Some(&token), None), None);
        assert_eq!(account_id_from_tokens(None, None), None);
    }

    #[test]
    fn near_expiry_logic() {
        let mut c = CodexCredential {
            access_token: "a".into(),
            refresh_token: "r".into(),
            id_token: None,
            expires_at: None,
            account_id: None,
            credential_type: CodexCredential::DEFAULT_TYPE.into(),
        };
        // None -> never near expiry
        assert!(!is_near_expiry(&c));
        // far future -> not near
        c.expires_at = Some(now_ms() + 60 * 60 * 1000);
        assert!(!is_near_expiry(&c));
        // within margin -> near
        c.expires_at = Some(now_ms() + 60 * 1000);
        assert!(is_near_expiry(&c));
        // already past -> near
        c.expires_at = Some(now_ms() - 1000);
        assert!(is_near_expiry(&c));
    }

    #[test]
    fn authorize_url_param_order() {
        let cfg = Config::from_env();
        let url = authorize_url(&cfg, "http://localhost:1455/auth/callback", "CH", "ST");
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?response_type=code&client_id="));
        // redirect_uri quoted with quote_plus
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("code_challenge=CH"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("&state=ST&"));
        assert!(url.ends_with(&format!("&originator={}", cfg.codex_originator)));
    }

    #[test]
    fn parse_callback_success() {
        let (cb, err) = parse_callback("/auth/callback?code=abc&state=xyz");
        assert!(err.is_none());
        let cb = cb.unwrap();
        assert_eq!(cb.code, "abc");
        assert_eq!(cb.state, "xyz");
    }

    #[test]
    fn parse_callback_errors() {
        assert_eq!(
            parse_callback("/wrong?code=a&state=b").1.as_deref(),
            Some("Unexpected OAuth callback path")
        );
        assert_eq!(
            parse_callback("/auth/callback?error=denied").1.as_deref(),
            Some("Codex OAuth rejected authorization")
        );
        assert_eq!(
            parse_callback("/auth/callback?state=b").1.as_deref(),
            Some("OAuth callback has no code")
        );
        assert_eq!(
            parse_callback("/auth/callback?code=a").1.as_deref(),
            Some("OAuth callback has no state")
        );
    }

    #[test]
    fn callback_html_is_russian_and_exact() {
        let ok = callback_html(true);
        assert!(ok.contains("Codex авторизован. Можно закрыть вкладку и вернуться в GM-Lab."));
        let bad = callback_html(false);
        assert!(bad.contains("Авторизация Codex не удалась. Вернись в GM-Lab и попробуй снова."));
    }
}
