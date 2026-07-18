//! `engine` — the apply loop, graph traversal, travel interruptions, offscreen
//! simulation, the gated player view and the debug/replay dump (TZ §5, §8, §10,
//! §12).
//!
//! The engine is the only thing that turns a *proposed* [`Action`] into committed
//! canon. Every mutation flows through [`apply`], which first calls the
//! [`Validator`]: an invalid proposal returns its [`Rejection`] and mutates
//! NOTHING (this is how "the LLM cannot commit a contradictory canon without the
//! validator" is enforced — TZ §8.3). On success the engine performs the typed
//! mutation and appends a structured [`CanonEvent`] describing what/when/where/
//! who/why/effects/scope/traces, returning the committed events.
//!
//! Deterministic engine-created ids use [`ids::stable_id`], entirely separate
//! from the campaign dice RNG, so worldgen never perturbs `golden_turns` replay
//! (TZ §7.3, §12).

use std::collections::BTreeSet;

use gml_types::ContentLocale;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::action::{Action, ProposedAction};
use super::entity::Containment;
use super::event_log::{Account, CanonEvent};
use super::ids;
use super::knowledge::{Scope, Truthfulness};
use super::navigation::{self, ActiveJourney, TravelPlan};
use super::travel;
use super::validator::{Rejection, Validator};
use super::{PassageDirectionality, Place, Provenance, Transition, WorldCanon};

/// Apply a proposed action to the canon, gated by the [`Validator`].
///
/// On `Ok` the canon is mutated and a structured [`CanonEvent`] (or several, for
/// actions that fan out — a lazy expansion creates many) is appended to the log
/// and returned. On `Err` the canon is left **completely untouched** and the
/// [`Rejection`] is returned, so a contradictory commit can never take effect.
pub fn apply(
    canon: &mut WorldCanon,
    proposed: &ProposedAction,
    turn: i64,
) -> Result<Vec<CanonEvent>, Rejection> {
    // Mandatory gate: reject before any mutation.
    Validator::validate(canon, &proposed.action)?;

    // Single clock mutator (TZ §6.9): `AdvanceClock` owns the clock for its own
    // action, so we must NOT also fold in `time_delta` here — doing so
    // double-advanced the clock. For every other action, the proposal's
    // `time_delta` is the sole advance.
    if proposed.time_delta > 0
        && !matches!(
            proposed.action,
            Action::AdvanceClock { .. }
                | Action::MovePlayer { .. }
                | Action::TravelPlayer { .. }
                | Action::RelocatePlayer { .. }
        )
    {
        canon.clock_minutes += proposed.time_delta;
    }

    let now = canon.clock_minutes;
    let scope = proposed.scope.clone();
    let reason = proposed.reason.clone();
    let source = proposed.source.clone();

    let committed: Vec<CanonEvent> = match &proposed.action {
        Action::MovePlayer { transition_id } => {
            apply_move_player(canon, transition_id, turn, &reason)
        }
        Action::TravelPlayer {
            destination_place_id,
            network_id,
        } => apply_travel_player(
            canon,
            destination_place_id,
            network_id.as_deref(),
            turn,
            &reason,
        ),
        Action::RelocatePlayer {
            destination_place_id,
            elapsed_minutes,
        } => apply_relocate_player(canon, destination_place_id, *elapsed_minutes, turn, &reason),
        Action::CreatePlace {
            place_id,
            name,
            kind,
            parent,
            region_id,
            district_id,
            description,
            features,
            visited,
            shell,
        } => {
            let mut flags = BTreeSet::new();
            if *shell {
                flags.insert("shell".to_string());
            }
            if *visited {
                flags.insert("visited".to_string());
            }
            if source == "location_generator" {
                flags.insert("generated".to_string());
            }
            canon.insert_place(Place {
                place_id: place_id.clone(),
                name: name.clone(),
                kind: kind.clone(),
                parent: parent.clone(),
                region_id: region_id.clone(),
                district_id: district_id.clone(),
                default_description: description.clone(),
                state_flags: flags,
                features: features.clone(),
                transition_ids: Vec::new(),
                occupant_ids: BTreeSet::new(),
                item_ids: Vec::new(),
                event_ids: Vec::new(),
                fact_ids: Vec::new(),
                provenance: Provenance::by(nonempty(&source, "llm"), &reason, turn),
            });
            vec![event(
                canon,
                "create_place",
                turn,
                now,
                place_id,
                &[],
                &reason,
                &[format!("place:{place_id}")],
                &scope,
                &[],
            )]
        }
        Action::UpdatePlace {
            place_id,
            name,
            kind,
            description,
            features,
            visited,
        } => {
            if let Some(place) = canon.places.get_mut(place_id) {
                if !name.trim().is_empty() {
                    place.name = name.clone();
                }
                if !kind.trim().is_empty() {
                    place.kind = kind.clone();
                }
                if !description.trim().is_empty() {
                    place.default_description = description.clone();
                }
                for feature in features {
                    if !feature.trim().is_empty() && !place.features.contains(feature) {
                        place.features.push(feature.clone());
                    }
                }
                if *visited {
                    place.mark_visited();
                }
                place.state_flags.insert("generated".to_string());
                if source == "location_generator" {
                    place.state_flags.remove("shell");
                    place.state_flags.insert("revealed".to_string());
                }
            }
            vec![event(
                canon,
                "update_place",
                turn,
                now,
                place_id,
                &[],
                &reason,
                &[format!("place:{place_id}")],
                &scope,
                &[],
            )]
        }
        Action::CreateTransition {
            transition_id,
            passage_id,
            directionality,
            from_place,
            to_place,
            destination_hint,
            label,
            kind,
            visible,
            passable,
            blocked_by,
            time_cost,
            risk,
        } => {
            let clean_passable = passable.unwrap_or_else(|| blocked_by.is_empty());
            canon.insert_transition(Transition {
                transition_id: transition_id.clone(),
                source_exit_id: transition_id.clone(),
                passage_id: passage_id.clone(),
                directionality: *directionality,
                from_place: from_place.clone(),
                to_place: to_place.clone(),
                destination_hint: destination_hint.clone(),
                label: label.clone(),
                kind: kind.clone(),
                visible: visible.unwrap_or(true),
                passable: clean_passable && blocked_by.is_empty(),
                conditions: Vec::new(),
                blocked_by: blocked_by.clone(),
                time_cost: *time_cost,
                risk: risk.clone(),
                provenance: Provenance::by(nonempty(&source, "llm"), &reason, turn),
            });
            vec![event(
                canon,
                "create_transition",
                turn,
                now,
                from_place,
                &[],
                &reason,
                &[format!("transition:{transition_id}")],
                &scope,
                &[],
            )]
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
            let provenance = Provenance::by(nonempty(&source, "llm"), &reason, turn);
            canon.insert_transition(passage_transition(
                forward_transition_id,
                passage_id,
                *directionality,
                from_place,
                to_place,
                forward_label,
                kind,
                *time_cost,
                risk,
                provenance.clone(),
            ));
            let mut effects = vec![
                format!("passage:{passage_id}"),
                format!("transition:{forward_transition_id}"),
                format!(
                    "directionality:{}",
                    match directionality {
                        PassageDirectionality::OneWay => "one_way",
                        PassageDirectionality::Bidirectional => "bidirectional",
                        PassageDirectionality::Unspecified => "unspecified",
                    }
                ),
            ];
            if *directionality == PassageDirectionality::Bidirectional {
                canon.insert_transition(passage_transition(
                    reverse_transition_id,
                    passage_id,
                    *directionality,
                    to_place,
                    from_place,
                    reverse_label,
                    kind,
                    *time_cost,
                    risk,
                    provenance,
                ));
                effects.push(format!("transition:{reverse_transition_id}"));
            }
            vec![event(
                canon,
                "create_passage",
                turn,
                now,
                from_place,
                &[],
                &reason,
                &effects,
                &scope,
                &[],
            )]
        }
        Action::SetPassageState {
            transition_id,
            open,
            state_reason,
        } => {
            let selected = canon
                .transition(transition_id)
                .expect("validated passage transition");
            let passage_id = selected.passage_id.clone();
            let place_id = selected.from_place.clone();
            let mut affected = Vec::new();
            for transition in canon
                .transitions
                .values_mut()
                .filter(|transition| transition.passage_id == passage_id)
            {
                transition.passable = *open;
                transition.blocked_by = if *open {
                    String::new()
                } else {
                    state_reason.clone()
                };
                affected.push(transition.transition_id.clone());
            }
            affected.sort();
            let mut effects = vec![
                format!("passage:{passage_id}"),
                format!("passage_state:{}", if *open { "open" } else { "closed" }),
                format!("state_reason:{state_reason}"),
            ];
            effects.extend(affected.iter().map(|id| format!("transition:{id}")));
            vec![event(
                canon,
                "set_passage_state",
                turn,
                now,
                &place_id,
                &[],
                &reason,
                &effects,
                &scope,
                &[],
            )]
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
            let transition = canon
                .transitions
                .get_mut(transition_id)
                .expect("validated transition");
            transition.passage_id = passage_id.clone();
            transition.directionality = *directionality;
            transition.to_place = to_place.clone();
            transition.label = label.clone();
            transition.kind = kind.clone();
            transition.time_cost = *time_cost;
            transition.risk = risk.clone();
            vec![event(
                canon,
                "configure_transition",
                turn,
                now,
                to_place,
                &[],
                &reason,
                &[format!("transition:{transition_id}")],
                &scope,
                &[],
            )]
        }
        Action::CreateActor {
            actor_id,
            public_label,
            place_id,
            role,
            faction_id,
        } => {
            let location = if place_id.is_empty() {
                Containment::OutOfPlay
            } else {
                Containment::Place {
                    place_id: place_id.clone(),
                }
            };
            canon.actors.insert(
                actor_id.clone(),
                super::entity::Actor {
                    actor_id: actor_id.clone(),
                    public_label: public_label.clone(),
                    location,
                    home_place_id: place_id.clone(),
                    role: role.clone(),
                    faction_id: faction_id.clone(),
                    status: "alive".to_string(),
                    provenance: Provenance::by(nonempty(&source, "llm"), &reason, turn),
                    ..Default::default()
                },
            );
            if !place_id.is_empty() {
                if let Some(p) = canon.places.get_mut(place_id) {
                    p.occupant_ids.insert(actor_id.clone());
                }
            }
            vec![event(
                canon,
                "create_actor",
                turn,
                now,
                place_id,
                std::slice::from_ref(actor_id),
                &reason,
                &[format!("actor:{actor_id}")],
                &scope,
                &[],
            )]
        }
        Action::MoveActor { actor_id, to_place } => {
            move_actor(canon, actor_id, to_place);
            vec![event(
                canon,
                "move_actor",
                turn,
                now,
                to_place,
                std::slice::from_ref(actor_id),
                &reason,
                &[format!("actor_at:{actor_id}->{to_place}")],
                &scope,
                &[],
            )]
        }
        Action::UpdateRelation {
            actor_id,
            other_id,
            value,
        } => {
            if let Some(a) = canon.actors.get_mut(actor_id) {
                a.relations.insert(other_id.clone(), *value);
            }
            vec![event(
                canon,
                "update_relation",
                turn,
                now,
                "",
                &[actor_id.clone(), other_id.clone()],
                &reason,
                &[format!("relation:{actor_id}->{other_id}={value}")],
                &scope,
                &[],
            )]
        }
        Action::CreateEvent {
            kind,
            place_id,
            actors,
            causes,
            effects,
            visible_to_player,
            scope: ev_scope,
            traces,
        } => {
            let mut ev = CanonEvent {
                event_id: String::new(),
                seq: 0,
                kind: kind.clone(),
                time_minutes: now,
                time_label: String::new(),
                place_id: place_id.clone(),
                actors: actors.clone(),
                causes: causes.clone(),
                effects: effects.clone(),
                visible_to_player: *visible_to_player,
                scope: ev_scope.clone(),
                possible_traces: traces.clone(),
                scheduled: false,
                due_minutes: 0,
                provenance: Provenance::by(nonempty(&source, "llm"), &reason, turn),
            };
            ev.event_id = next_event_id(canon, kind, turn);
            vec![append_event(canon, ev)]
        }
        Action::ScheduleEvent {
            kind,
            due_minutes,
            place_id,
            actors,
            causes,
        } => {
            let mut ev = CanonEvent {
                event_id: String::new(),
                seq: 0,
                kind: kind.clone(),
                time_minutes: now,
                time_label: String::new(),
                place_id: place_id.clone(),
                actors: actors.clone(),
                causes: causes.clone(),
                effects: Vec::new(),
                visible_to_player: false,
                scope: Scope::GmPrivate,
                possible_traces: Vec::new(),
                scheduled: true,
                due_minutes: *due_minutes,
                provenance: Provenance::by(nonempty(&source, "gm"), &reason, turn),
            };
            ev.event_id = next_event_id(canon, kind, turn);
            vec![append_event(canon, ev)]
        }
        Action::ResolveEvent { event_id } => {
            // Same resolution semantics as the time-driven offscreen tick.
            resolve_due_event(canon, event_id, now, turn)
        }
        Action::ChangeResource {
            target_id,
            resource,
            delta,
        } => {
            if let Some(a) = canon.actors.get_mut(target_id) {
                a.resources.push(format!("{resource}:{delta:+}"));
            } else if let Some(f) = canon.factions.get_mut(target_id) {
                f.resources.push(format!("{resource}:{delta:+}"));
            }
            vec![event(
                canon,
                "change_resource",
                turn,
                now,
                "",
                std::slice::from_ref(target_id),
                &reason,
                &[format!("{target_id}.{resource}{delta:+}")],
                &scope,
                &[],
            )]
        }
        Action::RevealInformation { fact_id, to } => {
            // Widen the durable fact's scope (a reveal promotes who may know it).
            if let Some(f) = canon.facts.get_mut(fact_id) {
                f.scope = to.clone();
            }
            vec![event(
                canon,
                "reveal_information",
                turn,
                now,
                "",
                &[],
                &reason,
                &[format!("reveal:{fact_id}")],
                to,
                &[],
            )]
        }
        Action::CreateOrUpdateFact {
            fact_id,
            text,
            scope: fscope,
        } => {
            // Write durable, queryable scoped state — not just an event effect.
            canon.facts.insert(
                fact_id.clone(),
                super::CanonFact {
                    fact_id: fact_id.clone(),
                    text: text.clone(),
                    scope: fscope.clone(),
                },
            );
            vec![event(
                canon,
                "fact",
                turn,
                now,
                "",
                &[],
                &reason,
                &[format!("fact:{fact_id}={text}")],
                fscope,
                &[],
            )]
        }
        Action::AdvanceClock { minutes } => {
            canon.clock_minutes += minutes;
            let now2 = canon.clock_minutes;
            let mut evs = vec![event(
                canon,
                "advance_clock",
                turn,
                now2,
                "",
                &[],
                &reason,
                &[format!("clock:{now2}")],
                &scope,
                &[],
            )];
            evs.extend(tick_offscreen(canon, now2, turn));
            evs
        }
    };

    Ok(committed)
}

/// Move the player along a fully configured transition.
fn apply_move_player(
    canon: &mut WorldCanon,
    transition_id: &str,
    turn: i64,
    reason: &str,
) -> Vec<CanonEvent> {
    // The validator guarantees that the endpoint and profile are complete.
    let (from_place, to_place, stored_time, stored_risk) = {
        let t = canon
            .transition(transition_id)
            .expect("validated transition");
        (
            t.from_place.clone(),
            t.to_place.clone(),
            t.time_cost,
            t.risk.clone(),
        )
    };
    let risk = travel::TravelRisk::parse(&stored_risk).expect("validated travel risk");
    execute_player_travel(
        canon,
        PlayerTravel {
            route_id: transition_id.to_string(),
            from_place,
            to_place,
            duration_minutes: stored_time,
            risk,
            event_kind: "move_player",
            extra_effects: Vec::new(),
            plan: None,
        },
        turn,
        reason,
    )
}

fn apply_relocate_player(
    canon: &mut WorldCanon,
    destination_place_id: &str,
    elapsed_minutes: i64,
    turn: i64,
    reason: &str,
) -> Vec<CanonEvent> {
    let origin_place_id = canon.player_place_id.clone();
    canon.clock_minutes += elapsed_minutes;
    let now = canon.clock_minutes;
    canon.player_place_id = destination_place_id.to_string();
    canon.active_journey = None;
    if let Some(destination) = canon.places.get_mut(destination_place_id) {
        destination.mark_visited();
    }
    let mut events = vec![event(
        canon,
        "relocate_player",
        turn,
        now,
        destination_place_id,
        &[],
        nonempty(reason, "one-off player relocation"),
        &[
            format!("player_at:{destination_place_id}"),
            format!("relocated_from:{origin_place_id}"),
            format!("elapsed_minutes:{elapsed_minutes}"),
            "reusable_passage:none".to_string(),
        ],
        &Scope::Player,
        &[],
    )];
    events.extend(tick_offscreen(canon, now, turn));
    events
}

fn apply_travel_player(
    canon: &mut WorldCanon,
    destination_place_id: &str,
    requested_network_id: Option<&str>,
    turn: i64,
    reason: &str,
) -> Vec<CanonEvent> {
    let plan = navigation::plan_travel(canon, destination_place_id, requested_network_id)
        .expect("validated travel plan");
    let route_id = ids::stable_id(
        &canon.world_seed,
        &plan.origin_place_id,
        "journey",
        &format!(
            "{}|{}|{}|{}",
            plan.destination_place_id, plan.network_id, turn, canon.clock_minutes
        ),
    );
    let risk = travel::TravelRisk::parse(&plan.risk).expect("validated travel plan risk");
    let extra_effects = vec![
        format!("travel_network:{}", plan.network_id),
        format!("travel_links:{}", plan.link_ids.join(",")),
        format!("travel_minutes:{}", plan.total_time_minutes),
        format!("risk:{}", plan.risk),
    ];
    execute_player_travel(
        canon,
        PlayerTravel {
            route_id,
            from_place: plan.origin_place_id.clone(),
            to_place: plan.destination_place_id.clone(),
            duration_minutes: plan.total_time_minutes,
            risk,
            event_kind: "travel_player",
            extra_effects,
            plan: Some(plan),
        },
        turn,
        reason,
    )
}

struct PlayerTravel {
    route_id: String,
    from_place: String,
    to_place: String,
    duration_minutes: i64,
    risk: travel::TravelRisk,
    event_kind: &'static str,
    extra_effects: Vec<String>,
    plan: Option<TravelPlan>,
}

#[allow(clippy::too_many_arguments)]
fn passage_transition(
    transition_id: &str,
    passage_id: &str,
    directionality: PassageDirectionality,
    from_place: &str,
    to_place: &str,
    label: &str,
    kind: &str,
    time_cost: i64,
    risk: &str,
    provenance: Provenance,
) -> Transition {
    Transition {
        transition_id: transition_id.to_string(),
        source_exit_id: transition_id.to_string(),
        passage_id: passage_id.to_string(),
        directionality,
        from_place: from_place.to_string(),
        to_place: to_place.to_string(),
        destination_hint: String::new(),
        label: label.to_string(),
        kind: kind.to_string(),
        visible: true,
        passable: true,
        conditions: Vec::new(),
        blocked_by: String::new(),
        time_cost,
        risk: risk.to_string(),
        provenance,
    }
}

fn execute_player_travel(
    canon: &mut WorldCanon,
    travel: PlayerTravel,
    turn: i64,
    reason: &str,
) -> Vec<CanonEvent> {
    let mut events = Vec::new();
    let start_minutes = canon.clock_minutes;
    let content_locale = canon.content_locale;

    if can_create_travel_situation(canon) {
        if let Some(situation) = travel::roll_travel_situation_for_locale(
            travel::TravelRoll {
                world_seed: &canon.world_seed,
                transition_id: &travel.route_id,
                from_place: &travel.from_place,
                to_place: &travel.to_place,
                turn,
                start_minutes,
                duration_minutes: travel.duration_minutes,
                risk: travel.risk,
            },
            content_locale,
        ) {
            events.extend(apply_travel_situation(
                canon,
                &travel.route_id,
                &travel.from_place,
                &travel.to_place,
                travel.risk,
                &situation,
                turn,
                reason,
            ));
            if let Some(plan) = travel.plan.as_ref() {
                canon.active_journey = Some(active_journey_at_situation(
                    canon,
                    &travel.route_id,
                    plan,
                    &situation.site_id,
                    situation.elapsed_minutes,
                ));
            }
            let now = canon.clock_minutes;
            events.extend(tick_offscreen(canon, now, turn));
            return events;
        }
    }

    canon.clock_minutes += travel.duration_minutes.max(0);
    let now = canon.clock_minutes;

    // Move the player.
    canon.player_place_id = travel.to_place.clone();
    if let Some(p) = canon.places.get_mut(&travel.to_place) {
        p.mark_visited();
    }
    if canon
        .active_journey
        .as_ref()
        .is_some_and(|journey| journey.interruption_place_id == travel.from_place)
    {
        canon.active_journey = None;
    }

    let mut effects = vec![
        format!("player_at:{}", travel.to_place),
        format!("via:{}", travel.route_id),
    ];
    effects.extend(travel.extra_effects);
    events.push(event(
        canon,
        travel.event_kind,
        turn,
        now,
        &travel.to_place,
        &[],
        nonempty(reason, "player traversal"),
        &effects,
        &Scope::Player,
        &[],
    ));
    events.extend(tick_offscreen(canon, now, turn));

    events
}

fn active_journey_at_situation(
    canon: &WorldCanon,
    journey_id: &str,
    plan: &TravelPlan,
    place_id: &str,
    elapsed_minutes: i64,
) -> ActiveJourney {
    let mut journey = ActiveJourney::from_plan(canon, journey_id, plan)
        .expect("validated travel plan can create an active journey");
    let mut elapsed_on_route = elapsed_minutes.max(0);
    for (index, link_id) in plan.link_ids.iter().enumerate() {
        let link = canon
            .travel_link(link_id)
            .expect("validated travel plan link");
        if elapsed_on_route < link.time_cost_minutes {
            journey.next_link_index = index;
            journey.remaining_minutes_on_link = link.time_cost_minutes - elapsed_on_route;
            break;
        }
        elapsed_on_route -= link.time_cost_minutes;
    }
    journey.elapsed_minutes = elapsed_minutes.max(0);
    journey.interrupt_at(place_id);
    journey
}

#[allow(clippy::too_many_arguments)]
fn apply_travel_situation(
    canon: &mut WorldCanon,
    transition_id: &str,
    from_place: &str,
    to_place: &str,
    risk: travel::TravelRisk,
    situation: &travel::TravelSituation,
    turn: i64,
    reason: &str,
) -> Vec<CanonEvent> {
    let mut events = Vec::new();
    let content_locale = canon.content_locale;
    let site_id = situation.site_id.clone();
    let region_id = canon
        .place(from_place)
        .map(|p| p.region_id.clone())
        .unwrap_or_default();
    let district_id = canon
        .place(from_place)
        .map(|p| p.district_id.clone())
        .unwrap_or_default();

    if !canon.places.contains_key(&site_id) {
        let mut flags = BTreeSet::new();
        flags.insert("travel_site".to_string());
        flags.insert("temporary".to_string());
        flags.insert("visited".to_string());
        canon.insert_place(Place {
            place_id: site_id.clone(),
            name: situation.title.clone(),
            kind: "travel_site".to_string(),
            parent: from_place.to_string(),
            region_id,
            district_id,
            default_description: situation.summary.clone(),
            state_flags: flags,
            features: vec![
                content_text(content_locale, "дорожная ситуация", "road situation").to_string(),
            ],
            transition_ids: Vec::new(),
            occupant_ids: BTreeSet::new(),
            item_ids: Vec::new(),
            event_ids: Vec::new(),
            fact_ids: Vec::new(),
            provenance: Provenance::by("travel", "road situation stop", turn),
        });
        events.push(event(
            canon,
            "create_place",
            turn,
            canon.clock_minutes,
            &site_id,
            &[],
            "travel situation site",
            &[format!("place:{site_id}")],
            &Scope::GmPrivate,
            &[],
        ));
    }

    let continue_id = ids::stable_id(&canon.world_seed, &site_id, "transition", to_place);
    if !canon.transitions.contains_key(&continue_id) {
        canon.insert_transition(Transition {
            transition_id: continue_id.clone(),
            source_exit_id: continue_id.clone(),
            passage_id: continue_id.clone(),
            directionality: PassageDirectionality::OneWay,
            from_place: site_id.clone(),
            to_place: to_place.to_string(),
            destination_hint: String::new(),
            label: content_text(content_locale, "Продолжить путь", "Continue journey").to_string(),
            kind: "road_segment".to_string(),
            visible: true,
            passable: true,
            conditions: Vec::new(),
            blocked_by: String::new(),
            time_cost: situation.remaining_minutes,
            risk: risk.as_str().to_string(),
            provenance: Provenance::by("travel", "remaining route", turn),
        });
    }
    let back_id = ids::stable_id(&canon.world_seed, &site_id, "transition", from_place);
    if !canon.transitions.contains_key(&back_id) {
        canon.insert_transition(Transition {
            transition_id: back_id.clone(),
            source_exit_id: ids::stable_id(&canon.world_seed, &site_id, "exit", from_place),
            passage_id: back_id,
            directionality: PassageDirectionality::OneWay,
            from_place: site_id.clone(),
            to_place: from_place.to_string(),
            destination_hint: String::new(),
            label: content_text(content_locale, "Вернуться назад", "Turn back").to_string(),
            kind: "road_segment".to_string(),
            visible: true,
            passable: true,
            conditions: Vec::new(),
            blocked_by: String::new(),
            time_cost: situation.elapsed_minutes,
            risk: risk.as_str().to_string(),
            provenance: Provenance::by("travel", "return route", turn),
        });
    }

    canon.clock_minutes += situation.elapsed_minutes.max(0);
    let now = canon.clock_minutes;
    canon.player_place_id = site_id.clone();

    events.push(event(
        canon,
        "move_player",
        turn,
        now,
        &site_id,
        &[],
        nonempty(reason, "player traversal interrupted by road situation"),
        &[
            format!("player_at:{site_id}"),
            format!("via:{transition_id}"),
            format!("elapsed_minutes:{}", situation.elapsed_minutes),
            format!("remaining_minutes:{}", situation.remaining_minutes),
        ],
        &Scope::Player,
        &[],
    ));
    events.push(event(
        canon,
        "travel_situation",
        turn,
        now,
        &site_id,
        &[],
        "road situation roll",
        &[
            match content_locale {
                ContentLocale::Russian => {
                    format!("На дороге возникает ситуация: {}", situation.summary)
                }
                ContentLocale::English => {
                    format!("A situation arises on the road: {}", situation.summary)
                }
            },
            match content_locale {
                ContentLocale::Russian => format!(
                    "До цели остается примерно {} мин.",
                    situation.remaining_minutes
                ),
                ContentLocale::English => format!(
                    "About {} min. remain to the destination.",
                    situation.remaining_minutes
                ),
            },
            format!("situation_type:{}", situation.tone),
            format!("rarity:{}", situation.rarity),
            format!("risk:{}", risk.as_str()),
            format!("chance_percent:{}", situation.chance_percent),
            format!("roll:{}", situation.roll),
            format!("elapsed_minutes:{}", situation.elapsed_minutes),
            format!("remaining_minutes:{}", situation.remaining_minutes),
        ],
        &Scope::Player,
        &[content_text(content_locale, "дорожные следы", "roadside traces").to_string()],
    ));

    events
}

fn can_create_travel_situation(canon: &WorldCanon) -> bool {
    canon.gen_budget.max_events_per_turn >= 3
        && canon.gen_budget.max_transitions_per_turn >= 2
        && canon.gen_budget.max_rooms_per_turn >= 1
}

fn move_actor(canon: &mut WorldCanon, actor_id: &str, to_place: &str) {
    // Remove from any place occupant set it was in.
    let old = canon
        .actors
        .get(actor_id)
        .and_then(|a| a.location.place().map(|s| s.to_string()));
    if let Some(old_place) = old {
        if let Some(p) = canon.places.get_mut(&old_place) {
            p.occupant_ids.remove(actor_id);
        }
    }
    if let Some(a) = canon.actors.get_mut(actor_id) {
        a.location = Containment::Place {
            place_id: to_place.to_string(),
        };
    }
    if let Some(p) = canon.places.get_mut(to_place) {
        p.occupant_ids.insert(actor_id.to_string());
    }
}

/// Resolve scheduled events whose `due_minutes <= now_minutes`, applying their
/// simple effects and leaving a causal trail (a resolution event / account) but
/// NEVER promoting hidden info into a player-visible scope (TZ §10). Deterministic
/// and bounded by `gen_budget.max_events_per_turn`.
pub fn tick_offscreen(canon: &mut WorldCanon, now_minutes: i64, turn: i64) -> Vec<CanonEvent> {
    let mut produced = Vec::new();
    let due = canon.event_log.due_scheduled(now_minutes);
    let cap = canon.gen_budget.max_events_per_turn;

    for event_id in due.into_iter().take(cap) {
        produced.extend(resolve_due_event(canon, &event_id, now_minutes, turn));
    }

    produced
}

/// Resolve a SINGLE scheduled event with identical semantics whether it came due
/// via [`tick_offscreen`] (time-driven) or via the explicit `ResolveEvent`
/// action: mark it resolved (a projection — the recorded event is never
/// mutated), apply its simple offscreen effect (named actors relocate to its
/// place), and leave a causal trail (a GM-private resolution event + a rumour
/// account). NEVER promotes hidden info into a player-visible scope (TZ §10).
fn resolve_due_event(
    canon: &mut WorldCanon,
    event_id: &str,
    now: i64,
    turn: i64,
) -> Vec<CanonEvent> {
    let content_locale = canon.content_locale;
    let snap = canon
        .event_log
        .events
        .iter()
        .find(|e| e.event_id == event_id)
        .map(|e| (e.kind.clone(), e.place_id.clone(), e.actors.clone()));
    let (kind, place_id, actors) = match snap {
        Some(s) => s,
        None => return Vec::new(),
    };

    // Mark it resolved (projection — the recorded event stays immutable).
    canon.event_log.resolve(event_id);

    // Simple effect: named actors physically relocate to the event's place.
    for actor_id in &actors {
        if !place_id.is_empty()
            && canon.actors.contains_key(actor_id)
            && canon.places.contains_key(&place_id)
        {
            move_actor(canon, actor_id, &place_id);
        }
    }

    // Causal trail: a GM-private resolution event with a discoverable trace.
    let mut ev = event(
        canon,
        &format!("resolved_{kind}"),
        turn,
        now,
        &place_id,
        &actors,
        "scheduled event came due",
        &[format!("resolved:{event_id}")],
        &Scope::GmPrivate,
        &[format!("trace_of:{event_id}")],
    );
    ev.causes.push(event_id.to_string());

    // A rumour-scoped account leaves a discoverable (but unverified) trail.
    canon.event_log.add_account(Account {
        account_id: ids::stable_id(&canon.world_seed, event_id, "account", "rumor"),
        event_id: event_id.to_string(),
        source: "rumor".to_string(),
        text: content_text(
            content_locale,
            "Говорят, что-то случилось в стороне.",
            "Rumor has it that something happened nearby.",
        )
        .to_string(),
        truth: Truthfulness::Partial,
        scope: Scope::Rumor,
    });

    vec![ev]
}

/// Advance the clock and run the offscreen tick for newly-due events. A thin
/// convenience over `apply(AdvanceClock)` for callers holding the canon directly.
pub fn advance_clock(canon: &mut WorldCanon, minutes: i64, turn: i64) -> Vec<CanonEvent> {
    let proposed = ProposedAction::new(Action::AdvanceClock { minutes }, "gm", "advance clock");
    apply(canon, &proposed, turn).unwrap_or_default()
}

// =========================================================================
// Gated player view (TZ §11): only what the player may see.
// =========================================================================

/// A visible exit in the player view — no hidden internals leak out.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewExit {
    pub label: String,
    pub transition_id: String,
    pub passable: bool,
    pub blocked_by: String,
}

/// A present actor as the player may perceive them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewActor {
    pub actor_id: String,
    pub label: String,
    pub role: String,
}

/// A player-visible event summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewEvent {
    pub kind: String,
    pub summary: String,
    pub time_minutes: i64,
}

/// The fully gated player-facing projection of the canon (TZ §11). Constructed
/// only from information whose [`Scope`] is visible to the player; shell secrets,
/// `GmPrivate` / `TrueCanon` and other actors' private knowledge never appear.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerView {
    pub place_id: String,
    pub title: String,
    pub description: String,
    pub exits: Vec<ViewExit>,
    pub present_actors: Vec<ViewActor>,
    pub known_paths: Vec<String>,
    pub recent_events: Vec<ViewEvent>,
}

/// Build the gated [`PlayerView`] for the player's current place. Hidden
/// knowledge MUST NOT leak: only visible exits, living present actors, and
/// player-visible events are included (TZ §10, §11).
pub fn player_view(canon: &WorldCanon) -> PlayerView {
    let place_id = canon.player_place_id.clone();
    let place = canon.place(&place_id);

    let (title, description) = match place {
        Some(p) => (p.name.clone(), p.default_description.clone()),
        None => (String::new(), String::new()),
    };

    // Visible exits only.
    let mut exits = Vec::new();
    let mut known_paths = Vec::new();
    for t in canon.exits_from(&place_id) {
        if !t.visible {
            continue;
        }
        known_paths.push(t.label.clone());
        exits.push(ViewExit {
            label: t.label.clone(),
            transition_id: t.transition_id.clone(),
            passable: t.passable,
            blocked_by: t.blocked_by.clone(),
        });
    }

    // Present, living actors only.
    let present_actors = canon
        .actors_at(&place_id)
        .into_iter()
        .map(|a| ViewActor {
            actor_id: a.actor_id.clone(),
            label: if a.public_label.is_empty() {
                a.actor_id.clone()
            } else {
                a.public_label.clone()
            },
            role: a.role.clone(),
        })
        .collect();

    // Player-visible events only (most recent last).
    let recent_events = canon
        .event_log
        .player_visible()
        .into_iter()
        .rev()
        .take(8)
        .map(|e| ViewEvent {
            kind: e.kind.clone(),
            summary: if e.effects.is_empty() {
                e.kind.clone()
            } else {
                e.effects.join(", ")
            },
            time_minutes: e.time_minutes,
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    PlayerView {
        place_id,
        title,
        description,
        exits,
        present_actors,
        known_paths,
        recent_events,
    }
}

// =========================================================================
// Debug / replay (TZ §12): understand WHY the world is as it is.
// =========================================================================

/// Full canon dump as JSON, for a debug/replay view of the entire world state
/// (TZ §12). Unlike [`player_view`], this is NOT gated — it is a developer tool.
pub fn debug_dump(canon: &WorldCanon) -> Value {
    serde_json::to_value(canon).unwrap_or(Value::Null)
}

/// A causal summary of the event log: each committed event as
/// `seq | t=minutes | kind @ place | actors | effects (<- causes)`, in append
/// order — the replay backbone for "why is the world like this" (TZ §12).
pub fn causal_log(canon: &WorldCanon) -> Vec<String> {
    canon
        .event_log
        .events
        .iter()
        .map(|e| {
            let causes = if e.causes.is_empty() {
                String::new()
            } else {
                format!(" <- {}", e.causes.join(","))
            };
            format!(
                "#{:03} t={} {} @ {} [{}] {}{}",
                e.seq,
                e.time_minutes,
                e.kind,
                nonempty(&e.place_id, "-"),
                e.actors.join(","),
                e.effects.join(", "),
                causes,
            )
        })
        .collect()
}

/// Convenience: the full debug bundle (canon + causal log).
pub fn debug_bundle(canon: &WorldCanon) -> Value {
    json!({
        "canon": debug_dump(canon),
        "causal_log": causal_log(canon),
    })
}

// =========================================================================
// helpers
// =========================================================================

/// Build and append a [`CanonEvent`], returning the appended clone (with seq).
#[allow(clippy::too_many_arguments)]
fn event(
    canon: &mut WorldCanon,
    kind: &str,
    turn: i64,
    now: i64,
    place_id: &str,
    actors: &[String],
    reason: &str,
    effects: &[String],
    scope: &Scope,
    traces: &[String],
) -> CanonEvent {
    let ev = CanonEvent {
        event_id: next_event_id(canon, kind, turn),
        seq: 0,
        kind: kind.to_string(),
        time_minutes: now,
        time_label: String::new(),
        place_id: place_id.to_string(),
        actors: actors.to_vec(),
        causes: Vec::new(),
        effects: effects.to_vec(),
        visible_to_player: scope.visible_to_player(),
        scope: scope.clone(),
        possible_traces: traces.to_vec(),
        scheduled: false,
        due_minutes: 0,
        provenance: Provenance::by("engine", reason, turn),
    };
    append_event(canon, ev)
}

fn append_event(canon: &mut WorldCanon, mut event: CanonEvent) -> CanonEvent {
    event.seq = canon.event_log.append(event.clone());
    if !event.place_id.is_empty() {
        if let Some(place) = canon.places.get_mut(&event.place_id) {
            if !place.event_ids.contains(&event.event_id) {
                place.event_ids.push(event.event_id.clone());
            }
        }
    }
    event
}

/// A stable, unique event id derived from the canon's identity inputs plus the
/// current log length (so repeated same-kind events at the same turn differ).
fn next_event_id(canon: &WorldCanon, kind: &str, turn: i64) -> String {
    let salt = format!("{turn}:{}", canon.event_log.events.len());
    ids::stable_id(&canon.world_seed, kind, "event", &salt)
}

fn nonempty<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.is_empty() {
        fallback
    } else {
        s
    }
}

const fn content_text(
    content_locale: ContentLocale,
    russian: &'static str,
    english: &'static str,
) -> &'static str {
    match content_locale {
        ContentLocale::Russian => russian,
        ContentLocale::English => english,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contains_cyrillic(value: &str) -> bool {
        value
            .chars()
            .any(|character| matches!(character, '\u{0400}'..='\u{04ff}'))
    }

    fn travel_situation_fixture(content_locale: ContentLocale) -> (WorldCanon, Vec<CanonEvent>) {
        let mut canon = WorldCanon {
            world_seed: "localized-engine".to_string(),
            content_locale,
            ..Default::default()
        };
        let situation = travel::TravelSituation {
            site_id: "road-site".to_string(),
            title: content_text(content_locale, "Угроза на дороге", "Threat on the Road")
                .to_string(),
            summary: content_text(
                content_locale,
                "на дороге заметна угроза",
                "there is a clear threat on the road",
            )
            .to_string(),
            elapsed_minutes: 30,
            remaining_minutes: 90,
            chance_percent: 100,
            roll: 42,
            tone: "bad",
            rarity: "common",
        };
        let events = apply_travel_situation(
            &mut canon,
            "long-road",
            "village",
            "city",
            travel::TravelRisk::Certain,
            &situation,
            4,
            "test travel",
        );
        (canon, events)
    }

    fn rumor_fixture(content_locale: ContentLocale) -> WorldCanon {
        let mut canon = WorldCanon {
            world_seed: "localized-rumor".to_string(),
            content_locale,
            ..Default::default()
        };
        canon.event_log.append(CanonEvent {
            event_id: "scheduled-event".to_string(),
            kind: "storm".to_string(),
            scheduled: true,
            due_minutes: 10,
            ..Default::default()
        });
        resolve_due_event(&mut canon, "scheduled-event", 10, 2);
        canon
    }

    #[test]
    fn travel_situation_engine_text_follows_content_locale_without_changing_mechanics() {
        let (russian, russian_events) = travel_situation_fixture(ContentLocale::Russian);
        let (english, english_events) = travel_situation_fixture(ContentLocale::English);

        assert_eq!(russian.player_place_id, english.player_place_id);
        assert_eq!(russian.clock_minutes, english.clock_minutes);
        assert_eq!(
            russian.transitions.keys().collect::<Vec<_>>(),
            english.transitions.keys().collect::<Vec<_>>()
        );
        assert_eq!(
            russian_events
                .iter()
                .map(|event| (&event.event_id, &event.kind, event.time_minutes))
                .collect::<Vec<_>>(),
            english_events
                .iter()
                .map(|event| (&event.event_id, &event.kind, event.time_minutes))
                .collect::<Vec<_>>()
        );

        let russian_site = russian.place("road-site").expect("Russian travel site");
        let english_site = english.place("road-site").expect("English travel site");
        assert_eq!(russian_site.features, ["дорожная ситуация"]);
        assert_eq!(english_site.features, ["road situation"]);

        let mut russian_labels = russian
            .exits_from("road-site")
            .into_iter()
            .map(|transition| transition.label.as_str())
            .collect::<Vec<_>>();
        let mut english_labels = english
            .exits_from("road-site")
            .into_iter()
            .map(|transition| transition.label.as_str())
            .collect::<Vec<_>>();
        russian_labels.sort_unstable();
        english_labels.sort_unstable();
        assert_eq!(russian_labels, ["Вернуться назад", "Продолжить путь"]);
        assert_eq!(english_labels, ["Continue journey", "Turn back"]);

        let russian_event = russian_events
            .iter()
            .find(|event| event.kind == "travel_situation")
            .expect("Russian situation event");
        let english_event = english_events
            .iter()
            .find(|event| event.kind == "travel_situation")
            .expect("English situation event");
        assert_eq!(
            &russian_event.effects[..2],
            [
                "На дороге возникает ситуация: на дороге заметна угроза",
                "До цели остается примерно 90 мин.",
            ]
        );
        assert_eq!(
            &english_event.effects[..2],
            [
                "A situation arises on the road: there is a clear threat on the road",
                "About 90 min. remain to the destination.",
            ]
        );
        assert_eq!(russian_event.possible_traces, ["дорожные следы"]);
        assert_eq!(english_event.possible_traces, ["roadside traces"]);
        assert_eq!(&russian_event.effects[2..], &english_event.effects[2..]);

        for text in english_site
            .features
            .iter()
            .map(String::as_str)
            .chain(english_labels)
            .chain(english_event.effects[..2].iter().map(String::as_str))
            .chain(english_event.possible_traces.iter().map(String::as_str))
        {
            assert!(
                !contains_cyrillic(text),
                "English text contains Cyrillic: {text}"
            );
        }
    }

    #[test]
    fn scheduled_event_rumor_follows_content_locale_without_changing_identity() {
        let russian = rumor_fixture(ContentLocale::Russian);
        let english = rumor_fixture(ContentLocale::English);
        let russian_account = russian.event_log.accounts.first().expect("Russian rumor");
        let english_account = english.event_log.accounts.first().expect("English rumor");

        assert_eq!(russian_account.account_id, english_account.account_id);
        assert_eq!(russian_account.event_id, english_account.event_id);
        assert_eq!(russian_account.source, english_account.source);
        assert_eq!(russian_account.truth, english_account.truth);
        assert_eq!(russian_account.scope, english_account.scope);
        assert_eq!(russian_account.text, "Говорят, что-то случилось в стороне.");
        assert_eq!(
            english_account.text,
            "Rumor has it that something happened nearby."
        );
        assert!(!contains_cyrillic(&english_account.text));
    }
}
