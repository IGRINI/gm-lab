//! Per-client session identity (`session_id` / `thread_id` / `installation_id`).
//!
//! Faithful port of the identity handling in `codex_client.py`:
//! ```python
//! self._session_id = str(uuid.uuid4())
//! self._thread_id = str(uuid.uuid4())
//! self._installation_id = str(uuid.uuid4())
//!
//! def set_session_identity(self, session_id="", thread_id=""):
//!     if (session_id or "").strip():
//!         self._session_id = session_id.strip()
//!     if (thread_id or "").strip():
//!         self._thread_id = thread_id.strip()
//! ```
//! and the prompt-cache-key rule:
//! ```python
//! payload["prompt_cache_key"] = config.CODEX_PROMPT_CACHE_KEY or self._thread_id
//! ```
//!
//! The Codex backend lives in `gml-codex`, but the identity machinery is generic
//! and is exposed here so that crate can reuse the exact, restorable behaviour
//! (PORT_PLAN §4.5: "uuid4 per client, persisted and restored via
//! `set_session_identity`"). The plain OpenAI-compatible / mock backends do not
//! key the cache on a thread id, so they leave [`Backend::set_session_identity`]
//! as the default no-op.

use std::sync::Mutex;

/// A restorable per-client identity. Each field is a fresh uuid4 at construction
/// and may be overridden later (e.g. from a persisted snapshot).
///
/// Interior-mutable so it can sit behind a shared `&self` client, matching the
/// Python instance-attribute mutation.
#[derive(Debug)]
pub struct SessionIdentity {
    session_id: Mutex<String>,
    thread_id: Mutex<String>,
    installation_id: Mutex<String>,
}

impl Default for SessionIdentity {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionIdentity {
    /// Construct with three fresh uuid4 values (Python `__init__`).
    pub fn new() -> Self {
        SessionIdentity {
            session_id: Mutex::new(new_uuid4()),
            thread_id: Mutex::new(new_uuid4()),
            installation_id: Mutex::new(new_uuid4()),
        }
    }

    /// `session_id` property.
    pub fn session_id(&self) -> String {
        self.session_id.lock().expect("session_id lock").clone()
    }

    /// `thread_id` property.
    pub fn thread_id(&self) -> String {
        self.thread_id.lock().expect("thread_id lock").clone()
    }

    /// `installation_id` (not exposed as a Python property, but used in headers).
    pub fn installation_id(&self) -> String {
        self.installation_id
            .lock()
            .expect("installation_id lock")
            .clone()
    }

    /// `set_session_identity(session_id, thread_id)` — override each id only when
    /// the supplied value is non-empty after trimming. `None` / empty strings
    /// leave the existing id untouched (restorable).
    pub fn set(&self, session_id: Option<&str>, thread_id: Option<&str>) {
        if let Some(s) = session_id {
            let s = s.trim();
            if !s.is_empty() {
                *self.session_id.lock().expect("session_id lock") = s.to_string();
            }
        }
        if let Some(t) = thread_id {
            let t = t.trim();
            if !t.is_empty() {
                *self.thread_id.lock().expect("thread_id lock") = t.to_string();
            }
        }
    }

    /// `prompt_cache_key`: `codex_prompt_cache_key or thread_id`. The configured
    /// key wins when non-empty; otherwise the per-client `thread_id` is used.
    pub fn prompt_cache_key(&self, configured: &str) -> String {
        if !configured.is_empty() {
            configured.to_string()
        } else {
            self.thread_id()
        }
    }
}

/// `str(uuid.uuid4())` — a random v4 UUID in canonical lowercase form.
fn new_uuid4() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_ids_are_distinct_uuids() {
        let id = SessionIdentity::new();
        let s = id.session_id();
        let t = id.thread_id();
        let i = id.installation_id();
        assert_ne!(s, t);
        assert_ne!(t, i);
        // canonical uuid4: 36 chars, 4 hyphens
        assert_eq!(s.len(), 36);
        assert_eq!(s.matches('-').count(), 4);
        assert!(uuid::Uuid::parse_str(&s).is_ok());
    }

    #[test]
    fn set_overrides_only_nonempty() {
        let id = SessionIdentity::new();
        let orig_session = id.session_id();
        let orig_thread = id.thread_id();

        // empty / whitespace -> unchanged
        id.set(Some(""), Some("   "));
        assert_eq!(id.session_id(), orig_session);
        assert_eq!(id.thread_id(), orig_thread);

        // None -> unchanged
        id.set(None, None);
        assert_eq!(id.session_id(), orig_session);

        // restore from a snapshot
        id.set(Some(" sess-123 "), Some(" thr-456 "));
        assert_eq!(id.session_id(), "sess-123");
        assert_eq!(id.thread_id(), "thr-456");
    }

    #[test]
    fn set_each_independently() {
        let id = SessionIdentity::new();
        let orig_thread = id.thread_id();
        id.set(Some("only-session"), None);
        assert_eq!(id.session_id(), "only-session");
        assert_eq!(id.thread_id(), orig_thread);
    }

    #[test]
    fn prompt_cache_key_rule() {
        let id = SessionIdentity::new();
        // configured non-empty wins
        assert_eq!(id.prompt_cache_key("fixed-key"), "fixed-key");
        // empty configured -> thread_id
        assert_eq!(id.prompt_cache_key(""), id.thread_id());
    }
}
