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
use super::WorldCanon;

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
                // Target must exist or be an explicit shell (empty to_place is a
                // shell that the engine will lazily expand).
                if !t.to_place.is_empty() && !canon.places.contains_key(&t.to_place) {
                    return Err(Rejection::new(
                        "dangling_target",
                        format!("transition target '{}' does not exist", t.to_place),
                    ));
                }
                Ok(())
            }
            Action::CreatePlace {
                place_id, parent, ..
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
                    && !canon.regions.contains_key(parent)
                {
                    return Err(Rejection::new(
                        "unknown_parent",
                        format!("parent '{parent}' does not exist"),
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
                from_place,
                to_place,
                time_cost,
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
                // to_place must exist unless it is an explicit shell (empty).
                if !to_place.is_empty() && !canon.places.contains_key(to_place) {
                    return Err(Rejection::new(
                        "unknown_target",
                        format!("to_place '{to_place}' must be created first (no implicit places)"),
                    ));
                }
                if *time_cost < 0 {
                    return Err(Rejection::new(
                        "negative_travel_time",
                        "transition time_cost cannot be negative",
                    ));
                }
                Ok(())
            }
            Action::RevealPlace { place_id } => {
                let p = canon.place(place_id).ok_or_else(|| {
                    Rejection::new("unknown_place", format!("no place '{place_id}'"))
                })?;
                if !p.has_flag("shell") {
                    return Err(Rejection::new(
                        "not_shell",
                        format!("place '{place_id}' is already revealed"),
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

fn nonempty<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.is_empty() {
        fallback
    } else {
        s
    }
}
