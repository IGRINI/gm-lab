//! Unified inference sidecar process manager (NEW design — no Python reference).
//!
//! PORT_PLAN §3.2 sidecar row / risk #7. Spawns the unified `serve.py` sidecar
//! (one process hosting embeddings + rerank + TTS) once (guarded by a
//! `OnceCell<Mutex<...>>`), polls a health endpoint until ready or timeout,
//! exposes readiness, and kills the process tree on shutdown (cross-platform,
//! via [`crate::proc`]).
//!
//! The app must run FULLY without TTS: if TTS is disabled
//! (`runtime_settings.tts_enabled == false`) or the sidecar can't start, every
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

use std::path::{Path, PathBuf};
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

/// Default sidecar script, relative to the resolved sidecar directory.
pub const DEFAULT_SCRIPT: &str = "serve.py";
/// Default HF cache home (where the TTS weights live, on E:).
pub const DEFAULT_HF_HOME: &str = r"E:/gemma/gm-lab/hf_models/.hf-home";
/// Default TTS asset dir (voice refs + local `qwen17b_base` model).
pub const DEFAULT_TTS_HOME: &str = r"E:/gemma/gm-lab/hf_models/faster-qwen3-tts";
/// Default readiness timeout in seconds. The TTS 1.7B load + CUDA-graph capture
/// is ~120s; allow margin for a cold model download.
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
    #[error("TTS disabled")]
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
}

impl SidecarConfig {
    /// Build the config from environment + sensible defaults.
    ///
    /// `GM_TTS_SPAWN_CMD` is split on whitespace (first token = program). If
    /// unset, the manager uses `PYTHON`/`python`/`python3` and runs `serve.py`
    /// from the resolved sidecar directory. The base URL comes from `GM_TTS_URL`.
    pub fn from_env() -> Self {
        let base_url = crate::tts::tts_url();
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
                let prog = parts.next().unwrap_or_else(|| default_python().to_string());
                (prog, parts.collect())
            }
            _ => (
                default_python().to_string(),
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
        let envs = vec![
            ("GMLAB_SIDECAR_PORT".to_string(), port),
            ("HF_HOME".to_string(), env_or("HF_HOME", DEFAULT_HF_HOME)),
            ("TTS_HOME".to_string(), env_or("TTS_HOME", DEFAULT_TTS_HOME)),
        ];

        SidecarConfig {
            base_url,
            spawn_program,
            spawn_args,
            spawn_dir,
            ready_timeout,
            envs,
        }
    }
}

/// Read an env var, falling back to `default` when unset or blank.
fn env_or(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

fn default_python() -> String {
    let from_env = std::env::var("PYTHON").unwrap_or_default();
    if !from_env.trim().is_empty() {
        return from_env;
    }
    if cfg!(windows) {
        "python".to_string()
    } else {
        "python3".to_string()
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

    let candidates = [
        cwd.as_ref().map(|p| p.join("sidecar")),
        cwd,
        exe_dir.as_ref().map(|p| p.join("sidecar")),
        exe_dir
            .as_ref()
            .and_then(|p| p.parent().map(|parent| parent.join("sidecar"))),
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

    /// Override one spawn environment variable for future starts.
    pub fn set_env(&self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
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
        cfg.envs.push(("TTS_ENABLED".to_string(), "0".to_string()));

        assert!(health_payload_ready(
            &cfg,
            &json!({
                "embedder": {"up": true},
                "reranker": {"up": true},
                "tts": {"up": false}
            })
        ));
        assert!(!health_payload_ready(
            &cfg,
            &json!({
                "embedder": {"up": true},
                "reranker": {"up": false},
                "tts": {"up": true}
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

    #[tokio::test]
    async fn ensure_started_disabled_is_clean_noop() {
        let cfg = SidecarConfig {
            base_url: "http://127.0.0.1:59999".to_string(),
            spawn_program: "definitely-not-a-real-binary-xyz".to_string(),
            spawn_args: vec![],
            spawn_dir: ".".to_string(),
            ready_timeout: Duration::from_millis(10),
            envs: vec![],
        };
        let s = Sidecar::new(cfg);
        let err = s.ensure_started(false).await.unwrap_err();
        assert!(matches!(err, SidecarError::Disabled));
        // No process launched; state untouched (Disabled).
        assert_eq!(s.state(), SidecarState::Disabled);
        assert!(!s.is_ready());
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
