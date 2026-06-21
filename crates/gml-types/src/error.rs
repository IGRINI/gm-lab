//! Workspace-level error types. Kept minimal; individual crates own their own
//! richer error enums. These cover only failures that surface at the shared
//! value-type boundary.

use thiserror::Error;

/// Returned by [`crate::Role::parse`] when a role string is not one of the three
/// canonical values defined in Python `config.py` (`ROLE_GM`/`ROLE_NPC`/`ROLE_COMPACT`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown role string: {0:?} (expected \"gm\", \"npc\", or \"compact\")")]
pub struct ParseRoleError(pub String);

/// General-purpose error for the shared types layer.
#[derive(Debug, Error)]
pub enum TypesError {
    #[error(transparent)]
    Role(#[from] ParseRoleError),

    /// A serde_json (de)serialization failure on a shared shape.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}
