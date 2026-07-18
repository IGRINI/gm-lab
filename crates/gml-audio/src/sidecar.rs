//! Unified inference sidecar process manager (NEW design — no Python reference).
//!
//! PORT_PLAN §3.2 sidecar row / risk #7. Spawns the unified `serve.py` sidecar
//! (one process hosting embeddings + rerank + STT + TTS + images) once (guarded by a
//! `OnceCell<Mutex<...>>`), polls a health endpoint until ready or timeout,
//! exposes readiness, and kills the process tree on shutdown (cross-platform,
//! via [`crate::proc`]).
//!
//! The app must run fully without local media models: if they are disabled or
//! the sidecar can't start, every
//! call returns a clean [`SidecarError`] and nothing else is affected.
//!
//! ## Readiness state machine
//!
//! ```text
//!  Disabled ── (enabled + ensure_started) ──▶ Starting
//!  Starting ── health ok ───────────────────▶ Ready
//!  Starting ── timeout / spawn fail ────────▶ Failed
//!  any      ── shutdown ─────────────────────▶ Disabled (process tree killed)
//! ```
//!
//! The state transitions are factored into [`StateMachine`] so they can be
//! unit-tested against a stubbed health check with zero process spawning.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::OnceCell;
use serde_json::Value;

use crate::proc::ProcessTree;

/// Env var holding the spawn command line for the sidecar. When unset, the
/// manager launches `serve.py` with `PYTHON`, `python`, or `python3`.
pub const SPAWN_CMD_ENV: &str = "GM_TTS_SPAWN_CMD";
/// Env var pointing at the sidecar working directory (the `serve.py` dir).
pub const SPAWN_DIR_ENV: &str = "GM_TTS_SPAWN_DIR";
/// Env var: how long (seconds) to wait for the sidecar to become healthy.
pub const READY_TIMEOUT_ENV: &str = "GM_TTS_READY_TIMEOUT";
/// Root of the reproducible local inference installation created by
/// `setup.ps1`. The application seeds a per-OS default before constructing the
/// sidecar, while explicit user configuration always wins.
pub const INFERENCE_HOME_ENV: &str = "GM_INFERENCE_HOME";
/// Advanced override for a custom TTS installation. Managed installs derive
/// this capability from their readiness markers instead of trusting a toggle.
pub const LOCAL_TTS_AVAILABLE_ENV: &str = "GM_LOCAL_TTS_AVAILABLE";
/// Advanced override for a custom local speech-to-text installation.
pub const LOCAL_STT_AVAILABLE_ENV: &str = "GM_LOCAL_STT_AVAILABLE";
/// Advanced override for a custom local image installation.
pub const LOCAL_IMAGE_AVAILABLE_ENV: &str = "GM_LOCAL_IMAGE_AVAILABLE";

/// Default sidecar script, relative to the resolved sidecar directory.
pub const DEFAULT_SCRIPT: &str = "serve.py";
/// Default readiness timeout in seconds. The TTS 1.7B load + CUDA-graph capture
/// is ~120s; allow margin for a cold model load.
pub const DEFAULT_READY_TIMEOUT_SECS: u64 = 300;
/// Interval between health polls.
pub const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Sidecar lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarState {
    /// TTS disabled or never started. No process running.
    Disabled,
    /// Spawned; polling health, not yet ready.
    Starting,
    /// Health check passed; the sidecar is accepting requests.
    Ready,
    /// Spawn failed or readiness timed out.
    Failed,
}

impl SidecarState {
    /// Stable wire label used by the UI status endpoint.
    pub fn as_str(self) -> &'static str {
        match self {
            SidecarState::Disabled => "disabled",
            SidecarState::Starting => "starting",
            SidecarState::Ready => "ready",
            SidecarState::Failed => "failed",
        }
    }
}

/// Read-only snapshot of the live sidecar manager.
#[derive(Debug, Clone)]
pub struct SidecarSnapshot {
    pub state: SidecarState,
    pub ready: bool,
    pub pid: Option<u32>,
    pub base_url: String,
    pub started_elapsed: Option<Duration>,
    pub ready_timeout: Duration,
    pub error: Option<String>,
}

/// Sidecar failures.
#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    /// TTS is disabled via runtime settings — a clean, expected no-op path.
    #[error("local inference disabled or not installed")]
    Disabled,
    /// The sidecar process failed to spawn.
    #[error("failed to spawn TTS sidecar: {0}")]
    Spawn(String),
    /// The spawned sidecar process exited before it became healthy.
    #[error("TTS sidecar process exited before readiness: {0}")]
    Exited(String),
    /// The sidecar did not become healthy within the timeout.
    #[error("TTS sidecar did not become ready within {0:?}")]
    Timeout(Duration),
    /// A request to a ready sidecar failed.
    #[error("local inference request failed: {0}")]
    Request(String),
    /// The sidecar returned a successful but malformed response.
    #[error("invalid local inference response: {0}")]
    InvalidResponse(String),
}

/// Static configuration for the sidecar manager.
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// Base URL of the sidecar (its `/health` is polled). Defaults to
    /// [`crate::tts::tts_url`].
    pub base_url: String,
    /// The program + args to launch. `None` => derive from env / default.
    pub spawn_program: String,
    /// Args for the spawn program.
    pub spawn_args: Vec<String>,
    /// Working directory for the spawned process.
    pub spawn_dir: String,
    /// How long to wait for readiness.
    pub ready_timeout: Duration,
    /// Extra environment variables to set on the spawned process — the unified
    /// sidecar reads these (HF_HOME, GMLAB_SIDECAR_PORT, EMBEDDER_QUANT /
    /// RERANKER_QUANT, EMBEDDER/RERANKER/TTS_ENABLED, TTS_HOME, TTS_MODEL_ID…).
    pub envs: Vec<(String, String)>,
    /// Whether all files required by managed local STT are installed.
    pub stt_available: bool,
    /// Whether all files required by managed local TTS are installed.
    pub tts_available: bool,
    /// Whether all files required by managed local image generation are installed.
    pub image_available: bool,
}

impl SidecarConfig {
    /// Build the config from environment + sensible defaults.
    ///
    /// `GM_TTS_SPAWN_CMD` is split on whitespace (first token = program). If
    /// unset, the manager uses `PYTHON`/`python`/`python3` and runs `serve.py`
    /// from the resolved sidecar directory. The base URL comes from `GM_TTS_URL`.
    pub fn from_env() -> Self {
        let base_url = crate::tts::tts_url();
        let inference_home = default_inference_home();
        let installed = installed_inference_features(&inference_home);
        let stt_available = capability_override(LOCAL_STT_AVAILABLE_ENV).unwrap_or(installed.stt);
        let tts_available = capability_override(LOCAL_TTS_AVAILABLE_ENV).unwrap_or(installed.tts);
        let image_available =
            capability_override(LOCAL_IMAGE_AVAILABLE_ENV).unwrap_or(installed.images);
        let spawn_dir = {
            let d = std::env::var(SPAWN_DIR_ENV).unwrap_or_default();
            if d.trim().is_empty() {
                default_spawn_dir()
            } else {
                d
            }
        };
        let (spawn_program, spawn_args) = match std::env::var(SPAWN_CMD_ENV) {
            Ok(cmd) if !cmd.trim().is_empty() => {
                let mut parts = cmd.split_whitespace().map(|s| s.to_string());
                let prog = parts
                    .next()
                    .unwrap_or_else(|| default_python(&inference_home));
                (prog, parts.collect())
            }
            _ => (
                default_python(&inference_home),
                vec![DEFAULT_SCRIPT.to_string()],
            ),
        };
        let ready_timeout = std::env::var(READY_TIMEOUT_ENV)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(DEFAULT_READY_TIMEOUT_SECS));

        // Port for serve.py's uvicorn bind, parsed from base_url so they stay in
        // sync (base_url = http://127.0.0.1:8077 -> "8077").
        let port = base_url
            .rsplit(':')
            .next()
            .map(|s| s.trim_end_matches('/'))
            .filter(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or("8077")
            .to_string();
        let hf_home = env_path_or("HF_HOME", inference_home.join("hf"));
        let stt_model = env_path_or("GM_STT_MODEL", inference_home.join("models").join("stt"));
        let tts_home = env_path_or("TTS_HOME", inference_home.join("tts"));
        let image_root = env_path_or("IMAGE_RUNTIME_ROOT", inference_home.join("image"));
        let image_comfy_dir = env_path_or("IMAGE_COMFY_DIR", image_root.join("ComfyUI"));
        let image_python = env_path_or("IMAGE_PYTHON", venv_python(&image_root.join(".venv")));
        let image_hf_home = env_path_or("IMAGE_HF_HOME", image_root.join("hf"));
        let envs = vec![
            (
                INFERENCE_HOME_ENV.to_string(),
                inference_home.to_string_lossy().into_owned(),
            ),
            ("GMLAB_SIDECAR_PORT".to_string(), port),
            (
                "HF_HOME".to_string(),
                hf_home.to_string_lossy().into_owned(),
            ),
            (
                "STT_MODEL".to_string(),
                stt_model.to_string_lossy().into_owned(),
            ),
            (
                "TTS_HOME".to_string(),
                tts_home.to_string_lossy().into_owned(),
            ),
            (
                "IMAGE_RUNTIME_ROOT".to_string(),
                image_root.to_string_lossy().into_owned(),
            ),
            (
                "IMAGE_COMFY_DIR".to_string(),
                image_comfy_dir.to_string_lossy().into_owned(),
            ),
            (
                "IMAGE_PYTHON".to_string(),
                image_python.to_string_lossy().into_owned(),
            ),
            (
                "IMAGE_HF_HOME".to_string(),
                image_hf_home.to_string_lossy().into_owned(),
            ),
            (
                "GMLAB_SIDECAR_LOG".to_string(),
                inference_home
                    .join("logs")
                    .join("sidecar.log")
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("HF_HUB_DISABLE_TELEMETRY".to_string(), "1".to_string()),
        ];

        SidecarConfig {
            base_url,
            spawn_program,
            spawn_args,
            spawn_dir,
            ready_timeout,
            envs,
            stt_available,
            tts_available,
            image_available,
        }
    }
}

fn capability_override(key: &str) -> Option<bool> {
    std::env::var(key).ok().and_then(|value| {
        let value = value.trim().to_ascii_lowercase();
        match value.as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        }
    })
}

/// Read a path env var, falling back to `default` when unset or blank.
fn env_path_or(key: &str, default: PathBuf) -> PathBuf {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => default,
    }
}

fn default_python(inference_home: &Path) -> String {
    let from_env = std::env::var("PYTHON").unwrap_or_default();
    if !from_env.trim().is_empty() {
        return from_env;
    }
    let managed = venv_python(&inference_home.join("runtime").join(".venv"));
    if managed.is_file() {
        return managed.to_string_lossy().into_owned();
    }
    if cfg!(windows) {
        "python".to_string()
    } else {
        "python3".to_string()
    }
}

fn venv_python(venv: &Path) -> PathBuf {
    if cfg!(windows) {
        venv.join("Scripts").join("python.exe")
    } else {
        venv.join("bin").join("python")
    }
}

/// Resolve the shared per-user inference root used by setup and the launcher.
/// An explicit `GM_INFERENCE_HOME` always wins.
pub fn default_inference_home() -> PathBuf {
    if let Ok(configured) = std::env::var(INFERENCE_HOME_ENV) {
        if !configured.trim().is_empty() {
            return PathBuf::from(configured);
        }
    }
    if cfg!(windows) {
        for key in ["LOCALAPPDATA", "APPDATA"] {
            if let Ok(base) = std::env::var(key) {
                if !base.trim().is_empty() {
                    return PathBuf::from(base).join("gm-lab").join("inference");
                }
            }
        }
    }
    if let Ok(base) = std::env::var("XDG_DATA_HOME") {
        if !base.trim().is_empty() {
            return PathBuf::from(base).join("gm-lab").join("inference");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.trim().is_empty() {
            let home = PathBuf::from(home);
            if cfg!(target_os = "macos") {
                return home
                    .join("Library")
                    .join("Application Support")
                    .join("gm-lab")
                    .join("inference");
            }
            return home
                .join(".local")
                .join("share")
                .join("gm-lab")
                .join("inference");
        }
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("gm-lab-data")
        .join("inference")
}

/// Local components proven complete by setup state plus per-artifact markers.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InferenceFeatures {
    pub rag: bool,
    pub stt: bool,
    pub tts: bool,
    pub images: bool,
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn completed_install_profile(body: &Value) -> Option<&str> {
    if body.get("schema_version").and_then(Value::as_u64) != Some(2)
        || body.get("build_complete").and_then(Value::as_bool) != Some(true)
        || [
            "source_fingerprint",
            "executable_sha256",
            "web_dist_fingerprint",
        ]
        .iter()
        .any(|key| {
            !body
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(is_sha256)
        })
    {
        return None;
    }
    body.get("profile").and_then(Value::as_str)
}

/// Read the managed installation without touching the network or loading a
/// model. Markers are written only after the installer verifies each artifact.
pub fn installed_inference_features(inference_home: &Path) -> InferenceFeatures {
    let profile = std::fs::read_to_string(inference_home.join("install.json"))
        .ok()
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
        .and_then(|body| completed_install_profile(&body).map(str::to_owned));
    let profile = profile.as_deref().unwrap_or("Minimal");

    let snapshot_ready = |relative: &str, component: &str| {
        let root = inference_home.join(relative);
        marker_ready(&root.join(".gml-model.json"), component, &root, None)
    };
    let file_ready = |relative: &str, component: &str| {
        let path = inference_home.join(relative);
        let (Some(name), Some(root)) = (path.file_name(), path.parent()) else {
            return false;
        };
        marker_ready(
            &path.with_file_name(format!("{}.gml-model.json", name.to_string_lossy())),
            component,
            root,
            Some(&path),
        )
    };
    let runtime_ready = venv_python(&inference_home.join("runtime/.venv")).is_file();

    let wants_rag = matches!(profile, "Rag" | "Voice" | "Images" | "Full");
    let rag_ready = runtime_ready
        && wants_rag
        && snapshot_ready("models/embedder", "embedder")
        && snapshot_ready("models/reranker", "reranker");
    let wants_stt = matches!(profile, "Voice" | "Full");
    let stt_ready = runtime_ready && wants_stt && snapshot_ready("models/stt", "stt-model");
    let wants_tts = matches!(profile, "Voice" | "Full");
    let tts_ready = runtime_ready
        && wants_tts
        && snapshot_ready("tts/qwen17b_base", "tts-model")
        && file_ready("tts/ref_audio.wav", "tts-male-voice")
        && file_ready("tts/ref_audio_2.wav", "tts-gm-voice")
        && file_ready("tts/ref_audio_3.wav", "tts-female-voice");
    let wants_images = matches!(profile, "Images" | "Full");
    let image_ready = runtime_ready
        && wants_images
        && inference_home.join("image/ComfyUI/main.py").is_file()
        && venv_python(&inference_home.join("image/.venv")).is_file()
        && file_ready(
            "image/ComfyUI/models/diffusion_models/flux-2-klein-4b-nvfp4.safetensors",
            "image-diffusion",
        )
        && file_ready(
            "image/ComfyUI/models/text_encoders/qwen_3_4b_fp4_flux2.safetensors",
            "image-text-encoder",
        )
        && file_ready(
            "image/ComfyUI/models/vae/flux2-vae.safetensors",
            "image-vae",
        );

    InferenceFeatures {
        rag: rag_ready,
        stt: stt_ready,
        tts: tts_ready,
        images: image_ready,
    }
}

fn marker_ready(
    marker: &Path,
    component: &str,
    component_root: &Path,
    single_file: Option<&Path>,
) -> bool {
    let Some((expected_manifest, expected_artifacts)) = marker_spec(component) else {
        return false;
    };
    let Some(body) = std::fs::read_to_string(marker)
        .ok()
        .and_then(|body| serde_json::from_str::<Value>(&body).ok())
    else {
        return false;
    };
    if body.get("schema_version").and_then(Value::as_u64) != Some(2)
        || body.get("component").and_then(Value::as_str) != Some(component)
        || body.get("manifest_sha256").and_then(Value::as_str) != Some(expected_manifest)
    {
        return false;
    }
    let Some(inventory) = body.get("artifacts").and_then(Value::as_object) else {
        return false;
    };
    if inventory.len() != expected_artifacts.len()
        || !expected_artifacts
            .iter()
            .all(|relative| inventory.contains_key(*relative))
    {
        return false;
    }

    let Some(canonical_root) = component_root.canonicalize().ok() else {
        return false;
    };
    expected_artifacts.iter().all(|relative| {
        let artifact = if *relative == "." {
            let Some(file) = single_file else {
                return false;
            };
            file.to_path_buf()
        } else {
            if single_file.is_some() || !safe_relative_path(relative) {
                return false;
            }
            component_root.join(relative)
        };
        let Some(record) = inventory.get(*relative).and_then(Value::as_object) else {
            return false;
        };
        let Some(recorded_size) = record.get("size").and_then(Value::as_u64) else {
            return false;
        };
        let recorded_sha = record
            .get("sha256")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !is_sha256(recorded_sha) {
            return false;
        }
        let Ok(metadata) = artifact.metadata() else {
            return false;
        };
        let Some(canonical_artifact) = artifact.canonicalize().ok() else {
            return false;
        };
        metadata.is_file()
            && metadata.len() == recorded_size
            && canonical_artifact.starts_with(&canonical_root)
    })
}

fn safe_relative_path(relative: &str) -> bool {
    let path = Path::new(relative);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn marker_spec(component: &str) -> Option<(&'static str, &'static [&'static str])> {
    const EMBEDDER: &[&str] = &[
        "1_Pooling/config.json",
        "config.json",
        "config_sentence_transformers.json",
        "generation_config.json",
        "merges.txt",
        "model.safetensors",
        "modules.json",
        "tokenizer.json",
        "tokenizer_config.json",
        "vocab.json",
    ];
    const RERANKER: &[&str] = &[
        "added_tokens.json",
        "config.json",
        "generation_config.json",
        "merges.txt",
        "model.safetensors",
        "modeling.py",
        "special_tokens_map.json",
        "tokenizer.json",
        "tokenizer_config.json",
        "vocab.json",
    ];
    const STT: &[&str] = &[
        "added_tokens.json",
        "config.json",
        "generation_config.json",
        "merges.txt",
        "model.safetensors",
        "normalizer.json",
        "preprocessor_config.json",
        "special_tokens_map.json",
        "tokenizer.json",
        "tokenizer_config.json",
        "vocab.json",
    ];
    const TTS: &[&str] = &[
        "config.json",
        "generation_config.json",
        "merges.txt",
        "model.safetensors",
        "preprocessor_config.json",
        "speech_tokenizer/config.json",
        "speech_tokenizer/configuration.json",
        "speech_tokenizer/model.safetensors",
        "speech_tokenizer/preprocessor_config.json",
        "tokenizer_config.json",
        "vocab.json",
    ];
    const FILE: &[&str] = &["."];
    match component {
        "embedder" => Some((
            "1e6298ca5951abf6efcb7d47a2288c0e362e45889fd6ab4630f93e82181e8b8b",
            EMBEDDER,
        )),
        "reranker" => Some((
            "22e050d32cabd283ac3a66e087f7630134563db52d7bbe3dc151df468fc4a875",
            RERANKER,
        )),
        "stt-model" => Some((
            "b84ca8daa2a4faf65f882bf08b3fc15718d5f42d4e2305388054c93430751a19",
            STT,
        )),
        "tts-model" => Some((
            "69a233c62e435a58bdcdfff28255189064410d95d2a65dda7f569113903f53f5",
            TTS,
        )),
        "tts-gm-voice" => Some((
            "ec5709f6e43bd96fa9df54e77c2febcc5dd189d1378fe060d4a431c85d0c219a",
            FILE,
        )),
        "tts-male-voice" => Some((
            "cc57f500ae418a80a47829d325d125a537393aafdb369b28fe9044671f89329d",
            FILE,
        )),
        "tts-female-voice" => Some((
            "510a03a2d85cfe194b8822c330a03aa4474e114ac0c5ee3836b00336b8dc7392",
            FILE,
        )),
        "image-diffusion" => Some((
            "0bceaa8033bf39a8a126a3ea06a9e66e2c0cd3f72f00072b18f2bfd8d4bc965b",
            FILE,
        )),
        "image-text-encoder" => Some((
            "d16374ca413a9fed1786787955d7f5a1fa185d0bd9530d18eaa19b829ee067e8",
            FILE,
        )),
        "image-vae" => Some((
            "60a649d1bb7e37e5a570412c108bece623cbb79aac41cfd3e52ea80bed5364cf",
            FILE,
        )),
        _ => None,
    }
}

fn default_spawn_dir() -> String {
    resolve_sidecar_dir()
        .unwrap_or_else(|| PathBuf::from("sidecar"))
        .to_string_lossy()
        .into_owned()
}

fn resolve_sidecar_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));

    let source_checkout = Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("sidecar"),
    );
    let candidates = [
        cwd.as_ref().map(|p| p.join("sidecar")),
        cwd,
        exe_dir.as_ref().map(|p| p.join("sidecar")),
        exe_dir
            .as_ref()
            .map(|p| p.join("resources").join("sidecar")),
        exe_dir
            .as_ref()
            .and_then(|p| p.parent().map(|parent| parent.join("sidecar"))),
        source_checkout,
        exe_dir,
    ];

    candidates
        .into_iter()
        .flatten()
        .find(|dir| dir.join(DEFAULT_SCRIPT).is_file())
}

/// The pure readiness state machine, decoupled from any real process / network
/// so it can be unit-tested with a stubbed health check.
///
/// `now`/`health` are injected. `tick` advances the state given the current
/// time and the latest health-probe result.
#[derive(Debug)]
pub struct StateMachine {
    state: SidecarState,
    started_at: Option<Instant>,
    timeout: Duration,
}

impl StateMachine {
    /// New machine in [`SidecarState::Disabled`].
    pub fn new(timeout: Duration) -> Self {
        StateMachine {
            state: SidecarState::Disabled,
            started_at: None,
            timeout,
        }
    }

    /// Current state.
    pub fn state(&self) -> SidecarState {
        self.state
    }

    /// Elapsed time since the current start attempt began.
    pub fn started_elapsed(&self) -> Option<Duration> {
        self.started_at.map(|started_at| started_at.elapsed())
    }

    /// Configured readiness timeout.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Whether the sidecar is ready to take requests.
    pub fn is_ready(&self) -> bool {
        self.state == SidecarState::Ready
    }

    /// Mark spawn success; transition to [`SidecarState::Starting`].
    pub fn on_spawned(&mut self, now: Instant) {
        self.state = SidecarState::Starting;
        self.started_at = Some(now);
    }

    /// Mark spawn failure; transition to [`SidecarState::Failed`].
    pub fn on_spawn_failed(&mut self) {
        self.state = SidecarState::Failed;
        self.started_at = None;
    }

    /// Feed a health-probe outcome while `Starting`. Returns the new state.
    ///
    /// - `healthy == true` -> [`SidecarState::Ready`].
    /// - else, if elapsed >= timeout -> [`SidecarState::Failed`].
    /// - else stay [`SidecarState::Starting`].
    pub fn on_health(&mut self, now: Instant, healthy: bool) -> SidecarState {
        if self.state != SidecarState::Starting {
            return self.state;
        }
        if healthy {
            self.state = SidecarState::Ready;
            return self.state;
        }
        if let Some(start) = self.started_at {
            if now.duration_since(start) >= self.timeout {
                self.state = SidecarState::Failed;
            }
        }
        self.state
    }

    /// Transition to [`SidecarState::Disabled`] (shutdown).
    pub fn on_shutdown(&mut self) {
        self.state = SidecarState::Disabled;
        self.started_at = None;
    }
}

/// The live sidecar manager. Spawn-once is guarded by an internal `OnceCell`;
/// the process handle + state live behind a `Mutex` so readiness can be queried
/// and shutdown can kill the tree from anywhere.
pub struct Sidecar {
    cfg: SidecarConfig,
    http: reqwest::Client,
    inner: OnceCell<Mutex<Inner>>,
    env_overrides: Mutex<Vec<(String, String)>>,
}

struct Inner {
    sm: StateMachine,
    /// Owned child + its process-tree killer. `None` once shut down or if spawn
    /// failed.
    child: Option<tokio::process::Child>,
    tree: Option<ProcessTree>,
    last_error: Option<String>,
}

impl Sidecar {
    /// Build a manager from [`SidecarConfig`].
    pub fn new(cfg: SidecarConfig) -> Self {
        let http = reqwest::Client::builder()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Sidecar {
            cfg,
            http,
            inner: OnceCell::new(),
            env_overrides: Mutex::new(Vec::new()),
        }
    }

    /// Build from environment defaults.
    pub fn from_env() -> Self {
        Self::new(SidecarConfig::from_env())
    }

    /// Whether setup proved that managed local speech-to-text can be started.
    pub fn stt_available(&self) -> bool {
        self.cfg.stt_available
    }

    /// Whether local speech-to-text is both installed and enabled for this process.
    pub fn stt_enabled(&self) -> bool {
        self.stt_available() && self.env_enabled("STT_ENABLED")
    }

    /// Whether setup proved that managed local TTS can be started.
    pub fn tts_available(&self) -> bool {
        self.cfg.tts_available
    }

    /// Whether setup proved that managed local image generation can be started.
    pub fn image_available(&self) -> bool {
        self.cfg.image_available
    }

    /// Override one spawn environment variable for future starts.
    pub fn set_env(&self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let mut value = value.into();
        if (key == "STT_ENABLED" && !self.stt_available())
            || (key == "TTS_ENABLED" && !self.tts_available())
            || (key == "IMAGE_ENABLED" && !self.image_available())
        {
            value = "0".to_string();
        }
        let mut overrides = self.env_overrides.lock().unwrap();
        if let Some((_, current)) = overrides.iter_mut().find(|(k, _)| k == &key) {
            *current = value;
        } else {
            overrides.push((key, value));
        }
    }

    fn effective_envs(&self) -> Vec<(String, String)> {
        let mut envs = self.cfg.envs.clone();
        envs.extend(self.env_overrides.lock().unwrap().iter().cloned());
        envs
    }

    fn env_enabled(&self, key: &str) -> bool {
        self.effective_envs()
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim() == "1")
            .unwrap_or(false)
    }

    fn inner(&self) -> &Mutex<Inner> {
        self.inner.get_or_init(|| {
            Mutex::new(Inner {
                sm: StateMachine::new(self.cfg.ready_timeout),
                child: None,
                tree: None,
                last_error: None,
            })
        })
    }

    /// Current state.
    pub fn state(&self) -> SidecarState {
        let mut g = self.inner().lock().unwrap();
        let _ = reap_exited_child(&mut g);
        g.sm.state()
    }

    /// Whether the sidecar is ready.
    pub fn is_ready(&self) -> bool {
        let mut g = self.inner().lock().unwrap();
        let _ = reap_exited_child(&mut g);
        g.sm.is_ready()
    }

    /// Current read-only process/readiness snapshot.
    pub fn snapshot(&self) -> SidecarSnapshot {
        let mut g = self.inner().lock().unwrap();
        let _ = reap_exited_child(&mut g);
        SidecarSnapshot {
            state: g.sm.state(),
            ready: g.sm.is_ready(),
            pid: g.child.as_ref().and_then(tokio::process::Child::id),
            base_url: self.cfg.base_url.clone(),
            started_elapsed: g.sm.started_elapsed(),
            ready_timeout: g.sm.timeout(),
            error: g.last_error.clone(),
        }
    }

    /// Ensure the sidecar is started and become-ready, returning once it is
    /// [`SidecarState::Ready`] or erroring on spawn failure / timeout.
    ///
    /// `enabled` reflects `runtime_settings.tts_enabled`: when false this is a
    /// clean [`SidecarError::Disabled`] no-op and no process is launched. Idempotent:
    /// once `Ready`, returns immediately; a concurrent caller during `Starting`
    /// joins the same readiness wait.
    pub async fn ensure_started(&self, enabled: bool) -> Result<(), SidecarError> {
        if !enabled {
            return Err(SidecarError::Disabled);
        }
        let envs = self.effective_envs();
        let component_keys = [
            "EMBEDDER_ENABLED",
            "RERANKER_ENABLED",
            "STT_ENABLED",
            "TTS_ENABLED",
            "IMAGE_ENABLED",
        ];
        let has_component_configuration = envs
            .iter()
            .any(|(key, _)| component_keys.contains(&key.as_str()));
        let has_enabled_component = component_keys.iter().any(|key| {
            envs.iter()
                .rev()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.trim() == "1")
                .unwrap_or(false)
        });
        if has_component_configuration && !has_enabled_component {
            return Err(SidecarError::Disabled);
        }

        // Fast path / spawn-once decision under the lock.
        {
            let mut g = self.inner().lock().unwrap();
            if let Some(message) = reap_exited_child(&mut g) {
                return Err(SidecarError::Exited(message));
            }
            match g.sm.state() {
                SidecarState::Ready => return Ok(()),
                SidecarState::Disabled | SidecarState::Failed => {
                    // (Re)spawn from Disabled. From Failed we also retry.
                    match self.spawn() {
                        Ok((child, tree)) => {
                            g.child = Some(child);
                            g.tree = Some(tree);
                            g.last_error = None;
                            g.sm.on_spawned(Instant::now());
                        }
                        Err(e) => {
                            g.sm.on_spawn_failed();
                            g.last_error = Some(e.clone());
                            return Err(SidecarError::Spawn(e));
                        }
                    }
                }
                SidecarState::Starting => { /* another caller is starting it; just wait */ }
            }
        }

        // Poll health until Ready / Failed.
        loop {
            if let Some(message) = {
                let mut g = self.inner().lock().unwrap();
                reap_exited_child(&mut g)
            } {
                return Err(SidecarError::Exited(message));
            }
            let healthy = self.probe_health().await;
            let new_state = {
                let mut g = self.inner().lock().unwrap();
                g.sm.on_health(Instant::now(), healthy)
            };
            match new_state {
                SidecarState::Ready => return Ok(()),
                SidecarState::Failed => {
                    let message = format!(
                        "sidecar did not become ready within {:?}",
                        self.cfg.ready_timeout
                    );
                    let mut g = self.inner().lock().unwrap();
                    if g.last_error.is_none() {
                        g.last_error = Some(message);
                    }
                    return Err(SidecarError::Timeout(self.cfg.ready_timeout));
                }
                _ => tokio::time::sleep(HEALTH_POLL_INTERVAL).await,
            }
        }
    }

    /// Spawn the sidecar child + attach a process-tree killer.
    fn spawn(&self) -> Result<(tokio::process::Child, ProcessTree), String> {
        let mut cmd = tokio::process::Command::new(&self.cfg.spawn_program);
        cmd.args(&self.cfg.spawn_args)
            .current_dir(&self.cfg.spawn_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        for (k, v) in self.effective_envs() {
            cmd.env(k, v);
        }
        crate::proc::no_window(&mut cmd);

        let child = cmd.spawn().map_err(|e| e.to_string())?;
        let pid = child.id().unwrap_or(0);
        let tree = ProcessTree::attach(pid);
        Ok((child, tree))
    }

    /// Probe `GET {base}/health`; return true only when every enabled component
    /// reports `up:true`. A connection failure (sidecar not yet listening) is
    /// simply "not healthy yet".
    async fn probe_health(&self) -> bool {
        self.health_payload()
            .await
            .map(|body| self.health_payload_ready(&body))
            .unwrap_or(false)
    }

    fn health_payload_ready(&self, body: &Value) -> bool {
        let expected = [
            ("EMBEDDER_ENABLED", "embedder"),
            ("RERANKER_ENABLED", "reranker"),
            ("STT_ENABLED", "stt"),
            ("TTS_ENABLED", "tts"),
            ("IMAGE_ENABLED", "image"),
        ];
        let mut any_expected = false;
        for (env_key, health_key) in expected {
            if self.env_enabled(env_key) {
                any_expected = true;
                if !health_component_up(body, health_key) {
                    return false;
                }
            }
        }
        any_expected
    }

    /// Fetch the raw sidecar `/health` payload for status UIs.
    pub async fn health_payload(&self) -> Result<Value, String> {
        let url = format!("{}/health", self.cfg.base_url);
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            return Err(format!("health returned {}", resp.status()));
        }
        resp.json::<Value>().await.map_err(|e| e.to_string())
    }

    /// Transcribe browser-recorded audio through the managed local STT model.
    /// The sidecar accepts the original container bytes (WebM, WAV, and other
    /// formats supported by its bundled PyAV wheel).
    pub async fn transcribe(
        &self,
        audio: bytes::Bytes,
        content_type: &str,
    ) -> Result<String, SidecarError> {
        if !self.stt_enabled() {
            return Err(SidecarError::Disabled);
        }
        self.ensure_started(true).await?;
        let response = self
            .http
            .post(format!("{}/transcribe", self.cfg.base_url))
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(audio)
            .timeout(Duration::from_secs(300))
            .send()
            .await
            .map_err(|error| SidecarError::Request(error.to_string()))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| SidecarError::Request(error.to_string()))?;
        if !status.is_success() {
            let detail = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| String::from_utf8_lossy(&body).into_owned());
            return Err(SidecarError::Request(format!("HTTP {status}: {detail}")));
        }
        serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|value| value.get("text").and_then(Value::as_str).map(str::to_owned))
            .ok_or_else(|| SidecarError::InvalidResponse("missing text field".to_string()))
    }

    /// Kill the sidecar process tree and reset to [`SidecarState::Disabled`].
    /// Idempotent and best-effort.
    pub async fn shutdown(&self) {
        let (mut child, mut tree) = {
            let mut g = self.inner().lock().unwrap();
            g.sm.on_shutdown();
            g.last_error = None;
            (g.child.take(), g.tree.take())
        };
        if let Some(t) = tree.as_mut() {
            t.kill();
        }
        if let Some(c) = child.as_mut() {
            let _ = c.start_kill();
            let _ = c.wait().await;
        }
        drop(tree); // Drop closes the Job Object (KILL_ON_JOB_CLOSE).
    }
}

fn reap_exited_child(g: &mut Inner) -> Option<String> {
    let status = match g.child.as_mut() {
        Some(child) => match child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => return None,
            Err(e) => {
                let message = format!("failed to poll sidecar process: {e}");
                g.child = None;
                g.tree = None;
                g.sm.on_spawn_failed();
                g.last_error = Some(message.clone());
                return Some(message);
            }
        },
        None => return None,
    };
    let message = format!("sidecar process exited before readiness: {status}");
    g.child = None;
    g.tree = None;
    g.sm.on_spawn_failed();
    g.last_error = Some(message.clone());
    Some(message)
}

#[cfg(test)]
fn sidecar_env_enabled(cfg: &SidecarConfig, key: &str) -> bool {
    cfg.envs
        .iter()
        .rev()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.trim() == "1")
        .unwrap_or(false)
}

fn health_component_up(body: &Value, key: &str) -> bool {
    body.get(key)
        .and_then(|v| v.get("up"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

#[cfg(test)]
fn health_payload_ready(cfg: &SidecarConfig, body: &Value) -> bool {
    let expected = [
        ("EMBEDDER_ENABLED", "embedder"),
        ("RERANKER_ENABLED", "reranker"),
        ("STT_ENABLED", "stt"),
        ("TTS_ENABLED", "tts"),
        ("IMAGE_ENABLED", "image"),
    ];
    let mut any_expected = false;
    for (env_key, health_key) in expected {
        if sidecar_env_enabled(cfg, env_key) {
            any_expected = true;
            if !health_component_up(body, health_key) {
                return false;
            }
        }
    }
    any_expected
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn state_machine_disabled_to_ready() {
        let mut sm = StateMachine::new(Duration::from_secs(10));
        assert_eq!(sm.state(), SidecarState::Disabled);
        assert!(!sm.is_ready());

        let start = t0();
        sm.on_spawned(start);
        assert_eq!(sm.state(), SidecarState::Starting);

        // Not healthy yet, well within timeout -> still Starting.
        let s = sm.on_health(start + Duration::from_secs(1), false);
        assert_eq!(s, SidecarState::Starting);

        // Health passes -> Ready.
        let s = sm.on_health(start + Duration::from_secs(2), true);
        assert_eq!(s, SidecarState::Ready);
        assert!(sm.is_ready());
    }

    #[test]
    fn install_profile_requires_a_completed_fingerprinted_build() {
        assert_eq!(completed_install_profile(&json!({"profile": "Full"})), None);
        let fingerprint = "0".repeat(64);
        let state = |build_complete, executable_sha256: &str| {
            json!({
                "schema_version": 2,
                "profile": "Full",
                "build_complete": build_complete,
                "source_fingerprint": fingerprint.as_str(),
                "executable_sha256": executable_sha256,
                "web_dist_fingerprint": fingerprint.as_str(),
            })
        };
        assert_eq!(completed_install_profile(&state(false, &fingerprint)), None);
        assert_eq!(completed_install_profile(&state(true, "invalid")), None);
        assert_eq!(
            completed_install_profile(&state(true, &fingerprint)),
            Some("Full")
        );
    }

    #[test]
    fn state_machine_times_out_to_failed() {
        let mut sm = StateMachine::new(Duration::from_secs(5));
        let start = t0();
        sm.on_spawned(start);
        // Still unhealthy past the timeout -> Failed.
        let s = sm.on_health(start + Duration::from_secs(6), false);
        assert_eq!(s, SidecarState::Failed);
        assert!(!sm.is_ready());
    }

    #[test]
    fn state_machine_spawn_failure() {
        let mut sm = StateMachine::new(Duration::from_secs(5));
        sm.on_spawn_failed();
        assert_eq!(sm.state(), SidecarState::Failed);
    }

    #[test]
    fn state_machine_health_ignored_unless_starting() {
        let mut sm = StateMachine::new(Duration::from_secs(5));
        // Disabled: health is a no-op.
        let s = sm.on_health(t0(), true);
        assert_eq!(s, SidecarState::Disabled);

        // Ready stays Ready even if a later probe says unhealthy.
        let start = t0();
        sm.on_spawned(start);
        sm.on_health(start, true);
        assert_eq!(sm.state(), SidecarState::Ready);
        let s = sm.on_health(start + Duration::from_secs(100), false);
        assert_eq!(s, SidecarState::Ready);
    }

    #[test]
    fn health_payload_requires_enabled_components() {
        let mut cfg = SidecarConfig::from_env();
        cfg.envs
            .push(("EMBEDDER_ENABLED".to_string(), "1".to_string()));
        cfg.envs
            .push(("RERANKER_ENABLED".to_string(), "1".to_string()));
        cfg.envs.push(("STT_ENABLED".to_string(), "1".to_string()));
        cfg.envs.push(("TTS_ENABLED".to_string(), "0".to_string()));

        assert!(health_payload_ready(
            &cfg,
            &json!({
                "embedder": {"up": true},
                "reranker": {"up": true},
                "stt": {"up": true},
                "tts": {"up": false}
            })
        ));
        assert!(!health_payload_ready(
            &cfg,
            &json!({
                "embedder": {"up": true},
                "reranker": {"up": false},
                "stt": {"up": true},
                "tts": {"up": true}
            })
        ));
        assert!(!health_payload_ready(
            &cfg,
            &json!({
                "embedder": {"up": true},
                "reranker": {"up": true},
                "stt": {"up": false}
            })
        ));
    }

    #[test]
    fn state_machine_shutdown_resets() {
        let mut sm = StateMachine::new(Duration::from_secs(5));
        let start = t0();
        sm.on_spawned(start);
        sm.on_health(start, true);
        assert!(sm.is_ready());
        sm.on_shutdown();
        assert_eq!(sm.state(), SidecarState::Disabled);
        assert!(!sm.is_ready());
    }

    #[test]
    fn state_machine_boundary_exactly_at_timeout_fails() {
        let mut sm = StateMachine::new(Duration::from_secs(5));
        let start = t0();
        sm.on_spawned(start);
        // elapsed == timeout (>=) -> Failed.
        let s = sm.on_health(start + Duration::from_secs(5), false);
        assert_eq!(s, SidecarState::Failed);
    }

    #[test]
    fn file_marker_requires_exact_manifest_inventory_and_size() {
        let directory = tempfile::tempdir().unwrap();
        let artifact = directory.path().join("model.safetensors");
        std::fs::write(&artifact, b"fixture").unwrap();
        let marker = directory.path().join("model.safetensors.gml-model.json");
        let write_marker = |manifest: &str, size: u64| {
            std::fs::write(
                &marker,
                serde_json::to_vec(&json!({
                    "schema_version": 2,
                    "component": "image-vae",
                    "manifest_sha256": manifest,
                    "artifacts": {".": {
                        "size": size,
                        "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
                    }}
                }))
                .unwrap(),
            )
            .unwrap();
        };

        write_marker(
            "60a649d1bb7e37e5a570412c108bece623cbb79aac41cfd3e52ea80bed5364cf",
            7,
        );
        assert!(marker_ready(
            &marker,
            "image-vae",
            directory.path(),
            Some(&artifact)
        ));

        write_marker(
            "60a649d1bb7e37e5a570412c108bece623cbb79aac41cfd3e52ea80bed5364cf",
            8,
        );
        assert!(!marker_ready(
            &marker,
            "image-vae",
            directory.path(),
            Some(&artifact)
        ));
        write_marker(
            "0000000000000000000000000000000000000000000000000000000000000000",
            7,
        );
        assert!(!marker_ready(
            &marker,
            "image-vae",
            directory.path(),
            Some(&artifact)
        ));
    }

    #[tokio::test]
    async fn ensure_started_disabled_is_clean_noop() {
        let cfg = SidecarConfig {
            base_url: "http://127.0.0.1:59999".to_string(),
            spawn_program: "definitely-not-a-real-binary-xyz".to_string(),
            spawn_args: vec![],
            spawn_dir: ".".to_string(),
            ready_timeout: Duration::from_millis(10),
            envs: vec![],
            stt_available: false,
            tts_available: false,
            image_available: false,
        };
        let s = Sidecar::new(cfg);
        let err = s.ensure_started(false).await.unwrap_err();
        assert!(matches!(err, SidecarError::Disabled));
        // No process launched; state untouched (Disabled).
        assert_eq!(s.state(), SidecarState::Disabled);
        assert!(!s.is_ready());
    }

    #[tokio::test]
    async fn unavailable_component_toggle_cannot_spawn_a_runtime() {
        let cfg = SidecarConfig {
            base_url: "http://127.0.0.1:59996".to_string(),
            spawn_program: "definitely-not-a-real-binary-xyz".to_string(),
            spawn_args: vec![],
            spawn_dir: ".".to_string(),
            ready_timeout: Duration::from_millis(10),
            envs: vec![("STT_ENABLED".to_string(), "0".to_string())],
            stt_available: false,
            tts_available: false,
            image_available: false,
        };
        let sidecar = Sidecar::new(cfg);
        sidecar.set_env("STT_ENABLED", "1");

        let error = sidecar.ensure_started(true).await.unwrap_err();

        assert!(matches!(error, SidecarError::Disabled));
        assert_eq!(sidecar.state(), SidecarState::Disabled);
    }

    #[tokio::test]
    async fn ensure_started_spawn_failure_is_reported() {
        let cfg = SidecarConfig {
            base_url: "http://127.0.0.1:59998".to_string(),
            spawn_program: "definitely-not-a-real-binary-xyz".to_string(),
            spawn_args: vec![],
            spawn_dir: ".".to_string(),
            ready_timeout: Duration::from_millis(50),
            envs: vec![],
            stt_available: false,
            tts_available: false,
            image_available: false,
        };
        let s = Sidecar::new(cfg);
        let err = s.ensure_started(true).await.unwrap_err();
        assert!(matches!(err, SidecarError::Spawn(_)));
        assert_eq!(s.state(), SidecarState::Failed);
    }

    #[tokio::test]
    async fn ensure_started_reports_fast_child_exit() {
        let (program, args) = if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/C".to_string(), "exit 7".to_string()],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), "exit 7".to_string()],
            )
        };
        let cfg = SidecarConfig {
            base_url: "http://127.0.0.1:59997".to_string(),
            spawn_program: program,
            spawn_args: args,
            spawn_dir: ".".to_string(),
            ready_timeout: Duration::from_secs(5),
            envs: vec![],
            stt_available: false,
            tts_available: false,
            image_available: false,
        };
        let s = Sidecar::new(cfg);
        let err = s.ensure_started(true).await.unwrap_err();
        assert!(matches!(err, SidecarError::Exited(_)));
        let snapshot = s.snapshot();
        assert_eq!(snapshot.state, SidecarState::Failed);
        assert_eq!(snapshot.pid, None);
        assert!(snapshot
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("exited"));
    }
}
