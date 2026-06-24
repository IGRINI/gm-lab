//! STT (speech-to-text / dictation) over the Codex subscription OAuth token.
//!
//! Faithful port of `gm-lab/codex_transcribe.py`. Posts audio to the ChatGPT
//! backend `/backend-api/transcribe` route as `multipart/form-data` with a
//! `file` part + `model` field (+ optional `language`); the response is JSON
//! `{"text": ...}`.
//!
//! Cloudflare fronts this path with bot mitigation that flags ordinary
//! TLS/HTTP2 fingerprints. We send the request through a Chrome TLS/JA3
//! impersonation client (`wreq` + `wreq-util` Emulation) so it passes the same
//! way Codex Desktop (Chromium) does — *without* overriding the User-Agent
//! (the preset sets a matching UA + client hints). A `cf-mitigated` response
//! header is treated as a challenge failure even on 2xx.
//!
//! ## Feature gating (risk #2)
//!
//! The impersonation client pulls in BoringSSL (`boring-sys2`), which needs a
//! C/C++ toolchain to build. To keep `gml-audio` ALWAYS buildable, the real
//! implementation is behind the default-on `stt` cargo feature; with the
//! feature off, [`transcribe`] is a stub returning
//! [`TranscribeError::Unavailable`]. STT is gated to `BACKEND==codex` anyway.

use gml_config::Config;

/// Default transcription model (`GM_CODEX_TRANSCRIBE_MODEL`).
pub const DEFAULT_MODEL: &str = "gpt-4o-mini-transcribe";
/// Default language hint (`GM_CODEX_TRANSCRIBE_LANGUAGE`); empty disables it.
pub const DEFAULT_LANGUAGE: &str = "ru";
/// Default request timeout in seconds (`GM_CODEX_TRANSCRIBE_TIMEOUT`).
pub const DEFAULT_TIMEOUT_SECS: f64 = 120.0;

/// Typed STT failure, mirroring Python's `TranscribeError(message, status)`.
#[derive(Debug, thiserror::Error)]
pub enum TranscribeError {
    /// `audio` was empty.
    #[error("empty audio")]
    EmptyAudio,
    /// Could not obtain / refresh the Codex credential (not authorized).
    #[error("{0}")]
    Auth(String),
    /// Cloudflare returned a challenge (`cf-mitigated` header present).
    #[error("Cloudflare заблокировал транскрипцию (challenge) — TLS-обход не прошёл")]
    Cloudflare {
        /// HTTP status of the challenge response.
        status: u16,
    },
    /// The request itself failed (network / TLS).
    #[error("transcribe request failed: {0}")]
    Request(String),
    /// A non-2xx HTTP status (with a truncated body).
    #[error("transcribe HTTP {status}: {body}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Truncated, newline-stripped body.
        body: String,
    },
    /// The response carried no usable text.
    #[error("{0}")]
    NoText(String),
    /// STT was compiled out (`--no-default-features`, no `stt` feature).
    #[error("STT unavailable: built without stt feature")]
    Unavailable,
}

impl TranscribeError {
    /// The HTTP status associated with the error, if any (mirrors Python's
    /// `ex.status`). The server surfaces this in the JSON error payload.
    pub fn status(&self) -> Option<u16> {
        match self {
            TranscribeError::Cloudflare { status } => Some(*status),
            TranscribeError::Http { status, .. } => Some(*status),
            _ => None,
        }
    }
}

/// Compute the transcribe URL from config + env override, mirroring
/// `codex_transcribe.transcribe_url()`.
///
/// `GM_CODEX_TRANSCRIBE_URL` wins if set. Otherwise take `CODEX_BASE_URL`
/// (already rstripped of `/` in `Config`), strip a trailing `/codex` segment
/// (the transcribe route lives directly under `/backend-api`, NOT under
/// `/codex`), and append `/transcribe`.
pub fn transcribe_url(cfg: &Config) -> String {
    let override_url = std::env::var("GM_CODEX_TRANSCRIBE_URL").unwrap_or_default();
    let override_url = override_url.trim();
    if !override_url.is_empty() {
        return override_url.to_string();
    }
    let mut base = cfg.codex_base_url.trim_end_matches('/').to_string();
    if let Some(stripped) = base.strip_suffix("/codex") {
        base = stripped.to_string();
    }
    format!("{base}/transcribe")
}

/// The model to send (`GM_CODEX_TRANSCRIBE_MODEL` or default).
pub fn transcribe_model() -> String {
    env_or("GM_CODEX_TRANSCRIBE_MODEL", DEFAULT_MODEL)
}

/// The language hint (`GM_CODEX_TRANSCRIBE_LANGUAGE` or default `ru`); empty
/// disables the hint.
pub fn transcribe_language() -> String {
    // Python: os.environ.get(..., "ru") — only the default is "ru"; an
    // explicitly-set empty string disables the hint.
    std::env::var("GM_CODEX_TRANSCRIBE_LANGUAGE").unwrap_or_else(|_| DEFAULT_LANGUAGE.to_string())
}

/// The request timeout (`GM_CODEX_TRANSCRIBE_TIMEOUT` or default 120s).
pub fn transcribe_timeout_secs() -> f64 {
    std::env::var("GM_CODEX_TRANSCRIBE_TIMEOUT")
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

fn env_or(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.is_empty() => v,
        _ => default.to_string(),
    }
}

/// Pick the upload filename from the content type, exactly like
/// `_filename_for`.
pub fn filename_for(content_type: &str) -> &'static str {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("webm") {
        "audio.webm"
    } else if ct.contains("ogg") {
        "audio.ogg"
    } else if ct.contains("mp4") || ct.contains("m4a") || ct.contains("aac") {
        "audio.mp4"
    } else if ct.contains("mpeg") || ct.contains("mp3") {
        "audio.mp3"
    } else if ct.contains("wav") || ct.contains("wave") || ct.contains("pcm") {
        "audio.wav"
    } else {
        "audio.webm"
    }
}

/// Extract the transcript text from the JSON response body, mirroring the
/// Python tail: prefer `text`, then `transcript`; a JSON string is `.trim()`ed
/// and returned (empty is a valid result, e.g. silence). Non-JSON falls back to
/// the trimmed raw body.
pub fn extract_text(body: &str) -> Result<String, TranscribeError> {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(serde_json::Value::Object(map)) => {
            let text = map
                .get("text")
                .and_then(|v| v.as_str())
                .or_else(|| map.get("transcript").and_then(|v| v.as_str()));
            match text {
                Some(s) => Ok(s.trim().to_string()),
                None => Err(TranscribeError::NoText(
                    "transcribe response had no text field".to_string(),
                )),
            }
        }
        // JSON but not an object, or invalid JSON -> fall back to raw text.
        _ => {
            let t = body.trim();
            if !t.is_empty() {
                Ok(t.to_string())
            } else {
                Err(TranscribeError::NoText(
                    "transcribe returned no JSON and no text".to_string(),
                ))
            }
        }
    }
}

/// Truncate + newline-strip a body for the HTTP-error message (Python: replace
/// `\n` with space, cap at 400 chars + `…`). Used by the `stt`-feature impl
/// (and by tests); marked allow(dead_code) so the no-feature lib build is clean.
#[cfg_attr(not(any(feature = "stt", test)), allow(dead_code))]
fn truncate_body(body: &str) -> String {
    let mut s: String = body.replace('\n', " ").trim().to_string();
    if s.chars().count() > 400 {
        let truncated: String = s.chars().take(400).collect();
        s = format!("{truncated}…");
    }
    s
}

#[cfg(feature = "stt")]
mod imp {
    use super::*;
    use uuid::Uuid;
    use wreq::multipart::{Form, Part};
    use wreq_util::Emulation;

    /// Transcribe `audio` via the Codex OAuth token behind a Chrome TLS/JA3
    /// impersonation client. Faithful port of `codex_transcribe.transcribe`.
    pub async fn transcribe(
        cfg: &Config,
        audio: &[u8],
        content_type: &str,
    ) -> Result<String, TranscribeError> {
        if audio.is_empty() {
            return Err(TranscribeError::EmptyAudio);
        }

        // Credential — plain reqwest is fine for the (non-CF) token refresh.
        let refresh_http = reqwest::Client::new();
        let credential = gml_codex::ensure_fresh_credential(&refresh_http, cfg)
            .await
            .map_err(|e| TranscribeError::Auth(e.to_string()))?;

        // Impersonating client. The preset sets UA + client hints — DO NOT
        // override the User-Agent (Python note: a codex UA over a Chrome TLS
        // fingerprint looks inconsistent and can re-trigger CF).
        let client = wreq::Client::builder()
            .emulation(Emulation::Chrome137)
            .build()
            .map_err(|e| TranscribeError::Request(e.to_string()))?;

        let ct = if content_type.is_empty() {
            "audio/webm"
        } else {
            content_type
        };
        let filename = filename_for(ct);

        // Headers — identical set to Python (note: NO User-Agent).
        let mut req = client
            .post(transcribe_url(cfg))
            .header(
                "Authorization",
                format!("Bearer {}", credential.access_token.trim()),
            )
            .header("originator", cfg.codex_originator.clone())
            .header("version", cfg.codex_client_version.clone())
            .header("session-id", Uuid::new_v4().to_string())
            .header("thread-id", Uuid::new_v4().to_string())
            .header("x-codex-installation-id", Uuid::new_v4().to_string());
        if let Some(account_id) = credential.account_id.as_deref() {
            if !account_id.is_empty() {
                req = req.header("ChatGPT-Account-Id", account_id.to_string());
            }
        }

        // multipart: file part (filename + content type) + model (+ language).
        let file_part = Part::bytes(audio.to_vec())
            .file_name(filename)
            .mime_str(ct)
            .map_err(|e| TranscribeError::Request(e.to_string()))?;
        let mut form = Form::new()
            .part("file", file_part)
            .text("model", transcribe_model());
        let language = transcribe_language();
        if !language.is_empty() {
            form = form.text("language", language);
        }

        let timeout = std::time::Duration::from_secs_f64(transcribe_timeout_secs().max(0.0));
        let response = req
            .multipart(form)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| TranscribeError::Request(e.to_string()))?;

        let status = response.status().as_u16();

        // Cloudflare challenge — flagged even on 2xx.
        if response.headers().get("cf-mitigated").is_some() {
            return Err(TranscribeError::Cloudflare { status });
        }

        if !(200..300).contains(&status) {
            let body = response.text().await.unwrap_or_default();
            return Err(TranscribeError::Http {
                status,
                body: super::truncate_body(&body),
            });
        }

        let body = response
            .text()
            .await
            .map_err(|e| TranscribeError::Request(e.to_string()))?;
        super::extract_text(&body)
    }
}

#[cfg(not(feature = "stt"))]
mod imp {
    use super::*;

    /// Stub used when the crate is built without the `stt` feature. Always
    /// returns [`TranscribeError::Unavailable`] so `gml-audio` still builds and
    /// the rest of the app is unaffected.
    pub async fn transcribe(
        _cfg: &Config,
        _audio: &[u8],
        _content_type: &str,
    ) -> Result<String, TranscribeError> {
        Err(TranscribeError::Unavailable)
    }
}

/// Transcribe `audio` (raw bytes, `content_type` like `audio/webm`) to text via
/// the Codex subscription OAuth token. See module docs for the wire shape and
/// the `stt` feature gate.
pub async fn transcribe(
    cfg: &Config,
    audio: &[u8],
    content_type: &str,
) -> Result<String, TranscribeError> {
    imp::transcribe(cfg, audio, content_type).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_base(base: &str) -> Config {
        let mut c = Config::from_env();
        c.codex_base_url = base.to_string();
        c
    }

    // The transcribe URL reads a process-global env var; these three tests
    // mutate it, so serialize them under one mutex to avoid cross-test races.
    static URL_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn transcribe_url_strips_codex_suffix() {
        let _g = URL_ENV_LOCK.lock().unwrap();
        std::env::remove_var("GM_CODEX_TRANSCRIBE_URL");
        let c = cfg_with_base("https://chatgpt.com/backend-api/codex");
        // /codex stripped, /transcribe appended under /backend-api.
        assert_eq!(
            transcribe_url(&c),
            "https://chatgpt.com/backend-api/transcribe"
        );
    }

    #[test]
    fn transcribe_url_without_codex_suffix() {
        let _g = URL_ENV_LOCK.lock().unwrap();
        std::env::remove_var("GM_CODEX_TRANSCRIBE_URL");
        let c = cfg_with_base("https://example.com/backend-api");
        assert_eq!(
            transcribe_url(&c),
            "https://example.com/backend-api/transcribe"
        );
    }

    #[test]
    fn transcribe_url_env_override_wins() {
        let _g = URL_ENV_LOCK.lock().unwrap();
        std::env::set_var("GM_CODEX_TRANSCRIBE_URL", "https://override/t");
        let c = cfg_with_base("https://chatgpt.com/backend-api/codex");
        assert_eq!(transcribe_url(&c), "https://override/t");
        std::env::remove_var("GM_CODEX_TRANSCRIBE_URL");
    }

    #[test]
    fn filename_for_by_content_type() {
        assert_eq!(filename_for("audio/webm"), "audio.webm");
        assert_eq!(filename_for("audio/ogg"), "audio.ogg");
        assert_eq!(filename_for("audio/mp4"), "audio.mp4");
        assert_eq!(filename_for("audio/m4a"), "audio.mp4");
        assert_eq!(filename_for("audio/aac"), "audio.mp4");
        assert_eq!(filename_for("audio/mpeg"), "audio.mp3");
        assert_eq!(filename_for("audio/mp3"), "audio.mp3");
        assert_eq!(filename_for("audio/wav"), "audio.wav");
        assert_eq!(filename_for("audio/wave"), "audio.wav");
        assert_eq!(filename_for("application/pcm"), "audio.wav");
        // Unknown -> webm default.
        assert_eq!(filename_for("application/octet-stream"), "audio.webm");
        assert_eq!(filename_for(""), "audio.webm");
    }

    #[test]
    fn extract_text_prefers_text_then_transcript() {
        assert_eq!(extract_text(r#"{"text":"  hi  "}"#).unwrap(), "hi");
        assert_eq!(
            extract_text(r#"{"transcript":"fallback"}"#).unwrap(),
            "fallback"
        );
        // text wins over transcript.
        assert_eq!(
            extract_text(r#"{"text":"a","transcript":"b"}"#).unwrap(),
            "a"
        );
        // Empty string is a valid result (silence).
        assert_eq!(extract_text(r#"{"text":""}"#).unwrap(), "");
    }

    #[test]
    fn extract_text_object_without_text_errors() {
        let err = extract_text(r#"{"foo":1}"#).unwrap_err();
        assert!(matches!(err, TranscribeError::NoText(_)));
    }

    #[test]
    fn extract_text_non_json_falls_back_to_raw() {
        assert_eq!(extract_text("  plain text  ").unwrap(), "plain text");
        // Empty non-JSON -> NoText.
        assert!(matches!(
            extract_text("   ").unwrap_err(),
            TranscribeError::NoText(_)
        ));
    }

    #[test]
    fn truncate_body_strips_newlines_and_caps() {
        assert_eq!(truncate_body("a\nb\nc"), "a b c");
        let long = "x".repeat(500);
        let t = truncate_body(&long);
        assert_eq!(t.chars().count(), 401); // 400 + ellipsis
        assert!(t.ends_with('…'));
    }

    #[test]
    fn error_status_surface() {
        assert_eq!(
            TranscribeError::Cloudflare { status: 403 }.status(),
            Some(403)
        );
        assert_eq!(
            TranscribeError::Http {
                status: 500,
                body: "x".into()
            }
            .status(),
            Some(500)
        );
        assert_eq!(TranscribeError::EmptyAudio.status(), None);
        assert_eq!(TranscribeError::Unavailable.status(), None);
    }

    #[tokio::test]
    async fn empty_audio_is_rejected_offline() {
        // Does not hit the network: empty-audio guard fires first.
        let c = Config::from_env();
        let err = transcribe(&c, &[], "audio/webm").await.unwrap_err();
        // With `stt` on this is EmptyAudio; without the feature it's Unavailable.
        // Either way it's a clean typed error, never a panic.
        assert!(matches!(
            err,
            TranscribeError::EmptyAudio | TranscribeError::Unavailable
        ));
    }
}
