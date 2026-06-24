//! Persisted per-install `x-codex-installation-id`.
//!
//! PORT_PLAN risk #9: the Python `CodexClient` generates `installation_id` as a
//! fresh uuid4 *per process*, which churns the header on every restart. The plan
//! decisive default is to "persist `installation_id` per-install" — closer to the
//! real Codex CLI, which keeps a stable installation id. We implement the
//! persisted variant here (noted as a deviation in the port followups).
//!
//! The id is stored as a small text file next to the Codex credential file
//! (same app-data dir), honoring the same `GM_CODEX_CREDENTIAL_PATH` override so
//! it travels with the credential. The value is read once and memoized for the
//! process lifetime.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::oauth::credential_path;

static INSTALL_ID: OnceLock<String> = OnceLock::new();

/// The path to the installation-id file (`codex-installation-id` next to the
/// credential file).
fn install_id_path() -> PathBuf {
    let cred = credential_path();
    let dir = cred.parent().map(|p| p.to_path_buf()).unwrap_or_default();
    dir.join("codex-installation-id")
}

/// The stable per-install `x-codex-installation-id`.
///
/// Reads (or creates) a persisted uuid file. On any IO failure it falls back to
/// a process-lifetime uuid (memoized) so the header is at least stable within
/// the run — matching the Python per-process behavior in the degraded case.
pub fn installation_id() -> String {
    INSTALL_ID
        .get_or_init(|| load_or_create().unwrap_or_else(|| uuid::Uuid::new_v4().to_string()))
        .clone()
}

fn load_or_create() -> Option<String> {
    let path = install_id_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(&path, &id) {
        Ok(()) => Some(id),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Both tests read `installation_id()` / `load_or_create()`, which resolve the
    // process-global `GM_CODEX_CREDENTIAL_PATH` env var and a shared install-id
    // file. Running them concurrently lets one test's set/remove of that env var
    // race the other's reads, so serialize them on a shared guard.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn installation_id_is_stable_within_process() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        let a = installation_id();
        let b = installation_id();
        assert_eq!(a, b);
        assert_eq!(a.len(), 36);
        assert!(uuid::Uuid::parse_str(&a).is_ok());
    }

    #[test]
    fn persisted_id_round_trips_via_env_override() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|p| p.into_inner());
        // Save and restore any pre-existing override so we leave the process env
        // exactly as we found it.
        let prev = std::env::var_os("GM_CODEX_CREDENTIAL_PATH");
        // Point the credential path at a temp file so the install-id file lands
        // in a temp dir; verify a written id is read back.
        let dir = tempfile::tempdir().unwrap();
        let cred = dir.path().join("codex-oauth.json");
        std::env::set_var("GM_CODEX_CREDENTIAL_PATH", &cred);
        // direct file-level check (installation_id() memoizes globally, so we
        // exercise load_or_create twice via the path helper instead).
        let first = load_or_create().unwrap();
        let second = load_or_create().unwrap();
        match prev {
            Some(v) => std::env::set_var("GM_CODEX_CREDENTIAL_PATH", v),
            None => std::env::remove_var("GM_CODEX_CREDENTIAL_PATH"),
        }
        assert_eq!(first, second, "second read returns the persisted id");
    }
}
