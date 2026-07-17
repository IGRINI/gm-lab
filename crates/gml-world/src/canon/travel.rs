//! Travel-time and road-situation rules for canon traversal.
//!
//! This module deliberately uses deterministic generation (`DetRng`) rather
//! than campaign dice RNG. Travel situations must replay from the same
//! world/transition/time inputs without perturbing player-facing dice rolls.

use super::{
    ids::{self, DetRng},
    PassageDirectionality, Transition, WorldCanon,
};

pub const SITUATION_THRESHOLD_MINUTES: i64 = 30;

/// Structured encounter risk for a transition.
///
/// Persistence still stores the canonical lowercase string, but runtime rules
/// only consume values that parse as one of these exact variants. Free-form
/// prose belongs in a separate description field and never changes travel
/// mechanics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TravelRisk {
    None,
    Low,
    Medium,
    High,
    Certain,
}

impl TravelRisk {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "certain" => Some(Self::Certain),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Certain => "certain",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TravelSituation {
    pub site_id: String,
    pub title: String,
    pub summary: String,
    pub elapsed_minutes: i64,
    pub remaining_minutes: i64,
    pub chance_percent: u8,
    pub roll: u8,
    pub tone: &'static str,
    pub rarity: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TravelRoll<'a> {
    pub world_seed: &'a str,
    pub transition_id: &'a str,
    pub from_place: &'a str,
    pub to_place: &'a str,
    pub turn: i64,
    pub start_minutes: i64,
    pub duration_minutes: i64,
    pub risk: TravelRisk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SituationTone {
    Good,
    Bad,
    Neutral,
    Mixed,
}

impl SituationTone {
    fn as_str(self) -> &'static str {
        match self {
            SituationTone::Good => "good",
            SituationTone::Bad => "bad",
            SituationTone::Neutral => "neutral",
            SituationTone::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rarity {
    Common,
    Uncommon,
    Rare,
    Legendary,
}

impl Rarity {
    fn as_str(self) -> &'static str {
        match self {
            Rarity::Common => "common",
            Rarity::Uncommon => "uncommon",
            Rarity::Rare => "rare",
            Rarity::Legendary => "legendary",
        }
    }
}

fn reciprocal_transition<'a>(
    canon: &'a WorldCanon,
    transition: &Transition,
) -> Option<&'a Transition> {
    if transition.directionality != PassageDirectionality::Bidirectional
        || transition.passage_id.is_empty()
    {
        return None;
    }

    let mut candidates = canon.transitions.values().filter(|candidate| {
        candidate.transition_id != transition.transition_id
            && candidate.directionality == PassageDirectionality::Bidirectional
            && candidate.passage_id == transition.passage_id
            && candidate.from_place == transition.to_place
            && candidate.to_place == transition.from_place
    });
    let candidate = candidates.next()?;
    candidates.next().is_none().then_some(candidate)
}

fn transition_profiles_match(left: &Transition, right: &Transition) -> bool {
    left.kind == right.kind && left.time_cost == right.time_cost && left.risk == right.risk
}

/// Returns true only for an explicitly identified bidirectional passage whose
/// two directed sides have conflicting mechanical profiles. Labels may differ
/// because they are directional display text; kind, duration, and risk belong
/// to the shared physical passage.
pub fn has_asymmetric_reciprocal_profile(canon: &WorldCanon, transition_id: &str) -> bool {
    let Some(transition) = canon.transition(transition_id) else {
        return false;
    };
    reciprocal_transition(canon, transition)
        .is_some_and(|reciprocal| !transition_profiles_match(transition, reciprocal))
}

/// Invalidates an explicitly identified bidirectional passage when its two
/// stored mechanical profiles disagree. No side is chosen as authoritative
/// and no value is copied or inferred. The location creator must provide one
/// new shared profile before either direction can be traversed again.
pub fn invalidate_asymmetric_reciprocal_profiles(canon: &mut WorldCanon) -> usize {
    let mut invalidated_ids = std::collections::BTreeSet::new();
    for transition in canon.transitions.values() {
        let Some(reciprocal) = reciprocal_transition(canon, transition) else {
            continue;
        };
        if transition.transition_id >= reciprocal.transition_id {
            continue;
        }
        if !transition_profiles_match(transition, reciprocal) {
            invalidated_ids.insert(transition.transition_id.clone());
            invalidated_ids.insert(reciprocal.transition_id.clone());
        }
    }

    for transition_id in &invalidated_ids {
        let Some(transition) = canon.transitions.get_mut(transition_id) else {
            continue;
        };
        transition.kind.clear();
        transition.time_cost = 0;
        transition.risk.clear();
    }
    invalidated_ids.len()
}

pub fn situation_chance_percent(duration_minutes: i64, risk: TravelRisk) -> u8 {
    if duration_minutes <= SITUATION_THRESHOLD_MINUTES || risk == TravelRisk::None {
        return 0;
    }
    if risk == TravelRisk::Certain {
        return 100;
    }
    let base = match risk {
        TravelRisk::Low => 18,
        TravelRisk::Medium => 30,
        TravelRisk::High => 64,
        TravelRisk::None => unreachable!("none risk returned above"),
        TravelRisk::Certain => unreachable!("certain risk returned above"),
    };
    let time_bonus = ((duration_minutes - SITUATION_THRESHOLD_MINUTES) / 120).clamp(0, 20) as u8;
    (base as u8).saturating_add(time_bonus).min(85)
}

pub fn roll_travel_situation(input: TravelRoll<'_>) -> Option<TravelSituation> {
    let chance = situation_chance_percent(input.duration_minutes, input.risk);
    if chance == 0 {
        return None;
    }

    let turn_s = input.turn.to_string();
    let start_s = input.start_minutes.to_string();
    let duration_s = input.duration_minutes.to_string();
    let mut rng = DetRng::from_parts(&[
        input.world_seed,
        input.transition_id,
        input.from_place,
        input.to_place,
        &turn_s,
        &start_s,
        &duration_s,
        "travel_situation",
    ]);
    let roll = rng.range(1, 100) as u8;
    if roll > chance {
        return None;
    }

    let latest = (input.duration_minutes - 1).max(1) as usize;
    let elapsed = rng.range(1, latest) as i64;
    let remaining = (input.duration_minutes - elapsed).max(0);
    let tone = pick_tone(&mut rng, input.risk);
    let rarity = pick_rarity(&mut rng, input.risk, input.duration_minutes);
    let salt = format!("{}:{}:{elapsed}", input.turn, input.start_minutes);
    let site_id = ids::stable_id(input.world_seed, input.transition_id, "travel_site", &salt);
    let title = title_for(tone);
    let summary = summary_for(tone);

    Some(TravelSituation {
        site_id,
        title: title.to_string(),
        summary: summary.to_string(),
        elapsed_minutes: elapsed,
        remaining_minutes: remaining,
        chance_percent: chance,
        roll,
        tone: tone.as_str(),
        rarity: rarity.as_str(),
    })
}

fn pick_tone(rng: &mut DetRng, risk: TravelRisk) -> SituationTone {
    let weights = match risk {
        TravelRisk::High | TravelRisk::Certain => [
            (SituationTone::Good, 10),
            (SituationTone::Bad, 45),
            (SituationTone::Neutral, 20),
            (SituationTone::Mixed, 25),
        ],
        TravelRisk::Low => [
            (SituationTone::Good, 22),
            (SituationTone::Bad, 18),
            (SituationTone::Neutral, 38),
            (SituationTone::Mixed, 22),
        ],
        TravelRisk::None | TravelRisk::Medium => [
            (SituationTone::Good, 18),
            (SituationTone::Bad, 28),
            (SituationTone::Neutral, 32),
            (SituationTone::Mixed, 22),
        ],
    };
    pick_weighted(rng, &weights)
}

fn pick_rarity(rng: &mut DetRng, risk: TravelRisk, duration_minutes: i64) -> Rarity {
    let long_or_dangerous =
        duration_minutes >= 8 * 60 || matches!(risk, TravelRisk::High | TravelRisk::Certain);
    let weights = if long_or_dangerous {
        [
            (Rarity::Common, 58),
            (Rarity::Uncommon, 28),
            (Rarity::Rare, 12),
            (Rarity::Legendary, 2),
        ]
    } else {
        [
            (Rarity::Common, 70),
            (Rarity::Uncommon, 22),
            (Rarity::Rare, 7),
            (Rarity::Legendary, 1),
        ]
    };
    pick_weighted(rng, &weights)
}

fn pick_weighted<T: Copy>(rng: &mut DetRng, items: &[(T, u32)]) -> T {
    let total: u32 = items.iter().map(|(_, w)| *w).sum();
    let mut roll = rng.below(total.max(1) as usize) as u32;
    for (item, weight) in items {
        if roll < *weight {
            return *item;
        }
        roll -= *weight;
    }
    items[0].0
}

fn title_for(tone: SituationTone) -> &'static str {
    match tone {
        SituationTone::Good => "Возможность на дороге",
        SituationTone::Bad => "Угроза на дороге",
        SituationTone::Neutral => "Встреча на дороге",
        SituationTone::Mixed => "Выбор на дороге",
    }
}

fn summary_for(tone: SituationTone) -> &'static str {
    match tone {
        SituationTone::Good => {
            "путь открывает полезную возможность: след, помощь или находку"
        }
        SituationTone::Bad => {
            "дорогу осложняет заметная угроза: опасные следы, препятствие или чужое присутствие"
        }
        SituationTone::Neutral => {
            "путь прерывает встреча, знак или след, требующий решения"
        }
        SituationTone::Mixed => {
            "дорога предлагает шанс с ценой: можно выиграть сведения, время или добычу, но это несет риск"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canon::Provenance;

    fn transition(
        id: &str,
        from: &str,
        to: &str,
        kind: &str,
        time_cost: i64,
        risk: &str,
        reason: &str,
    ) -> Transition {
        Transition {
            transition_id: id.to_string(),
            source_exit_id: id.to_string(),
            passage_id: id.to_string(),
            directionality: PassageDirectionality::OneWay,
            from_place: from.to_string(),
            to_place: to.to_string(),
            label: format!("route-{id}"),
            kind: kind.to_string(),
            visible: true,
            passable: true,
            time_cost,
            risk: risk.to_string(),
            provenance: Provenance::by("llm", reason, 3),
            ..Default::default()
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn bidirectional_transition(
        id: &str,
        passage_id: &str,
        from: &str,
        to: &str,
        kind: &str,
        time_cost: i64,
        risk: &str,
        reason: &str,
    ) -> Transition {
        Transition {
            passage_id: passage_id.to_string(),
            directionality: PassageDirectionality::Bidirectional,
            ..transition(id, from, to, kind, time_cost, risk, reason)
        }
    }

    #[test]
    fn travel_risk_parser_accepts_only_structured_values() {
        for (raw, expected) in [
            ("none", TravelRisk::None),
            ("low", TravelRisk::Low),
            ("medium", TravelRisk::Medium),
            ("high", TravelRisk::High),
            ("certain", TravelRisk::Certain),
        ] {
            assert_eq!(TravelRisk::parse(raw), Some(expected));
            assert_eq!(expected.as_str(), raw);
        }

        assert_eq!(TravelRisk::parse("High"), None);
        assert_eq!(TravelRisk::parse(" high "), None);
        assert_eq!(TravelRisk::parse("wild road"), None);
        assert_eq!(TravelRisk::parse("high: bandits"), None);
    }

    #[test]
    fn structured_risk_controls_situation_chance() {
        assert_eq!(situation_chance_percent(31, TravelRisk::None), 0);
        assert_eq!(situation_chance_percent(24 * 60, TravelRisk::None), 0);
        assert_eq!(situation_chance_percent(31, TravelRisk::Low), 18);
        assert_eq!(situation_chance_percent(31, TravelRisk::Medium), 30);
        assert_eq!(situation_chance_percent(31, TravelRisk::High), 64);
        assert_eq!(situation_chance_percent(31, TravelRisk::Certain), 100);
        assert_eq!(situation_chance_percent(30, TravelRisk::Certain), 0);
    }

    #[test]
    fn asymmetric_reciprocal_profiles_are_invalidated_without_copying_values() {
        let mut canon = WorldCanon {
            world_seed: "legacy-travel".to_string(),
            ..Default::default()
        };
        let forward_id = ids::stable_id(&canon.world_seed, "alley", "transition", "shop");
        let return_id = ids::stable_id(&canon.world_seed, "shop", "transition", "alley");
        canon.insert_transition(bidirectional_transition(
            &forward_id,
            "shop_door",
            "alley",
            "shop",
            "path",
            4,
            "medium",
            "link generated place",
        ));
        canon.insert_transition(bidirectional_transition(
            &return_id,
            "shop_door",
            "shop",
            "alley",
            "back",
            1,
            "none",
            "return from generated place",
        ));

        assert!(has_asymmetric_reciprocal_profile(&canon, &forward_id));
        assert!(has_asymmetric_reciprocal_profile(&canon, &return_id));
        assert_eq!(invalidate_asymmetric_reciprocal_profiles(&mut canon), 2);
        for transition_id in [&forward_id, &return_id] {
            let transition = canon.transition(transition_id).expect("transition");
            assert_eq!(transition.time_cost, 0);
            assert!(transition.risk.is_empty());
            assert!(transition.kind.is_empty());
        }
        assert!(!has_asymmetric_reciprocal_profile(&canon, &forward_id));
        assert_eq!(invalidate_asymmetric_reciprocal_profiles(&mut canon), 0);
    }

    #[test]
    fn matching_reciprocal_profiles_are_left_unchanged() {
        let mut canon = WorldCanon {
            world_seed: "legacy-incomplete".to_string(),
            ..Default::default()
        };
        let forward_id = ids::stable_id(&canon.world_seed, "alley", "transition", "shop");
        let return_id = ids::stable_id(&canon.world_seed, "shop", "transition", "alley");
        canon.insert_transition(bidirectional_transition(
            &forward_id,
            "shop_door",
            "alley",
            "shop",
            "path",
            4,
            "low",
            "link generated place",
        ));
        canon.insert_transition(bidirectional_transition(
            &return_id,
            "shop_door",
            "shop",
            "alley",
            "path",
            4,
            "low",
            "return from generated place",
        ));

        let before = canon.clone();
        assert!(!has_asymmetric_reciprocal_profile(&canon, &forward_id));
        assert_eq!(invalidate_asymmetric_reciprocal_profiles(&mut canon), 0);
        assert_eq!(canon, before);
    }

    #[test]
    fn leaves_one_way_or_ambiguous_transitions_unchanged() {
        let mut ambiguous = WorldCanon {
            world_seed: "ambiguous".to_string(),
            ..Default::default()
        };
        ambiguous.insert_transition(transition(
            "one-way", "ridge", "river", "path", 4, "medium", "authored",
        ));
        ambiguous.insert_transition(transition(
            "door-forward",
            "alley",
            "shop",
            "door",
            1,
            "none",
            "authored",
        ));
        ambiguous.insert_transition(transition(
            "path-forward",
            "alley",
            "shop",
            "path",
            4,
            "medium",
            "authored",
        ));
        ambiguous.insert_transition(transition(
            "legacy-return",
            "shop",
            "alley",
            "back",
            1,
            "none",
            "legacy inferred return path",
        ));
        let before = ambiguous.clone();
        assert_eq!(invalidate_asymmetric_reciprocal_profiles(&mut ambiguous), 0);
        assert_eq!(ambiguous, before);
        assert_eq!(ambiguous.transition("legacy-return").unwrap().time_cost, 1);
    }

    #[test]
    fn opposite_one_way_passages_between_same_places_are_never_paired() {
        let mut canon = WorldCanon::default();
        canon.insert_transition(transition(
            "fall",
            "cave",
            "chasm",
            "drop",
            1,
            "high",
            "one-way fall",
        ));
        canon.insert_transition(transition(
            "climb",
            "chasm",
            "cave",
            "climb",
            15,
            "medium",
            "separate climb",
        ));

        assert!(!has_asymmetric_reciprocal_profile(&canon, "fall"));
        assert!(!has_asymmetric_reciprocal_profile(&canon, "climb"));
        let before = canon.clone();
        assert_eq!(invalidate_asymmetric_reciprocal_profiles(&mut canon), 0);
        assert_eq!(canon, before);
    }

    #[test]
    fn different_passage_ids_are_not_paired_even_for_opposite_bidirectional_edges() {
        let mut canon = WorldCanon::default();
        canon.insert_transition(bidirectional_transition(
            "door",
            "door_passage",
            "cave",
            "chasm",
            "door",
            2,
            "none",
            "door route",
        ));
        canon.insert_transition(bidirectional_transition(
            "rope",
            "rope_passage",
            "chasm",
            "cave",
            "climb",
            15,
            "medium",
            "separate rope route",
        ));

        assert!(!has_asymmetric_reciprocal_profile(&canon, "door"));
        assert!(!has_asymmetric_reciprocal_profile(&canon, "rope"));
        let before = canon.clone();
        assert_eq!(invalidate_asymmetric_reciprocal_profiles(&mut canon), 0);
        assert_eq!(canon, before);
    }

    #[test]
    fn legacy_endpoint_reverses_never_gain_reciprocal_identity() {
        let mut canon = WorldCanon::default();
        let mut forward = transition(
            "legacy_forward",
            "cave",
            "chasm",
            "drop",
            1,
            "high",
            "legacy route",
        );
        forward.passage_id.clear();
        forward.directionality = PassageDirectionality::Unspecified;
        let mut reverse = transition(
            "legacy_reverse",
            "chasm",
            "cave",
            "climb",
            15,
            "medium",
            "legacy route",
        );
        reverse.passage_id.clear();
        reverse.directionality = PassageDirectionality::Unspecified;
        canon.insert_transition(forward);
        canon.insert_transition(reverse);

        assert!(!has_asymmetric_reciprocal_profile(&canon, "legacy_forward"));
        assert!(!has_asymmetric_reciprocal_profile(&canon, "legacy_reverse"));
        let before = canon.clone();
        assert_eq!(invalidate_asymmetric_reciprocal_profiles(&mut canon), 0);
        assert_eq!(canon, before);
    }
}
