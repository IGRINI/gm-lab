//! Canonical long-distance navigation, separate from immediate scene exits.
//!
//! [`super::Transition`] remains the physical, local edge used for doors,
//! stairs, and adjacent playable places.  This module models persistent travel
//! corridors between explicit anchors.  Anchors do not have to be playable
//! places, so a journey can cross ungenerated streets or countryside without
//! manufacturing scene nodes for every step.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, BinaryHeap},
    error::Error,
    fmt,
};

use serde::{Deserialize, Serialize};

use super::{Provenance, TravelRisk, WorldCanon};

/// A separately routable travel layer, such as a city's public surface routes.
///
/// `default_for_normal_travel` is the only input used to choose a network when
/// the caller does not request one.  Names and labels are deliberately absent:
/// route selection never guesses a network from prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TravelNetwork {
    #[serde(default)]
    pub network_id: String,
    /// Explicit owning settlement/region/other canonical scope id.
    #[serde(default)]
    pub scope_id: String,
    #[serde(default)]
    pub default_for_normal_travel: bool,
    #[serde(default)]
    pub passable: bool,
    /// Exact non-rumor canonical fact id. Empty exactly when `passable` is true.
    #[serde(default)]
    pub blocked_by: String,
    #[serde(default)]
    pub provenance: Provenance,
}

impl TravelNetwork {
    pub fn is_available(&self) -> bool {
        self.passable && self.blocked_by.is_empty()
    }
}

/// An abstract endpoint or junction inside one [`TravelNetwork`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TravelAnchor {
    #[serde(default)]
    pub anchor_id: String,
    #[serde(default)]
    pub network_id: String,
    #[serde(default)]
    pub passable: bool,
    /// Exact non-rumor canonical fact id. Empty exactly when `passable` is true.
    #[serde(default)]
    pub blocked_by: String,
    #[serde(default)]
    pub provenance: Provenance,
}

impl TravelAnchor {
    pub fn is_available(&self) -> bool {
        self.passable && self.blocked_by.is_empty()
    }
}

/// Explicit permission for a playable place to start/end travel at an anchor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TravelAccess {
    #[serde(default)]
    pub access_id: String,
    #[serde(default)]
    pub place_id: String,
    #[serde(default)]
    pub anchor_id: String,
    #[serde(default)]
    pub passable: bool,
    /// Exact non-rumor canonical fact id. Empty exactly when `passable` is true.
    #[serde(default)]
    pub blocked_by: String,
    /// Canonical fact ids whose truth has already been reflected in `passable`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_fact_ids: Vec<String>,
    #[serde(default)]
    pub provenance: Provenance,
}

impl TravelAccess {
    pub fn is_available(&self) -> bool {
        self.passable && self.blocked_by.is_empty()
    }
}

/// One persistent, undirected corridor between two anchors.
///
/// There is one duration and one risk profile for both directions. Directional
/// exceptions belong in explicit world state, not in a duplicated reverse link.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TravelLink {
    #[serde(default)]
    pub link_id: String,
    #[serde(default)]
    pub anchor_a: String,
    #[serde(default)]
    pub anchor_b: String,
    #[serde(default)]
    pub time_cost_minutes: i64,
    #[serde(default)]
    pub risk: String,
    #[serde(default)]
    pub passable: bool,
    /// Exact non-rumor canonical fact id. Empty exactly when `passable` is true.
    #[serde(default)]
    pub blocked_by: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_fact_ids: Vec<String>,
    #[serde(default)]
    pub provenance: Provenance,
}

impl TravelLink {
    /// Return the opposite endpoint without assigning a direction to the link.
    pub fn other_anchor(&self, anchor_id: &str) -> Option<&str> {
        if anchor_id == self.anchor_a {
            Some(self.anchor_b.as_str())
        } else if anchor_id == self.anchor_b {
            Some(self.anchor_a.as_str())
        } else {
            None
        }
    }

    /// Validate the complete mechanical profile and return its exact risk.
    pub fn validate(&self) -> Result<TravelRisk, TravelLinkValidationError> {
        if self.link_id.is_empty() {
            return Err(TravelLinkValidationError::MissingLinkId);
        }
        if self.anchor_a.is_empty() || self.anchor_b.is_empty() {
            return Err(TravelLinkValidationError::MissingAnchorId);
        }
        if self.anchor_a == self.anchor_b {
            return Err(TravelLinkValidationError::SameAnchor);
        }
        if self.time_cost_minutes <= 0 {
            return Err(TravelLinkValidationError::NonPositiveTime);
        }
        TravelRisk::parse(&self.risk).ok_or(TravelLinkValidationError::InvalidRisk)
    }

    pub fn is_available(&self) -> bool {
        self.passable && self.blocked_by.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TravelLinkValidationError {
    MissingLinkId,
    MissingAnchorId,
    SameAnchor,
    NonPositiveTime,
    InvalidRisk,
}

impl fmt::Display for TravelLinkValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingLinkId => "link_id is empty",
            Self::MissingAnchorId => "an anchor id is empty",
            Self::SameAnchor => "both endpoints name the same anchor",
            Self::NonPositiveTime => "time_cost_minutes must be positive",
            Self::InvalidRisk => "risk must be exactly none, low, medium, high, or certain",
        };
        f.write_str(message)
    }
}

impl Error for TravelLinkValidationError {}

/// A deterministic route preview over already-authored canonical travel data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TravelPlan {
    #[serde(default)]
    pub origin_place_id: String,
    #[serde(default)]
    pub destination_place_id: String,
    #[serde(default)]
    pub network_id: String,
    #[serde(default)]
    pub origin_access_id: String,
    #[serde(default)]
    pub destination_access_id: String,
    #[serde(default)]
    pub anchor_ids: Vec<String>,
    #[serde(default)]
    pub link_ids: Vec<String>,
    #[serde(default)]
    pub total_time_minutes: i64,
    /// Highest exact risk among the selected links, for route presentation.
    #[serde(default)]
    pub risk: String,
}

/// Persistent progress for a journey interrupted between playable places.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ActiveJourney {
    #[serde(default)]
    pub journey_id: String,
    #[serde(default)]
    pub origin_place_id: String,
    #[serde(default)]
    pub destination_place_id: String,
    #[serde(default)]
    pub network_id: String,
    #[serde(default)]
    pub origin_access_id: String,
    #[serde(default)]
    pub destination_access_id: String,
    #[serde(default)]
    pub anchor_ids: Vec<String>,
    #[serde(default)]
    pub link_ids: Vec<String>,
    #[serde(default)]
    pub next_link_index: usize,
    #[serde(default)]
    pub remaining_minutes_on_link: i64,
    #[serde(default)]
    pub elapsed_minutes: i64,
    /// Generated travel-situation place id, empty while movement is uninterrupted.
    #[serde(default)]
    pub interruption_place_id: String,
}

impl ActiveJourney {
    pub fn from_plan(
        canon: &WorldCanon,
        journey_id: impl Into<String>,
        plan: &TravelPlan,
    ) -> Result<Self, TravelPlanError> {
        let journey_id = journey_id.into();
        if journey_id.is_empty() {
            return Err(TravelPlanError::InvalidJourneyId);
        }
        let first_link_id = plan.link_ids.first().ok_or(TravelPlanError::InvalidPlan)?;
        let first_link = canon
            .travel_links
            .get(first_link_id)
            .ok_or_else(|| TravelPlanError::UnknownPlanLink(first_link_id.clone()))?;
        first_link
            .validate()
            .map_err(|reason| TravelPlanError::InvalidLink {
                link_id: first_link_id.clone(),
                reason,
            })?;

        Ok(Self {
            journey_id,
            origin_place_id: plan.origin_place_id.clone(),
            destination_place_id: plan.destination_place_id.clone(),
            network_id: plan.network_id.clone(),
            origin_access_id: plan.origin_access_id.clone(),
            destination_access_id: plan.destination_access_id.clone(),
            anchor_ids: plan.anchor_ids.clone(),
            link_ids: plan.link_ids.clone(),
            next_link_index: 0,
            remaining_minutes_on_link: first_link.time_cost_minutes,
            elapsed_minutes: 0,
            interruption_place_id: String::new(),
        })
    }

    pub fn interrupt_at(&mut self, place_id: impl Into<String>) {
        self.interruption_place_id = place_id.into();
    }

    pub fn resume(&mut self) {
        self.interruption_place_id.clear();
    }

    pub fn is_interrupted(&self) -> bool {
        !self.interruption_place_id.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TravelPlanError {
    UnknownOrigin(String),
    UnknownDestination(String),
    DestinationNotVisited(String),
    AlreadyAtDestination(String),
    UnknownNetwork(String),
    NetworkUnavailable(String),
    NoDefaultNetwork,
    InvalidNetworkIdentity(String),
    InvalidAnchorIdentity(String),
    InvalidAccessIdentity(String),
    InvalidLink {
        link_id: String,
        reason: TravelLinkValidationError,
    },
    InvalidLinkTopology(String),
    NoRoute {
        origin_place_id: String,
        destination_place_id: String,
    },
    RouteCostOverflow,
    InvalidJourneyId,
    InvalidPlan,
    UnknownPlanLink(String),
}

impl fmt::Display for TravelPlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOrigin(id) => write!(f, "unknown travel origin '{id}'"),
            Self::UnknownDestination(id) => write!(f, "unknown travel destination '{id}'"),
            Self::DestinationNotVisited(id) => {
                write!(f, "travel destination '{id}' has not been visited")
            }
            Self::AlreadyAtDestination(id) => write!(f, "player is already at '{id}'"),
            Self::UnknownNetwork(id) => write!(f, "unknown travel network '{id}'"),
            Self::NetworkUnavailable(id) => write!(f, "travel network '{id}' is unavailable"),
            Self::NoDefaultNetwork => f.write_str("no available default travel network exists"),
            Self::InvalidNetworkIdentity(id) => {
                write!(f, "travel network '{id}' has an invalid identity")
            }
            Self::InvalidAnchorIdentity(id) => {
                write!(f, "travel anchor '{id}' has an invalid identity")
            }
            Self::InvalidAccessIdentity(id) => {
                write!(f, "travel access '{id}' has an invalid identity")
            }
            Self::InvalidLink { link_id, reason } => {
                write!(f, "travel link '{link_id}' is invalid: {reason}")
            }
            Self::InvalidLinkTopology(id) => {
                write!(f, "travel link '{id}' does not belong to one network")
            }
            Self::NoRoute {
                origin_place_id,
                destination_place_id,
            } => write!(
                f,
                "no available travel route from '{origin_place_id}' to '{destination_place_id}'"
            ),
            Self::RouteCostOverflow => f.write_str("travel route duration overflowed"),
            Self::InvalidJourneyId => f.write_str("journey_id is empty"),
            Self::InvalidPlan => f.write_str("travel plan has no traversable links"),
            Self::UnknownPlanLink(id) => write!(f, "travel plan references unknown link '{id}'"),
        }
    }
}

impl Error for TravelPlanError {}

/// Plan normal travel from the player's current canonical place.
///
/// Without `requested_network_id`, only networks explicitly marked
/// `default_for_normal_travel` are considered. A non-default network is never a
/// silent fallback, even when its route is shorter.
pub fn plan_travel(
    canon: &WorldCanon,
    destination_place_id: &str,
    requested_network_id: Option<&str>,
) -> Result<TravelPlan, TravelPlanError> {
    plan_travel_from(
        canon,
        &canon.player_place_id,
        destination_place_id,
        requested_network_id,
    )
}

/// Plan travel between two explicit playable place ids.
pub fn plan_travel_from(
    canon: &WorldCanon,
    origin_place_id: &str,
    destination_place_id: &str,
    requested_network_id: Option<&str>,
) -> Result<TravelPlan, TravelPlanError> {
    if canon.place(origin_place_id).is_none() {
        return Err(TravelPlanError::UnknownOrigin(origin_place_id.to_string()));
    }
    let destination = canon
        .place(destination_place_id)
        .ok_or_else(|| TravelPlanError::UnknownDestination(destination_place_id.to_string()))?;
    if !destination.is_visited() {
        return Err(TravelPlanError::DestinationNotVisited(
            destination_place_id.to_string(),
        ));
    }
    if origin_place_id == destination_place_id {
        return Err(TravelPlanError::AlreadyAtDestination(
            destination_place_id.to_string(),
        ));
    }

    let network_ids = eligible_network_ids(canon, requested_network_id)?;
    let mut best: Option<TravelPlan> = None;
    for network_id in network_ids {
        if let Some(candidate) =
            plan_in_network(canon, origin_place_id, destination_place_id, &network_id)?
        {
            let replace = best
                .as_ref()
                .is_none_or(|current| plan_sort_key(&candidate) < plan_sort_key(current));
            if replace {
                best = Some(candidate);
            }
        }
    }

    best.ok_or_else(|| TravelPlanError::NoRoute {
        origin_place_id: origin_place_id.to_string(),
        destination_place_id: destination_place_id.to_string(),
    })
}

fn eligible_network_ids(
    canon: &WorldCanon,
    requested_network_id: Option<&str>,
) -> Result<Vec<String>, TravelPlanError> {
    if let Some(network_id) = requested_network_id {
        let network = canon
            .travel_networks
            .get(network_id)
            .ok_or_else(|| TravelPlanError::UnknownNetwork(network_id.to_string()))?;
        validate_network_identity(network_id, network)?;
        if !network.is_available() {
            return Err(TravelPlanError::NetworkUnavailable(network_id.to_string()));
        }
        return Ok(vec![network_id.to_string()]);
    }

    let mut network_ids = Vec::new();
    for (network_id, network) in &canon.travel_networks {
        validate_network_identity(network_id, network)?;
        if network.default_for_normal_travel && network.is_available() {
            network_ids.push(network_id.clone());
        }
    }
    if network_ids.is_empty() {
        return Err(TravelPlanError::NoDefaultNetwork);
    }
    Ok(network_ids)
}

fn validate_network_identity(map_id: &str, network: &TravelNetwork) -> Result<(), TravelPlanError> {
    if map_id.is_empty() || network.network_id != map_id {
        return Err(TravelPlanError::InvalidNetworkIdentity(map_id.to_string()));
    }
    Ok(())
}

#[derive(Clone)]
struct RouteEdge {
    neighbor: String,
    link_id: String,
    minutes: i64,
    risk: TravelRisk,
}

fn plan_in_network(
    canon: &WorldCanon,
    origin_place_id: &str,
    destination_place_id: &str,
    network_id: &str,
) -> Result<Option<TravelPlan>, TravelPlanError> {
    let mut anchors = BTreeMap::new();
    for (map_id, anchor) in &canon.travel_anchors {
        if anchor.network_id != network_id {
            continue;
        }
        if map_id.is_empty() || anchor.anchor_id != *map_id || anchor.network_id.is_empty() {
            return Err(TravelPlanError::InvalidAnchorIdentity(map_id.clone()));
        }
        if anchor.is_available() {
            anchors.insert(map_id.clone(), anchor);
        }
    }

    let origin_accesses = available_accesses(canon, origin_place_id, &anchors)?;
    let destination_accesses = available_accesses(canon, destination_place_id, &anchors)?;
    if origin_accesses.is_empty() || destination_accesses.is_empty() {
        return Ok(None);
    }

    let mut graph: BTreeMap<String, Vec<RouteEdge>> = anchors
        .keys()
        .cloned()
        .map(|anchor_id| (anchor_id, Vec::new()))
        .collect();
    for (map_id, link) in &canon.travel_links {
        let endpoint_a = canon.travel_anchors.get(&link.anchor_a);
        let endpoint_b = canon.travel_anchors.get(&link.anchor_b);
        let touches_network = endpoint_a.is_some_and(|a| a.network_id == network_id)
            || endpoint_b.is_some_and(|b| b.network_id == network_id);
        if !touches_network {
            continue;
        }
        if map_id.is_empty() || link.link_id != *map_id {
            return Err(TravelPlanError::InvalidLink {
                link_id: map_id.clone(),
                reason: TravelLinkValidationError::MissingLinkId,
            });
        }
        let risk = link
            .validate()
            .map_err(|reason| TravelPlanError::InvalidLink {
                link_id: map_id.clone(),
                reason,
            })?;
        let (Some(endpoint_a), Some(endpoint_b)) = (endpoint_a, endpoint_b) else {
            return Err(TravelPlanError::InvalidLinkTopology(map_id.clone()));
        };
        if endpoint_a.network_id != network_id || endpoint_b.network_id != network_id {
            return Err(TravelPlanError::InvalidLinkTopology(map_id.clone()));
        }
        if !link.is_available()
            || !anchors.contains_key(&link.anchor_a)
            || !anchors.contains_key(&link.anchor_b)
        {
            continue;
        }
        graph
            .get_mut(&link.anchor_a)
            .expect("available anchor is present in route graph")
            .push(RouteEdge {
                neighbor: link.anchor_b.clone(),
                link_id: map_id.clone(),
                minutes: link.time_cost_minutes,
                risk,
            });
        graph
            .get_mut(&link.anchor_b)
            .expect("available anchor is present in route graph")
            .push(RouteEdge {
                neighbor: link.anchor_a.clone(),
                link_id: map_id.clone(),
                minutes: link.time_cost_minutes,
                risk,
            });
    }
    for edges in graph.values_mut() {
        edges.sort_by(|left, right| {
            (&left.link_id, &left.neighbor).cmp(&(&right.link_id, &right.neighbor))
        });
    }

    let mut distances: BTreeMap<String, i64> = BTreeMap::new();
    let mut predecessors: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut heap = BinaryHeap::new();
    let mut origin_access_by_anchor = BTreeMap::new();
    for (access_id, anchor_id) in origin_accesses {
        origin_access_by_anchor
            .entry(anchor_id.clone())
            .and_modify(|current: &mut String| {
                if access_id < *current {
                    *current = access_id.clone();
                }
            })
            .or_insert(access_id);
        if distances.insert(anchor_id.clone(), 0).is_none() {
            heap.push(Reverse((0_i64, anchor_id)));
        }
    }

    while let Some(Reverse((distance, anchor_id))) = heap.pop() {
        if distances.get(&anchor_id).copied() != Some(distance) {
            continue;
        }
        for edge in graph.get(&anchor_id).into_iter().flatten() {
            let candidate = distance
                .checked_add(edge.minutes)
                .ok_or(TravelPlanError::RouteCostOverflow)?;
            let should_update = match distances.get(&edge.neighbor).copied() {
                None => true,
                Some(current) if candidate < current => true,
                Some(current) if candidate == current => {
                    predecessors.get(&edge.neighbor).is_none_or(|previous| {
                        (&edge.link_id, &anchor_id) < (&previous.1, &previous.0)
                    })
                }
                Some(_) => false,
            };
            if should_update {
                distances.insert(edge.neighbor.clone(), candidate);
                predecessors.insert(
                    edge.neighbor.clone(),
                    (anchor_id.clone(), edge.link_id.clone()),
                );
                heap.push(Reverse((candidate, edge.neighbor.clone())));
            }
        }
    }

    let destination_access_by_anchor = destination_accesses.into_iter().fold(
        BTreeMap::<String, String>::new(),
        |mut by_anchor, (access_id, anchor_id)| {
            by_anchor
                .entry(anchor_id)
                .and_modify(|current| {
                    if access_id < *current {
                        *current = access_id.clone();
                    }
                })
                .or_insert(access_id);
            by_anchor
        },
    );
    let destination_anchor = destination_access_by_anchor
        .keys()
        .filter_map(|anchor_id| {
            distances
                .get(anchor_id)
                .copied()
                .map(|minutes| (minutes, anchor_id.clone()))
        })
        .filter(|(minutes, _)| *minutes > 0)
        .min();
    let Some((total_time_minutes, destination_anchor)) = destination_anchor else {
        return Ok(None);
    };

    let mut anchor_ids = vec![destination_anchor.clone()];
    let mut link_ids = Vec::new();
    let mut cursor = destination_anchor.clone();
    while let Some((previous_anchor, link_id)) = predecessors.get(&cursor) {
        link_ids.push(link_id.clone());
        anchor_ids.push(previous_anchor.clone());
        cursor = previous_anchor.clone();
    }
    anchor_ids.reverse();
    link_ids.reverse();
    let Some(origin_access_id) = origin_access_by_anchor.get(&cursor).cloned() else {
        return Ok(None);
    };
    let destination_access_id = destination_access_by_anchor
        .get(&destination_anchor)
        .cloned()
        .expect("selected destination anchor has an access");

    let risk = highest_route_risk(&link_ids, &graph);
    Ok(Some(TravelPlan {
        origin_place_id: origin_place_id.to_string(),
        destination_place_id: destination_place_id.to_string(),
        network_id: network_id.to_string(),
        origin_access_id,
        destination_access_id,
        anchor_ids,
        link_ids,
        total_time_minutes,
        risk: risk.as_str().to_string(),
    }))
}

fn available_accesses(
    canon: &WorldCanon,
    place_id: &str,
    anchors: &BTreeMap<String, &TravelAnchor>,
) -> Result<Vec<(String, String)>, TravelPlanError> {
    let mut accesses = Vec::new();
    for (map_id, access) in &canon.travel_accesses {
        if access.place_id != place_id {
            continue;
        }
        if map_id.is_empty()
            || access.access_id != *map_id
            || access.place_id.is_empty()
            || access.anchor_id.is_empty()
        {
            return Err(TravelPlanError::InvalidAccessIdentity(map_id.clone()));
        }
        if access.is_available() && anchors.contains_key(&access.anchor_id) {
            accesses.push((map_id.clone(), access.anchor_id.clone()));
        }
    }
    Ok(accesses)
}

fn highest_route_risk(link_ids: &[String], graph: &BTreeMap<String, Vec<RouteEdge>>) -> TravelRisk {
    let selected: BTreeSet<&str> = link_ids.iter().map(String::as_str).collect();
    graph
        .values()
        .flatten()
        .filter(|edge| selected.contains(edge.link_id.as_str()))
        .map(|edge| edge.risk)
        .max_by_key(|risk| risk_rank(*risk))
        .unwrap_or(TravelRisk::None)
}

const fn risk_rank(risk: TravelRisk) -> u8 {
    match risk {
        TravelRisk::None => 0,
        TravelRisk::Low => 1,
        TravelRisk::Medium => 2,
        TravelRisk::High => 3,
        TravelRisk::Certain => 4,
    }
}

fn plan_sort_key(plan: &TravelPlan) -> (i64, &str, &[String]) {
    (
        plan.total_time_minutes,
        plan.network_id.as_str(),
        plan.link_ids.as_slice(),
    )
}
