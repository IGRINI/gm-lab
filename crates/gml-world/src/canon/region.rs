//! Regions & settlements (TZ §6.2, §6.3).
//!
//! A `Region` is a large area (a valley, an island, a wasteland). At start it
//! need not be fully generated — a light shell with seed, theme, climate and a
//! few hinted sites plus reveal rules is enough (TZ §6.2 `RegionShell`).
//! A `Settlement` is never just a list of buildings: it must have a *function*
//! in the world — economy, routes, power, conflict, important NPCs (TZ §6.3).

use std::{error::Error, fmt};

use serde::{Deserialize, Serialize};

use super::Provenance;

/// A large area of the world.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Region {
    pub region_id: String,
    pub name: String,
    #[serde(default)]
    pub theme: String,
    #[serde(default)]
    pub climate: String,
    #[serde(default)]
    pub biomes: Vec<String>,
    /// Major routes / roads crossing the region.
    #[serde(default)]
    pub routes: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    /// Faction influence as `faction_id -> short note`.
    #[serde(default)]
    pub faction_influence: Vec<String>,
    /// Danger rating, 0..=5.
    #[serde(default)]
    pub danger_level: u8,
    #[serde(default)]
    pub settlement_ids: Vec<String>,
    /// Known points of interest (canonical place ids).
    #[serde(default)]
    pub site_ids: Vec<String>,
    /// Hinted-but-not-yet-canonical sites (TZ §7.3 hinted content): label hints
    /// that later lazy generation must respect.
    #[serde(default)]
    pub hinted_sites: Vec<String>,
    /// Region history (event ids).
    #[serde(default)]
    pub history_event_ids: Vec<String>,
    /// True while the region is only a shell (details not yet revealed).
    #[serde(default)]
    pub is_shell: bool,
    #[serde(default)]
    pub provenance: Provenance,
}

/// A populated place-of-power: town, village, fort, port, camp, ruin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Settlement {
    pub settlement_id: String,
    pub name: String,
    #[serde(default)]
    pub region_id: String,
    #[serde(default)]
    pub kind: String,
    /// What it lives on (TZ §6.3 economy).
    #[serde(default)]
    pub economy: Vec<String>,
    /// Who it is connected to (route descriptions / place ids).
    #[serde(default)]
    pub routes: Vec<String>,
    /// Who rules it.
    #[serde(default)]
    pub power: String,
    #[serde(default)]
    pub social_groups: Vec<String>,
    /// The conflict / tension that gives it a function.
    #[serde(default)]
    pub conflict: String,
    #[serde(default)]
    pub faction_ids: Vec<String>,
    #[serde(default)]
    pub important_npc_ids: Vec<String>,
    #[serde(default)]
    pub local_rumors: Vec<String>,
    #[serde(default)]
    pub threats: Vec<String>,
    /// Explicit city/settlement subdivisions. Older saves may leave this empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub district_ids: Vec<String>,
    /// Atomic places that make up the settlement.
    #[serde(default)]
    pub place_ids: Vec<String>,
    /// Consequences of past events here (event ids).
    #[serde(default)]
    pub history_event_ids: Vec<String>,
    #[serde(default)]
    pub provenance: Provenance,
}

impl Settlement {
    /// A settlement is well-formed only if it has a function — at minimum an
    /// economy and either a conflict or a route (TZ §15 antipattern: no
    /// settlements without economy/routes/conflict/function).
    pub fn has_function(&self) -> bool {
        !self.economy.is_empty() && (!self.conflict.is_empty() || !self.routes.is_empty())
    }
}

/// A stable subdivision of a [`Settlement`].
///
/// Districts are structural geography, not prose labels. A place belongs to a
/// district only through its explicit `district_id`; names and visit history
/// are never used to infer membership.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct District {
    pub district_id: String,
    pub name: String,
    #[serde(default)]
    pub settlement_id: String,
    #[serde(default)]
    pub region_id: String,
    #[serde(default)]
    pub kind: String,
    /// Canonical places assigned to this district.
    #[serde(default)]
    pub place_ids: Vec<String>,
    #[serde(default)]
    pub provenance: Provenance,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DistrictValidationError {
    MissingId,
    MissingName,
    DuplicateId(String),
    UnknownSettlement(String),
    UnknownRegion(String),
    SettlementRegionMismatch {
        settlement_id: String,
        settlement_region_id: String,
        district_region_id: String,
    },
    DuplicatePlace(String),
    UnknownPlace(String),
    PlaceMembershipMismatch {
        place_id: String,
        district_id: String,
    },
}

impl fmt::Display for DistrictValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingId => f.write_str("district_id is empty"),
            Self::MissingName => f.write_str("district name is empty"),
            Self::DuplicateId(id) => write!(f, "district '{id}' already exists"),
            Self::UnknownSettlement(id) => {
                write!(f, "district references unknown settlement '{id}'")
            }
            Self::UnknownRegion(id) => write!(f, "district references unknown region '{id}'"),
            Self::SettlementRegionMismatch {
                settlement_id,
                settlement_region_id,
                district_region_id,
            } => write!(
                f,
                "district region '{district_region_id}' does not match settlement '{settlement_id}' region '{settlement_region_id}'"
            ),
            Self::DuplicatePlace(id) => write!(f, "district repeats place '{id}'"),
            Self::UnknownPlace(id) => write!(f, "district references unknown place '{id}'"),
            Self::PlaceMembershipMismatch {
                place_id,
                district_id,
            } => write!(
                f,
                "place '{place_id}' is not explicitly assigned to district '{district_id}'"
            ),
        }
    }
}

impl Error for DistrictValidationError {}
