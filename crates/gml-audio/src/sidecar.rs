//! TTS sidecar process manager (NEW design — no Python reference).
//!
//! PORT_PLAN §3.2 sidecar row / risk #7. Spawns the faster-qwen3-tts Python
//! sidecar once (guarded by a `OnceCell<Mutex<...>>`), polls a health endpoint
//! until ready or timeout, exposes readiness, and kills the process tree on
//! shutdown (cross-platform, via [`crate::proc`]).
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

use std::sync::Mutex;
use std::time::{Duration, Instant};

use once_cell::sync::OnceCell;

use crate::proc::ProcessTree;

/// Env var holding the spawn command line for the sidecar. Default launches the
/// faster-qwen3-tts demo server in `hf_models/faster-qwen3-tts`.
pub const SPAWN_CMD_ENV: &str = "GM_TTS_SPAWN_CMD";
/// Env var pointing at the sidecar working directory (the faster-qwen3-tts dir).
pub const SPAWN_DIR_ENV: &str = "GM_TTS_SPAWN_DIR";
/// Env var: how long (seconds) to wait for the sidecar to become healthy.
pub const READY_TIMEOUT_ENV: &str = "GM_TTS_READY_TIMEOUT";

/// Default spawn working directory (the faster-qwen3-tts repo).
pub const DEFAULT_SPAWN_DIR: &str = r"E:/gemma/gm-lab/hf_models/faster-qwen3-tts";
/// Default readiness timeout in seconds.
pub const DEFAULT_READY_TIMEOUT_SECS: u64 = 180;
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

/// Sidecar failures.
#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    /// TTS is disabled via runtime settings — a clean, expected no-op path.
    #[error("TTS disabled")]
    Disabled,
    /// The sidecar process failed to spawn.
    #[error("failed to spawn TTS sidecar: {0}")]
    Spawn(String),
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
}

impl SidecarConfig {
    /// Build the config from environment + sensible defaults.
    ///
    /// `GM_TTS_SPAWN_CMD` is split on whitespace (first token = program). If
    /// unset, the default is `python demo/server.py` run from the
    /// faster-qwen3-tts directory. The base URL comes from `GM_TTS_URL`.
    pub fn from_env() -> Self {
        let base_url = crate::tts::tts_url();
        let spawn_dir = {
            let d = std::env::var(SPAWN_DIR_ENV).unwrap_or_default();
            if d.trim().is_empty() {
                DEFAULT_SPAWN_DIR.to_string()
            } else {
                d
            }
        };
        let (spawn_program, spawn_args) = match std::env::var(SPAWN_CMD_ENV) {
            Ok(cmd) if !cmd.trim().is_empty() => {
                let mut parts = cmd.split_whitespace().map(|s| s.to_string());
                let prog = parts.next().unwrap_or_else(|| "python".to_string());
                (prog, parts.collect())
            }
            _ => (
                default_python().to_string(),
                vec!["demo/server.py".to_string()],
            ),
        };
        let ready_timeout = std::env::var(READY_TIMEOUT_ENV)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(DEFAULT_READY_TIMEOUT_SECS));

        SidecarConfig {
            base_url,
            spawn_program,
            spawn_args,
            spawn_dir,
            ready_timeout,
        }
    }
}

fn default_python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "python3"
    }
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
}

struct Inner {
    sm: StateMachine,
    /// Owned child + its process-tree killer. `None` once shut down or if spawn
    /// failed.
    child: Option<tokio::process::Child>,
    tree: Option<ProcessTree>,
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
        }
    }

    /// Build from environment defaults.
    pub fn from_env() -> Self {
        Self::new(SidecarConfig::from_env())
    }

    fn inner(&self) -> &Mutex<Inner> {
        self.inner.get_or_init(|| {
            Mutex::new(Inner {
                sm: StateMachine::new(self.cfg.ready_timeout),
                child: None,
                tree: None,
            })
        })
    }

    /// Current state.
    pub fn state(&self) -> SidecarState {
        self.inner().lock().unwrap().sm.state()
    }

    /// Whether the sidecar is ready.
    pub fn is_ready(&self) -> bool {
        self.inner().lock().unwrap().sm.is_ready()
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
            match g.sm.state() {
                SidecarState::Ready => return Ok(()),
                SidecarState::Disabled | SidecarState::Failed => {
                    // (Re)spawn from Disabled. From Failed we also retry.
                    match self.spawn() {
                        Ok((child, tree)) => {
                            g.child = Some(child);
                            g.tree = Some(tree);
                            g.sm.on_spawned(Instant::now());
                        }
                        Err(e) => {
                            g.sm.on_spawn_failed();
                            return Err(SidecarError::Spawn(e));
                        }
                    }
                }
                SidecarState::Starting => { /* another caller is starting it; just wait */ }
            }
        }

        // Poll health until Ready / Failed.
        loop {
            let healthy = self.probe_health().await;
            let new_state = {
                let mut g = self.inner().lock().unwrap();
                g.sm.on_health(Instant::now(), healthy)
            };
            match new_state {
                SidecarState::Ready => return Ok(()),
                SidecarState::Failed => return Err(SidecarError::Timeout(self.cfg.ready_timeout)),
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
        crate::proc::no_window(&mut cmd);

        let child = cmd.spawn().map_err(|e| e.to_string())?;
        let pid = child.id().unwrap_or(0);
        let tree = ProcessTree::attach(pid);
        Ok((child, tree))
    }

    /// Probe `GET {base}/health`; return true on a 2xx response. A connection
    /// failure (sidecar not yet listening) is simply "not healthy yet".
    async fn probe_health(&self) -> bool {
        let url = format!("{}/health", self.cfg.base_url);
        match self
            .http
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    /// Kill the sidecar process tree and reset to [`SidecarState::Disabled`].
    /// Idempotent and best-effort.
    pub async fn shutdown(&self) {
        let (mut child, mut tree) = {
            let mut g = self.inner().lock().unwrap();
            g.sm.on_shutdown();
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

#[cfg(test)]
mod tests {
    use super::*;

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
        };
        let s = Sidecar::new(cfg);
        let err = s.ensure_started(true).await.unwrap_err();
        assert!(matches!(err, SidecarError::Spawn(_)));
        assert_eq!(s.state(), SidecarState::Failed);
    }
}
