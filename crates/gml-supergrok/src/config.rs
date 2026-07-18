use std::path::PathBuf;
use std::time::Duration;

pub const DEFAULT_INFERENCE_BASE_URL: &str = "https://api.x.ai/v1";
pub const DEFAULT_DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
pub const DEFAULT_DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";
pub const DEFAULT_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const DEFAULT_SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
pub const DEFAULT_MODEL_ID: &str = "grok-4.5";
/// Empty by default so xAI detects the spoken language automatically.
pub const DEFAULT_STT_LANGUAGE: &str = "";
pub const DEFAULT_STT_MAX_AUDIO_BYTES: usize = 32 * 1024 * 1024;
pub const MIN_RESPONSES_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Connector-owned settings. The app may override every value at composition
/// time; no provider setting leaks into the core model contract.
#[derive(Clone, Debug)]
pub struct SuperGrokConfig {
    pub inference_base_url: String,
    pub discovery_url: String,
    pub device_code_url: String,
    pub client_id: String,
    pub scope: String,
    pub credential_path: PathBuf,
    pub model: String,
    pub compact_model: String,
    pub prompt_cache_key: String,
    pub compact_input_chars: usize,
    pub stt_language: String,
    pub stt_max_audio_bytes: usize,
    pub stt_timeout: Duration,
    pub image_timeout: Duration,
    pub responses_idle_timeout: Duration,
    pub user_agent: String,
    pub oauth_timeout: Duration,
    pub refresh_margin: Duration,
}

impl SuperGrokConfig {
    pub fn new(credential_path: impl Into<PathBuf>) -> Self {
        Self {
            inference_base_url: DEFAULT_INFERENCE_BASE_URL.to_string(),
            discovery_url: DEFAULT_DISCOVERY_URL.to_string(),
            device_code_url: DEFAULT_DEVICE_CODE_URL.to_string(),
            client_id: DEFAULT_CLIENT_ID.to_string(),
            scope: DEFAULT_SCOPE.to_string(),
            credential_path: credential_path.into(),
            model: DEFAULT_MODEL_ID.to_string(),
            compact_model: String::new(),
            prompt_cache_key: String::new(),
            compact_input_chars: 12_000,
            stt_language: DEFAULT_STT_LANGUAGE.to_string(),
            stt_max_audio_bytes: DEFAULT_STT_MAX_AUDIO_BYTES,
            stt_timeout: Duration::from_secs(120),
            image_timeout: Duration::from_secs(180),
            responses_idle_timeout: MIN_RESPONSES_IDLE_TIMEOUT,
            user_agent: format!(
                "taleshift/{} (SuperGrok connector)",
                env!("CARGO_PKG_VERSION")
            ),
            oauth_timeout: Duration::from_secs(20),
            refresh_margin: Duration::from_secs(120),
        }
    }

    /// Standalone default for tests/tools. The shipped app should pass its own
    /// app-data path to [`Self::new`].
    pub fn from_env() -> Self {
        let path = std::env::var_os("GM_SUPERGROK_CREDENTIAL_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                directories::ProjectDirs::from("", "", "gm-lab")
                    .map(|dirs| dirs.config_dir().join("supergrok_credential.json"))
            })
            .unwrap_or_else(|| PathBuf::from("supergrok_credential.json"));
        let mut config = Self::new(path);
        config.apply_env();
        config
    }

    pub fn apply_env(&mut self) {
        if let Some(path) =
            std::env::var_os("GM_SUPERGROK_CREDENTIAL_PATH").filter(|value| !value.is_empty())
        {
            self.credential_path = PathBuf::from(path);
        }
        set_string(&mut self.inference_base_url, "GM_SUPERGROK_BASE_URL");
        set_string(&mut self.model, "GM_SUPERGROK_MODEL");
        set_string(&mut self.compact_model, "GM_SUPERGROK_COMPACT_MODEL");
        set_string(&mut self.prompt_cache_key, "GM_SUPERGROK_PROMPT_CACHE_KEY");
        set_duration_seconds(
            &mut self.responses_idle_timeout,
            "GM_SUPERGROK_STREAM_IDLE_TIMEOUT_SECONDS",
        );
    }

    pub(crate) fn effective_responses_idle_timeout(&self) -> Duration {
        self.responses_idle_timeout.max(MIN_RESPONSES_IDLE_TIMEOUT)
    }

    pub(crate) fn responses_url(&self) -> String {
        format!(
            "{}/responses",
            self.inference_base_url.trim_end_matches('/')
        )
    }

    pub(crate) fn models_url(&self) -> String {
        format!("{}/models", self.inference_base_url.trim_end_matches('/'))
    }

    pub(crate) fn stt_url(&self) -> String {
        format!("{}/stt", self.inference_base_url.trim_end_matches('/'))
    }

    pub(crate) fn images_url(&self) -> String {
        format!(
            "{}/images/generations",
            self.inference_base_url.trim_end_matches('/')
        )
    }
}

fn set_string(target: &mut String, key: &str) {
    if let Ok(value) = std::env::var(key) {
        let value = value.trim();
        if !value.is_empty() {
            *target = value.to_string();
        }
    }
}

fn set_duration_seconds(target: &mut Duration, key: &str) {
    let Some(seconds) = std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
    else {
        return;
    };
    *target = Duration::from_secs(seconds).max(MIN_RESPONSES_IDLE_TIMEOUT);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speech_to_text_defaults_to_language_detection() {
        assert_eq!(SuperGrokConfig::new("auth.json").stt_language, "");
    }

    #[test]
    fn responses_idle_timeout_never_drops_below_minimum() {
        let mut config = SuperGrokConfig::new("auth.json");
        assert_eq!(
            config.effective_responses_idle_timeout(),
            MIN_RESPONSES_IDLE_TIMEOUT
        );

        config.responses_idle_timeout = Duration::from_secs(1);
        assert_eq!(
            config.effective_responses_idle_timeout(),
            MIN_RESPONSES_IDLE_TIMEOUT
        );

        config.responses_idle_timeout = Duration::from_secs(300);
        assert_eq!(
            config.effective_responses_idle_timeout(),
            Duration::from_secs(300)
        );
    }
}
