//! The validator (TZ §8.3): the mandatory gate between an LLM/GM *proposal* and
//! a canon *commit*. A schema-valid action can still be world-invalid, so this
//! layer enforces the world invariants — unique/stable ids, references resolve,
//! no traversal through a closed blocker, no impossible actor moves, timeline
//! sanity, no hidden-knowledge leak, no contradiction with committed canon.
//!
//! On rejection the engine returns a compact [`Rejection`] so the GM can repair
//! or fall back (TZ §8.3 last paragraph).

use serde::{Deserialize, Serialize};

use super::action::Action;
use super::knowledge::Scope;
use super::navigation::{self, TravelPlanError};
use super::travel::{self, TravelRisk};
use super::{PassageDirectionality, Transition, WorldCanon};

/// A compact, model-facing rejection reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    /// Machine code (e.g. `unknown_transition`, `blocked`, `duplicate_id`).
    pub code: String,
    /// Short human/model-readable explanation.
    pub reason: String,
}

impl Rejection {
    fn new(code: &str, reason: impl Into<String>) -> Self {
        Rejection {
            code: code.to_string(),
            reason: reason.into(),
        }
    }
}

/// Stateless validator over the current canon.
pub struct Validator;

impl Validator {
    /// Validate a proposed action against the committed canon. `Ok(())` means
    /// the engine may apply it.
    pub fn validate(canon: &WorldCanon, action: &Action) -> Result<(), Rejection> {
        match action {
            Action::MovePlayer { transition_id } => {
                let t = canon.transition(transition_id).ok_or_else(|| {
                    Rejection::new(
                        "unknown_transition",
                        format!("no transition '{transition_id}'"),
                    )
                })?;
                if t.from_place != canon.player_place_id {
                    return Err(Rejection::new(
                        "not_here",
                        format!(
                            "transition '{transition_id}' starts at '{}', player is at '{}'",
                            t.from_place, canon.player_place_id
                        ),
                    ));
                }
                if !t.visible {
                    return Err(Rejection::new("hidden_exit", "that exit is not visible"));
                }
                if !t.passable || !t.blocked_by.is_empty() {
                    return Err(Rejection::new(
                        "blocked",
                        format!(
                            "the way is blocked: {}",
                            nonempty(&t.blocked_by, "impassable")
                        ),
                    ));
                }
                if !t.has_explicit_passage_profile() {
                    return Err(Rejection::new(
                        "needs_transition_profile",
                        format!(
                            "transition '{transition_id}' requires an explicit passage_id and directionality"
                        ),
                    ));
                }
                if t.to_place.is_empty() {
                    return Err(Rejection::new(
                        "needs_transition_profile",
                        format!("transition '{transition_id}' has no configured target"),
                    ));
                }
                if !canon.places.contains_key(&t.to_place) {
                    return Err(Rejection::new(
                        "dangling_target",
                        format!("transition target '{}' does not exist", t.to_place),
                    ));
                }
                if canon
                    .place(&t.to_place)
                    .is_some_and(|place| place.has_flag("shell"))
                {
                    return Err(Rejection::new(
                        "needs_location_generation",
                        format!(
                            "transition target '{}' must be completed by the location creator",
                            t.to_place
                        ),
                    ));
                }
                if let Some(problem) =
                    transition_profile_problem(&t.label, &t.kind, t.time_cost, &t.risk)
                {
                    return Err(Rejection::new(
                        "needs_transition_profile",
                        format!("transition '{transition_id}' {problem}"),
                    ));
                }
                if travel::has_asymmetric_reciprocal_profile(canon, transition_id) {
                    return Err(Rejection::new(
                        "needs_transition_profile",
                        format!(
                            "transition '{transition_id}' disagrees with its reciprocal route profile"
                        ),
                    ));
                }
                Ok(())
            }
            Action::TravelPlayer {
                destination_place_id,
                network_id,
            } => {
                if network_id.as_ref().is_some_and(|id| id.trim().is_empty()) {
                    return Err(Rejection::new(
                        "invalid_travel_network",
                        "network_id cannot be empty when provided",
                    ));
                }
                navigation::plan_travel(canon, destination_place_id, network_id.as_deref())
                    .map(|_| ())
                    .map_err(|error| {
                        let code = match &error {
                            TravelPlanError::UnknownOrigin(_)
                            | TravelPlanError::UnknownDestination(_) => {
                                "unknown_travel_destination"
                            }
                            TravelPlanError::DestinationNotVisited(_) => "destination_not_visited",
                            TravelPlanError::AlreadyAtDestination(_) => "already_at_destination",
                            TravelPlanError::UnknownNetwork(_) => "unknown_travel_network",
                            TravelPlanError::NetworkUnavailable(_) => "travel_network_unavailable",
                            TravelPlanError::NoDefaultNetwork | TravelPlanError::NoRoute { .. } => {
                                "travel_route_unavailable"
                            }
                            _ => "invalid_travel_graph",
                        };
                        Rejection::new(code, error.to_string())
                    })
            }
            Action::RelocatePlayer {
                destination_place_id,
                elapsed_minutes,
            } => {
                if !is_exact_nonempty(destination_place_id) {
                    return Err(Rejection::new(
                        "invalid_relocation_destination",
                        "destination_place_id must be a non-empty exact id",
                    ));
                }
                if *elapsed_minutes < 0 {
                    return Err(Rejection::new(
                        "negative_time",
                        "relocation elapsed_minutes cannot be negative",
                    ));
                }
                if destination_place_id == &canon.player_place_id {
                    return Err(Rejection::new(
                        "already_at_destination",
                        format!("player is already at '{destination_place_id}'"),
                    ));
                }
                let destination = canon.place(destination_place_id).ok_or_else(|| {
                    Rejection::new(
                        "unknown_relocation_destination",
                        format!("place '{destination_place_id}' does not exist"),
                    )
                })?;
                if destination.has_flag("shell") {
                    return Err(Rejection::new(
                        "incomplete_relocation_destination",
                        format!("place '{destination_place_id}' is an incomplete shell"),
                    ));
                }
                Ok(())
            }
            Action::CreatePlace {
                place_id,
                parent,
                region_id,
                district_id,
                ..
            } => {
                if place_id.is_empty() {
                    return Err(Rejection::new("empty_id", "place_id is required"));
                }
                if canon.places.contains_key(place_id) {
                    return Err(Rejection::new(
                        "duplicate_id",
                        format!("place '{place_id}' already exists"),
                    ));
                }
                if !parent.is_empty()
                    && !canon.places.contains_key(parent)
                    && !canon.settlements.contains_key(parent)
                    && !canon.districts.contains_key(parent)
                    && !canon.regions.contains_key(parent)
                {
                    return Err(Rejection::new(
                        "unknown_parent",
                        format!("parent '{parent}' does not exist"),
                    ));
                }
                if !district_id.is_empty() {
                    let Some(district) = canon.district(district_id) else {
                        return Err(Rejection::new(
                            "unknown_district",
                            format!("district '{district_id}' does not exist"),
                        ));
                    };
                    if !canon.settlements.contains_key(&district.settlement_id) {
                        return Err(Rejection::new(
                            "invalid_district",
                            format!(
                                "district '{}' references unknown settlement '{}'",
                                district.district_id, district.settlement_id
                            ),
                        ));
                    }
                    if !district.region_id.is_empty()
                        && !canon.regions.contains_key(&district.region_id)
                    {
                        return Err(Rejection::new(
                            "invalid_district",
                            format!(
                                "district '{}' references unknown region '{}'",
                                district.district_id, district.region_id
                            ),
                        ));
                    }
                    if !region_id.is_empty()
                        && !district.region_id.is_empty()
                        && region_id != &district.region_id
                    {
                        return Err(Rejection::new(
                            "district_region_mismatch",
                            format!(
                                "place region '{region_id}' does not match district '{}' region '{}'",
                                district.district_id, district.region_id
                            ),
                        ));
                    }
                    if let Some(parent_district) = canon.district(parent) {
                        if parent_district.district_id != *district_id {
                            return Err(Rejection::new(
                                "district_parent_mismatch",
                                format!(
                                    "parent district '{}' does not match place district '{district_id}'",
                                    parent_district.district_id
                                ),
                            ));
                        }
                    }
                    if let Some(parent_settlement) = canon.settlement(parent) {
                        if parent_settlement.settlement_id != district.settlement_id {
                            return Err(Rejection::new(
                                "district_parent_mismatch",
                                format!(
                                    "parent settlement '{}' does not own district '{district_id}'",
                                    parent_settlement.settlement_id
                                ),
                            ));
                        }
                    }
                    if let Some(parent_place) = canon.place(parent) {
                        if !parent_place.district_id.is_empty()
                            && parent_place.district_id != *district_id
                        {
                            return Err(Rejection::new(
                                "district_parent_mismatch",
                                format!(
                                    "parent place '{}' belongs to district '{}', not '{district_id}'",
                                    parent_place.place_id, parent_place.district_id
                                ),
                            ));
                        }
                    }
                } else if canon.districts.contains_key(parent) {
                    return Err(Rejection::new(
                        "missing_district",
                        format!(
                            "place with district parent '{parent}' requires the same explicit district_id"
                        ),
                    ));
                }
                Ok(())
            }
            Action::UpdatePlace { place_id, .. } => {
                if place_id.is_empty() {
                    return Err(Rejection::new("empty_id", "place_id is required"));
                }
                if !canon.places.contains_key(place_id) {
                    return Err(Rejection::new(
                        "unknown_place",
                        format!("place '{place_id}' does not exist"),
                    ));
                }
                Ok(())
            }
            Action::CreateTransition {
                transition_id,
                passage_id,
                directionality,
                from_place,
                to_place,
                label,
                kind,
                time_cost,
                risk,
                ..
            } => {
                if transition_id.is_empty() {
                    return Err(Rejection::new("empty_id", "transition_id is required"));
                }
                if canon.transitions.contains_key(transition_id) {
                    return Err(Rejection::new(
                        "duplicate_id",
                        format!("transition '{transition_id}' already exists"),
                    ));
                }
                if !canon.places.contains_key(from_place) {
                    return Err(Rejection::new(
                        "unknown_from",
                        format!("from_place '{from_place}' does not exist"),
                    ));
                }
                if !to_place.is_empty() && !canon.places.contains_key(to_place) {
                    return Err(Rejection::new(
                        "unknown_target",
                        format!("to_place '{to_place}' must be created first (no implicit places)"),
                    ));
                }
                if let Some(problem) = transition_passage_problem(
                    canon,
                    transition_id,
                    from_place,
                    to_place,
                    passage_id,
                    *directionality,
                ) {
                    return Err(Rejection::new(
                        "invalid_transition_passage",
                        format!("transition '{transition_id}' {problem}"),
                    ));
                }
                if let Some(problem) = transition_profile_problem(label, kind, *time_cost, risk) {
                    return Err(Rejection::new(
                        "invalid_transition_profile",
                        format!("transition '{transition_id}' {problem}"),
                    ));
                }
                Ok(())
            }
            Action::CreatePassage {
                passage_id,
                directionality,
                forward_transition_id,
                reverse_transition_id,
                from_place,
                to_place,
                forward_label,
                reverse_label,
                kind,
                time_cost,
                risk,
            } => {
                if !is_exact_nonempty(passage_id) {
                    return Err(Rejection::new(
                        "invalid_passage_id",
                        "passage_id must be a non-empty exact id",
                    ));
                }
                if canon
                    .transitions
                    .values()
                    .any(|transition| transition.passage_id == *passage_id)
                {
                    return Err(Rejection::new(
                        "duplicate_passage_id",
                        format!("passage '{passage_id}' already exists"),
                    ));
                }
                if !directionality.is_explicit() {
                    return Err(Rejection::new(
                        "invalid_passage_directionality",
                        "passage directionality must be exactly one_way or bidirectional",
                    ));
                }
                if !is_exact_nonempty(from_place) || !canon.places.contains_key(from_place) {
                    return Err(Rejection::new(
                        "unknown_from",
                        format!("from_place '{from_place}' does not exist"),
                    ));
                }
                if !is_exact_nonempty(to_place) || !canon.places.contains_key(to_place) {
                    return Err(Rejection::new(
                        "unknown_target",
                        format!("to_place '{to_place}' does not exist"),
                    ));
                }
                if from_place == to_place {
                    return Err(Rejection::new(
                        "same_passage_endpoint",
                        "a passage must connect two different places",
                    ));
                }
                if canon
                    .place(from_place)
                    .is_some_and(|place| place.has_flag("shell"))
                    || canon
                        .place(to_place)
                        .is_some_and(|place| place.has_flag("shell"))
                {
                    return Err(Rejection::new(
                        "incomplete_passage_endpoint",
                        "both passage endpoints must be complete existing places",
                    ));
                }
                if !is_exact_nonempty(forward_transition_id) {
                    return Err(Rejection::new(
                        "invalid_transition_id",
                        "forward_transition_id must be a non-empty exact id",
                    ));
                }
                if canon.transitions.contains_key(forward_transition_id) {
                    return Err(Rejection::new(
                        "duplicate_id",
                        format!("transition '{forward_transition_id}' already exists"),
                    ));
                }
                match directionality {
                    PassageDirectionality::OneWay => {
                        if !reverse_transition_id.is_empty() || !reverse_label.is_empty() {
                            return Err(Rejection::new(
                                "unexpected_reverse_transition",
                                "a one-way passage cannot include reverse transition data",
                            ));
                        }
                    }
                    PassageDirectionality::Bidirectional => {
                        if !is_exact_nonempty(reverse_transition_id)
                            || reverse_transition_id == forward_transition_id
                        {
                            return Err(Rejection::new(
                                "invalid_reverse_transition_id",
                                "a bidirectional passage requires a distinct exact reverse_transition_id",
                            ));
                        }
                        if canon.transitions.contains_key(reverse_transition_id) {
                            return Err(Rejection::new(
                                "duplicate_id",
                                format!("transition '{reverse_transition_id}' already exists"),
                            ));
                        }
                        if reverse_label.trim().is_empty() {
                            return Err(Rejection::new(
                                "invalid_transition_profile",
                                "a bidirectional passage requires a non-empty reverse_label",
                            ));
                        }
                    }
                    PassageDirectionality::Unspecified => unreachable!("checked above"),
                }
                if let Some(problem) =
                    transition_profile_problem(forward_label, kind, *time_cost, risk)
                {
                    return Err(Rejection::new(
                        "invalid_transition_profile",
                        format!("passage '{passage_id}' {problem}"),
                    ));
                }
                Ok(())
            }
            Action::SetPassageState {
                transition_id,
                state_reason,
                ..
            } => {
                if !is_exact_nonempty(transition_id) {
                    return Err(Rejection::new(
                        "invalid_transition_id",
                        "transition_id must be a non-empty exact id",
                    ));
                }
                if !is_exact_nonempty(state_reason) {
                    return Err(Rejection::new(
                        "missing_passage_state_reason",
                        "a non-empty exact canonical state reason is required",
                    ));
                }
                let selected = canon.transition(transition_id).ok_or_else(|| {
                    Rejection::new(
                        "unknown_transition",
                        format!("no transition '{transition_id}'"),
                    )
                })?;
                validate_passage_state_target(canon, selected).map_err(|reason| {
                    Rejection::new(
                        "invalid_passage_identity",
                        format!("transition '{transition_id}' {reason}"),
                    )
                })
            }
            Action::ConfigureTransition {
                transition_id,
                passage_id,
                directionality,
                to_place,
                label,
                kind,
                time_cost,
                risk,
            } => {
                if transition_id.is_empty() {
                    return Err(Rejection::new("empty_id", "transition_id is required"));
                }
                let transition = canon.transition(transition_id).ok_or_else(|| {
                    Rejection::new(
                        "unknown_transition",
                        format!("no transition '{transition_id}'"),
                    )
                })?;
                if !canon.places.contains_key(&transition.from_place) {
                    return Err(Rejection::new(
                        "unknown_from",
                        format!(
                            "from_place '{}' for transition '{transition_id}' does not exist",
                            transition.from_place
                        ),
                    ));
                }
                if to_place.is_empty() || !canon.places.contains_key(to_place) {
                    return Err(Rejection::new(
                        "unknown_target",
                        format!("to_place '{to_place}' must already exist"),
                    ));
                }
                if let Some(problem) = transition_passage_problem(
                    canon,
                    transition_id,
                    &transition.from_place,
                    to_place,
                    passage_id,
                    *directionality,
                ) {
                    return Err(Rejection::new(
                        "invalid_transition_passage",
                        format!("transition '{transition_id}' {problem}"),
                    ));
                }
                if let Some(problem) = transition_profile_problem(label, kind, *time_cost, risk) {
                    return Err(Rejection::new(
                        "invalid_transition_profile",
                        format!("transition '{transition_id}' {problem}"),
                    ));
                }
                Ok(())
            }
            Action::CreateActor {
                actor_id,
                place_id,
                faction_id,
                ..
            } => {
                if actor_id.is_empty() {
                    return Err(Rejection::new("empty_id", "actor_id is required"));
                }
                if canon.actors.contains_key(actor_id) {
                    return Err(Rejection::new(
                        "duplicate_id",
                        format!("actor '{actor_id}' already exists"),
                    ));
                }
                if !place_id.is_empty() && !canon.places.contains_key(place_id) {
                    return Err(Rejection::new(
                        "unknown_place",
                        format!("place '{place_id}' does not exist"),
                    ));
                }
                if !faction_id.is_empty() && !canon.factions.contains_key(faction_id) {
                    return Err(Rejection::new(
                        "unknown_faction",
                        format!("faction '{faction_id}' does not exist"),
                    ));
                }
                Ok(())
            }
            Action::MoveActor { actor_id, to_place } => {
                if !canon.actors.contains_key(actor_id) {
                    return Err(Rejection::new(
                        "unknown_actor",
                        format!("no actor '{actor_id}'"),
                    ));
                }
                if !canon.places.contains_key(to_place) {
                    return Err(Rejection::new(
                        "unknown_place",
                        format!("place '{to_place}' does not exist"),
                    ));
                }
                Ok(())
            }
            Action::UpdateRelation {
                actor_id,
                other_id,
                value,
            } => {
                if !canon.actors.contains_key(actor_id) {
                    return Err(Rejection::new(
                        "unknown_actor",
                        format!("no actor '{actor_id}'"),
                    ));
                }
                if !canon.actors.contains_key(other_id) {
                    return Err(Rejection::new(
                        "unknown_actor",
                        format!("no actor '{other_id}'"),
                    ));
                }
                if *value < -100 || *value > 100 {
                    return Err(Rejection::new(
                        "out_of_range",
                        "relation value must be -100..=100",
                    ));
                }
                Ok(())
            }
            Action::CreateEvent {
                place_id,
                actors,
                visible_to_player,
                scope,
                ..
            } => {
                if !place_id.is_empty() && !canon.places.contains_key(place_id) {
                    return Err(Rejection::new(
                        "unknown_place",
                        format!("place '{place_id}' does not exist"),
                    ));
                }
                for a in actors {
                    if !canon.actors.contains_key(a) {
                        return Err(Rejection::new(
                            "unknown_actor",
                            format!("event actor '{a}' does not exist"),
                        ));
                    }
                }
                // No hidden-knowledge leak (TZ §10/§11): an event flagged
                // player-visible under a hidden scope (TrueCanon/GmPrivate, or
                // another actor's private knowledge) is a contradiction — the
                // flag claims the player saw it while the scope says they may not.
                if *visible_to_player && !scope.visible_to_player() {
                    return Err(Rejection::new(
                        "hidden_leak",
                        "visible_to_player cannot be true under a hidden scope",
                    ));
                }
                Ok(())
            }
            Action::ScheduleEvent {
                due_minutes,
                place_id,
                actors,
                ..
            } => {
                if *due_minutes < canon.clock_minutes {
                    return Err(Rejection::new(
                        "past_schedule",
                        format!(
                            "cannot schedule at {due_minutes} (clock is {})",
                            canon.clock_minutes
                        ),
                    ));
                }
                if !place_id.is_empty() && !canon.places.contains_key(place_id) {
                    return Err(Rejection::new(
                        "unknown_place",
                        format!("place '{place_id}' does not exist"),
                    ));
                }
                for a in actors {
                    if !canon.actors.contains_key(a) {
                        return Err(Rejection::new(
                            "unknown_actor",
                            format!("scheduled event actor '{a}' does not exist"),
                        ));
                    }
                }
                Ok(())
            }
            Action::ResolveEvent { event_id } => {
                if !canon.event_log.is_pending(event_id) {
                    return Err(Rejection::new(
                        "no_pending_event",
                        format!("no pending event '{event_id}'"),
                    ));
                }
                Ok(())
            }
            Action::ChangeResource { target_id, .. } => {
                if !canon.actors.contains_key(target_id) && !canon.factions.contains_key(target_id)
                {
                    return Err(Rejection::new(
                        "unknown_target",
                        format!("no actor/faction '{target_id}'"),
                    ));
                }
                Ok(())
            }
            Action::RevealInformation { to, .. } => {
                // You reveal INTO a player/public/actor/rumor scope — never into
                // hidden scopes (that would be the opposite of revealing).
                if matches!(to, Scope::TrueCanon | Scope::GmPrivate) {
                    return Err(Rejection::new(
                        "cannot_hide_via_reveal",
                        "reveal target must be a visible scope",
                    ));
                }
                Ok(())
            }
            Action::CreateOrUpdateFact { fact_id, text, .. } => {
                if fact_id.is_empty() {
                    return Err(Rejection::new("empty_id", "fact_id is required"));
                }
                if text.trim().is_empty() {
                    return Err(Rejection::new("empty_fact", "fact text is required"));
                }
                Ok(())
            }
            Action::AdvanceClock { minutes } => {
                if *minutes < 0 {
                    return Err(Rejection::new("negative_time", "cannot rewind the clock"));
                }
                Ok(())
            }
        }
    }
}

fn transition_passage_problem(
    canon: &WorldCanon,
    transition_id: &str,
    from_place: &str,
    to_place: &str,
    passage_id: &str,
    directionality: PassageDirectionality,
) -> Option<String> {
    if passage_id.trim().is_empty() {
        return Some("requires a non-empty passage_id".to_string());
    }
    if !directionality.is_explicit() {
        return Some("requires explicit one_way or bidirectional directionality".to_string());
    }

    let mut reciprocal_found = false;
    for candidate in canon.transitions.values().filter(|candidate| {
        candidate.transition_id != transition_id && candidate.passage_id == passage_id
    }) {
        if candidate.directionality != directionality {
            return Some(format!(
                "conflicts with directionality of passage '{}'",
                candidate.transition_id
            ));
        }
        if directionality == PassageDirectionality::OneWay {
            return Some(format!(
                "reuses one-way passage_id from transition '{}'",
                candidate.transition_id
            ));
        }
        if candidate.from_place != to_place || candidate.to_place != from_place {
            return Some(format!(
                "shares passage_id with non-reciprocal transition '{}'",
                candidate.transition_id
            ));
        }
        if reciprocal_found {
            return Some("has more than one reciprocal transition".to_string());
        }
        reciprocal_found = true;
    }
    None
}

fn transition_profile_problem(
    label: &str,
    kind: &str,
    time_cost: i64,
    risk: &str,
) -> Option<&'static str> {
    if label.trim().is_empty() {
        return Some("requires a non-empty label");
    }
    if kind.trim().is_empty() {
        return Some("requires a non-empty kind");
    }
    if time_cost <= 0 {
        return Some("requires time_cost greater than zero");
    }
    if TravelRisk::parse(risk).is_none() {
        return Some("requires an exact travel risk");
    }
    None
}

fn validate_passage_state_target(canon: &WorldCanon, selected: &Transition) -> Result<(), String> {
    if !selected.has_explicit_passage_profile() {
        return Err("has no explicit physical passage identity".to_string());
    }
    let members = canon
        .transitions
        .values()
        .filter(|transition| transition.passage_id == selected.passage_id)
        .collect::<Vec<_>>();
    match selected.directionality {
        PassageDirectionality::OneWay => {
            if members.len() != 1 {
                return Err("reuses a one-way passage_id across multiple edges".to_string());
            }
        }
        PassageDirectionality::Bidirectional => {
            if members.len() != 2
                || members.iter().any(|transition| {
                    transition.directionality != PassageDirectionality::Bidirectional
                })
            {
                return Err(
                    "does not have exactly two bidirectional sides with one passage_id".to_string(),
                );
            }
            let other = members
                .iter()
                .copied()
                .find(|transition| transition.transition_id != selected.transition_id)
                .ok_or_else(|| "has no reciprocal side".to_string())?;
            if other.from_place != selected.to_place || other.to_place != selected.from_place {
                return Err("shares passage_id with a non-reciprocal edge".to_string());
            }
        }
        PassageDirectionality::Unspecified => {
            return Err("has unspecified directionality".to_string());
        }
    }
    Ok(())
}

fn is_exact_nonempty(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

fn nonempty<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.is_empty() {
        fallback
    } else {
        s
    }
}
