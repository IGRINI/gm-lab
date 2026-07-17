//! `canon` — the living-world canonical layer (LIVING_WORLD_ARCHITECTURE_TZ.md).
//!
//! The structural source of truth: a graph of [`Region`]s / [`Settlement`]s /
//! [`District`]s / [`Place`]s connected by directed [`Transition`]s, populated by [`Actor`]s and
//! [`Faction`]s, with an append-only [`EventLog`] and a game clock. Prose and the
//! live [`crate::SceneState`] are a *view* over it, never the owner (TZ §5).
//!
//! Mutations flow through structured [`action::Action`]s gated by the
//! [`validator::Validator`] and applied by the [`engine`] as committed events —
//! the LLM proposes but the engine decides what becomes true (TZ §5, §8).
//!
//! Determinism: generated ids and bounded choices derive from
//! `(world_seed, parent, kind)` via [`ids`], on a PRNG entirely separate from
//! the campaign dice RNG, so worldgen never perturbs `golden_turns` replay.

pub mod action;
pub mod engine;
pub mod entity;
pub mod event_log;
pub mod ids;
pub mod knowledge;
pub mod lore;
pub mod memory;
pub mod navigation;
mod place;
pub mod region;
pub mod rumor;
mod transition;
pub mod travel;
pub mod validator;
pub mod view;
pub mod worldgen;

pub use action::{Action, ProposedAction};
pub use engine::{
    advance_clock, apply, causal_log, debug_bundle, debug_dump, player_view, tick_offscreen,
    PlayerView, ViewActor, ViewEvent, ViewExit,
};
pub use entity::{Actor, Containment, Faction};
pub use event_log::{Account, CanonEvent, EventLog};
pub use knowledge::{Scope, Truthfulness};
pub use lore::WorldLore;
pub use memory::{
    canonical_scope, MemoryAccess, MemoryInjectionState, MemoryStore, MemoryTier,
    MemoryTruthStatus, MemoryUnit,
};
pub use navigation::{
    plan_travel, plan_travel_from, ActiveJourney, TravelAccess, TravelAnchor, TravelLink,
    TravelLinkValidationError, TravelNetwork, TravelPlan, TravelPlanError,
};
pub use place::Place;
pub use region::{District, DistrictValidationError, Region, Settlement};
pub use rumor::{
    memory_id_for_rumor, memory_unit_for_rumor, route_scope_for_transition,
    scopes_added_by_carrier_at_place, scopes_for_place, scopes_for_transition, should_decay_rumor,
    should_spread_place_rumor,
};
pub use transition::{PassageDirectionality, Transition};
pub use travel::{roll_travel_situation, TravelRisk, TravelRoll, TravelSituation};
pub use validator::{Rejection, Validator};
pub use worldgen::{generate, generate_with_lore, WorldSpec};

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::SceneState;

/// Generator/schema version stamped onto generated canon objects so saves can
/// be migrated and replays validated (TZ §12 "generator version").
pub const GENERATOR_VERSION: &str = "living-world/1";

/// Why an object exists, by whom and at what stage it was created, and from
/// which seed/event/decision it appeared (TZ §6.1 provenance).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Provenance {
    /// How this object came to exist: `seed`, `worldgen`, `lazy_gen`, `llm`,
    /// `offscreen`, `migration`.
    #[serde(default)]
    pub origin: String,
    /// Game turn at which it was created (0 at seed time).
    #[serde(default)]
    pub created_turn: i64,
    /// Source event id, if it was created by a committed event.
    #[serde(default)]
    pub source_event: String,
    /// Short human-readable reason.
    #[serde(default)]
    pub reason: String,
    /// Generator version that produced it.
    #[serde(default)]
    pub generator_version: String,
}

impl Provenance {
    /// Seed-time provenance: created at turn 0 directly from the story seed.
    pub fn seed() -> Self {
        Provenance {
            origin: "seed".to_string(),
            created_turn: 0,
            source_event: String::new(),
            reason: "derived from starting scene".to_string(),
            generator_version: GENERATOR_VERSION.to_string(),
        }
    }

    /// Provenance for an object created by a given origin/reason at a turn.
    pub fn by(origin: &str, reason: &str, turn: i64) -> Self {
        Provenance {
            origin: origin.to_string(),
            created_turn: turn,
            source_event: String::new(),
            reason: reason.to_string(),
            generator_version: GENERATOR_VERSION.to_string(),
        }
    }
}

/// A durable, scoped fact in the canon (TZ §8.2 `create_or_update_fact` /
/// `reveal_information`). Unlike a one-off event effect, a fact is queryable
/// state whose [`Scope`] can be widened by a reveal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CanonFact {
    pub fact_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub scope: Scope,
}

/// Per-turn generation caps (TZ §7.3 "bounded": limits on rooms / NPCs /
/// transitions / events created in one turn).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenBudget {
    pub max_rooms_per_turn: usize,
    pub max_npcs_per_turn: usize,
    pub max_transitions_per_turn: usize,
    pub max_events_per_turn: usize,
}

impl Default for GenBudget {
    fn default() -> Self {
        GenBudget {
            max_rooms_per_turn: 8,
            max_npcs_per_turn: 6,
            max_transitions_per_turn: 16,
            max_events_per_turn: 12,
        }
    }
}

/// The canonical world graph. `BTreeMap` keying gives deterministic,
/// replay-stable serialization order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WorldCanon {
    /// Campaign world seed (string form of `World.dice_seed`) — also the seed
    /// for deterministic generation. Never consumed as dice RNG entropy.
    #[serde(default)]
    pub world_seed: String,
    /// Generator version that produced this canon.
    #[serde(default)]
    pub generator_version: String,
    /// High-level world premise and generation guardrails.
    #[serde(default, skip_serializing_if = "WorldLore::is_empty")]
    pub world_lore: WorldLore,
    #[serde(default)]
    pub regions: BTreeMap<String, Region>,
    #[serde(default)]
    pub settlements: BTreeMap<String, Settlement>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub districts: BTreeMap<String, District>,
    #[serde(default)]
    pub places: BTreeMap<String, Place>,
    #[serde(default)]
    pub transitions: BTreeMap<String, Transition>,
    /// Abstract long-distance networks, separate from immediate place exits.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub travel_networks: BTreeMap<String, TravelNetwork>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub travel_anchors: BTreeMap<String, TravelAnchor>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub travel_accesses: BTreeMap<String, TravelAccess>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub travel_links: BTreeMap<String, TravelLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_journey: Option<ActiveJourney>,
    #[serde(default)]
    pub actors: BTreeMap<String, Actor>,
    #[serde(default)]
    pub factions: BTreeMap<String, Faction>,
    #[serde(default)]
    pub event_log: EventLog,
    /// Durable, scoped facts keyed by `fact_id` (TZ §8.2).
    #[serde(default)]
    pub facts: BTreeMap<String, CanonFact>,
    /// Scoped actor/place/player/faction memories and derived "crystals".
    #[serde(default, skip_serializing_if = "MemoryStore::is_empty")]
    pub memory: MemoryStore,
    /// Canonical game clock in absolute minutes (mirrors/leads `World.time`).
    #[serde(default)]
    pub clock_minutes: i64,
    /// The player's current canonical place id.
    #[serde(default)]
    pub player_place_id: String,
    #[serde(default)]
    pub gen_budget: GenBudget,
}

impl WorldCanon {
    /// True when there is no canon to persist. Drives the "emit `world_canon`
    /// only when non-empty" rule that keeps legacy (pre-canon) saves
    /// byte-identical.
    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
            && self.transitions.is_empty()
            && self.travel_networks.is_empty()
            && self.travel_anchors.is_empty()
            && self.travel_accesses.is_empty()
            && self.travel_links.is_empty()
            && self.active_journey.is_none()
            && self.regions.is_empty()
            && self.settlements.is_empty()
            && self.districts.is_empty()
            && self.actors.is_empty()
            && self.factions.is_empty()
            && self.event_log.is_empty()
            && self.facts.is_empty()
            && self.memory.is_empty()
            && self.world_lore.is_empty()
    }

    // --- accessors --------------------------------------------------------

    pub fn place(&self, place_id: &str) -> Option<&Place> {
        self.places.get(place_id)
    }
    pub fn place_mut(&mut self, place_id: &str) -> Option<&mut Place> {
        self.places.get_mut(place_id)
    }
    pub fn transition(&self, transition_id: &str) -> Option<&Transition> {
        self.transitions.get(transition_id)
    }
    pub fn travel_network(&self, network_id: &str) -> Option<&TravelNetwork> {
        self.travel_networks.get(network_id)
    }
    pub fn travel_anchor(&self, anchor_id: &str) -> Option<&TravelAnchor> {
        self.travel_anchors.get(anchor_id)
    }
    pub fn travel_access(&self, access_id: &str) -> Option<&TravelAccess> {
        self.travel_accesses.get(access_id)
    }
    pub fn travel_link(&self, link_id: &str) -> Option<&TravelLink> {
        self.travel_links.get(link_id)
    }
    pub fn actor(&self, actor_id: &str) -> Option<&Actor> {
        self.actors.get(actor_id)
    }
    pub fn region(&self, region_id: &str) -> Option<&Region> {
        self.regions.get(region_id)
    }
    pub fn settlement(&self, settlement_id: &str) -> Option<&Settlement> {
        self.settlements.get(settlement_id)
    }
    pub fn district(&self, district_id: &str) -> Option<&District> {
        self.districts.get(district_id)
    }

    /// Resolve a place's explicitly assigned district. This never falls back
    /// to names, descriptions, visit history, or collection membership.
    pub fn district_for_place(&self, place_id: &str) -> Option<&District> {
        let district_id = &self.place(place_id)?.district_id;
        (!district_id.is_empty())
            .then(|| self.district(district_id))
            .flatten()
    }

    /// Resolve the settlement that structurally owns a place.
    pub fn settlement_for_place(&self, place_id: &str) -> Option<&Settlement> {
        let place = self.place(place_id)?;
        if let Some(district) = self.district_for_place(place_id) {
            return self.settlement(&district.settlement_id);
        }
        self.settlement(&place.parent)
    }

    /// Resolve the region that structurally owns a place.
    pub fn region_for_place(&self, place_id: &str) -> Option<&Region> {
        let place = self.place(place_id)?;
        if !place.region_id.is_empty() {
            return self.region(&place.region_id);
        }
        if let Some(district) = self.district_for_place(place_id) {
            if !district.region_id.is_empty() {
                return self.region(&district.region_id);
            }
        }
        self.settlement_for_place(place_id)
            .and_then(|settlement| self.region(&settlement.region_id))
    }

    /// Outgoing transitions from a place, in the place's stored order.
    pub fn exits_from(&self, place_id: &str) -> Vec<&Transition> {
        match self.places.get(place_id) {
            Some(p) => p
                .transition_ids
                .iter()
                .filter_map(|tid| self.transitions.get(tid))
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn travel_accesses_from(&self, place_id: &str) -> Vec<&TravelAccess> {
        self.travel_accesses
            .values()
            .filter(|access| access.place_id == place_id)
            .collect()
    }

    pub fn travel_links_from(&self, anchor_id: &str) -> Vec<&TravelLink> {
        self.travel_links
            .values()
            .filter(|link| link.anchor_a == anchor_id || link.anchor_b == anchor_id)
            .collect()
    }

    /// Actors physically located at a place (the canonical source for the
    /// derived `present_npcs`, TZ §6.7).
    pub fn actors_at(&self, place_id: &str) -> Vec<&Actor> {
        self.actors
            .values()
            .filter(|a| a.is_at(place_id) && a.status != "dead")
            .collect()
    }

    /// Register a place and maintain its explicit district/settlement links.
    pub fn insert_place(&mut self, place: Place) {
        let place_id = place.place_id.clone();
        let previous_district_id = self
            .places
            .get(&place_id)
            .map(|existing| existing.district_id.clone())
            .unwrap_or_default();
        if previous_district_id != place.district_id {
            if let Some(previous) = self.districts.get_mut(&previous_district_id) {
                previous.place_ids.retain(|id| id != &place_id);
            }
        }

        let district_id = place.district_id.clone();
        self.places.insert(place_id.clone(), place);
        if let Some(district) = self.districts.get_mut(&district_id) {
            if !district.place_ids.contains(&place_id) {
                district.place_ids.push(place_id.clone());
            }
            if let Some(settlement) = self.settlements.get_mut(&district.settlement_id) {
                if !settlement.place_ids.contains(&place_id) {
                    settlement.place_ids.push(place_id);
                }
            }
        }
    }

    /// Validate and register a district, maintaining the owning settlement's
    /// link set. District membership must be explicit on every listed place.
    pub fn insert_district(&mut self, district: District) -> Result<(), DistrictValidationError> {
        if district.district_id.trim().is_empty() {
            return Err(DistrictValidationError::MissingId);
        }
        if district.name.trim().is_empty() {
            return Err(DistrictValidationError::MissingName);
        }
        let district_id = district.district_id.clone();
        if self.districts.contains_key(&district_id) {
            return Err(DistrictValidationError::DuplicateId(district_id));
        }
        let Some(settlement) = self.settlements.get(&district.settlement_id) else {
            return Err(DistrictValidationError::UnknownSettlement(
                district.settlement_id,
            ));
        };
        if !district.region_id.is_empty() && !self.regions.contains_key(&district.region_id) {
            return Err(DistrictValidationError::UnknownRegion(district.region_id));
        }
        if district.region_id != settlement.region_id {
            return Err(DistrictValidationError::SettlementRegionMismatch {
                settlement_id: settlement.settlement_id.clone(),
                settlement_region_id: settlement.region_id.clone(),
                district_region_id: district.region_id,
            });
        }
        let mut seen_place_ids = std::collections::BTreeSet::new();
        for place_id in &district.place_ids {
            if !seen_place_ids.insert(place_id) {
                return Err(DistrictValidationError::DuplicatePlace(place_id.clone()));
            }
            let Some(place) = self.place(place_id) else {
                return Err(DistrictValidationError::UnknownPlace(place_id.clone()));
            };
            if place.district_id != district_id {
                return Err(DistrictValidationError::PlaceMembershipMismatch {
                    place_id: place_id.clone(),
                    district_id,
                });
            }
        }
        let settlement_id = district.settlement_id.clone();
        let district_place_ids = district.place_ids.clone();
        self.districts.insert(district_id.clone(), district);
        if let Some(settlement) = self.settlements.get_mut(&settlement_id) {
            if !settlement.district_ids.contains(&district_id) {
                settlement.district_ids.push(district_id);
            }
            for place_id in district_place_ids {
                if !settlement.place_ids.contains(&place_id) {
                    settlement.place_ids.push(place_id);
                }
            }
        }
        Ok(())
    }

    /// Register a transition and wire it into its source place's exit list.
    pub fn insert_transition(&mut self, t: Transition) {
        if let Some(p) = self.places.get_mut(&t.from_place) {
            if !p.transition_ids.contains(&t.transition_id) {
                p.transition_ids.push(t.transition_id.clone());
            }
        }
        self.transitions.insert(t.transition_id.clone(), t);
    }

    pub fn insert_travel_network(&mut self, network: TravelNetwork) {
        self.travel_networks
            .insert(network.network_id.clone(), network);
    }

    pub fn insert_travel_anchor(&mut self, anchor: TravelAnchor) {
        self.travel_anchors.insert(anchor.anchor_id.clone(), anchor);
    }

    pub fn insert_travel_access(&mut self, access: TravelAccess) {
        self.travel_accesses
            .insert(access.access_id.clone(), access);
    }

    pub fn insert_travel_link(&mut self, link: TravelLink) {
        self.travel_links.insert(link.link_id.clone(), link);
    }

    // --- Phase-1 seed derivation -----------------------------------------

    /// Derive a Phase-1 canon from a seeded [`SceneState`]: one starting place
    /// plus one shell transition per exit. `world_seed` is recorded for
    /// provenance and as the generation seed. **Consumes no RNG.**
    pub fn from_scene(scene: &SceneState, world_seed: &str) -> Self {
        let mut canon = WorldCanon {
            world_seed: world_seed.to_string(),
            generator_version: GENERATOR_VERSION.to_string(),
            ..Default::default()
        };

        let place_id = if scene.location_id.is_empty() {
            "start_location".to_string()
        } else {
            scene.location_id.clone()
        };

        let prov = Provenance::seed();
        let mut transition_ids: Vec<String> = Vec::with_capacity(scene.exits.len());
        for exit in &scene.exits {
            // A canon edge id must be unique; seeds can produce colliding exit
            // ids (safe_id collapses 'North'/'north'). Uniquify with a numeric
            // suffix (mirrors seed_npcs); keep the original in source_exit_id so
            // the projected scene stays byte-identical.
            let base = if exit.exit_id.is_empty() {
                "exit".to_string()
            } else {
                exit.exit_id.clone()
            };
            let mut tid = base.clone();
            let mut n = 2;
            while canon.transitions.contains_key(&tid) {
                tid = format!("{base}_{n}");
                n += 1;
            }
            transition_ids.push(tid.clone());
            canon.transitions.insert(
                tid.clone(),
                Transition {
                    transition_id: tid,
                    source_exit_id: exit.exit_id.clone(),
                    passage_id: String::new(),
                    directionality: PassageDirectionality::Unspecified,
                    from_place: place_id.clone(),
                    to_place: String::new(),
                    destination_hint: exit.destination.clone(),
                    label: exit.name.clone(),
                    kind: String::new(),
                    visible: exit.visible,
                    passable: exit.blocked_by.is_empty(),
                    conditions: Vec::new(),
                    blocked_by: exit.blocked_by.clone(),
                    time_cost: 0,
                    risk: String::new(),
                    provenance: prov.clone(),
                },
            );
        }

        let mut state_flags = std::collections::BTreeSet::new();
        state_flags.insert("visited".to_string());

        canon.places.insert(
            place_id.clone(),
            Place {
                place_id: place_id.clone(),
                name: scene.title.clone(),
                kind: "scene".to_string(),
                parent: String::new(),
                region_id: String::new(),
                district_id: String::new(),
                default_description: scene.description.clone(),
                state_flags,
                features: Vec::new(),
                transition_ids,
                occupant_ids: scene.present_npcs.clone(),
                item_ids: scene.items.iter().map(|i| i.item_id.clone()).collect(),
                event_ids: Vec::new(),
                fact_ids: Vec::new(),
                provenance: prov,
            },
        );

        canon.player_place_id = place_id;
        canon
    }
}
