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
//! A configured prompt-cache key is treated as a stable namespace, while the
//! restorable thread id remains the cache scope. This guarantees that model
//! changes and history resets can rotate the effective provider cache key.
//!
//! Provider backends live in connector crates, while the identity machinery is
//! generic so cache-aware connectors can share the same restorable behaviour
//! (PORT_PLAN §4.5: "uuid4 per client, persisted and restored via
//! `set_session_identity`"). Backends without provider cache identity keep the
//! default no-op.

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

    /// Start a fresh provider cache scope while keeping the installation id.
    /// Connectors call this when a model change invalidates model-specific
    /// conversation or prompt-cache state.
    pub fn reset_cache_scope(&self) {
        *self.session_id.lock().expect("session_id lock") = new_uuid4();
        *self.thread_id.lock().expect("thread_id lock") = new_uuid4();
    }

    /// Build the effective provider prompt-cache key. A configured value is a
    /// namespace, not a global fixed cache id: the rotating thread scope is
    /// always included so separate histories and models cannot share state.
    pub fn prompt_cache_key(&self, configured: &str) -> String {
        let configured = configured.trim();
        let thread_id = self.thread_id();
        if !configured.is_empty() {
            format!("{configured}:{thread_id}")
        } else {
            thread_id
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
        let thread_id = id.thread_id();
        assert_eq!(
            id.prompt_cache_key(" fixed-key "),
            format!("fixed-key:{thread_id}")
        );
        // empty configured -> thread_id
        assert_eq!(id.prompt_cache_key(""), thread_id);
    }

    #[test]
    fn configured_cache_namespace_still_rotates_with_scope() {
        let id = SessionIdentity::new();
        let before = id.prompt_cache_key("fixed-key");

        id.reset_cache_scope();

        assert_ne!(id.prompt_cache_key("fixed-key"), before);
    }

    #[test]
    fn reset_cache_scope_rotates_session_and_thread_only() {
        let id = SessionIdentity::new();
        let session = id.session_id();
        let thread = id.thread_id();
        let installation = id.installation_id();

        id.reset_cache_scope();

        assert_ne!(id.session_id(), session);
        assert_ne!(id.thread_id(), thread);
        assert_eq!(id.installation_id(), installation);
    }
}
