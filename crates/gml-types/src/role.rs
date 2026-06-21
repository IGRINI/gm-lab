//! Reasoning roles — the single source of truth for role strings.
//!
//! Python origin: `config.py`
//! ```python
//! ROLE_GM = "gm"
//! ROLE_NPC = "npc"
//! ROLE_COMPACT = "compact"
//! REASONING_ROLES = (ROLE_GM, ROLE_NPC, ROLE_COMPACT)
//! ```
//! All string values of roles come from here. Renaming here changes everything:
//! runtime_settings (setting keys, validator), clients, orchestrator, agents.
//! These strings must never be hardcoded anywhere else.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ParseRoleError;

/// A reasoning role. Serializes to / parses from exactly `"gm"`, `"npc"`,
/// `"compact"` — matching `config.ROLE_GM` / `ROLE_NPC` / `ROLE_COMPACT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    /// `config.ROLE_GM == "gm"`. The game master with tools.
    Gm,
    /// `config.ROLE_NPC == "npc"`. The NPC sub-agent (JSON output).
    Npc,
    /// `config.ROLE_COMPACT == "compact"`. The summarization/compaction role.
    Compact,
}

/// The reasoning roles, in declaration order — port of
/// `config.REASONING_ROLES = (ROLE_GM, ROLE_NPC, ROLE_COMPACT)`.
pub const REASONING_ROLES: [Role; 3] = [Role::Gm, Role::Npc, Role::Compact];

impl Role {
    /// The canonical string for this role (`"gm"` / `"npc"` / `"compact"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Role::Gm => "gm",
            Role::Npc => "npc",
            Role::Compact => "compact",
        }
    }

    /// Parse a role string. Mirrors the implicit Python contract that only the
    /// three canonical values are valid role strings.
    pub fn parse(s: &str) -> Result<Self, ParseRoleError> {
        match s {
            "gm" => Ok(Role::Gm),
            "npc" => Ok(Role::Npc),
            "compact" => Ok(Role::Compact),
            other => Err(ParseRoleError(other.to_string())),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Role {
    type Err = ParseRoleError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Role::parse(s)
    }
}

impl Serialize for Role {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Role::parse(&s).map_err(serde::de::Error::custom)
    }
}
