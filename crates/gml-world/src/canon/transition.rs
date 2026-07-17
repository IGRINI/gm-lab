//! `Transition` — a first-class directed edge between two [`super::Place`]s.
//!
//! TZ §6.5: an exit must be a real entity, not a substring of prose. A
//! `Transition` is *directed* (from → to). A two-way path is modelled as two
//! directed transitions sharing one explicit physical `passage_id` — never by
//! endpoint inference — so one-way drops, doors locked from one side, and real
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

/// Whether a physical passage may be traversed in one or both directions.
///
/// `Unspecified` exists solely for safe deserialization of legacy saves. It is
/// not a valid value for newly authored transitions and cannot be traversed
/// until the location creator explicitly profiles the passage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PassageDirectionality {
    #[default]
    Unspecified,
    OneWay,
    Bidirectional,
}

impl PassageDirectionality {
    pub const fn is_unspecified(&self) -> bool {
        matches!(self, Self::Unspecified)
    }

    pub const fn is_explicit(self) -> bool {
        !matches!(self, Self::Unspecified)
    }
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
    /// Stable identity of the physical passage represented by this directed
    /// edge. The two directed sides of one bidirectional passage share this id.
    /// An empty id is reserved for legacy, not-yet-profiled transitions.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub passage_id: String,
    /// Explicit physical directionality. Missing legacy data deserializes as
    /// `Unspecified` and is never treated as reciprocal by endpoint inference.
    #[serde(default, skip_serializing_if = "PassageDirectionality::is_unspecified")]
    pub directionality: PassageDirectionality,
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

    /// True only after the physical passage has an explicit identity and
    /// directionality. Legacy transitions intentionally return false.
    pub fn has_explicit_passage_profile(&self) -> bool {
        !self.passage_id.trim().is_empty() && self.directionality.is_explicit()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_transition_deserializes_without_inventing_passage_identity() {
        let transition: Transition = serde_json::from_value(serde_json::json!({
            "transition_id": "legacy_drop",
            "from_place": "cave",
            "to_place": "chasm"
        }))
        .expect("legacy transition");

        assert!(transition.passage_id.is_empty());
        assert_eq!(
            transition.directionality,
            PassageDirectionality::Unspecified
        );
        assert!(!transition.has_explicit_passage_profile());

        let serialized = serde_json::to_value(&transition).expect("serialize transition");
        assert!(serialized.get("passage_id").is_none());
        assert!(serialized.get("directionality").is_none());
    }

    #[test]
    fn explicit_directionality_uses_stable_snake_case_values() {
        let transition = Transition {
            passage_id: "cave_drop".to_string(),
            directionality: PassageDirectionality::OneWay,
            ..Default::default()
        };

        let serialized = serde_json::to_value(&transition).expect("serialize transition");
        assert_eq!(serialized["passage_id"], "cave_drop");
        assert_eq!(serialized["directionality"], "one_way");
    }
}
