//! xAI batch speech-to-text transport.
//!
//! The connector keeps OAuth refresh, request shaping, retry policy and
//! provider response parsing on its side of the provider-neutral boundary.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, RETRY_AFTER, USER_AGENT};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, Response, StatusCode};
use serde_json::Value;

use gml_llm::{ConnectorError, ConnectorId};

use crate::oauth::{OAuthCredential, SuperGrokOAuth};
use crate::SuperGrokConfig;

const CONNECTOR_ID: &str = "xai";
const OPERATION: &str = "speech-to-text";
const MAX_TRANSIENT_RETRIES: usize = 2;
const MAX_RETRY_AFTER_SECONDS: u64 = 10;
const BASE_RETRY_DELAY_MS: u64 = 500;
const MAX_RETRY_JITTER_MS: u64 = 250;
const MAX_ERROR_CHARS: usize = 500;

pub(crate) async fn transcribe(
    config: &SuperGrokConfig,
    http: &Client,
    oauth: &SuperGrokOAuth,
    audio: &[u8],
    content_type: &str,
    language: Option<&str>,
) -> Result<String, ConnectorError> {
    validate_audio(config, audio)?;
    let content_type = normalized_content_type(content_type);
    let language = language
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| config.stt_language.trim());

    let mut rejected_access_token = None;
    for authentication_attempt in 0..2 {
        let auth_epoch = oauth
            .begin_request()
            .map_err(|error| operation_error(error.to_string()))?;
        let credential = match rejected_access_token.as_deref() {
            Some(rejected) => oauth.refresh_after_unauthorized(rejected).await,
            None => oauth.ensure_fresh(false).await,
        }
        .map_err(|error| operation_error(error.to_string()))?;

        let response = send_with_retry(
            config,
            http,
            oauth,
            auth_epoch,
            &credential,
            audio,
            content_type,
            language,
        )
        .await?;
        if response.status() == StatusCode::UNAUTHORIZED && authentication_attempt == 0 {
            rejected_access_token = Some(credential.access_token);
            continue;
        }
        return parse_response(response).await;
    }

    Err(operation_error("SuperGrok authentication failed"))
}

#[allow(clippy::too_many_arguments)]
async fn send_with_retry(
    config: &SuperGrokConfig,
    http: &Client,
    oauth: &SuperGrokOAuth,
    auth_epoch: u64,
    credential: &OAuthCredential,
    audio: &[u8],
    content_type: &str,
    language: &str,
) -> Result<Response, ConnectorError> {
    for retry in 0..=MAX_TRANSIENT_RETRIES {
        ensure_auth_epoch(oauth, auth_epoch)?;
        let request = build_request(config, http, credential, audio, content_type, language)?;
        let result = request.send().await;
        ensure_auth_epoch(oauth, auth_epoch)?;
        match result {
            Ok(response) => {
                if retry < MAX_TRANSIENT_RETRIES && is_transient_status(response.status()) {
                    let delay = response_retry_delay(&response, retry);
                    tracing::warn!(
                        status = response.status().as_u16(),
                        retry = retry + 1,
                        delay_ms = delay.as_millis(),
                        "retrying transient SuperGrok speech-to-text response"
                    );
                    drop(response);
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Ok(response);
            }
            Err(error)
                if retry < MAX_TRANSIENT_RETRIES && (error.is_connect() || error.is_timeout()) =>
            {
                let delay = fallback_retry_delay(retry);
                tracing::warn!(
                    error = %error,
                    retry = retry + 1,
                    delay_ms = delay.as_millis(),
                    "retrying transient SuperGrok speech-to-text transport failure"
                );
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(operation_error(format!("request failed: {error}"))),
        }
    }
    unreachable!("bounded retry loop always returns")
}

fn build_request(
    config: &SuperGrokConfig,
    http: &Client,
    credential: &OAuthCredential,
    audio: &[u8],
    content_type: &str,
    language: &str,
) -> Result<reqwest::RequestBuilder, ConnectorError> {
    let mut form = Form::new();
    if !language.is_empty() {
        // xAI requires formatting controls before `file` in the multipart body.
        form = form
            .text("format", "true")
            .text("language", language.to_string());
    }
    let file = Part::bytes(audio.to_vec())
        .file_name(filename_for(content_type))
        .mime_str(content_type)
        .map_err(|error| operation_error(format!("invalid audio content type: {error}")))?;
    form = form.part("file", file);

    Ok(http
        .post(config.stt_url())
        .headers(request_headers(config, credential)?)
        .timeout(config.stt_timeout)
        .multipart(form))
}

fn request_headers(
    config: &SuperGrokConfig,
    credential: &OAuthCredential,
) -> Result<HeaderMap, ConnectorError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", credential.access_token.trim()))
            .map_err(|error| operation_error(format!("invalid OAuth credential: {error}")))?,
    );
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&config.user_agent)
            .map_err(|error| operation_error(format!("invalid user agent: {error}")))?,
    );
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    Ok(headers)
}

async fn parse_response(response: Response) -> Result<String, ConnectorError> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| operation_error(format!("read response failed: {error}")))?;
    if !status.is_success() {
        return Err(ConnectorError::http_operation(
            connector_id(),
            OPERATION,
            status.as_u16(),
            provider_message(&body),
        ));
    }
    extract_transcript(&body)
}

fn extract_transcript(body: &str) -> Result<String, ConnectorError> {
    let value: Value = serde_json::from_str(body)
        .map_err(|error| operation_error(format!("invalid response JSON: {error}")))?;
    value
        .get("text")
        .and_then(Value::as_str)
        .map(|text| text.trim().to_string())
        .ok_or_else(|| operation_error("response did not contain transcript text"))
}

fn validate_audio(config: &SuperGrokConfig, audio: &[u8]) -> Result<(), ConnectorError> {
    if audio.is_empty() {
        return Err(operation_error("audio is empty"));
    }
    if audio.len() > config.stt_max_audio_bytes {
        return Err(operation_error(format!(
            "audio is too large ({} bytes; limit is {} bytes)",
            audio.len(),
            config.stt_max_audio_bytes
        )));
    }
    Ok(())
}

fn normalized_content_type(content_type: &str) -> &str {
    content_type
        .split(';')
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("audio/webm")
}

fn filename_for(content_type: &str) -> &'static str {
    let content_type = content_type.to_ascii_lowercase();
    if content_type.contains("wav") || content_type.contains("wave") {
        "audio.wav"
    } else if content_type.contains("mpeg") || content_type.contains("mp3") {
        "audio.mp3"
    } else if content_type.contains("ogg") {
        "audio.ogg"
    } else if content_type.contains("opus") {
        "audio.opus"
    } else if content_type.contains("flac") {
        "audio.flac"
    } else if content_type.contains("m4a") {
        "audio.m4a"
    } else if content_type.contains("mp4") {
        "audio.mp4"
    } else if content_type.contains("aac") {
        "audio.aac"
    } else if content_type.contains("matroska") || content_type.contains("mkv") {
        "audio.mkv"
    } else {
        "audio.webm"
    }
}

fn ensure_auth_epoch(oauth: &SuperGrokOAuth, auth_epoch: u64) -> Result<(), ConnectorError> {
    if oauth.request_epoch_is_current(auth_epoch) {
        Ok(())
    } else {
        Err(operation_error(
            "authentication changed while speech-to-text was pending; retry the action",
        ))
    }
}

fn is_transient_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn response_retry_delay(response: &Response, retry: usize) -> Duration {
    response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.min(MAX_RETRY_AFTER_SECONDS)))
        .unwrap_or_else(|| fallback_retry_delay(retry))
}

fn fallback_retry_delay(retry: usize) -> Duration {
    let exponential = BASE_RETRY_DELAY_MS.saturating_mul(1u64 << retry.min(8));
    Duration::from_millis(exponential.saturating_add(retry_jitter_ms()))
}

fn retry_jitter_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64
        % (MAX_RETRY_JITTER_MS + 1)
}

fn provider_message(body: &str) -> String {
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| {
                    error
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| error.get("message")?.as_str().map(str::to_string))
                })
                .or_else(|| value.get("message")?.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| body.trim().to_string());
    let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if message.is_empty() {
        return "empty provider response".to_string();
    }
    if message.chars().count() <= MAX_ERROR_CHARS {
        return message;
    }
    format!(
        "{}…",
        message.chars().take(MAX_ERROR_CHARS).collect::<String>()
    )
}

fn operation_error(message: impl std::fmt::Display) -> ConnectorError {
    ConnectorError::operation(connector_id(), message)
}

fn connector_id() -> ConnectorId {
    ConnectorId::new(CONNECTOR_ID).expect("static xAI connector id is valid")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::{OAuthCredential, SuperGrokOAuth};

    #[test]
    fn response_parser_requires_text_and_trims_it() {
        assert_eq!(
            extract_transcript(r#"{"text":"  hello  "}"#).unwrap(),
            "hello"
        );
        assert!(extract_transcript(r#"{"language":"en"}"#).is_err());
        assert!(extract_transcript("not-json").is_err());
    }

    #[test]
    fn error_parser_is_safe_and_bounded() {
        assert_eq!(
            provider_message(r#"{"error":{"message":"bad audio"}}"#),
            "bad audio"
        );
        assert!(provider_message(&"x".repeat(1_000)).chars().count() <= MAX_ERROR_CHARS + 1);
    }

    #[test]
    fn filename_covers_browser_audio_formats() {
        assert_eq!(filename_for("audio/webm"), "audio.webm");
        assert_eq!(filename_for("audio/ogg"), "audio.ogg");
        assert_eq!(filename_for("audio/wav"), "audio.wav");
        assert_eq!(filename_for("audio/mpeg"), "audio.mp3");
        assert_eq!(filename_for("audio/mp4"), "audio.mp4");
    }

    #[test]
    fn audio_validation_prevents_empty_and_oversized_uploads() {
        let mut config = SuperGrokConfig::new("unused.json");
        config.stt_max_audio_bytes = 3;
        assert!(validate_audio(&config, b"").is_err());
        assert!(validate_audio(&config, b"123").is_ok());
        assert!(validate_audio(&config, b"1234").is_err());
        assert_eq!(
            normalized_content_type("audio/webm; codecs=opus"),
            "audio/webm"
        );
    }

    #[test]
    fn retry_statuses_include_transient_internal_failures_only() {
        assert!(is_transient_status(StatusCode::INTERNAL_SERVER_ERROR));
        assert!(is_transient_status(StatusCode::BAD_GATEWAY));
        assert!(!is_transient_status(StatusCode::NOT_IMPLEMENTED));
        assert!(!is_transient_status(StatusCode::BAD_REQUEST));
    }

    #[tokio::test]
    async fn oauth_transcription_posts_file_last_and_retries_transient_status() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut final_request = Vec::new();
            for response in [
                "HTTP/1.1 503 Service Unavailable\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                json_response(r#"{"text":"  готово  "}"#),
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                final_request = read_request(&mut socket).await;
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            final_request
        });

        let directory = tempfile::tempdir().unwrap();
        let credential_path = directory.path().join("auth.json");
        std::fs::write(
            &credential_path,
            serde_json::to_vec(&OAuthCredential {
                version: 1,
                access_token: "test-access".to_string(),
                refresh_token: "test-refresh".to_string(),
                id_token: String::new(),
                token_type: "Bearer".to_string(),
                expires_at_ms: Some(4_102_444_800_000),
                token_endpoint: "https://auth.x.ai/oauth2/token".to_string(),
            })
            .unwrap(),
        )
        .unwrap();
        let http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let oauth_config = Arc::new(SuperGrokConfig::new(&credential_path));
        let oauth = SuperGrokOAuth::with_http(oauth_config, http.clone()).unwrap();
        let mut stt_config = SuperGrokConfig::new(&credential_path);
        stt_config.inference_base_url = format!("http://{address}/v1");

        let text = transcribe(
            &stt_config,
            &http,
            &oauth,
            b"test audio",
            "audio/webm; codecs=opus",
            Some("ru"),
        )
        .await
        .unwrap();
        assert_eq!(text, "готово");

        let request = String::from_utf8_lossy(&server.await.unwrap()).to_string();
        assert!(request.starts_with("POST /v1/stt HTTP/1.1\r\n"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-access"));
        assert!(request.contains("Content-Type: audio/webm"));
        let format = request.find("name=\"format\"").unwrap();
        let language = request.find("name=\"language\"").unwrap();
        let file = request.find("name=\"file\"").unwrap();
        assert!(
            format < language && language < file,
            "multipart file must be last"
        );
        assert!(request.contains("filename=\"audio.webm\""));
    }

    async fn read_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut request = Vec::new();
        let mut buffer = [0u8; 4_096];
        let mut expected = None;
        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if expected.is_none() {
                if let Some(headers_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..headers_end]);
                    let content_length = headers.lines().find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    });
                    expected = content_length.map(|length| headers_end + 4 + length);
                }
            }
            if expected.is_some_and(|expected| request.len() >= expected) {
                break;
            }
        }
        request
    }

    fn json_response(body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }
}
