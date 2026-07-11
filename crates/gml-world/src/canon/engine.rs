//! `engine` — the apply loop, graph traversal, lazy generation, offscreen
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
//! Determinism: lazy generation and offscreen simulation derive every id and
//! every bounded choice from [`ids::stable_id`] / [`ids::DetRng`], a stream
//! entirely separate from the campaign dice RNG, so worldgen never perturbs
//! `golden_turns` replay (TZ §7.3, §12).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::action::{Action, ProposedAction};
use super::entity::Containment;
use super::event_log::{Account, CanonEvent};
use super::ids;
use super::knowledge::{Scope, Truthfulness};
use super::travel;
use super::validator::{Rejection, Validator};
use super::{Place, Provenance, Transition, WorldCanon};

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
            Action::AdvanceClock { .. } | Action::MovePlayer { .. }
        )
    {
        canon.clock_minutes += proposed.time_delta;
    }

    let now = canon.clock_minutes;
    let scope = proposed.scope.clone();
    let reason = proposed.reason.clone();
    let source = proposed.source.clone();

    let committed: Vec<CanonEvent> = match &proposed.action {
        Action::MovePlayer { transition_id } => apply_move_player(
            canon,
            transition_id,
            turn,
            proposed.time_delta,
            &source,
            &reason,
        ),
        Action::CreatePlace {
            place_id,
            name,
            kind,
            parent,
            region_id,
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
            let clean_risk = risk.trim();
            let clean_time = if *time_cost > 0 {
                *time_cost
            } else {
                travel::infer_time_cost(kind, label, destination_hint)
            };
            let clean_passable = passable.unwrap_or_else(|| blocked_by.is_empty());
            canon.insert_transition(Transition {
                transition_id: transition_id.clone(),
                source_exit_id: transition_id.clone(),
                from_place: from_place.clone(),
                to_place: to_place.clone(),
                destination_hint: destination_hint.clone(),
                label: label.clone(),
                kind: kind.clone(),
                visible: visible.unwrap_or(true),
                passable: clean_passable && blocked_by.is_empty(),
                conditions: Vec::new(),
                blocked_by: blocked_by.clone(),
                time_cost: clean_time,
                risk: if clean_risk.is_empty() {
                    travel::infer_risk(kind, label, destination_hint)
                } else {
                    clean_risk.to_string()
                },
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
        Action::RevealPlace { place_id } => expand_place_interior(canon, place_id, turn),
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

/// Move the player along a transition, lazily creating/expanding the destination
/// as needed and guaranteeing a return edge (TZ §7.4 "can always leave").
fn apply_move_player(
    canon: &mut WorldCanon,
    transition_id: &str,
    turn: i64,
    time_override: i64,
    source: &str,
    reason: &str,
) -> Vec<CanonEvent> {
    let mut events = Vec::new();
    let start_minutes = canon.clock_minutes;

    // Snapshot what we need before mutating.
    let (from_place, mut to_place, dest_hint, label, kind, stored_time, stored_risk) = {
        let t = canon
            .transition(transition_id)
            .expect("validated transition");
        (
            t.from_place.clone(),
            t.to_place.clone(),
            t.destination_hint.clone(),
            t.label.clone(),
            t.kind.clone(),
            t.time_cost,
            t.risk.clone(),
        )
    };
    let travel_minutes = if time_override > 0 {
        time_override
    } else {
        travel::normalized_time_cost(&kind, &label, &dest_hint, stored_time)
    };
    let risk = travel::normalized_risk(&kind, &label, &dest_hint, &stored_risk);

    // Lazily materialise a dangling (shell) edge target.
    if to_place.is_empty() {
        let new_place_id = ids::stable_id(&canon.world_seed, &from_place, "place", transition_id);
        // Name pick: the destination hint, unless it is a coercion placeholder
        // ("unknown destination") or a bare id slug (no whitespace) — neither
        // may become a player-facing place title. Fall back to the transition
        // label, itself stripped of a trailing "-> id" (old saves carry the
        // unsplit architect string).
        let hint = dest_hint.trim();
        let hint_usable = !hint.is_empty()
            && !crate::helpers::is_placeholder_destination(hint)
            && hint.chars().any(char::is_whitespace);
        let name = if hint_usable {
            hint.to_string()
        } else {
            crate::helpers::split_exit_label(&label).0
        };
        let mut flags = BTreeSet::new();
        // A freshly lazy-generated destination is itself a shell interior until
        // entered/expanded, EXCEPT we keep simple "named exits" concrete; here we
        // mark it shell so the first entry expands an interior chain.
        flags.insert("shell".to_string());
        canon.insert_place(Place {
            place_id: new_place_id.clone(),
            name: nonempty(&name, "Неизведанное место").to_string(),
            kind: "lazy".to_string(),
            parent: String::new(),
            region_id: String::new(),
            default_description: String::new(),
            state_flags: flags,
            features: Vec::new(),
            transition_ids: Vec::new(),
            occupant_ids: BTreeSet::new(),
            item_ids: Vec::new(),
            event_ids: Vec::new(),
            fact_ids: Vec::new(),
            provenance: Provenance::by("lazy_gen", "materialised dangling exit", turn),
        });
        // Wire the forward edge's target.
        if let Some(t) = canon.transitions.get_mut(transition_id) {
            t.to_place = new_place_id.clone();
        }
        // Add a back transition so the player can always return (TZ §7.4).
        let back_id = ids::stable_id(&canon.world_seed, &new_place_id, "transition", &from_place);
        if !canon.transitions.contains_key(&back_id) {
            canon.insert_transition(Transition {
                transition_id: back_id.clone(),
                source_exit_id: back_id.clone(),
                from_place: new_place_id.clone(),
                to_place: from_place.clone(),
                destination_hint: String::new(),
                label: "Назад".to_string(),
                kind: "back".to_string(),
                visible: true,
                passable: true,
                conditions: Vec::new(),
                blocked_by: String::new(),
                time_cost: travel_minutes,
                risk: risk.clone(),
                provenance: Provenance::by("lazy_gen", "return path", turn),
            });
        }
        events.push(event(
            canon,
            "create_place",
            turn,
            start_minutes,
            &new_place_id,
            &[],
            "lazy materialise destination",
            &[format!("place:{new_place_id}")],
            &Scope::GmPrivate,
            &[],
        ));
        to_place = new_place_id;
    }

    if can_create_travel_situation(canon) {
        if let Some(situation) = travel::roll_travel_situation(travel::TravelRoll {
            world_seed: &canon.world_seed,
            transition_id,
            from_place: &from_place,
            to_place: &to_place,
            turn,
            start_minutes,
            duration_minutes: travel_minutes,
            risk: &risk,
        }) {
            events.extend(apply_travel_situation(
                canon,
                transition_id,
                &from_place,
                &to_place,
                &risk,
                &situation,
                turn,
                reason,
            ));
            let now = canon.clock_minutes;
            events.extend(tick_offscreen(canon, now, turn));
            let _ = source;
            return events;
        }
    }

    canon.clock_minutes += travel_minutes.max(0);
    let now = canon.clock_minutes;

    // Move the player.
    canon.player_place_id = to_place.clone();
    if let Some(p) = canon.places.get_mut(&to_place) {
        p.mark_visited();
    }

    events.push(event(
        canon,
        "move_player",
        turn,
        now,
        &to_place,
        &[],
        nonempty(reason, "player traversal"),
        &[
            format!("player_at:{to_place}"),
            format!("via:{transition_id}"),
        ],
        &Scope::Player,
        &[],
    ));
    let _ = source;

    // If the destination is a shell, expand its interior now (first entry).
    let is_shell = canon
        .place(&to_place)
        .map(|p| p.has_flag("shell"))
        .unwrap_or(false);
    if is_shell {
        events.extend(expand_place_interior(canon, &to_place, turn));
    }
    events.extend(tick_offscreen(canon, now, turn));

    events
}

#[allow(clippy::too_many_arguments)]
fn apply_travel_situation(
    canon: &mut WorldCanon,
    transition_id: &str,
    from_place: &str,
    to_place: &str,
    risk: &str,
    situation: &travel::TravelSituation,
    turn: i64,
    reason: &str,
) -> Vec<CanonEvent> {
    let mut events = Vec::new();
    let site_id = situation.site_id.clone();
    let region_id = canon
        .place(from_place)
        .map(|p| p.region_id.clone())
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
            default_description: situation.summary.clone(),
            state_flags: flags,
            features: vec!["дорожная ситуация".to_string()],
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
            from_place: site_id.clone(),
            to_place: to_place.to_string(),
            destination_hint: String::new(),
            label: "Продолжить путь".to_string(),
            kind: "road_segment".to_string(),
            visible: true,
            passable: true,
            conditions: Vec::new(),
            blocked_by: String::new(),
            time_cost: situation.remaining_minutes,
            risk: risk.to_string(),
            provenance: Provenance::by("travel", "remaining route", turn),
        });
    }
    let back_id = ids::stable_id(&canon.world_seed, &site_id, "transition", from_place);
    if !canon.transitions.contains_key(&back_id) {
        canon.insert_transition(Transition {
            transition_id: back_id,
            source_exit_id: ids::stable_id(&canon.world_seed, &site_id, "exit", from_place),
            from_place: site_id.clone(),
            to_place: from_place.to_string(),
            destination_hint: String::new(),
            label: "Вернуться назад".to_string(),
            kind: "road_segment".to_string(),
            visible: true,
            passable: true,
            conditions: Vec::new(),
            blocked_by: String::new(),
            time_cost: situation.elapsed_minutes,
            risk: risk.to_string(),
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
            format!("На дороге возникает ситуация: {}", situation.summary),
            format!(
                "До цели остается примерно {} мин.",
                situation.remaining_minutes
            ),
            format!("situation_type:{}", situation.tone),
            format!("rarity:{}", situation.rarity),
            format!("chance_percent:{}", situation.chance_percent),
            format!("roll:{}", situation.roll),
            format!("elapsed_minutes:{}", situation.elapsed_minutes),
            format!("remaining_minutes:{}", situation.remaining_minutes),
        ],
        &Scope::Player,
        &["дорожные следы".to_string()],
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

/// Lazily generate a shell place's interior: a bounded chain of 2..=4 rooms, each
/// linked forward AND back so the player can always leave (TZ §7.4). Removes the
/// `shell` flag. Deterministic — uses only [`ids::DetRng`] and stable ids, zero
/// campaign RNG. Bounded by `gen_budget.max_rooms_per_turn` /
/// `max_events_per_turn`.
pub fn expand_place_interior(canon: &mut WorldCanon, place_id: &str, turn: i64) -> Vec<CanonEvent> {
    let mut events = Vec::new();
    // Only expand a shell.
    let was_shell = canon
        .place(place_id)
        .map(|p| p.has_flag("shell"))
        .unwrap_or(false);
    if !was_shell {
        return events;
    }
    // Remove the shell flag (it is now revealed).
    if let Some(p) = canon.places.get_mut(place_id) {
        p.state_flags.remove("shell");
        p.state_flags.insert("revealed".to_string());
        if p.kind.is_empty() || p.kind == "lazy" {
            p.kind = "dungeon_room".to_string();
        }
    }

    let mut rng = ids::DetRng::from_parts(&[&canon.world_seed, place_id, "interior"]);
    let cap = canon.gen_budget.max_rooms_per_turn.max(1);
    // Each room adds a forward + back transition (2 edges), so the room count is
    // also bounded by the per-turn transition budget (TZ §7.3 bounded).
    let transition_cap = (canon.gen_budget.max_transitions_per_turn / 2).max(1);
    let n_rooms = rng.range(2, 4).min(cap).min(transition_cap);
    let max_events = canon.gen_budget.max_events_per_turn;
    // Transition budget (TZ §7.3 "bounded"): each interior room wires a forward
    // and a back edge (2 transitions). Stop creating rooms once another room's
    // edges would exceed `max_transitions_per_turn`.
    let max_transitions = canon.gen_budget.max_transitions_per_turn;
    let mut transitions_made = 0usize;

    let room_themes = [
        "Тёмный коридор",
        "Сырая келья",
        "Обвалившийся зал",
        "Старая крипта",
    ];

    let mut prev = place_id.to_string();
    for i in 0..n_rooms {
        // A room needs up to 2 new transitions; respect the per-turn cap.
        if transitions_made + 2 > max_transitions {
            break;
        }
        let room_id = ids::stable_id(&canon.world_seed, place_id, "room", &i.to_string());
        if canon.places.contains_key(&room_id) {
            prev = room_id;
            continue;
        }
        let theme = rng.pick(&room_themes);
        let mut flags = BTreeSet::new();
        flags.insert("interior".to_string());
        canon.insert_place(Place {
            place_id: room_id.clone(),
            name: format!("{theme} {}", i + 1),
            kind: "dungeon_room".to_string(),
            parent: place_id.to_string(),
            region_id: String::new(),
            default_description: String::new(),
            state_flags: flags,
            features: Vec::new(),
            transition_ids: Vec::new(),
            occupant_ids: BTreeSet::new(),
            item_ids: Vec::new(),
            event_ids: Vec::new(),
            fact_ids: Vec::new(),
            provenance: Provenance::by("lazy_gen", "interior room", turn),
        });
        if events.len() < max_events {
            events.push(event(
                canon,
                "create_place",
                turn,
                canon.clock_minutes,
                &room_id,
                &[],
                "interior room",
                &[format!("place:{room_id}")],
                &Scope::GmPrivate,
                &[],
            ));
        }

        // Forward edge prev -> room.
        let fwd = ids::stable_id(&canon.world_seed, &prev, "transition", &room_id);
        if !canon.transitions.contains_key(&fwd) {
            canon.insert_transition(Transition {
                transition_id: fwd.clone(),
                source_exit_id: fwd.clone(),
                from_place: prev.clone(),
                to_place: room_id.clone(),
                destination_hint: String::new(),
                label: "Дальше".to_string(),
                kind: "passage".to_string(),
                visible: true,
                passable: true,
                conditions: Vec::new(),
                blocked_by: String::new(),
                time_cost: 2,
                risk: "none: short interior passage".to_string(),
                provenance: Provenance::by("lazy_gen", "interior passage", turn),
            });
            transitions_made += 1;
            if events.len() < max_events {
                events.push(event(
                    canon,
                    "create_transition",
                    turn,
                    canon.clock_minutes,
                    &prev,
                    &[],
                    "interior passage",
                    &[format!("transition:{fwd}")],
                    &Scope::GmPrivate,
                    &[],
                ));
            }
        }
        // Back edge room -> prev.
        let back = ids::stable_id(&canon.world_seed, &room_id, "transition", &prev);
        if !canon.transitions.contains_key(&back) {
            canon.insert_transition(Transition {
                transition_id: back.clone(),
                source_exit_id: back.clone(),
                from_place: room_id.clone(),
                to_place: prev.clone(),
                destination_hint: String::new(),
                label: "Назад".to_string(),
                kind: "back".to_string(),
                visible: true,
                passable: true,
                conditions: Vec::new(),
                blocked_by: String::new(),
                time_cost: 2,
                risk: "none: short interior return".to_string(),
                provenance: Provenance::by("lazy_gen", "interior return", turn),
            });
            transitions_made += 1;
        }
        prev = room_id;
    }

    events
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
        text: "Говорят, что-то случилось в стороне.".to_string(),
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
