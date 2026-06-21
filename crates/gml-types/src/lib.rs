//! gml-types — shared cross-crate value types for GM-Lab.
//!
//! This is the single home for value types passed across module boundaries, so
//! there are no circular dependencies (see PORT_PLAN.md §1.2). Every type here is
//! a faithful port of a Python shape; the Python origin is cited per item.
//!
//! Dependency-light by design: only `serde`, `serde_json`, `thiserror`.

pub mod error;
pub mod event;
pub mod npc;
pub mod role;
pub mod tool;

pub use error::{ParseRoleError, TypesError};
pub use event::{Event, event_kind};
pub use npc::NpcResponse;
pub use role::{Role, REASONING_ROLES};
pub use tool::{ParsedCall, ToolExecutionResult};
