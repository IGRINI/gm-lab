use std::time::Duration;

use base64::Engine as _;
use gml_llm::{
    ConnectorError, ConnectorId, GeneratedImage, ImageGenerationRequest, ImageGenerationResult,
};
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use reqwest::{Client, Response, StatusCode};
use serde_json::{json, Value};

use crate::{OAuthCredential, SuperGrokConfig, SuperGrokOAuth};

const CONNECTOR_ID: &str = "xai";
const DEFAULT_IMAGE_MODEL: &str = "grok-imagine-image";
const QUALITY_IMAGE_MODEL: &str = "grok-imagine-image-quality";
const MAX_IMAGES_PER_REQUEST: u32 = 4;
const MAX_BASE64_CHARS: usize = 64 * 1024 * 1024;
const MAX_TRANSIENT_RETRIES: usize = 2;

pub(crate) async fn generate(
    config: &SuperGrokConfig,
    http: &Client,
    oauth: &SuperGrokOAuth,
    request: &ImageGenerationRequest,
) -> Result<ImageGenerationResult, ConnectorError> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err(operation_error("image prompt is required"));
    }
    let count = request.count.max(1);
    if count > MAX_IMAGES_PER_REQUEST {
        return Err(operation_error(format!(
            "image count must be between 1 and {MAX_IMAGES_PER_REQUEST}"
        )));
    }
    let model = normalized_model(request.model.as_deref())?;
    let payload = request_payload(request, prompt, model, count);

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
        let response =
            send_with_retry(config, http, oauth, auth_epoch, &credential, &payload).await?;
        if response.status() == StatusCode::UNAUTHORIZED && authentication_attempt == 0 {
            rejected_access_token = Some(credential.access_token);
            continue;
        }
        return parse_response(response, model).await;
    }

    Err(operation_error("SuperGrok authentication failed"))
}

async fn send_with_retry(
    config: &SuperGrokConfig,
    http: &Client,
    oauth: &SuperGrokOAuth,
    auth_epoch: u64,
    credential: &OAuthCredential,
    payload: &Value,
) -> Result<Response, ConnectorError> {
    for retry in 0..=MAX_TRANSIENT_RETRIES {
        ensure_auth_epoch(oauth, auth_epoch)?;
        let result = http
            .post(config.images_url())
            .timeout(config.image_timeout)
            .header(
                AUTHORIZATION,
                format!("Bearer {}", credential.access_token.trim()),
            )
            .header(USER_AGENT, &config.user_agent)
            .header(ACCEPT, "application/json")
            .header(CONTENT_TYPE, "application/json")
            .json(payload)
            .send()
            .await;
        ensure_auth_epoch(oauth, auth_epoch)?;
        match result {
            Ok(response) if retry < MAX_TRANSIENT_RETRIES && transient(response.status()) => {
                let delay = retry_delay(&response, retry);
                tracing::warn!(
                    status = response.status().as_u16(),
                    retry = retry + 1,
                    delay_ms = delay.as_millis(),
                    "retrying transient SuperGrok image generation response"
                );
                drop(response);
                tokio::time::sleep(delay).await;
            }
            Ok(response) => return Ok(response),
            Err(error)
                if retry < MAX_TRANSIENT_RETRIES && (error.is_connect() || error.is_timeout()) =>
            {
                let delay = Duration::from_millis(500 * (1u64 << retry));
                tracing::warn!(
                    error = %error,
                    retry = retry + 1,
                    delay_ms = delay.as_millis(),
                    "retrying transient SuperGrok image generation transport failure"
                );
                tokio::time::sleep(delay).await;
            }
            Err(error) => return Err(operation_error(format!("request failed: {error}"))),
        }
    }
    unreachable!("bounded retry loop always returns")
}

fn request_payload(
    request: &ImageGenerationRequest,
    prompt: &str,
    model: &str,
    count: u32,
) -> Value {
    let mut payload = json!({
        "model": model,
        "prompt": prompt,
        "n": count,
        "response_format": "b64_json",
    });
    if let (Some(width), Some(height)) = (request.width, request.height) {
        payload["aspect_ratio"] = Value::String(nearest_aspect_ratio(width, height).to_string());
        payload["resolution"] = Value::String(
            if width.max(height) > 1_024 {
                "2k"
            } else {
                "1k"
            }
            .to_string(),
        );
    }
    payload
}

fn normalized_model(model: Option<&str>) -> Result<&str, ConnectorError> {
    let model = model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_IMAGE_MODEL);
    match model {
        DEFAULT_IMAGE_MODEL | QUALITY_IMAGE_MODEL => Ok(model),
        _ => Err(operation_error(format!(
            "unsupported Grok image model: {model}"
        ))),
    }
}

fn nearest_aspect_ratio(width: u32, height: u32) -> &'static str {
    if width == 0 || height == 0 {
        return "1:1";
    }
    const RATIOS: [(&str, f64); 11] = [
        ("1:1", 1.0),
        ("16:9", 16.0 / 9.0),
        ("9:16", 9.0 / 16.0),
        ("4:3", 4.0 / 3.0),
        ("3:4", 3.0 / 4.0),
        ("3:2", 3.0 / 2.0),
        ("2:3", 2.0 / 3.0),
        ("2:1", 2.0),
        ("1:2", 0.5),
        ("20:9", 20.0 / 9.0),
        ("9:20", 9.0 / 20.0),
    ];
    let target = width as f64 / height as f64;
    RATIOS
        .iter()
        .min_by(|left, right| {
            (target / left.1)
                .ln()
                .abs()
                .total_cmp(&(target / right.1).ln().abs())
        })
        .map(|(label, _)| *label)
        .unwrap_or("1:1")
}

async fn parse_response(
    response: Response,
    requested_model: &str,
) -> Result<ImageGenerationResult, ConnectorError> {
    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|error| operation_error(error.to_string()))?;
    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(ConnectorError::http_operation(
            connector_id(),
            "image generation",
            status.as_u16(),
            provider_message(&value, &body),
        ));
    }
    let items = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| operation_error("xAI image response is missing data"))?;
    let mut images = Vec::with_capacity(items.len());
    for item in items {
        let encoded = item
            .get("b64_json")
            .and_then(Value::as_str)
            .ok_or_else(|| operation_error("xAI image response is missing b64_json"))?;
        if encoded.len() > MAX_BASE64_CHARS {
            return Err(operation_error(
                "xAI image response exceeds the safe size limit",
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|error| operation_error(format!("invalid xAI image data: {error}")))?;
        let media_type = image_media_type(&bytes)
            .ok_or_else(|| operation_error("xAI returned an unsupported image format"))?;
        images.push(GeneratedImage {
            bytes,
            media_type: media_type.to_string(),
        });
    }
    if images.is_empty() {
        return Err(operation_error("xAI returned no generated images"));
    }
    let model = value
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(requested_model)
        .to_string();
    Ok(ImageGenerationResult { model, images })
}

fn image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn transient(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::BAD_GATEWAY
        || status == StatusCode::SERVICE_UNAVAILABLE
        || status == StatusCode::GATEWAY_TIMEOUT
        || status.as_u16() == 529
}

fn retry_delay(response: &Response, retry: usize) -> Duration {
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds <= 10);
    retry_after
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_millis(500 * (1u64 << retry.min(2))))
}

fn ensure_auth_epoch(oauth: &SuperGrokOAuth, epoch: u64) -> Result<(), ConnectorError> {
    if oauth.request_epoch_is_current(epoch) {
        Ok(())
    } else {
        Err(operation_error(
            "authentication changed while image generation was in progress",
        ))
    }
}

fn provider_message(value: &Value, raw: &str) -> String {
    value
        .get("error")
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .or_else(|| value.get("message").and_then(Value::as_str))
        .unwrap_or(raw)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(1_000)
        .collect()
}

fn connector_id() -> ConnectorId {
    ConnectorId::new(CONNECTOR_ID).expect("static connector id is valid")
}

fn operation_error(message: impl std::fmt::Display) -> ConnectorError {
    ConnectorError::operation(connector_id(), message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_maps_dimensions_to_supported_imagine_options() {
        let request = ImageGenerationRequest {
            prompt: "test".to_string(),
            model: None,
            width: Some(1_920),
            height: Some(1_080),
            count: 2,
        };
        let payload = request_payload(&request, "test", DEFAULT_IMAGE_MODEL, 2);
        assert_eq!(payload["aspect_ratio"], "16:9");
        assert_eq!(payload["resolution"], "2k");
        assert_eq!(payload["response_format"], "b64_json");
        assert_eq!(payload["n"], 2);
    }

    #[test]
    fn image_format_detection_accepts_supported_outputs_only() {
        assert_eq!(
            image_media_type(b"\x89PNG\r\n\x1a\nrest"),
            Some("image/png")
        );
        assert_eq!(image_media_type(b"\xff\xd8\xffrest"), Some("image/jpeg"));
        assert_eq!(image_media_type(b"RIFFxxxxWEBPrest"), Some("image/webp"));
        assert_eq!(image_media_type(b"GIF89a"), None);
    }

    #[test]
    fn image_models_are_allowlisted() {
        assert_eq!(normalized_model(None).unwrap(), DEFAULT_IMAGE_MODEL);
        assert_eq!(
            normalized_model(Some(QUALITY_IMAGE_MODEL)).unwrap(),
            QUALITY_IMAGE_MODEL
        );
        assert!(normalized_model(Some("grok-4.5")).is_err());
    }
}
