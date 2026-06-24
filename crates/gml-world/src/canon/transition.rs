//! `Transition` — a first-class directed edge between two [`super::Place`]s.
//!
//! TZ §6.5: an exit must be a real entity, not a substring of prose. A
//! `Transition` is *directed* (from → to). A two-way path is modelled as two
//! directed transitions (or, later, one edge with two explicit sides) — never
//! by implicit magic — so one-way drops, doors locked from one side, and real
//! pathfinding all work.
//!
//! Phase 1 derives one `Transition` per `SceneExit`. The target is left as a
//! *shell* (`to_place` empty) because the legacy exit only carries a freetext
//! `destination` label, which we preserve in [`Transition::destination_hint`]
//! for later resolution (P3 lazy expansion).

use serde::{Deserialize, Serialize};

use super::Provenance;

fn default_true() -> bool {
    true
}

/// A directed edge between places. Empty `to_place` means "no canonical target
/// resolved yet" (a dangling/shell edge); the freetext `destination_hint`
/// records what the seed/LLM called the destination so it can be resolved or
/// generated later without losing information.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Transition {
    /// Stable, unique id. In Phase 1 this normally equals the originating
    /// `SceneExit.exit_id`; if a seed produced colliding exit ids it is
    /// suffixed (`id_2`, `id_3`, …) so the canon graph keeps unique edge ids.
    pub transition_id: String,
    /// The originating `SceneExit.exit_id` verbatim (may repeat across edges
    /// when a seed had duplicate exit ids). The view projection restores the
    /// exit's id from this, so the scene round-trips byte-for-byte even when
    /// `transition_id` was uniquified.
    #[serde(default)]
    pub source_exit_id: String,
    /// Source place id.
    pub from_place: String,
    /// Target place id, or empty when not yet canonical (shell/dangling).
    #[serde(default)]
    pub to_place: String,
    /// Freetext destination label carried over from `SceneExit.destination`
    /// (e.g. "северные ворота") — the seed for later target resolution.
    #[serde(default)]
    pub destination_hint: String,
    /// Player-facing label (`SceneExit.name`).
    #[serde(default)]
    pub label: String,
    /// Edge type: `door` / `road` / `stairs` / `tunnel` / `path` / `portal`.
    /// Empty in Phase 1 (the legacy exit carries no type).
    #[serde(default)]
    pub kind: String,
    /// Whether the exit is currently visible to the player.
    #[serde(default = "default_true")]
    pub visible: bool,
    /// Whether the exit can currently be traversed. Phase 1 derives this from
    /// the legacy `blocked_by` (empty ⇒ passable).
    #[serde(default = "default_true")]
    pub passable: bool,
    /// Requirements to pass: key, strength, permission, time-of-day, revealed
    /// secret, … Empty in Phase 1.
    #[serde(default)]
    pub conditions: Vec<String>,
    /// Active blocker (guard, rubble, lock, barrier) — mirrors
    /// `SceneExit.blocked_by`.
    #[serde(default)]
    pub blocked_by: String,
    /// Time cost in minutes to traverse, 0 if unset.
    #[serde(default)]
    pub time_cost: i64,
    /// Risk / encounter policy note.
    #[serde(default)]
    pub risk: String,
    /// Why/when/how this transition was created (TZ §6.1 provenance).
    #[serde(default)]
    pub provenance: Provenance,
}

impl Transition {
    /// Whether the target place has not yet been canonicalised.
    pub fn is_shell(&self) -> bool {
        self.to_place.is_empty()
    }

    /// Whether the edge resolves to a concrete target place.
    pub fn has_target(&self) -> bool {
        !self.to_place.is_empty()
    }
}
