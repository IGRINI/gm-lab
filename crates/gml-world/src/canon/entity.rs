//! Entities: containment, actors and factions (TZ §6.6, §6.7, §6.8).
//!
//! Every game entity has a well-defined location. The containment invariant
//! (TZ §6.6) is enforced by the validator: a physical entity has exactly one
//! immediate container, with no cycles, and an NPC is never in two places.
//!
//! An `Actor` is the *world-side* of an NPC: it exists outside the current
//! scene (TZ §6.7) and owns physical location, home, agenda, relations,
//! faction, knowledge and status. The rich NPC card stays in `World.npcs`;
//! `actor_id == npc_id` links them. `present_npcs` becomes DERIVED from actor
//! location + visibility, not a separate owner of truth.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Provenance;

/// Where an entity physically is (TZ §6.6). Exactly one immediate container.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "where", rename_all = "snake_case")]
pub enum Containment {
    /// Directly in a place.
    Place { place_id: String },
    /// Inside a container entity.
    Container { entity_id: String },
    /// Carried in an actor's inventory.
    Inventory { actor_id: String },
    /// Removed from play (dead, destroyed, gone).
    #[default]
    OutOfPlay,
    /// Non-physical (a faction, a rumour) — no spatial location.
    Abstract,
}

impl Containment {
    /// The place id if the entity is directly in a place.
    pub fn place(&self) -> Option<&str> {
        match self {
            Containment::Place { place_id } => Some(place_id.as_str()),
            _ => None,
        }
    }
}

/// The world-side actor record for an NPC (TZ §6.7).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Actor {
    /// Equals the NPC id it shadows (`World.npcs[actor_id]`).
    pub actor_id: String,
    #[serde(default)]
    pub public_label: String,
    /// Current physical location.
    #[serde(default)]
    pub location: Containment,
    /// Home / base place id.
    #[serde(default)]
    pub home_place_id: String,
    #[serde(default)]
    pub role: String,
    /// Attitude toward the player, -100..=100.
    #[serde(default)]
    pub attitude_to_player: i32,
    /// Relations to other actors, `actor_id -> -100..=100`.
    #[serde(default)]
    pub relations: BTreeMap<String, i32>,
    #[serde(default)]
    pub faction_id: String,
    #[serde(default)]
    pub goals: Vec<String>,
    /// Current short-term agenda.
    #[serde(default)]
    pub agenda: String,
    /// Knowledge / secret references (state-record or fact ids).
    #[serde(default)]
    pub knowledge_ids: Vec<String>,
    #[serde(default)]
    pub secret_ids: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    /// Simple schedule entries (`time_label -> place_id`).
    #[serde(default)]
    pub schedule: BTreeMap<String, String>,
    /// alive | injured | missing | dead | hiding | travelling.
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub provenance: Provenance,
}

impl Actor {
    pub fn is_at(&self, place_id: &str) -> bool {
        self.location.place() == Some(place_id)
    }
}

/// A faction — needed for living history & offscreen events (TZ §6.8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Faction {
    pub faction_id: String,
    pub name: String,
    /// Territory of influence (region/place ids).
    #[serde(default)]
    pub territory: Vec<String>,
    #[serde(default)]
    pub goals: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    /// Relations to other factions, `faction_id -> -100..=100`.
    #[serde(default)]
    pub relations: BTreeMap<String, i32>,
    #[serde(default)]
    pub attitude_to_player: i32,
    #[serde(default)]
    pub member_ids: Vec<String>,
    #[serde(default)]
    pub plans: Vec<String>,
    /// Scheduled event ids this faction will trigger.
    #[serde(default)]
    pub pending_event_ids: Vec<String>,
    #[serde(default)]
    pub history_event_ids: Vec<String>,
    #[serde(default)]
    pub provenance: Provenance,
}
