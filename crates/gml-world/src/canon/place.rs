//! `Place` — a canonical, persistent location node in the world graph.
//!
//! TZ §6.4: a `Place` is the atomic playable location (a tavern hall, a village
//! square, a barrow chamber). Unlike the current [`crate::SceneState`], a `Place`
//! exists *always*; the scene is only the current *view* onto one of them. A
//! `Place` therefore owns stable structural truth (id, name, type, parent,
//! default description, state flags, features) and *links* to the entities and
//! events that make it significant — not their full bodies.
//!
//! Phase 1 keeps `Place` strictly additive: it is derived from the seeded
//! `SceneState` and never yet drives the live scene. The occupant/item/event/
//! fact id lists are link sets only; the rich entity bodies still live in
//! `World.npcs` / `SceneState.items` until later phases promote them.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::Provenance;

/// A canonical location node. Field order is the serialized order; `BTreeSet`
/// fields are emitted sorted for deterministic, replay-stable bytes (matching
/// the conventions in `crate::model`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Place {
    /// Stable id. In Phase 1 this equals the originating `SceneState.location_id`
    /// so a return-to-place keys off the same node.
    pub place_id: String,
    /// Human-readable name (player/world-facing).
    pub name: String,
    /// Category: `scene` (P1 seed placeholder), later `street` / `building` /
    /// `dungeon_room` / `region` / `settlement` etc.
    #[serde(default)]
    pub kind: String,
    /// Parent place / building / dungeon id, or empty for a root.
    #[serde(default)]
    pub parent: String,
    /// Owning region id, or empty until regions exist (P6).
    #[serde(default)]
    pub region_id: String,
    /// Explicit owning district id. Empty is valid for old saves and places
    /// outside settlements; membership is never inferred from names or visits.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub district_id: String,
    /// Default player-facing description when no event has altered the place.
    #[serde(default)]
    pub default_description: String,
    /// State markers: `visited`, `destroyed`, `locked`, `dangerous`, `lit`,
    /// `flooded`, … A sorted set so output bytes are deterministic.
    #[serde(default)]
    pub state_flags: BTreeSet<String>,
    /// Durable features of the place (a hearth, a collapsed floor, an altar).
    #[serde(default)]
    pub features: Vec<String>,
    /// Outgoing transition ids, in discovery order. Ordered (not a set) so the
    /// derived view reproduces the original exit order exactly.
    #[serde(default)]
    pub transition_ids: Vec<String>,
    /// Canonical NPC occupants (links into `World.npcs`).
    #[serde(default)]
    pub occupant_ids: BTreeSet<String>,
    /// Item links (bodies still live in the scene/inventory in Phase 1).
    #[serde(default)]
    pub item_ids: Vec<String>,
    /// Links to the events (P4 `EventLog`) that touched this place.
    #[serde(default)]
    pub event_ids: Vec<String>,
    /// Links to facts/state-records anchored here.
    #[serde(default)]
    pub fact_ids: Vec<String>,
    /// Why/when/how this place was created (TZ §6.1 provenance).
    #[serde(default)]
    pub provenance: Provenance,
}

impl Place {
    /// Whether the player has been here. Drives "same place, changed" framing.
    pub fn is_visited(&self) -> bool {
        self.state_flags.contains("visited")
    }

    /// Mark this place visited (idempotent).
    pub fn mark_visited(&mut self) {
        self.state_flags.insert("visited".to_string());
    }

    /// Whether a given state flag is set.
    pub fn has_flag(&self, flag: &str) -> bool {
        self.state_flags.contains(flag)
    }
}
