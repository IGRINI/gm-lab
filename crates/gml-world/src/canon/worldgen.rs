//! `worldgen` — a deterministic, layered procedural world pipeline (TZ §7.2).
//!
//! Generates a fresh [`WorldCanon`] from a [`WorldSpec`] in ordered layers:
//! region -> settlement (with a real *function*) -> start place -> neighbouring
//! places -> a point-of-interest shell (a dungeon entry) -> a faction -> an
//! initial history. Procedural worlds start with ZERO actors — significant
//! NPCs are generated lazily at play time. Every id and bounded choice derives
//! from
//! [`ids::stable_id`] / [`ids::DetRng`], a stream entirely separate from the
//! campaign dice RNG — so generating a world consumes ZERO dice entropy and two
//! runs with the same seed produce byte-identical canon (TZ §7.3, §12).
//!
//! This satisfies the TZ §14 "new campaign" acceptance criterion: a region, a
//! settlement, a start place, several neighbours, and an initial history.

use std::collections::{BTreeMap, BTreeSet};

use gml_types::ContentLocale;
use serde::{Deserialize, Serialize};

use super::entity::Faction;
use super::event_log::{Account, CanonEvent};
use super::ids;
use super::knowledge::{Scope, Truthfulness};
use super::region::{District, Region, Settlement};
use super::travel::TravelRisk;
use super::{
    PassageDirectionality, Place, Provenance, Transition, WorldCanon, WorldLore, GENERATOR_VERSION,
};

#[derive(Clone, Copy)]
struct LinkProfile {
    kind: &'static str,
    time_cost: i64,
    risk: TravelRisk,
}

const SMITHY_LINK: LinkProfile = LinkProfile {
    kind: "path",
    time_cost: 4,
    risk: TravelRisk::None,
};
const GATE_LINK: LinkProfile = LinkProfile {
    kind: "path",
    time_cost: 12,
    risk: TravelRisk::Low,
};
const ROAD_LINK: LinkProfile = LinkProfile {
    kind: "road",
    time_cost: 25,
    risk: TravelRisk::Low,
};
const CRYPT_LINK: LinkProfile = LinkProfile {
    kind: "path",
    time_cost: 30,
    risk: TravelRisk::Medium,
};

/// The high-level brief a campaign hands to world generation (TZ §7.2 input).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorldSpec {
    /// Generation seed (also recorded as `world_seed`).
    pub seed: String,
    /// Genre flavour (`fantasy`, `grimdark`, `frontier`, ...).
    pub genre: String,
    /// Emotional tone (`hopeful`, `bleak`, `tense`, ...).
    pub tone: String,
    /// Rough scale of the starting area (`village`, `town`, `outpost`).
    pub scale: String,
}

impl Default for WorldSpec {
    fn default() -> Self {
        WorldSpec {
            seed: "0".to_string(),
            genre: "fantasy".to_string(),
            tone: "tense".to_string(),
            scale: "village".to_string(),
        }
    }
}

impl WorldSpec {
    /// A spec from just a seed, with sensible defaults.
    pub fn from_seed(seed: &str) -> Self {
        WorldSpec {
            seed: seed.to_string(),
            ..Default::default()
        }
    }
}

/// Generate a complete starting [`WorldCanon`] from `spec`, deterministically.
///
/// The result satisfies TZ §14's "new campaign" criterion: at least one region,
/// one settlement (with `has_function() == true`), a start place, several
/// neighbouring places linked by two-way transitions, a point-of-interest shell
/// reachable via a transition (the location creator fills it before first entry),
/// one faction, and an initial history. No actors are seeded — the roster
/// starts empty and grows through lazy NPC generation at play time.
pub fn generate(spec: &WorldSpec) -> WorldCanon {
    generate_for_locale(spec, ContentLocale::Russian)
}

/// [`generate`] using the selected static content bundle.
pub fn generate_for_locale(spec: &WorldSpec, locale: ContentLocale) -> WorldCanon {
    generate_with_lore_for_locale(spec, None, locale)
}

/// Generate a complete starting [`WorldCanon`], optionally using a
/// model-authored world bible as the top-level lore layer. The structural
/// region/settlement/place graph remains deterministic from `spec.seed`; the
/// provided lore only constrains what the GM and later generators consider
/// plausible.
pub fn generate_with_lore(spec: &WorldSpec, world_lore: Option<WorldLore>) -> WorldCanon {
    generate_with_lore_for_locale(spec, world_lore, ContentLocale::Russian)
}

/// [`generate_with_lore`] using the selected static content bundle.
pub fn generate_with_lore_for_locale(
    spec: &WorldSpec,
    world_lore: Option<WorldLore>,
    locale: ContentLocale,
) -> WorldCanon {
    let seed = spec.seed.clone();
    let mut canon = WorldCanon {
        world_seed: seed.clone(),
        generator_version: GENERATOR_VERSION.to_string(),
        content_locale: locale,
        ..Default::default()
    };
    if let Some(mut lore) = world_lore.filter(|lore| !lore.is_empty()) {
        lore.normalize_for_worldgen(&seed, &spec.genre, &spec.tone, &spec.scale);
        canon.world_lore = lore;
    }

    let prov = || Provenance::by("worldgen", "procedural generation", 0);

    // --- Layer 0: deterministic RNG keyed on the world identity -----------
    let mut rng = ids::DetRng::from_parts(&[&seed, "worldgen", &spec.genre]);

    // --- Layer 1: Region --------------------------------------------------
    let region_id = ids::stable_id(&seed, "world", "region", "0");
    let region_names = match locale {
        ContentLocale::Russian => [
            "Долина Пепельных Сосен",
            "Серые Болота",
            "Край Тихих Ветров",
            "Каменная Гряда",
        ],
        ContentLocale::English => [
            "Ashen Pine Valley",
            "Grey Marshes",
            "Land of Quiet Winds",
            "Stone Ridge",
        ],
    };
    let climates = match locale {
        ContentLocale::Russian => ["холодный", "туманный", "сухой", "умеренный"],
        ContentLocale::English => ["cold", "misty", "dry", "temperate"],
    };
    let region = Region {
        region_id: region_id.clone(),
        name: rng.pick(&region_names).to_string(),
        theme: format!("{} / {}", spec.genre, spec.tone),
        climate: rng.pick(&climates).to_string(),
        biomes: localized_vec(locale, &["лес", "холмы"], &["forest", "hills"]),
        routes: localized_vec(locale, &["старый тракт"], &["old road"]),
        resources: localized_vec(locale, &["древесина", "руда"], &["timber", "ore"]),
        faction_influence: Vec::new(),
        danger_level: rng.range(1, 3) as u8,
        settlement_ids: Vec::new(),
        site_ids: Vec::new(),
        hinted_sites: localized_vec(locale, &["заброшенная крипта"], &["abandoned crypt"]),
        history_event_ids: Vec::new(),
        is_shell: false,
        provenance: prov(),
    };

    // --- Layer 2: Faction (needed before the settlement references it) ----
    let faction_id = ids::stable_id(&seed, &region_id, "faction", "0");
    let faction_names = match locale {
        ContentLocale::Russian => ["Гильдия Тракта", "Орден Серого Камня", "Вольное Братство"],
        ContentLocale::English => ["Road Guild", "Order of the Grey Stone", "Free Brotherhood"],
    };
    let faction = Faction {
        faction_id: faction_id.clone(),
        name: rng.pick(&faction_names).to_string(),
        territory: vec![region_id.clone()],
        goals: localized_vec(locale, &["контроль над трактом"], &["control of the road"]),
        resources: localized_vec(locale, &["золото", "наёмники"], &["gold", "mercenaries"]),
        relations: BTreeMap::new(),
        attitude_to_player: 0,
        member_ids: Vec::new(),
        plans: localized_vec(
            locale,
            &["перекрыть северную дорогу"],
            &["block the northern road"],
        ),
        pending_event_ids: Vec::new(),
        history_event_ids: Vec::new(),
        provenance: prov(),
    };

    // --- Layer 3: Settlement with a real function (TZ §6.3) ---------------
    let settlement_id = ids::stable_id(&seed, &region_id, "settlement", "0");
    let town_names = match locale {
        ContentLocale::Russian => ["Развилье", "Камнебор", "Тихий Брод", "Сосновый Дол"],
        ContentLocale::English => ["Crossroads", "Stoneford", "Quiet Ford", "Pine Hollow"],
    };
    let powers = match locale {
        ContentLocale::Russian => [
            "староста и совет старейшин",
            "капитан гарнизона",
            "торговая гильдия",
        ],
        ContentLocale::English => [
            "reeve and council of elders",
            "garrison captain",
            "merchants' guild",
        ],
    };
    let conflicts = match locale {
        ContentLocale::Russian => [
            "спор за права на тракт между гильдией и старостой",
            "налёты с болот истощают запасы",
            "две семьи борются за власть в совете",
        ],
        ContentLocale::English => [
            "the guild and the reeve dispute control of the road",
            "raids from the marshes are draining supplies",
            "two families are fighting for control of the council",
        ],
    };
    let district_id = ids::stable_id(&seed, &settlement_id, "district", "center");
    let settlement = Settlement {
        settlement_id: settlement_id.clone(),
        name: rng.pick(&town_names).to_string(),
        region_id: region_id.clone(),
        kind: spec.scale.clone(),
        economy: localized_vec(
            locale,
            &["торговля на тракте", "лесозаготовка"],
            &["road trade", "logging"],
        ),
        routes: localized_vec(
            locale,
            &["северный тракт", "брод через реку"],
            &["northern road", "river ford"],
        ),
        power: rng.pick(&powers).to_string(),
        social_groups: localized_vec(
            locale,
            &["торговцы", "лесорубы"],
            &["merchants", "woodcutters"],
        ),
        conflict: rng.pick(&conflicts).to_string(),
        faction_ids: vec![faction_id.clone()],
        important_npc_ids: Vec::new(),
        local_rumors: localized_vec(
            locale,
            &["в старой крипте на холме кто-то снова зажигает огни"],
            &["someone is lighting fires in the old crypt on the hill again"],
        ),
        threats: localized_vec(locale, &["разбойники на тракте"], &["road bandits"]),
        district_ids: vec![district_id.clone()],
        place_ids: Vec::new(),
        history_event_ids: Vec::new(),
        provenance: prov(),
    };
    let district = District {
        district_id: district_id.clone(),
        name: localized(locale, "Центральный район", "Central District").to_string(),
        settlement_id: settlement_id.clone(),
        region_id: region_id.clone(),
        kind: "center".to_string(),
        place_ids: Vec::new(),
        provenance: prov(),
    };
    debug_assert!(
        settlement.has_function(),
        "generated settlement must have a function"
    );

    // --- Layer 4: Start place + neighbouring places -----------------------
    let start_id = ids::stable_id(&seed, &settlement_id, "place", "square");
    let mut start_flags = BTreeSet::new();
    start_flags.insert("visited".to_string());
    let start = Place {
        place_id: start_id.clone(),
        name: localized(locale, "Рыночная площадь", "Market Square").to_string(),
        kind: "square".to_string(),
        parent: district_id.clone(),
        region_id: region_id.clone(),
        district_id: district_id.clone(),
        default_description: localized(
            locale,
            "Мощёная площадь в центре поселения: колодец, лавки, гул торга.",
            "A cobbled square at the heart of the settlement: a well, stalls, and the din of trade.",
        )
        .to_string(),
        state_flags: start_flags,
        features: localized_vec(locale, &["колодец", "доска объявлений"], &["well", "notice board"]),
        transition_ids: Vec::new(),
        occupant_ids: BTreeSet::new(),
        item_ids: Vec::new(),
        event_ids: Vec::new(),
        fact_ids: Vec::new(),
        provenance: prov(),
    };

    // Neighbour specs carry explicit travel metadata. Player-facing names never
    // participate in route mechanics.
    let neighbours_ru: [(&str, &str, &str, &str, LinkProfile); 3] = [
        (
            "smithy",
            "Кузница",
            "building",
            "Жаркая кузница: звон молота, запах угля и железа.",
            SMITHY_LINK,
        ),
        (
            "gate",
            "Северные ворота",
            "gate",
            "Окованные ворота, ведущие на тракт.",
            GATE_LINK,
        ),
        (
            "road",
            "Северный тракт",
            "road",
            "Разбитая дорога, уходящая в холмы.",
            ROAD_LINK,
        ),
    ];
    let neighbours_en: [(&str, &str, &str, &str, LinkProfile); 3] = [
        (
            "smithy",
            "Smithy",
            "building",
            "A sweltering smithy: ringing hammers and the smell of coal and iron.",
            SMITHY_LINK,
        ),
        (
            "gate",
            "North Gate",
            "gate",
            "Iron-bound gates leading onto the road.",
            GATE_LINK,
        ),
        (
            "road",
            "Northern Road",
            "road",
            "A battered road winding into the hills.",
            ROAD_LINK,
        ),
    ];
    let neighbours = match locale {
        ContentLocale::Russian => neighbours_ru,
        ContentLocale::English => neighbours_en,
    };

    canon.insert_place(start.clone());
    let mut settlement_place_ids = vec![start_id.clone()];

    // Transition budget (TZ §7.3): never wire more than `max_transitions_per_turn`
    // edges in this generation pass. Each two-way link is up to 2 edges.
    let max_transitions = canon.gen_budget.max_transitions_per_turn;
    let mut transitions_made = 0usize;

    for (salt, name, kind, desc, travel_profile) in neighbours {
        if transitions_made + 2 > max_transitions {
            break;
        }
        let pid = ids::stable_id(&seed, &settlement_id, "place", salt);
        canon.insert_place(Place {
            place_id: pid.clone(),
            name: name.to_string(),
            kind: kind.to_string(),
            parent: district_id.clone(),
            region_id: region_id.clone(),
            district_id: district_id.clone(),
            default_description: desc.to_string(),
            state_flags: BTreeSet::new(),
            features: Vec::new(),
            transition_ids: Vec::new(),
            occupant_ids: BTreeSet::new(),
            item_ids: Vec::new(),
            event_ids: Vec::new(),
            fact_ids: Vec::new(),
            provenance: prov(),
        });
        settlement_place_ids.push(pid.clone());
        // Two-way transitions start <-> neighbour.
        transitions_made += link_two_way(
            &mut canon,
            &seed,
            &start_id,
            &pid,
            name,
            localized(locale, "Площадь", "Square"),
            travel_profile,
        );
    }

    // --- Layer 5: a point-of-interest shell -------------------------------
    // The shell carries only established world structure. The dedicated
    // location creator authors its playable content before first entry.
    let road_id = ids::stable_id(&seed, &settlement_id, "place", "road");
    let crypt_id = ids::stable_id(&seed, &region_id, "place", "crypt");
    let mut crypt_flags = BTreeSet::new();
    crypt_flags.insert("shell".to_string());
    canon.insert_place(Place {
        place_id: crypt_id.clone(),
        name: localized(locale, "Заброшенная крипта", "Abandoned Crypt").to_string(),
        kind: "dungeon".to_string(),
        parent: String::new(),
        region_id: region_id.clone(),
        district_id: String::new(),
        default_description: localized(
            locale,
            "Покосившийся вход в старую крипту; из темноты тянет холодом.",
            "The crooked entrance to an old crypt; cold air seeps from the darkness.",
        )
        .to_string(),
        state_flags: crypt_flags,
        features: localized_vec(locale, &["разбитая плита"], &["broken slab"]),
        transition_ids: Vec::new(),
        occupant_ids: BTreeSet::new(),
        item_ids: Vec::new(),
        event_ids: Vec::new(),
        fact_ids: Vec::new(),
        provenance: Provenance::by("worldgen", "point of interest (shell)", 0),
    });
    // Forward edge road -> crypt (a real target that is a shell) + a back edge.
    if transitions_made + 2 <= max_transitions {
        transitions_made += link_two_way(
            &mut canon,
            &seed,
            &road_id,
            &crypt_id,
            localized(locale, "Вход в крипту", "Crypt Entrance"),
            localized(locale, "Тракт", "Road"),
            CRYPT_LINK,
        );
    }
    let _ = transitions_made;

    // --- Layer 6 removed: procedural worlds start with ZERO actors --------
    // Significant NPCs are now generated lazily at play time (the GM's
    // `generate_npc` tool), never hardcoded here. `settlement.important_npc_ids`
    // and `faction.member_ids` therefore stay empty until a generated NPC is
    // wired in.

    // --- Commit region/settlement/faction with cross-links ----------------
    let mut region = region;
    region.settlement_ids.push(settlement_id.clone());
    region.site_ids.push(crypt_id.clone());

    let mut settlement = settlement;
    settlement.place_ids = settlement_place_ids;
    let mut district = district;
    district.place_ids = settlement.place_ids.clone();

    canon.regions.insert(region_id.clone(), region);
    canon.settlements.insert(settlement_id.clone(), settlement);
    canon
        .insert_district(district)
        .expect("worldgen district must satisfy canonical geography");
    canon.factions.insert(faction_id.clone(), faction);

    // --- Layer 7: initial history (TZ §6.9, §14) --------------------------
    seed_initial_history(
        &mut canon,
        &seed,
        &region_id,
        &settlement_id,
        &faction_id,
        &start_id,
        locale,
    );

    // --- Player starts on the square --------------------------------------
    canon.player_place_id = start_id;
    canon
}

/// Add a forward and a back transition between two places (TZ §6.5: a two-way
/// path is two directed edges). Ids are stable and deterministic. Returns the
/// number of new transitions actually created (0..=2) for budget accounting.
fn link_two_way(
    canon: &mut WorldCanon,
    seed: &str,
    a: &str,
    b: &str,
    label_ab: &str,
    label_ba: &str,
    profile: LinkProfile,
) -> usize {
    let mut made = 0usize;
    let (passage_left, passage_right) = if a <= b { (a, b) } else { (b, a) };
    let passage_id = ids::stable_id(seed, passage_left, "passage", passage_right);
    let fwd = ids::stable_id(seed, a, "transition", b);
    if !canon.transitions.contains_key(&fwd) {
        made += 1;
        canon.insert_transition(Transition {
            transition_id: fwd.clone(),
            source_exit_id: fwd.clone(),
            passage_id: passage_id.clone(),
            directionality: PassageDirectionality::Bidirectional,
            from_place: a.to_string(),
            to_place: b.to_string(),
            destination_hint: String::new(),
            label: label_ab.to_string(),
            kind: profile.kind.to_string(),
            visible: true,
            passable: true,
            conditions: Vec::new(),
            blocked_by: String::new(),
            time_cost: profile.time_cost,
            risk: profile.risk.as_str().to_string(),
            provenance: Provenance::by("worldgen", "two-way link", 0),
        });
    }
    let back = ids::stable_id(seed, b, "transition", a);
    if !canon.transitions.contains_key(&back) {
        made += 1;
        canon.insert_transition(Transition {
            transition_id: back.clone(),
            source_exit_id: back.clone(),
            passage_id: passage_id.clone(),
            directionality: PassageDirectionality::Bidirectional,
            from_place: b.to_string(),
            to_place: a.to_string(),
            destination_hint: String::new(),
            label: label_ba.to_string(),
            kind: profile.kind.to_string(),
            visible: true,
            passable: true,
            conditions: Vec::new(),
            blocked_by: String::new(),
            time_cost: profile.time_cost,
            risk: profile.risk.as_str().to_string(),
            provenance: Provenance::by("worldgen", "two-way link", 0),
        });
    }
    made
}

/// Seed a small initial history: a founding event, a faction move, and a
/// player-visible public notice, plus a rumour account (TZ §7.5).
fn seed_initial_history(
    canon: &mut WorldCanon,
    seed: &str,
    region_id: &str,
    settlement_id: &str,
    faction_id: &str,
    start_id: &str,
    locale: ContentLocale,
) {
    let mk = |kind: &str, salt: &str| -> String { ids::stable_id(seed, kind, "event", salt) };

    let founding_id = mk("founding", "0");
    canon.event_log.append(CanonEvent {
        event_id: founding_id.clone(),
        seq: 0,
        kind: "founding".to_string(),
        time_minutes: 0,
        time_label: localized(locale, "давно", "long ago").to_string(),
        place_id: settlement_id.to_string(),
        actors: Vec::new(),
        causes: Vec::new(),
        effects: vec![format!("settlement:{settlement_id} founded")],
        visible_to_player: false,
        scope: Scope::Public,
        possible_traces: Vec::new(),
        scheduled: false,
        due_minutes: 0,
        provenance: Provenance::by("worldgen", "settlement founding", 0),
    });

    let faction_move_id = mk("faction_move", "0");
    canon.event_log.append(CanonEvent {
        event_id: faction_move_id,
        seq: 0,
        kind: "faction_move".to_string(),
        time_minutes: 0,
        time_label: localized(locale, "недавно", "recently").to_string(),
        place_id: region_id.to_string(),
        actors: vec![faction_id.to_string()],
        causes: Vec::new(),
        effects: vec!["faction tightened control of the road".to_string()],
        visible_to_player: false,
        scope: Scope::GmPrivate,
        possible_traces: localized_vec(
            locale,
            &["больше патрулей на тракте"],
            &["more patrols on the road"],
        ),
        scheduled: false,
        due_minutes: 0,
        provenance: Provenance::by("worldgen", "faction backstory", 0),
    });

    let notice_id = mk("public_notice", "0");
    canon.event_log.append(CanonEvent {
        event_id: notice_id.clone(),
        seq: 0,
        kind: "public_notice".to_string(),
        time_minutes: 0,
        time_label: localized(locale, "сегодня", "today").to_string(),
        place_id: start_id.to_string(),
        actors: Vec::new(),
        causes: Vec::new(),
        effects: localized_vec(
            locale,
            &["на доске объявлений висит предупреждение о крипте"],
            &["a warning about the crypt hangs on the notice board"],
        ),
        visible_to_player: true,
        scope: Scope::Public,
        possible_traces: Vec::new(),
        scheduled: false,
        due_minutes: 0,
        provenance: Provenance::by("worldgen", "starting hook", 0),
    });

    canon.event_log.add_account(Account {
        account_id: ids::stable_id(seed, &founding_id, "account", "legend"),
        event_id: founding_id,
        source: "rumor".to_string(),
        text: localized(
            locale,
            "Старики говорят, поселение выросло на костях старого святилища.",
            "The elders say the settlement rose on the bones of an ancient shrine.",
        )
        .to_string(),
        truth: Truthfulness::Partial,
        scope: Scope::Rumor,
    });
}

const fn localized(
    locale: ContentLocale,
    russian: &'static str,
    english: &'static str,
) -> &'static str {
    match locale {
        ContentLocale::Russian => russian,
        ContentLocale::English => english,
    }
}

fn localized_vec(locale: ContentLocale, russian: &[&str], english: &[&str]) -> Vec<String> {
    match locale {
        ContentLocale::Russian => russian,
        ContentLocale::English => english,
    }
    .iter()
    .map(|value| (*value).to_string())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_two_way_links_share_one_explicit_profile() {
        let canon = generate(&WorldSpec::from_seed("explicit-link-profiles"));

        for transition in canon.transitions.values() {
            let reverse = canon
                .transitions
                .values()
                .find(|candidate| {
                    candidate.from_place == transition.to_place
                        && candidate.to_place == transition.from_place
                })
                .expect("worldgen transition has a reciprocal edge");
            assert_eq!(reverse.kind, transition.kind);
            assert_eq!(reverse.time_cost, transition.time_cost);
            assert_eq!(reverse.risk, transition.risk);
            assert_eq!(reverse.passage_id, transition.passage_id);
            assert!(!transition.passage_id.is_empty());
            assert_eq!(
                transition.directionality,
                PassageDirectionality::Bidirectional
            );
            assert_eq!(reverse.directionality, transition.directionality);
            assert!(transition.time_cost > 0);
            assert!(TravelRisk::parse(&transition.risk).is_some());
        }
    }
}
