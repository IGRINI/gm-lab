//! Deterministic rumor propagation helpers.
//!
//! The compatibility surface is still `World.rumors: Vec<Rumor>`, but these
//! helpers keep each rumor addressable through scoped memory and make spread
//! decisions from canon graph state instead of prose.

use std::collections::BTreeSet;

use crate::helpers::actor_key;
use crate::model::Rumor;

use super::{
    MemoryInjectionState, MemoryTier, MemoryTruthStatus, MemoryUnit, Transition, WorldCanon,
};

pub const PLACE_SPREAD_THRESHOLD_MINUTES: i64 = 60;
pub const WIDER_SPREAD_THRESHOLD_MINUTES: i64 = 6 * 60;
pub const DECAY_BUCKET_MINUTES: i64 = 24 * 60;

pub fn memory_id_for_rumor(rumor: &Rumor) -> String {
    format!("rumor:{}", actor_key(&rumor.rumor_id))
}

pub fn route_scope_for_transition(transition_id: &str) -> String {
    format!("route:{}", actor_key(transition_id))
}

pub fn scopes_for_place(
    canon: &WorldCanon,
    place_id: &str,
    include_wide: bool,
) -> BTreeSet<String> {
    let mut scopes = BTreeSet::new();
    let mut current = actor_key(place_id);
    let mut guard = 0usize;
    while !current.is_empty() && guard < 16 {
        guard += 1;
        scopes.insert(format!("place:{current}"));
        let Some(place) = canon.places.get(&current) else {
            break;
        };
        if include_wide && !place.region_id.is_empty() {
            scopes.insert(format!("region:{}", actor_key(&place.region_id)));
        }
        if include_wide {
            if let Some(district) = canon.district(&place.district_id) {
                scopes.insert(format!("district:{}", actor_key(&district.district_id)));
                if let Some(settlement) = canon.settlement(&district.settlement_id) {
                    scopes.insert(format!(
                        "settlement:{}",
                        actor_key(&settlement.settlement_id)
                    ));
                    if !settlement.region_id.is_empty() {
                        scopes.insert(format!("region:{}", actor_key(&settlement.region_id)));
                    }
                }
            }
        }
        if place.parent.is_empty() {
            break;
        }
        let parent = actor_key(&place.parent);
        if canon.districts.contains_key(&parent) {
            break;
        }
        if canon.settlements.contains_key(&parent) {
            if include_wide {
                scopes.insert(format!("settlement:{parent}"));
                if let Some(settlement) = canon.settlements.get(&parent) {
                    if !settlement.region_id.is_empty() {
                        scopes.insert(format!("region:{}", actor_key(&settlement.region_id)));
                    }
                }
            }
            break;
        }
        if parent == current {
            break;
        }
        current = parent;
    }
    scopes
}

pub fn scopes_for_transition(transition: &Transition) -> BTreeSet<String> {
    let mut scopes = BTreeSet::new();
    if is_route_like(transition) {
        scopes.insert(route_scope_for_transition(&transition.transition_id));
    }
    scopes
}

pub fn scopes_added_by_carrier_at_place(
    canon: &WorldCanon,
    place_id: &str,
    elapsed_minutes: i64,
) -> BTreeSet<String> {
    if elapsed_minutes < PLACE_SPREAD_THRESHOLD_MINUTES {
        return BTreeSet::new();
    }
    scopes_for_place(
        canon,
        place_id,
        elapsed_minutes >= WIDER_SPREAD_THRESHOLD_MINUTES,
    )
}

pub fn should_spread_place_rumor(elapsed_minutes: i64, strength: i64) -> bool {
    strength > 0 && elapsed_minutes >= PLACE_SPREAD_THRESHOLD_MINUTES
}

pub fn should_decay_rumor(elapsed_minutes: i64) -> i64 {
    if elapsed_minutes < DECAY_BUCKET_MINUTES {
        0
    } else {
        elapsed_minutes / DECAY_BUCKET_MINUTES
    }
}

pub fn memory_unit_for_rumor(rumor: &Rumor, canon: &WorldCanon) -> MemoryUnit {
    let mut place_ids = Vec::new();
    for scope in &rumor.known_in {
        if let Some(place_id) = scope.strip_prefix("place:") {
            place_ids.push(place_id.to_string());
        }
    }
    place_ids.sort();
    place_ids.dedup();

    let mut actor_ids: Vec<String> = rumor
        .carriers
        .iter()
        .map(|id| actor_key(id))
        .filter(|id| !id.is_empty())
        .collect();
    let speaker = actor_key(&rumor.speaker);
    if !speaker.is_empty() {
        actor_ids.push(speaker);
    }
    actor_ids.sort();
    actor_ids.dedup();

    let status = if rumor.confirmed {
        MemoryTruthStatus::Claim
    } else {
        MemoryTruthStatus::Rumor
    };
    let mut details = vec![
        format!("speaker: {}", rumor.speaker),
        format!("origin_scope: {}", rumor.origin_scope),
        format!("strength: {}", rumor.strength),
        format!("distortion: {}", rumor.distortion),
        format!("created_minutes: {}", rumor.created_minutes),
        format!("last_spread_minutes: {}", rumor.last_spread_minutes),
    ];
    if let Some(place_id) = place_ids.first() {
        if let Some(place) = canon.place(place_id) {
            details.push(format!("first_known_place: {}", place.name));
        }
    }

    let summary_label = if canon.content_locale.is_russian() {
        "Слух"
    } else {
        "Rumor"
    };

    MemoryUnit {
        memory_id: memory_id_for_rumor(rumor),
        tier: MemoryTier::Raw,
        owner_scope: if rumor.origin_scope.trim().is_empty() {
            "gm_private".to_string()
        } else {
            rumor.origin_scope.clone()
        },
        visibility_scopes: rumor.known_in.iter().cloned().collect(),
        summary: format!("{summary_label}: {}", rumor.text),
        details: details.join("\n"),
        place_ids,
        actor_ids,
        topic_tags: vec![
            "rumor".to_string(),
            format!("rumor_strength:{}", rumor.strength),
            format!("rumor_distortion:{}", rumor.distortion),
        ],
        confidence: Some(if rumor.confirmed { 80 } else { 45 }),
        truth_status: status,
        injection_state: if rumor.strength <= 0 {
            MemoryInjectionState::Cold
        } else {
            MemoryInjectionState::Hot
        },
        created_by: "rumor_graph".to_string(),
        ..Default::default()
    }
}

fn is_route_like(transition: &Transition) -> bool {
    matches!(
        transition.kind.as_str(),
        "road" | "route" | "path" | "trail" | "road_segment"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gml_types::ContentLocale;
    use std::collections::BTreeSet;

    #[test]
    fn route_classification_uses_only_the_structured_kind() {
        let mut transition = Transition {
            label: "Дорога через лес".to_string(),
            destination_hint: "Северный тракт".to_string(),
            risk: "high".to_string(),
            ..Default::default()
        };
        assert!(!is_route_like(&transition));

        transition.kind = "road".to_string();
        assert!(is_route_like(&transition));

        transition.kind = "Road".to_string();
        assert!(!is_route_like(&transition));
    }

    #[test]
    fn rumor_memory_summary_follows_content_locale() {
        let rumor = Rumor {
            rumor_id: "whisper".to_string(),
            seq: 1,
            turn: 1,
            speaker: "guide".to_string(),
            text: "the bridge is closed".to_string(),
            witnesses: BTreeSet::new(),
            origin_scope: "public".to_string(),
            known_in: BTreeSet::new(),
            carriers: BTreeSet::new(),
            strength: 1,
            distortion: 0,
            created_minutes: 0,
            last_spread_minutes: 0,
            confirmed: false,
        };

        for (locale, expected) in [
            (ContentLocale::Russian, "Слух: the bridge is closed"),
            (ContentLocale::English, "Rumor: the bridge is closed"),
        ] {
            let canon = WorldCanon {
                content_locale: locale,
                ..Default::default()
            };
            assert_eq!(memory_unit_for_rumor(&rumor, &canon).summary, expected);
        }
    }
}
