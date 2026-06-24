//! Travel-time and road-situation rules for canon traversal.
//!
//! This module deliberately uses deterministic generation (`DetRng`) rather
//! than campaign dice RNG. Travel situations must replay from the same
//! world/transition/time inputs without perturbing player-facing dice rolls.

use super::ids::{self, DetRng};

pub const SITUATION_THRESHOLD_MINUTES: i64 = 30;

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
    pub risk: &'a str,
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

pub fn normalized_time_cost(kind: &str, label: &str, destination_hint: &str, stored: i64) -> i64 {
    if stored > 0 {
        stored
    } else {
        infer_time_cost(kind, label, destination_hint)
    }
}

pub fn normalized_risk(kind: &str, label: &str, destination_hint: &str, stored: &str) -> String {
    let clean = stored.trim();
    if !clean.is_empty() {
        clean.to_string()
    } else {
        infer_risk(kind, label, destination_hint)
    }
}

pub fn infer_time_cost(kind: &str, label: &str, destination_hint: &str) -> i64 {
    let kind = kind.to_lowercase();
    let text = format!("{label} {destination_hint}").to_lowercase();

    if contains_any(&kind, &["door", "back"]) || contains_any(&text, &["двер", "выход"]) {
        return 1;
    }
    if contains_any(&kind, &["stairs", "ladder", "passage", "tunnel"])
        || contains_any(&text, &["лестн", "люк", "погреб", "коридор", "ход"])
    {
        return 2;
    }
    if contains_any(&text, &["кузниц", "лавк", "дом", "таверн", "зал"]) {
        return 4;
    }
    if contains_any(&text, &["ворот", "gate", "окраин"]) {
        return 12;
    }
    if contains_any(&text, &["крипт", "курган", "руин", "мельниц", "брод"])
    {
        return 30;
    }
    if contains_any(&kind, &["road", "path"])
        || contains_any(&text, &["тракт", "дорог", "тропа", "road"])
    {
        return 25;
    }
    if contains_any(&text, &["город", "деревн", "монастыр", "перевал"]) {
        return 240;
    }
    5
}

pub fn infer_risk(kind: &str, label: &str, destination_hint: &str) -> String {
    let kind = kind.to_lowercase();
    let text = format!("{label} {destination_hint}").to_lowercase();

    if contains_any(&kind, &["door", "back", "stairs", "passage"])
        || contains_any(
            &text,
            &["двер", "лестн", "люк", "погреб", "кузниц", "таверн"],
        )
    {
        return "none: бытовой переход без дорожной ситуации".to_string();
    }
    if contains_any(&text, &["ворот", "площад", "рынок"]) {
        return "settled: людный путь внутри поселения".to_string();
    }
    if contains_any(&text, &["тракт", "road"]) {
        return "guarded_road: оживленная дорога с патрулями, пошлинами и свидетелями".to_string();
    }
    if contains_any(&text, &["крипт", "курган", "руин", "лес", "болот"]) {
        return "wild_road: старая опасная дорога с редкими путниками и следами угроз".to_string();
    }
    if contains_any(&kind, &["road", "path"]) {
        return "settled: обычный местный путь".to_string();
    }
    "none: короткий переход".to_string()
}

pub fn situation_chance_percent(duration_minutes: i64, risk: &str) -> u8 {
    if duration_minutes <= SITUATION_THRESHOLD_MINUTES {
        return 0;
    }
    let risk = risk.to_lowercase();
    if contains_any(&risk, &["certain", "forced", "guaranteed"]) {
        return 100;
    }
    let base = if contains_any(&risk, &["none", "safe", "бытовой"]) {
        0
    } else if contains_any(&risk, &["royal", "guarded", "патрул", "оживлен"]) {
        18
    } else if contains_any(&risk, &["settled", "людн", "обычный"]) {
        24
    } else if contains_any(&risk, &["wild", "forest", "болот", "старая", "опас"]) {
        46
    } else if contains_any(&risk, &["danger", "bandit", "haunt", "разбой", "прокля"]) {
        64
    } else {
        30
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

fn pick_tone(rng: &mut DetRng, risk: &str) -> SituationTone {
    let risk = risk.to_lowercase();
    let weights = if contains_any(
        &risk,
        &["danger", "bandit", "haunt", "разбой", "прокля", "wild"],
    ) {
        [
            (SituationTone::Good, 10),
            (SituationTone::Bad, 45),
            (SituationTone::Neutral, 20),
            (SituationTone::Mixed, 25),
        ]
    } else if contains_any(&risk, &["guarded", "royal", "патрул", "оживлен"]) {
        [
            (SituationTone::Good, 22),
            (SituationTone::Bad, 18),
            (SituationTone::Neutral, 38),
            (SituationTone::Mixed, 22),
        ]
    } else {
        [
            (SituationTone::Good, 18),
            (SituationTone::Bad, 28),
            (SituationTone::Neutral, 32),
            (SituationTone::Mixed, 22),
        ]
    };
    pick_weighted(rng, &weights)
}

fn pick_rarity(rng: &mut DetRng, risk: &str, duration_minutes: i64) -> Rarity {
    let risk = risk.to_lowercase();
    let long_or_strange =
        duration_minutes >= 8 * 60 || contains_any(&risk, &["haunt", "legend", "прокля", "древн"]);
    let weights = if long_or_strange {
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

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}
