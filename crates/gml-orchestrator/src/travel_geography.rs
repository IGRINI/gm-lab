//! Strict commit boundary for travel geography authored by the location creator.
//!
//! This module deliberately contains no route inference. It accepts only explicit
//! canonical ids and mechanical fields, validates the complete staged travel
//! graph, and returns a cloned [`WorldCanon`] on success. The caller's canon is
//! therefore never partially mutated by malformed generator output.

use std::{collections::BTreeSet, error::Error, fmt};

use gml_world::canon::{
    PassageDirectionality, Provenance, Scope, TravelAccess, TravelAnchor, TravelLink,
    TravelNetwork, WorldCanon,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const ENTITY_NETWORK: &str = "travel network";
const ENTITY_ANCHOR: &str = "travel anchor";
const ENTITY_ACCESS: &str = "travel access";
const ENTITY_LINK: &str = "travel link";

/// Exact canonical boundary for geography authored for one travel request.
///
/// The location creator receives broad world context, so prompt instructions
/// alone are not an authorization boundary. This policy records the only
/// existing travel entities and endpoint/scope ids that its response may use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TravelGeographyPolicy {
    origin_place_id: String,
    destination_place_id: String,
    requested_network_id: Option<String>,
    allowed_scope_ids: BTreeSet<String>,
    allowed_existing_network_ids: BTreeSet<String>,
    allowed_existing_anchor_ids: BTreeSet<String>,
    allowed_existing_access_ids: BTreeSet<String>,
    allowed_existing_link_ids: BTreeSet<String>,
}

impl TravelGeographyPolicy {
    pub fn for_route(
        canon: &WorldCanon,
        origin_place_id: impl Into<String>,
        destination_place_id: impl Into<String>,
        allowed_scope_ids: impl IntoIterator<Item = String>,
        requested_network_id: Option<&str>,
    ) -> Self {
        let origin_place_id = origin_place_id.into();
        let destination_place_id = destination_place_id.into();
        let requested_network_id = requested_network_id.map(ToString::to_string);
        let mut allowed_scope_ids = allowed_scope_ids.into_iter().collect::<BTreeSet<_>>();

        let boundary_place_ids =
            exact_access_boundary_place_ids(canon, &origin_place_id, &destination_place_id);
        let boundary_network_ids = canon
            .travel_accesses
            .values()
            .filter(|access| access.is_available() && boundary_place_ids.contains(&access.place_id))
            .filter_map(|access| canon.travel_anchors.get(&access.anchor_id))
            .filter(|anchor| anchor.is_available())
            .map(|anchor| anchor.network_id.clone())
            .collect::<BTreeSet<_>>();

        let allowed_existing_network_ids = canon
            .travel_networks
            .iter()
            .filter(|(network_id, network)| {
                let explicitly_requested = requested_network_id.as_deref() == Some(network_id);
                let relevant_default = requested_network_id.is_none()
                    && network.default_for_normal_travel
                    && network.is_available()
                    && (allowed_scope_ids.contains(&network.scope_id)
                        || boundary_network_ids.contains(*network_id));
                explicitly_requested || relevant_default
            })
            .map(|(network_id, _)| network_id.clone())
            .collect::<BTreeSet<_>>();

        for network_id in &allowed_existing_network_ids {
            if let Some(network) = canon.travel_networks.get(network_id) {
                allowed_scope_ids.insert(network.scope_id.clone());
            }
        }

        let allowed_existing_anchor_ids = canon
            .travel_anchors
            .iter()
            .filter(|(_, anchor)| allowed_existing_network_ids.contains(&anchor.network_id))
            .map(|(anchor_id, _)| anchor_id.clone())
            .collect::<BTreeSet<_>>();
        let allowed_existing_access_ids = canon
            .travel_accesses
            .iter()
            .filter(|(_, access)| {
                let is_endpoint =
                    access.place_id == origin_place_id || access.place_id == destination_place_id;
                is_endpoint && allowed_existing_anchor_ids.contains(&access.anchor_id)
            })
            .map(|(access_id, _)| access_id.clone())
            .collect::<BTreeSet<_>>();
        let allowed_existing_link_ids = canon
            .travel_links
            .iter()
            .filter(|(_, link)| {
                allowed_existing_anchor_ids.contains(&link.anchor_a)
                    && allowed_existing_anchor_ids.contains(&link.anchor_b)
            })
            .map(|(link_id, _)| link_id.clone())
            .collect::<BTreeSet<_>>();

        Self {
            origin_place_id,
            destination_place_id,
            requested_network_id,
            allowed_scope_ids,
            allowed_existing_network_ids,
            allowed_existing_anchor_ids,
            allowed_existing_access_ids,
            allowed_existing_link_ids,
        }
    }

    pub fn allowed_scope_ids(&self) -> &BTreeSet<String> {
        &self.allowed_scope_ids
    }

    pub fn allows_existing_network(&self, network_id: &str) -> bool {
        self.allowed_existing_network_ids.contains(network_id)
    }

    pub fn allows_existing_anchor(&self, anchor_id: &str) -> bool {
        self.allowed_existing_anchor_ids.contains(anchor_id)
    }

    pub fn allows_existing_access(&self, access_id: &str) -> bool {
        self.allowed_existing_access_ids.contains(access_id)
    }

    pub fn allows_existing_link(&self, link_id: &str) -> bool {
        self.allowed_existing_link_ids.contains(link_id)
    }

    fn allows_access_place(&self, place_id: &str) -> bool {
        place_id == self.origin_place_id || place_id == self.destination_place_id
    }

    fn allows_network(
        &self,
        network_id: &str,
        newly_declared_network_ids: &BTreeSet<String>,
    ) -> bool {
        self.allowed_existing_network_ids.contains(network_id)
            || newly_declared_network_ids.contains(network_id)
    }

    fn allows_anchor(&self, anchor_id: &str, newly_declared_anchor_ids: &BTreeSet<String>) -> bool {
        self.allowed_existing_anchor_ids.contains(anchor_id)
            || newly_declared_anchor_ids.contains(anchor_id)
    }
}

/// The only place identities allowed to establish relevance for an existing
/// default network. Besides the exact endpoints, this includes one immediate
/// neighbour reached by an explicit, open, bidirectional canonical passage.
///
/// The one-edge limit is deliberate: it lets a location immediately outside an
/// area attach to that area's already-authored network without traversing a
/// multi-step exploration chain. The mechanical filters separately exclude
/// one-way and currently closed passages.
fn exact_access_boundary_place_ids(
    canon: &WorldCanon,
    origin_place_id: &str,
    destination_place_id: &str,
) -> BTreeSet<String> {
    let mut place_ids = BTreeSet::from([
        origin_place_id.to_string(),
        destination_place_id.to_string(),
    ]);

    for endpoint_id in [origin_place_id, destination_place_id] {
        place_ids.extend(
            canon
                .exits_from(endpoint_id)
                .into_iter()
                .filter_map(|transition| {
                    let is_open_bidirectional_passage = transition.visible
                        && transition.passable
                        && transition.blocked_by.is_empty()
                        && transition.conditions.is_empty()
                        && transition.directionality == PassageDirectionality::Bidirectional
                        && transition.has_target()
                        && canon.places.contains_key(&transition.to_place);
                    is_open_bidirectional_passage.then(|| transition.to_place.clone())
                }),
        );
    }

    place_ids
}

/// Added/reused counts for one canonical travel entity type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TravelGeographyEntitySummary {
    pub added: usize,
    pub reused: usize,
}

/// A deterministic summary of an atomic travel-geography commit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct TravelGeographySummary {
    pub networks: TravelGeographyEntitySummary,
    pub anchors: TravelGeographyEntitySummary,
    pub accesses: TravelGeographyEntitySummary,
    pub links: TravelGeographyEntitySummary,
}

impl TravelGeographySummary {
    pub const fn total_added(self) -> usize {
        self.networks.added + self.anchors.added + self.accesses.added + self.links.added
    }

    pub const fn total_reused(self) -> usize {
        self.networks.reused + self.anchors.reused + self.accesses.reused + self.links.reused
    }
}

/// A generator-output rejection. No canonical state has changed when returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TravelGeographyError {
    MissingTravelGeography,
    Malformed(String),
    EmptyTravelGeography,
    InvalidId {
        entity: &'static str,
        id: String,
    },
    DuplicateId {
        entity: &'static str,
        id: String,
    },
    MechanicalConflict {
        entity: &'static str,
        id: String,
    },
    InconsistentPassability {
        entity: &'static str,
        id: String,
    },
    InvalidBlocker {
        entity: &'static str,
        id: String,
        blocker_id: String,
    },
    UnknownScope {
        network_id: String,
        scope_id: String,
    },
    UnknownReference {
        entity: &'static str,
        id: String,
        field: &'static str,
        target_id: String,
    },
    UnvisitedAccessPlace {
        access_id: String,
        place_id: String,
    },
    DuplicateRequiredFact {
        entity: &'static str,
        id: String,
        fact_id: String,
    },
    InvalidLink {
        link_id: String,
        reason: String,
    },
    CrossNetworkLink {
        link_id: String,
        network_a: String,
        network_b: String,
    },
    OutsideRequestBoundary {
        entity: &'static str,
        id: String,
        field: &'static str,
        target_id: String,
    },
    NewEntityRestriction {
        entity: &'static str,
        id: String,
    },
}

impl fmt::Display for TravelGeographyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTravelGeography => {
                f.write_str("location creator result has no travel_geography object")
            }
            Self::Malformed(reason) => write!(f, "malformed travel_geography: {reason}"),
            Self::EmptyTravelGeography => {
                f.write_str("travel_geography contains no canonical entities")
            }
            Self::InvalidId { entity, id } => {
                write!(f, "{entity} id must be a non-empty exact string: {id:?}")
            }
            Self::DuplicateId { entity, id } => {
                write!(f, "travel_geography repeats {entity} id '{id}'")
            }
            Self::MechanicalConflict { entity, id } => write!(
                f,
                "generated {entity} '{id}' conflicts with its canonical mechanics"
            ),
            Self::InconsistentPassability { entity, id } => write!(
                f,
                "{entity} '{id}' must be passable exactly when blocked_by is empty"
            ),
            Self::InvalidBlocker {
                entity,
                id,
                blocker_id,
            } => write!(
                f,
                "{entity} '{id}' blocked_by must reference an exact non-rumor canonical fact, got '{blocker_id}'"
            ),
            Self::UnknownScope {
                network_id,
                scope_id,
            } => write!(
                f,
                "travel network '{network_id}' references unknown scope '{scope_id}'"
            ),
            Self::UnknownReference {
                entity,
                id,
                field,
                target_id,
            } => write!(
                f,
                "{entity} '{id}' references unknown {field} '{target_id}'"
            ),
            Self::UnvisitedAccessPlace {
                access_id,
                place_id,
            } => write!(
                f,
                "travel access '{access_id}' references unvisited place '{place_id}'"
            ),
            Self::DuplicateRequiredFact {
                entity,
                id,
                fact_id,
            } => write!(f, "{entity} '{id}' repeats required fact '{fact_id}'"),
            Self::InvalidLink { link_id, reason } => {
                write!(f, "travel link '{link_id}' is invalid: {reason}")
            }
            Self::CrossNetworkLink {
                link_id,
                network_a,
                network_b,
            } => write!(
                f,
                "travel link '{link_id}' crosses networks '{network_a}' and '{network_b}'"
            ),
            Self::OutsideRequestBoundary {
                entity,
                id,
                field,
                target_id,
            } => write!(
                f,
                "{entity} '{id}' uses {field} '{target_id}' outside the exact travel request boundary"
            ),
            Self::NewEntityRestriction { entity, id } => write!(
                f,
                "new {entity} '{id}' must be passable with no blocker or required facts"
            ),
        }
    }
}

impl Error for TravelGeographyError {}

/// Parse, validate, and atomically stage travel geography from a location
/// creator result.
///
/// The expected input is the creator's whole JSON object. Only the explicit
/// `travel_geography` field is consumed. Existing ids are reused only when all
/// mechanical fields match; provenance is intentionally not part of mechanics.
pub fn stage_travel_geography(
    canon: &WorldCanon,
    generated: &Value,
    turn: i64,
    policy: &TravelGeographyPolicy,
) -> Result<(WorldCanon, TravelGeographySummary), TravelGeographyError> {
    let raw = generated
        .get("travel_geography")
        .ok_or(TravelGeographyError::MissingTravelGeography)?;
    let payload: TravelGeographyPayload = serde_json::from_value(raw.clone())
        .map_err(|error| TravelGeographyError::Malformed(error.to_string()))?;
    if payload.is_empty() {
        return Err(TravelGeographyError::EmptyTravelGeography);
    }

    ensure_unique_ids(
        ENTITY_NETWORK,
        payload.networks.iter().map(|item| item.network_id.as_str()),
    )?;

    let newly_declared_network_ids = payload
        .networks
        .iter()
        .filter(|item| !canon.travel_networks.contains_key(&item.network_id))
        .map(|item| item.network_id.clone())
        .collect::<BTreeSet<_>>();
    let newly_declared_anchor_ids = payload
        .anchors
        .iter()
        .filter(|item| !canon.travel_anchors.contains_key(&item.anchor_id))
        .map(|item| item.anchor_id.clone())
        .collect::<BTreeSet<_>>();
    if let Some(extra_network_id) = newly_declared_network_ids.iter().nth(1) {
        return Err(TravelGeographyError::OutsideRequestBoundary {
            entity: ENTITY_NETWORK,
            id: extra_network_id.clone(),
            field: "new_network_id",
            target_id: extra_network_id.clone(),
        });
    }
    ensure_unique_ids(
        ENTITY_ANCHOR,
        payload.anchors.iter().map(|item| item.anchor_id.as_str()),
    )?;
    ensure_unique_ids(
        ENTITY_ACCESS,
        payload.accesses.iter().map(|item| item.access_id.as_str()),
    )?;
    ensure_unique_ids(
        ENTITY_LINK,
        payload.links.iter().map(|item| item.link_id.as_str()),
    )?;

    let provenance = Provenance::by("location_generator", "travel geography", turn);
    let mut staged = canon.clone();
    let mut summary = TravelGeographySummary::default();

    for raw in payload.networks {
        validate_exact_id(ENTITY_NETWORK, &raw.network_id)?;
        validate_exact_id("travel network scope", &raw.scope_id)?;
        if !scope_exists(canon, &raw.scope_id) {
            return Err(TravelGeographyError::UnknownScope {
                network_id: raw.network_id,
                scope_id: raw.scope_id,
            });
        }
        if !policy.allowed_scope_ids.contains(&raw.scope_id) {
            return Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_NETWORK,
                id: raw.network_id,
                field: "scope_id",
                target_id: raw.scope_id,
            });
        }
        let existing = canon.travel_networks.get(&raw.network_id);
        if existing.is_some() && !policy.allows_existing_network(&raw.network_id) {
            return Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_NETWORK,
                id: raw.network_id.clone(),
                field: "network_id",
                target_id: raw.network_id,
            });
        }
        if existing.is_none()
            && (policy.requested_network_id.is_some() || !raw.default_for_normal_travel)
        {
            return Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_NETWORK,
                id: raw.network_id.clone(),
                field: "default_for_normal_travel",
                target_id: raw.default_for_normal_travel.to_string(),
            });
        }
        validate_authored_passability(
            canon,
            ENTITY_NETWORK,
            &raw.network_id,
            raw.passable,
            &raw.blocked_by,
        )?;
        if existing.is_none() {
            validate_new_entity_restrictions(
                ENTITY_NETWORK,
                &raw.network_id,
                raw.passable,
                &raw.blocked_by,
                &[],
            )?;
        }
        let network = TravelNetwork {
            network_id: raw.network_id,
            scope_id: raw.scope_id,
            default_for_normal_travel: raw.default_for_normal_travel,
            passable: raw.passable,
            blocked_by: raw.blocked_by,
            provenance: provenance.clone(),
        };
        match staged.travel_networks.get(&network.network_id) {
            Some(existing) if same_network_mechanics(existing, &network) => {
                summary.networks.reused += 1;
            }
            Some(_) => {
                return Err(TravelGeographyError::MechanicalConflict {
                    entity: ENTITY_NETWORK,
                    id: network.network_id,
                });
            }
            None => {
                staged.insert_travel_network(network);
                summary.networks.added += 1;
            }
        }
    }

    for raw in payload.anchors {
        validate_exact_id(ENTITY_ANCHOR, &raw.anchor_id)?;
        validate_exact_id("travel anchor network", &raw.network_id)?;
        if !policy.allows_network(&raw.network_id, &newly_declared_network_ids) {
            return Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_ANCHOR,
                id: raw.anchor_id,
                field: "network_id",
                target_id: raw.network_id,
            });
        }
        let existing = canon.travel_anchors.get(&raw.anchor_id);
        if existing.is_some() && !policy.allows_existing_anchor(&raw.anchor_id) {
            return Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_ANCHOR,
                id: raw.anchor_id.clone(),
                field: "anchor_id",
                target_id: raw.anchor_id,
            });
        }
        validate_authored_passability(
            canon,
            ENTITY_ANCHOR,
            &raw.anchor_id,
            raw.passable,
            &raw.blocked_by,
        )?;
        if existing.is_none() {
            validate_new_entity_restrictions(
                ENTITY_ANCHOR,
                &raw.anchor_id,
                raw.passable,
                &raw.blocked_by,
                &[],
            )?;
        }
        let anchor = TravelAnchor {
            anchor_id: raw.anchor_id,
            network_id: raw.network_id,
            passable: raw.passable,
            blocked_by: raw.blocked_by,
            provenance: provenance.clone(),
        };
        match staged.travel_anchors.get(&anchor.anchor_id) {
            Some(existing) if same_anchor_mechanics(existing, &anchor) => {
                summary.anchors.reused += 1;
            }
            Some(_) => {
                return Err(TravelGeographyError::MechanicalConflict {
                    entity: ENTITY_ANCHOR,
                    id: anchor.anchor_id,
                });
            }
            None => {
                staged.insert_travel_anchor(anchor);
                summary.anchors.added += 1;
            }
        }
    }

    for raw in payload.accesses {
        validate_exact_id(ENTITY_ACCESS, &raw.access_id)?;
        validate_exact_id("travel access place", &raw.place_id)?;
        validate_exact_id("travel access anchor", &raw.anchor_id)?;
        if !policy.allows_access_place(&raw.place_id) {
            return Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_ACCESS,
                id: raw.access_id,
                field: "place_id",
                target_id: raw.place_id,
            });
        }
        if !policy.allows_anchor(&raw.anchor_id, &newly_declared_anchor_ids) {
            return Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_ACCESS,
                id: raw.access_id,
                field: "anchor_id",
                target_id: raw.anchor_id,
            });
        }
        let existing = canon.travel_accesses.get(&raw.access_id);
        if existing.is_some() && !policy.allows_existing_access(&raw.access_id) {
            return Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_ACCESS,
                id: raw.access_id.clone(),
                field: "access_id",
                target_id: raw.access_id,
            });
        }
        validate_authored_passability(
            canon,
            ENTITY_ACCESS,
            &raw.access_id,
            raw.passable,
            &raw.blocked_by,
        )?;
        let required_fact_ids =
            validate_required_facts(ENTITY_ACCESS, &raw.access_id, raw.required_fact_ids)?;
        validate_canonical_required_facts(
            canon,
            ENTITY_ACCESS,
            &raw.access_id,
            &required_fact_ids,
        )?;
        if existing.is_none() {
            validate_new_entity_restrictions(
                ENTITY_ACCESS,
                &raw.access_id,
                raw.passable,
                &raw.blocked_by,
                &required_fact_ids,
            )?;
        }
        let access = TravelAccess {
            access_id: raw.access_id,
            place_id: raw.place_id,
            anchor_id: raw.anchor_id,
            passable: raw.passable,
            blocked_by: raw.blocked_by,
            required_fact_ids,
            provenance: provenance.clone(),
        };
        match staged.travel_accesses.get(&access.access_id) {
            Some(existing) if same_access_mechanics(existing, &access) => {
                summary.accesses.reused += 1;
            }
            Some(_) => {
                return Err(TravelGeographyError::MechanicalConflict {
                    entity: ENTITY_ACCESS,
                    id: access.access_id,
                });
            }
            None => {
                staged.insert_travel_access(access);
                summary.accesses.added += 1;
            }
        }
    }

    for raw in payload.links {
        validate_exact_id(ENTITY_LINK, &raw.link_id)?;
        validate_exact_id("travel link anchor", &raw.anchor_a)?;
        validate_exact_id("travel link anchor", &raw.anchor_b)?;
        for (field, anchor_id) in [("anchor_a", &raw.anchor_a), ("anchor_b", &raw.anchor_b)] {
            if !policy.allows_anchor(anchor_id, &newly_declared_anchor_ids) {
                return Err(TravelGeographyError::OutsideRequestBoundary {
                    entity: ENTITY_LINK,
                    id: raw.link_id,
                    field,
                    target_id: anchor_id.clone(),
                });
            }
        }
        let existing = canon.travel_links.get(&raw.link_id);
        if existing.is_some() && !policy.allows_existing_link(&raw.link_id) {
            return Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_LINK,
                id: raw.link_id.clone(),
                field: "link_id",
                target_id: raw.link_id,
            });
        }
        validate_authored_passability(
            canon,
            ENTITY_LINK,
            &raw.link_id,
            raw.passable,
            &raw.blocked_by,
        )?;
        let required_fact_ids =
            validate_required_facts(ENTITY_LINK, &raw.link_id, raw.required_fact_ids)?;
        validate_canonical_required_facts(canon, ENTITY_LINK, &raw.link_id, &required_fact_ids)?;
        if existing.is_none() {
            validate_new_entity_restrictions(
                ENTITY_LINK,
                &raw.link_id,
                raw.passable,
                &raw.blocked_by,
                &required_fact_ids,
            )?;
        }
        let link = TravelLink {
            link_id: raw.link_id,
            anchor_a: raw.anchor_a,
            anchor_b: raw.anchor_b,
            time_cost_minutes: raw.time_cost_minutes,
            risk: raw.risk,
            passable: raw.passable,
            blocked_by: raw.blocked_by,
            required_fact_ids,
            provenance: provenance.clone(),
        };
        link.validate()
            .map_err(|reason| TravelGeographyError::InvalidLink {
                link_id: link.link_id.clone(),
                reason: reason.to_string(),
            })?;
        match staged.travel_links.get(&link.link_id) {
            Some(existing) if same_link_mechanics(existing, &link) => {
                summary.links.reused += 1;
            }
            Some(_) => {
                return Err(TravelGeographyError::MechanicalConflict {
                    entity: ENTITY_LINK,
                    id: link.link_id,
                });
            }
            None => {
                staged.insert_travel_link(link);
                summary.links.added += 1;
            }
        }
    }

    validate_staged_graph(&staged)?;
    Ok((staged, summary))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TravelGeographyPayload {
    networks: Vec<RawNetwork>,
    anchors: Vec<RawAnchor>,
    accesses: Vec<RawAccess>,
    links: Vec<RawLink>,
}

impl TravelGeographyPayload {
    fn is_empty(&self) -> bool {
        self.networks.is_empty()
            && self.anchors.is_empty()
            && self.accesses.is_empty()
            && self.links.is_empty()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNetwork {
    network_id: String,
    scope_id: String,
    default_for_normal_travel: bool,
    passable: bool,
    #[serde(default)]
    blocked_by: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAnchor {
    anchor_id: String,
    network_id: String,
    passable: bool,
    #[serde(default)]
    blocked_by: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAccess {
    access_id: String,
    place_id: String,
    anchor_id: String,
    passable: bool,
    #[serde(default)]
    blocked_by: String,
    #[serde(default)]
    required_fact_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLink {
    link_id: String,
    anchor_a: String,
    anchor_b: String,
    time_cost_minutes: i64,
    risk: String,
    passable: bool,
    #[serde(default)]
    blocked_by: String,
    #[serde(default)]
    required_fact_ids: Vec<String>,
}

fn ensure_unique_ids<'a>(
    entity: &'static str,
    ids: impl Iterator<Item = &'a str>,
) -> Result<(), TravelGeographyError> {
    let mut seen = BTreeSet::new();
    for id in ids {
        validate_exact_id(entity, id)?;
        if !seen.insert(id) {
            return Err(TravelGeographyError::DuplicateId {
                entity,
                id: id.to_string(),
            });
        }
    }
    Ok(())
}

fn validate_exact_id(entity: &'static str, id: &str) -> Result<(), TravelGeographyError> {
    if id.is_empty() || id.trim() != id || id.chars().any(char::is_control) {
        return Err(TravelGeographyError::InvalidId {
            entity,
            id: id.to_string(),
        });
    }
    Ok(())
}

fn validate_authored_passability(
    canon: &WorldCanon,
    entity: &'static str,
    id: &str,
    passable: bool,
    blocker_id: &str,
) -> Result<(), TravelGeographyError> {
    if passable != blocker_id.is_empty() {
        return Err(TravelGeographyError::InconsistentPassability {
            entity,
            id: id.to_string(),
        });
    }
    validate_blocker(canon, entity, id, blocker_id)
}

fn validate_new_entity_restrictions(
    entity: &'static str,
    id: &str,
    passable: bool,
    blocker_id: &str,
    required_fact_ids: &[String],
) -> Result<(), TravelGeographyError> {
    if passable && blocker_id.is_empty() && required_fact_ids.is_empty() {
        Ok(())
    } else {
        Err(TravelGeographyError::NewEntityRestriction {
            entity,
            id: id.to_string(),
        })
    }
}

fn validate_required_facts(
    entity: &'static str,
    entity_id: &str,
    mut fact_ids: Vec<String>,
) -> Result<Vec<String>, TravelGeographyError> {
    let mut seen = BTreeSet::new();
    for fact_id in &fact_ids {
        validate_exact_id("required fact", fact_id)?;
        if !seen.insert(fact_id.clone()) {
            return Err(TravelGeographyError::DuplicateRequiredFact {
                entity,
                id: entity_id.to_string(),
                fact_id: fact_id.clone(),
            });
        }
    }
    fact_ids.sort();
    Ok(fact_ids)
}

fn validate_staged_graph(canon: &WorldCanon) -> Result<(), TravelGeographyError> {
    for (map_id, network) in &canon.travel_networks {
        validate_map_identity(ENTITY_NETWORK, map_id, &network.network_id)?;
        validate_exact_id("travel network scope", &network.scope_id)?;
        validate_authored_passability(
            canon,
            ENTITY_NETWORK,
            map_id,
            network.passable,
            &network.blocked_by,
        )?;
        if !scope_exists(canon, &network.scope_id) {
            return Err(TravelGeographyError::UnknownScope {
                network_id: network.network_id.clone(),
                scope_id: network.scope_id.clone(),
            });
        }
    }

    for (map_id, anchor) in &canon.travel_anchors {
        validate_map_identity(ENTITY_ANCHOR, map_id, &anchor.anchor_id)?;
        validate_exact_id("travel anchor network", &anchor.network_id)?;
        validate_authored_passability(
            canon,
            ENTITY_ANCHOR,
            map_id,
            anchor.passable,
            &anchor.blocked_by,
        )?;
        if !canon.travel_networks.contains_key(&anchor.network_id) {
            return Err(TravelGeographyError::UnknownReference {
                entity: ENTITY_ANCHOR,
                id: anchor.anchor_id.clone(),
                field: "network_id",
                target_id: anchor.network_id.clone(),
            });
        }
    }

    for (map_id, access) in &canon.travel_accesses {
        validate_map_identity(ENTITY_ACCESS, map_id, &access.access_id)?;
        validate_exact_id("travel access place", &access.place_id)?;
        validate_exact_id("travel access anchor", &access.anchor_id)?;
        validate_authored_passability(
            canon,
            ENTITY_ACCESS,
            map_id,
            access.passable,
            &access.blocked_by,
        )?;
        let place = canon.places.get(&access.place_id).ok_or_else(|| {
            TravelGeographyError::UnknownReference {
                entity: ENTITY_ACCESS,
                id: access.access_id.clone(),
                field: "place_id",
                target_id: access.place_id.clone(),
            }
        })?;
        if !place.is_visited() {
            return Err(TravelGeographyError::UnvisitedAccessPlace {
                access_id: access.access_id.clone(),
                place_id: access.place_id.clone(),
            });
        }
        if !canon.travel_anchors.contains_key(&access.anchor_id) {
            return Err(TravelGeographyError::UnknownReference {
                entity: ENTITY_ACCESS,
                id: access.access_id.clone(),
                field: "anchor_id",
                target_id: access.anchor_id.clone(),
            });
        }
        validate_canonical_required_facts(canon, ENTITY_ACCESS, map_id, &access.required_fact_ids)?;
    }

    for (map_id, link) in &canon.travel_links {
        validate_map_identity(ENTITY_LINK, map_id, &link.link_id)?;
        validate_authored_passability(canon, ENTITY_LINK, map_id, link.passable, &link.blocked_by)?;
        link.validate()
            .map_err(|reason| TravelGeographyError::InvalidLink {
                link_id: link.link_id.clone(),
                reason: reason.to_string(),
            })?;
        let anchor_a = canon.travel_anchors.get(&link.anchor_a).ok_or_else(|| {
            TravelGeographyError::UnknownReference {
                entity: ENTITY_LINK,
                id: link.link_id.clone(),
                field: "anchor_a",
                target_id: link.anchor_a.clone(),
            }
        })?;
        let anchor_b = canon.travel_anchors.get(&link.anchor_b).ok_or_else(|| {
            TravelGeographyError::UnknownReference {
                entity: ENTITY_LINK,
                id: link.link_id.clone(),
                field: "anchor_b",
                target_id: link.anchor_b.clone(),
            }
        })?;
        if anchor_a.network_id != anchor_b.network_id {
            return Err(TravelGeographyError::CrossNetworkLink {
                link_id: link.link_id.clone(),
                network_a: anchor_a.network_id.clone(),
                network_b: anchor_b.network_id.clone(),
            });
        }
        validate_canonical_required_facts(canon, ENTITY_LINK, map_id, &link.required_fact_ids)?;
    }
    Ok(())
}

fn validate_map_identity(
    entity: &'static str,
    map_id: &str,
    object_id: &str,
) -> Result<(), TravelGeographyError> {
    validate_exact_id(entity, map_id)?;
    if map_id != object_id {
        return Err(TravelGeographyError::MechanicalConflict {
            entity,
            id: map_id.to_string(),
        });
    }
    Ok(())
}

fn validate_blocker(
    canon: &WorldCanon,
    entity: &'static str,
    id: &str,
    blocker_id: &str,
) -> Result<(), TravelGeographyError> {
    if blocker_id.is_empty() {
        return Ok(());
    }
    validate_exact_id("blocker", blocker_id)?;
    if canon
        .facts
        .get(blocker_id)
        .is_some_and(|fact| fact.fact_id == blocker_id && !matches!(fact.scope, Scope::Rumor))
    {
        Ok(())
    } else {
        Err(TravelGeographyError::InvalidBlocker {
            entity,
            id: id.to_string(),
            blocker_id: blocker_id.to_string(),
        })
    }
}

fn validate_canonical_required_facts(
    canon: &WorldCanon,
    entity: &'static str,
    id: &str,
    fact_ids: &[String],
) -> Result<(), TravelGeographyError> {
    let mut seen = BTreeSet::new();
    for fact_id in fact_ids {
        validate_exact_id("required fact", fact_id)?;
        if !seen.insert(fact_id) {
            return Err(TravelGeographyError::DuplicateRequiredFact {
                entity,
                id: id.to_string(),
                fact_id: fact_id.clone(),
            });
        }
        let exists = canon
            .facts
            .get(fact_id)
            .is_some_and(|fact| fact.fact_id == *fact_id);
        if !exists {
            return Err(TravelGeographyError::UnknownReference {
                entity,
                id: id.to_string(),
                field: "required_fact_ids",
                target_id: fact_id.clone(),
            });
        }
    }
    Ok(())
}

fn scope_exists(canon: &WorldCanon, scope_id: &str) -> bool {
    canon.regions.contains_key(scope_id)
        || canon.settlements.contains_key(scope_id)
        || canon.districts.contains_key(scope_id)
        || canon.places.contains_key(scope_id)
}

fn same_network_mechanics(left: &TravelNetwork, right: &TravelNetwork) -> bool {
    left.network_id == right.network_id
        && left.scope_id == right.scope_id
        && left.default_for_normal_travel == right.default_for_normal_travel
        && left.passable == right.passable
        && left.blocked_by == right.blocked_by
}

fn same_anchor_mechanics(left: &TravelAnchor, right: &TravelAnchor) -> bool {
    left.anchor_id == right.anchor_id
        && left.network_id == right.network_id
        && left.passable == right.passable
        && left.blocked_by == right.blocked_by
}

fn same_access_mechanics(left: &TravelAccess, right: &TravelAccess) -> bool {
    left.access_id == right.access_id
        && left.place_id == right.place_id
        && left.anchor_id == right.anchor_id
        && left.passable == right.passable
        && left.blocked_by == right.blocked_by
        && same_fact_ids(&left.required_fact_ids, &right.required_fact_ids)
}

fn same_link_mechanics(left: &TravelLink, right: &TravelLink) -> bool {
    let same_endpoints = (left.anchor_a == right.anchor_a && left.anchor_b == right.anchor_b)
        || (left.anchor_a == right.anchor_b && left.anchor_b == right.anchor_a);
    left.link_id == right.link_id
        && same_endpoints
        && left.time_cost_minutes == right.time_cost_minutes
        && left.risk == right.risk
        && left.passable == right.passable
        && left.blocked_by == right.blocked_by
        && same_fact_ids(&left.required_fact_ids, &right.required_fact_ids)
}

fn same_fact_ids(left: &[String], right: &[String]) -> bool {
    left.iter().collect::<BTreeSet<_>>() == right.iter().collect::<BTreeSet<_>>()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use gml_world::canon::{
        CanonFact, District, PassageDirectionality, Place, Region, Scope, Settlement, Transition,
    };
    use serde_json::json;

    use super::*;

    fn stage_travel_geography(
        canon: &WorldCanon,
        generated: &Value,
        turn: i64,
    ) -> Result<(WorldCanon, TravelGeographySummary), TravelGeographyError> {
        let policy = TravelGeographyPolicy::for_route(
            canon,
            "origin",
            "destination",
            ["origin".to_string(), "destination".to_string()],
            None,
        );
        super::stage_travel_geography(canon, generated, turn, &policy)
    }

    fn visited_place(place_id: &str) -> Place {
        Place {
            place_id: place_id.to_string(),
            name: place_id.to_string(),
            state_flags: BTreeSet::from(["visited".to_string()]),
            ..Default::default()
        }
    }

    fn base_canon() -> WorldCanon {
        let mut canon = WorldCanon::default();
        canon.insert_place(visited_place("origin"));
        canon.insert_place(visited_place("destination"));
        canon.facts.insert(
            "gate_open".to_string(),
            CanonFact {
                fact_id: "gate_open".to_string(),
                text: "The public gate is open".to_string(),
                scope: Scope::TrueCanon,
            },
        );
        canon
    }

    fn valid_generated() -> Value {
        json!({
            "name": "explicit route",
            "travel_geography": {
                "networks": [{
                    "network_id": "surface",
                    "scope_id": "origin",
                    "default_for_normal_travel": true,
                    "passable": true
                }],
                "anchors": [
                    {"anchor_id": "origin_surface", "network_id": "surface", "passable": true},
                    {"anchor_id": "destination_surface", "network_id": "surface", "passable": true}
                ],
                "accesses": [
                    {
                        "access_id": "origin_access",
                        "place_id": "origin",
                        "anchor_id": "origin_surface",
                        "passable": true,
                        "required_fact_ids": []
                    },
                    {
                        "access_id": "destination_access",
                        "place_id": "destination",
                        "anchor_id": "destination_surface",
                        "passable": true
                    }
                ],
                "links": [{
                    "link_id": "surface_link",
                    "anchor_a": "origin_surface",
                    "anchor_b": "destination_surface",
                    "time_cost_minutes": 24,
                    "risk": "low",
                    "passable": true,
                    "required_fact_ids": []
                }]
            }
        })
    }

    fn install_default_network_access(
        canon: &mut WorldCanon,
        network_id: &str,
        scope_place_id: &str,
        access_place_id: &str,
    ) {
        if !canon.places.contains_key(scope_place_id) {
            canon.insert_place(visited_place(scope_place_id));
        }
        canon.insert_travel_network(TravelNetwork {
            network_id: network_id.to_string(),
            scope_id: scope_place_id.to_string(),
            default_for_normal_travel: true,
            passable: true,
            provenance: Provenance::by("test", "existing default network", 0),
            ..Default::default()
        });
        canon.insert_travel_anchor(TravelAnchor {
            anchor_id: format!("{network_id}_existing_anchor"),
            network_id: network_id.to_string(),
            passable: true,
            provenance: Provenance::by("test", "existing network anchor", 0),
            ..Default::default()
        });
        canon.insert_travel_access(TravelAccess {
            access_id: format!("{network_id}_existing_access"),
            place_id: access_place_id.to_string(),
            anchor_id: format!("{network_id}_existing_anchor"),
            passable: true,
            provenance: Provenance::by("test", "existing network access", 0),
            ..Default::default()
        });
    }

    fn connect_bidirectionally(canon: &mut WorldCanon, left: &str, right: &str) {
        let passage_id = format!("{left}_{right}_passage");
        for (transition_id, from_place, to_place) in [
            (format!("{left}_to_{right}"), left, right),
            (format!("{right}_to_{left}"), right, left),
        ] {
            canon.insert_transition(Transition {
                transition_id,
                passage_id: passage_id.clone(),
                directionality: PassageDirectionality::Bidirectional,
                from_place: from_place.to_string(),
                to_place: to_place.to_string(),
                visible: true,
                passable: true,
                provenance: Provenance::by("test", "open adjacent passage", 0),
                ..Default::default()
            });
        }
    }

    fn generated_existing_network_extension(network_id: &str, scope_id: &str) -> Value {
        json!({
            "name": "extend existing route",
            "travel_geography": {
                "networks": [{
                    "network_id": network_id,
                    "scope_id": scope_id,
                    "default_for_normal_travel": true,
                    "passable": true
                }],
                "anchors": [
                    {
                        "anchor_id": "new_origin_anchor",
                        "network_id": network_id,
                        "passable": true
                    },
                    {
                        "anchor_id": "new_destination_anchor",
                        "network_id": network_id,
                        "passable": true
                    }
                ],
                "accesses": [
                    {
                        "access_id": "new_origin_access",
                        "place_id": "origin",
                        "anchor_id": "new_origin_anchor",
                        "passable": true
                    },
                    {
                        "access_id": "new_destination_access",
                        "place_id": "destination",
                        "anchor_id": "new_destination_anchor",
                        "passable": true
                    }
                ],
                "links": [{
                    "link_id": "new_endpoint_link",
                    "anchor_a": "new_origin_anchor",
                    "anchor_b": "new_destination_anchor",
                    "time_cost_minutes": 24,
                    "risk": "low",
                    "passable": true
                }]
            }
        })
    }

    #[test]
    fn exact_endpoint_access_keeps_an_existing_default_network_inside_the_boundary() {
        let mut canon = base_canon();
        install_default_network_access(
            &mut canon,
            "city_streets",
            "legacy_city_hub",
            "destination",
        );
        let policy = TravelGeographyPolicy::for_route(
            &canon,
            "origin",
            "destination",
            ["origin".to_string(), "destination".to_string()],
            None,
        );

        assert!(policy.allows_existing_network("city_streets"));
        assert!(policy.allowed_scope_ids().contains("legacy_city_hub"));
        let (staged, summary) = super::stage_travel_geography(
            &canon,
            &generated_existing_network_extension("city_streets", "legacy_city_hub"),
            1,
            &policy,
        )
        .expect("extend exact endpoint network");
        assert_eq!(summary.networks.reused, 1);
        assert_eq!(staged.travel_networks.len(), 1);
        assert!(staged.travel_accesses.contains_key("new_origin_access"));
        assert!(staged
            .travel_accesses
            .contains_key("new_destination_access"));
    }

    #[test]
    fn outside_area_endpoint_can_join_a_network_at_one_exact_open_boundary_passage() {
        let mut canon = base_canon();
        install_default_network_access(
            &mut canon,
            "city_streets",
            "legacy_city_hub",
            "legacy_city_hub",
        );
        connect_bidirectionally(&mut canon, "destination", "legacy_city_hub");
        let policy = TravelGeographyPolicy::for_route(
            &canon,
            "origin",
            "destination",
            ["origin".to_string(), "destination".to_string()],
            None,
        );

        assert!(policy.allows_existing_network("city_streets"));
        let (staged, summary) = super::stage_travel_geography(
            &canon,
            &generated_existing_network_extension("city_streets", "legacy_city_hub"),
            1,
            &policy,
        )
        .expect("extend the adjacent city network from an outside endpoint");
        assert_eq!(summary.networks.reused, 1);
        assert_eq!(staged.travel_networks.len(), 1);
    }

    #[test]
    fn unrelated_default_network_without_boundary_access_remains_forbidden() {
        let mut canon = base_canon();
        canon.insert_place(visited_place("unrelated_access_place"));
        install_default_network_access(
            &mut canon,
            "unrelated_network",
            "unrelated_scope",
            "unrelated_access_place",
        );
        let policy = TravelGeographyPolicy::for_route(
            &canon,
            "origin",
            "destination",
            ["origin".to_string(), "destination".to_string()],
            None,
        );

        assert!(!policy.allows_existing_network("unrelated_network"));
        assert!(!policy.allowed_scope_ids().contains("unrelated_scope"));
        assert!(matches!(
            super::stage_travel_geography(
                &canon,
                &generated_existing_network_extension("unrelated_network", "unrelated_scope"),
                1,
                &policy,
            ),
            Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_NETWORK,
                ..
            })
        ));
    }

    #[test]
    fn network_access_two_passages_away_remains_outside_the_boundary() {
        let mut canon = base_canon();
        canon.insert_place(visited_place("intermediate"));
        install_default_network_access(
            &mut canon,
            "two_hops_away",
            "legacy_city_hub",
            "legacy_city_hub",
        );
        connect_bidirectionally(&mut canon, "destination", "intermediate");
        connect_bidirectionally(&mut canon, "intermediate", "legacy_city_hub");

        let policy = TravelGeographyPolicy::for_route(
            &canon,
            "origin",
            "destination",
            ["origin".to_string(), "destination".to_string()],
            None,
        );

        assert!(!policy.allows_existing_network("two_hops_away"));
        assert!(!policy.allowed_scope_ids().contains("legacy_city_hub"));
    }

    #[test]
    fn one_way_or_closed_neighbour_does_not_expand_the_boundary() {
        for (case, directionality, passable, blocked_by) in [
            (
                "one_way",
                PassageDirectionality::OneWay,
                true,
                String::new(),
            ),
            (
                "closed",
                PassageDirectionality::Bidirectional,
                false,
                "gate_open".to_string(),
            ),
        ] {
            let mut canon = base_canon();
            install_default_network_access(
                &mut canon,
                "guarded_network",
                "legacy_city_hub",
                "legacy_city_hub",
            );
            canon.insert_transition(Transition {
                transition_id: format!("destination_to_hub_{case}"),
                passage_id: format!("destination_hub_{case}"),
                directionality,
                from_place: "destination".to_string(),
                to_place: "legacy_city_hub".to_string(),
                visible: true,
                passable,
                blocked_by,
                provenance: Provenance::by("test", "restricted adjacent passage", 0),
                ..Default::default()
            });

            let policy = TravelGeographyPolicy::for_route(
                &canon,
                "origin",
                "destination",
                ["origin".to_string(), "destination".to_string()],
                None,
            );
            assert!(
                !policy.allows_existing_network("guarded_network"),
                "{case} neighbour must not authorize a travel network"
            );
            assert!(
                !policy.allowed_scope_ids().contains("legacy_city_hub"),
                "{case} neighbour must not authorize its scope"
            );
        }
    }

    #[test]
    fn stages_complete_explicit_geography_with_generator_provenance() {
        let canon = base_canon();
        let (staged, summary) = stage_travel_geography(&canon, &valid_generated(), 17).unwrap();

        assert_eq!(summary.total_added(), 6);
        assert_eq!(summary.total_reused(), 0);
        assert!(canon.travel_networks.is_empty());
        assert_eq!(staged.travel_links["surface_link"].time_cost_minutes, 24);
        assert_eq!(
            staged.travel_networks["surface"].provenance,
            Provenance::by("location_generator", "travel geography", 17)
        );
    }

    #[test]
    fn exact_existing_district_is_a_valid_travel_network_scope() {
        let mut canon = base_canon();
        canon.regions.insert(
            "region".to_string(),
            Region {
                region_id: "region".to_string(),
                name: "Region".to_string(),
                settlement_ids: vec!["city".to_string()],
                ..Default::default()
            },
        );
        canon.settlements.insert(
            "city".to_string(),
            Settlement {
                settlement_id: "city".to_string(),
                name: "City".to_string(),
                region_id: "region".to_string(),
                ..Default::default()
            },
        );
        for place_id in ["origin", "destination"] {
            let place = canon.places.get_mut(place_id).expect("endpoint");
            place.region_id = "region".to_string();
            place.district_id = "market_district".to_string();
        }
        canon
            .insert_district(District {
                district_id: "market_district".to_string(),
                name: "Market District".to_string(),
                settlement_id: "city".to_string(),
                region_id: "region".to_string(),
                place_ids: vec!["origin".to_string(), "destination".to_string()],
                ..Default::default()
            })
            .expect("district fixture");

        let mut generated = valid_generated();
        generated["travel_geography"]["networks"][0]["scope_id"] = json!("market_district");
        let policy = TravelGeographyPolicy::for_route(
            &canon,
            "origin",
            "destination",
            ["market_district".to_string()],
            None,
        );

        let (staged, _) =
            super::stage_travel_geography(&canon, &generated, 1, &policy).expect("district scope");
        assert_eq!(
            staged.travel_networks["surface"].scope_id,
            "market_district"
        );
    }

    #[test]
    fn reuses_semantically_identical_mechanics_without_replacing_provenance() {
        let canon = base_canon();
        let (mut existing, _) = stage_travel_geography(&canon, &valid_generated(), 3).unwrap();
        existing
            .travel_links
            .get_mut("surface_link")
            .unwrap()
            .anchor_a = "destination_surface".to_string();
        existing
            .travel_links
            .get_mut("surface_link")
            .unwrap()
            .anchor_b = "origin_surface".to_string();
        let original_provenance = existing.travel_links["surface_link"].provenance.clone();

        let (staged, summary) = stage_travel_geography(&existing, &valid_generated(), 99).unwrap();

        assert_eq!(summary.total_added(), 0);
        assert_eq!(summary.total_reused(), 6);
        assert_eq!(
            staged.travel_links["surface_link"].provenance,
            original_provenance
        );
    }

    #[test]
    fn mechanical_conflict_rejects_the_whole_stage() {
        let canon = base_canon();
        let (existing, _) = stage_travel_geography(&canon, &valid_generated(), 3).unwrap();
        let mut conflicting = valid_generated();
        conflicting["travel_geography"]["links"][0]["time_cost_minutes"] = json!(8);

        let error = stage_travel_geography(&existing, &conflicting, 4).unwrap_err();

        assert_eq!(
            error,
            TravelGeographyError::MechanicalConflict {
                entity: ENTITY_LINK,
                id: "surface_link".to_string()
            }
        );
        assert_eq!(existing.travel_links["surface_link"].time_cost_minutes, 24);
    }

    #[test]
    fn rejects_unvisited_access_place() {
        let mut canon = base_canon();
        canon
            .places
            .get_mut("destination")
            .unwrap()
            .state_flags
            .remove("visited");

        let error = stage_travel_geography(&canon, &valid_generated(), 1).unwrap_err();

        assert_eq!(
            error,
            TravelGeographyError::UnvisitedAccessPlace {
                access_id: "destination_access".to_string(),
                place_id: "destination".to_string()
            }
        );
    }

    #[test]
    fn rejects_unknown_scope_fact_and_blocker_references() {
        let canon = base_canon();
        let mut unknown_scope = valid_generated();
        unknown_scope["travel_geography"]["networks"][0]["scope_id"] = json!("missing");
        assert!(matches!(
            stage_travel_geography(&canon, &unknown_scope, 1),
            Err(TravelGeographyError::UnknownScope { .. })
        ));

        let mut unknown_fact = valid_generated();
        unknown_fact["travel_geography"]["links"][0]["required_fact_ids"] = json!(["missing_fact"]);
        assert!(matches!(
            stage_travel_geography(&canon, &unknown_fact, 1),
            Err(TravelGeographyError::UnknownReference {
                field: "required_fact_ids",
                ..
            })
        ));

        let mut unknown_blocker = valid_generated();
        unknown_blocker["travel_geography"]["anchors"][0]["passable"] = json!(false);
        unknown_blocker["travel_geography"]["anchors"][0]["blocked_by"] = json!("missing_blocker");
        assert!(matches!(
            stage_travel_geography(&canon, &unknown_blocker, 1),
            Err(TravelGeographyError::InvalidBlocker { .. })
        ));
    }

    #[test]
    fn rejects_invalid_link_mechanics_and_cross_network_topology() {
        let canon = base_canon();
        let mut invalid_risk = valid_generated();
        invalid_risk["travel_geography"]["links"][0]["risk"] = json!("Low");
        assert!(matches!(
            stage_travel_geography(&canon, &invalid_risk, 1),
            Err(TravelGeographyError::InvalidLink { .. })
        ));

        let mut zero_duration = valid_generated();
        zero_duration["travel_geography"]["links"][0]["time_cost_minutes"] = json!(0);
        assert!(matches!(
            stage_travel_geography(&canon, &zero_duration, 1),
            Err(TravelGeographyError::InvalidLink { .. })
        ));

        let mut cross_network = valid_generated();
        cross_network["travel_geography"]["networks"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "network_id": "sewer",
                "scope_id": "origin",
                "default_for_normal_travel": false,
                "passable": true
            }));
        cross_network["travel_geography"]["anchors"][1]["network_id"] = json!("sewer");
        assert!(matches!(
            stage_travel_geography(&canon, &cross_network, 1),
            Err(TravelGeographyError::OutsideRequestBoundary { .. })
        ));
    }

    #[test]
    fn parser_rejects_unknown_or_missing_mechanical_fields() {
        let canon = base_canon();
        let mut unknown_field = valid_generated();
        unknown_field["travel_geography"]["links"][0]["label"] = json!("inferred prose");
        assert!(matches!(
            stage_travel_geography(&canon, &unknown_field, 1),
            Err(TravelGeographyError::Malformed(_))
        ));

        let mut missing_passability = valid_generated();
        missing_passability["travel_geography"]["networks"][0]
            .as_object_mut()
            .unwrap()
            .remove("passable");
        assert!(matches!(
            stage_travel_geography(&canon, &missing_passability, 1),
            Err(TravelGeographyError::Malformed(_))
        ));
    }

    #[test]
    fn free_form_unavailability_cannot_override_explicit_geography() {
        let canon = base_canon();
        let mut generated = valid_generated();
        generated["travel_unavailable_reason"] = json!("The city gate is sealed");

        let (staged, _) = stage_travel_geography(&canon, &generated, 1).unwrap();
        assert!(staged.travel_links.contains_key("surface_link"));

        assert_eq!(
            stage_travel_geography(
                &canon,
                &json!({"travel_unavailable_reason": "The city gate is sealed"}),
                1
            )
            .unwrap_err(),
            TravelGeographyError::MissingTravelGeography
        );
    }

    #[test]
    fn impassable_authored_entities_require_an_existing_canonical_blocker() {
        let mut canon = base_canon();
        canon.facts.insert(
            "explicit_route_blocker".to_string(),
            CanonFact {
                fact_id: "explicit_route_blocker".to_string(),
                text: "An explicit mechanical route blocker".to_string(),
                scope: Scope::TrueCanon,
            },
        );
        canon.facts.insert(
            "unverified_route_rumor".to_string(),
            CanonFact {
                fact_id: "unverified_route_rumor".to_string(),
                text: "Someone claims the road is closed".to_string(),
                scope: Scope::Rumor,
            },
        );

        let mut no_blocker = valid_generated();
        no_blocker["travel_geography"]["links"][0]["passable"] = json!(false);
        assert!(matches!(
            stage_travel_geography(&canon, &no_blocker, 1),
            Err(TravelGeographyError::InconsistentPassability {
                entity: ENTITY_LINK,
                ..
            })
        ));

        let mut passable_with_blocker = valid_generated();
        passable_with_blocker["travel_geography"]["links"][0]["blocked_by"] =
            json!("explicit_route_blocker");
        assert!(matches!(
            stage_travel_geography(&canon, &passable_with_blocker, 1),
            Err(TravelGeographyError::InconsistentPassability {
                entity: ENTITY_LINK,
                ..
            })
        ));

        let mut place_as_blocker = valid_generated();
        place_as_blocker["travel_geography"]["links"][0]["passable"] = json!(false);
        place_as_blocker["travel_geography"]["links"][0]["blocked_by"] = json!("origin");
        assert!(matches!(
            stage_travel_geography(&canon, &place_as_blocker, 1),
            Err(TravelGeographyError::InvalidBlocker {
                entity: ENTITY_LINK,
                ..
            })
        ));

        let mut rumor_as_blocker = valid_generated();
        rumor_as_blocker["travel_geography"]["links"][0]["passable"] = json!(false);
        rumor_as_blocker["travel_geography"]["links"][0]["blocked_by"] =
            json!("unverified_route_rumor");
        assert!(matches!(
            stage_travel_geography(&canon, &rumor_as_blocker, 1),
            Err(TravelGeographyError::InvalidBlocker {
                entity: ENTITY_LINK,
                ..
            })
        ));

        let mut explicitly_blocked = valid_generated();
        explicitly_blocked["travel_geography"]["links"][0]["passable"] = json!(false);
        explicitly_blocked["travel_geography"]["links"][0]["blocked_by"] =
            json!("explicit_route_blocker");
        assert!(matches!(
            stage_travel_geography(&canon, &explicitly_blocked, 1),
            Err(TravelGeographyError::NewEntityRestriction {
                entity: ENTITY_LINK,
                ..
            })
        ));

        let (mut existing, _) = stage_travel_geography(&canon, &valid_generated(), 1).unwrap();
        existing
            .travel_links
            .get_mut("surface_link")
            .expect("existing travel link")
            .passable = false;
        existing
            .travel_links
            .get_mut("surface_link")
            .expect("existing travel link")
            .blocked_by = "explicit_route_blocker".to_string();
        let (staged, summary) = stage_travel_geography(&existing, &explicitly_blocked, 2).unwrap();
        assert!(!staged.travel_links["surface_link"].is_available());
        assert_eq!(summary.links.reused, 1);
    }

    #[test]
    fn rejects_inconsistent_passability_already_present_in_staged_graph() {
        let mut canon = base_canon();
        canon.insert_travel_network(TravelNetwork {
            network_id: "legacy_inconsistent".to_string(),
            scope_id: "origin".to_string(),
            default_for_normal_travel: false,
            passable: false,
            blocked_by: String::new(),
            provenance: Provenance::by("test", "legacy fixture", 0),
        });

        assert!(matches!(
            stage_travel_geography(&canon, &valid_generated(), 1),
            Err(TravelGeographyError::InconsistentPassability {
                entity: ENTITY_NETWORK,
                id
            }) if id == "legacy_inconsistent"
        ));
    }

    #[test]
    fn request_policy_rejects_unrelated_exact_ids_and_new_requirements() {
        let mut canon = base_canon();
        canon.insert_place(visited_place("unrelated_visited_place"));

        let mut unrelated_scope = valid_generated();
        unrelated_scope["travel_geography"]["networks"][0]["scope_id"] =
            json!("unrelated_visited_place");
        assert!(matches!(
            stage_travel_geography(&canon, &unrelated_scope, 1),
            Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_NETWORK,
                field: "scope_id",
                ..
            })
        ));

        let mut unrelated_access = valid_generated();
        unrelated_access["travel_geography"]["accesses"][0]["place_id"] =
            json!("unrelated_visited_place");
        assert!(matches!(
            stage_travel_geography(&canon, &unrelated_access, 1),
            Err(TravelGeographyError::OutsideRequestBoundary {
                entity: ENTITY_ACCESS,
                field: "place_id",
                ..
            })
        ));

        let mut required_fact = valid_generated();
        required_fact["travel_geography"]["links"][0]["required_fact_ids"] = json!(["gate_open"]);
        assert!(matches!(
            stage_travel_geography(&canon, &required_fact, 1),
            Err(TravelGeographyError::NewEntityRestriction {
                entity: ENTITY_LINK,
                ..
            })
        ));
    }
}
