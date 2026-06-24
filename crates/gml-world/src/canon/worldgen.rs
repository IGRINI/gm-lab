//! `worldgen` — a deterministic, layered procedural world pipeline (TZ §7.2).
//!
//! Generates a fresh [`WorldCanon`] from a [`WorldSpec`] in ordered layers:
//! region -> settlement (with a real *function*) -> start place -> neighbouring
//! places -> a point-of-interest shell (a dungeon entry) -> actors -> a faction
//! -> an initial history. Every id and bounded choice derives from
//! [`ids::stable_id`] / [`ids::DetRng`], a stream entirely separate from the
//! campaign dice RNG — so generating a world consumes ZERO dice entropy and two
//! runs with the same seed produce byte-identical canon (TZ §7.3, §12).
//!
//! This satisfies the TZ §14 "new campaign" acceptance criterion: a region, a
//! settlement, a start place, several neighbours, and an initial history.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::entity::{Actor, Containment, Faction};
use super::event_log::{Account, CanonEvent};
use super::ids;
use super::knowledge::{Scope, Truthfulness};
use super::region::{Region, Settlement};
use super::travel;
use super::{Place, Provenance, Transition, WorldCanon, WorldLore, GENERATOR_VERSION};

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
/// reachable via a transition (a dungeon entry — lazy interior on first entry),
/// a couple of actors at the start, one faction, and an initial history.
pub fn generate(spec: &WorldSpec) -> WorldCanon {
    generate_with_lore(spec, None)
}

/// Generate a complete starting [`WorldCanon`], optionally using a
/// model-authored world bible as the top-level lore layer. The structural
/// region/settlement/place graph remains deterministic from `spec.seed`; the
/// provided lore only constrains what the GM and later generators consider
/// plausible.
pub fn generate_with_lore(spec: &WorldSpec, world_lore: Option<WorldLore>) -> WorldCanon {
    let seed = spec.seed.clone();
    let mut canon = WorldCanon {
        world_seed: seed.clone(),
        generator_version: GENERATOR_VERSION.to_string(),
        ..Default::default()
    };
    canon.world_lore = match world_lore {
        Some(mut lore) if !lore.is_empty() => {
            lore.normalize_for_worldgen(&seed, &spec.genre, &spec.tone, &spec.scale);
            lore
        }
        _ => build_world_lore(spec, &seed),
    };

    let prov = || Provenance::by("worldgen", "procedural generation", 0);

    // --- Layer 0: deterministic RNG keyed on the world identity -----------
    let mut rng = ids::DetRng::from_parts(&[&seed, "worldgen", &spec.genre]);

    // --- Layer 1: Region --------------------------------------------------
    let region_id = ids::stable_id(&seed, "world", "region", "0");
    let region_names = [
        "Долина Пепельных Сосен",
        "Серые Болота",
        "Край Тихих Ветров",
        "Каменная Гряда",
    ];
    let climates = ["холодный", "туманный", "сухой", "умеренный"];
    let region = Region {
        region_id: region_id.clone(),
        name: rng.pick(&region_names).to_string(),
        theme: format!("{} / {}", spec.genre, spec.tone),
        climate: rng.pick(&climates).to_string(),
        biomes: vec!["лес".to_string(), "холмы".to_string()],
        routes: vec!["старый тракт".to_string()],
        resources: vec!["древесина".to_string(), "руда".to_string()],
        faction_influence: Vec::new(),
        danger_level: rng.range(1, 3) as u8,
        settlement_ids: Vec::new(),
        site_ids: Vec::new(),
        hinted_sites: vec!["заброшенная крипта".to_string()],
        history_event_ids: Vec::new(),
        is_shell: false,
        provenance: prov(),
    };

    // --- Layer 2: Faction (needed before the settlement references it) ----
    let faction_id = ids::stable_id(&seed, &region_id, "faction", "0");
    let faction_names = ["Гильдия Тракта", "Орден Серого Камня", "Вольное Братство"];
    let faction = Faction {
        faction_id: faction_id.clone(),
        name: rng.pick(&faction_names).to_string(),
        territory: vec![region_id.clone()],
        goals: vec!["контроль над трактом".to_string()],
        resources: vec!["золото".to_string(), "наёмники".to_string()],
        relations: BTreeMap::new(),
        attitude_to_player: 0,
        member_ids: Vec::new(),
        plans: vec!["перекрыть северную дорогу".to_string()],
        pending_event_ids: Vec::new(),
        history_event_ids: Vec::new(),
        provenance: prov(),
    };

    // --- Layer 3: Settlement with a real function (TZ §6.3) ---------------
    let settlement_id = ids::stable_id(&seed, &region_id, "settlement", "0");
    let town_names = ["Развилье", "Камнебор", "Тихий Брод", "Сосновый Дол"];
    let powers = [
        "староста и совет старейшин",
        "капитан гарнизона",
        "торговая гильдия",
    ];
    let conflicts = [
        "спор за права на тракт между гильдией и старостой",
        "налёты с болот истощают запасы",
        "две семьи борются за власть в совете",
    ];
    let settlement = Settlement {
        settlement_id: settlement_id.clone(),
        name: rng.pick(&town_names).to_string(),
        region_id: region_id.clone(),
        kind: spec.scale.clone(),
        economy: vec![
            "торговля на тракте".to_string(),
            "лесозаготовка".to_string(),
        ],
        routes: vec!["северный тракт".to_string(), "брод через реку".to_string()],
        power: rng.pick(&powers).to_string(),
        social_groups: vec!["торговцы".to_string(), "лесорубы".to_string()],
        conflict: rng.pick(&conflicts).to_string(),
        faction_ids: vec![faction_id.clone()],
        important_npc_ids: Vec::new(),
        local_rumors: vec!["в старой крипте на холме кто-то снова зажигает огни".to_string()],
        threats: vec!["разбойники на тракте".to_string()],
        place_ids: Vec::new(),
        history_event_ids: Vec::new(),
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
        name: "Рыночная площадь".to_string(),
        kind: "square".to_string(),
        parent: settlement_id.clone(),
        region_id: region_id.clone(),
        default_description: "Мощёная площадь в центре поселения: колодец, лавки, гул торга."
            .to_string(),
        state_flags: start_flags,
        features: vec!["колодец".to_string(), "доска объявлений".to_string()],
        transition_ids: Vec::new(),
        occupant_ids: BTreeSet::new(),
        item_ids: Vec::new(),
        event_ids: Vec::new(),
        fact_ids: Vec::new(),
        provenance: prov(),
    };

    // Neighbour specs: (salt, name, kind, description).
    let neighbours: [(&str, &str, &str, &str); 3] = [
        (
            "smithy",
            "Кузница",
            "building",
            "Жаркая кузница: звон молота, запах угля и железа.",
        ),
        (
            "gate",
            "Северные ворота",
            "gate",
            "Окованные ворота, ведущие на тракт.",
        ),
        (
            "road",
            "Северный тракт",
            "road",
            "Разбитая дорога, уходящая в холмы.",
        ),
    ];

    canon.insert_place(start.clone());
    let mut settlement_place_ids = vec![start_id.clone()];

    // Transition budget (TZ §7.3): never wire more than `max_transitions_per_turn`
    // edges in this generation pass. Each two-way link is up to 2 edges.
    let max_transitions = canon.gen_budget.max_transitions_per_turn;
    let mut transitions_made = 0usize;

    for (salt, name, kind, desc) in neighbours {
        if transitions_made + 2 > max_transitions {
            break;
        }
        let pid = ids::stable_id(&seed, &settlement_id, "place", salt);
        canon.insert_place(Place {
            place_id: pid.clone(),
            name: name.to_string(),
            kind: kind.to_string(),
            parent: settlement_id.clone(),
            region_id: region_id.clone(),
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
        transitions_made += link_two_way(&mut canon, &seed, &start_id, &pid, name, "Площадь");
    }

    // --- Layer 5: a point-of-interest reached via a SHELL transition ------
    // From the north road, a transition whose target is a shell dungeon entry:
    // entering it triggers lazy interior generation (engine::expand_place_interior).
    let road_id = ids::stable_id(&seed, &settlement_id, "place", "road");
    let crypt_id = ids::stable_id(&seed, &region_id, "place", "crypt");
    let mut crypt_flags = BTreeSet::new();
    crypt_flags.insert("shell".to_string());
    canon.insert_place(Place {
        place_id: crypt_id.clone(),
        name: "Заброшенная крипта".to_string(),
        kind: "dungeon".to_string(),
        parent: String::new(),
        region_id: region_id.clone(),
        default_description: "Покосившийся вход в старую крипту; из темноты тянет холодом."
            .to_string(),
        state_flags: crypt_flags,
        features: vec!["разбитая плита".to_string()],
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
            "Вход в крипту",
            "Тракт",
        );
    }
    let _ = transitions_made;

    // --- Layer 6: Actors at the start (TZ §6.7) ---------------------------
    // Bounded by the generation budget (TZ §7.3): never create more than
    // `max_npcs_per_turn` actors in this generation pass.
    let actor_specs: [(&str, &str, &str); 2] = [
        ("warden", "Страж ворот", "guard"),
        ("trader", "Торговец на площади", "merchant"),
    ];
    let max_npcs = canon.gen_budget.max_npcs_per_turn;
    let mut important_npc_ids = Vec::new();
    for (salt, label, role) in actor_specs.into_iter().take(max_npcs) {
        let aid = ids::stable_id(&seed, &settlement_id, "actor", salt);
        let home = if salt == "warden" {
            ids::stable_id(&seed, &settlement_id, "place", "gate")
        } else {
            start_id.clone()
        };
        canon.actors.insert(
            aid.clone(),
            Actor {
                actor_id: aid.clone(),
                public_label: label.to_string(),
                location: Containment::Place {
                    place_id: home.clone(),
                },
                home_place_id: home.clone(),
                role: role.to_string(),
                attitude_to_player: 0,
                relations: BTreeMap::new(),
                faction_id: faction_id.clone(),
                goals: vec!["держать пост".to_string()],
                agenda: "следить за порядком".to_string(),
                knowledge_ids: Vec::new(),
                secret_ids: Vec::new(),
                resources: Vec::new(),
                schedule: BTreeMap::new(),
                status: "alive".to_string(),
                provenance: prov(),
            },
        );
        if let Some(p) = canon.places.get_mut(&home) {
            p.occupant_ids.insert(aid.clone());
        }
        important_npc_ids.push(aid);
    }

    // --- Commit region/settlement/faction with cross-links ----------------
    let mut region = region;
    region.settlement_ids.push(settlement_id.clone());
    region.site_ids.push(crypt_id.clone());

    let mut settlement = settlement;
    settlement.place_ids = settlement_place_ids;
    settlement.important_npc_ids = important_npc_ids.clone();

    let mut faction = faction;
    faction.member_ids = important_npc_ids;

    canon.regions.insert(region_id.clone(), region);
    canon.settlements.insert(settlement_id.clone(), settlement);
    canon.factions.insert(faction_id.clone(), faction);

    // --- Layer 7: initial history (TZ §6.9, §14) --------------------------
    seed_initial_history(
        &mut canon,
        &seed,
        &region_id,
        &settlement_id,
        &faction_id,
        &start_id,
    );

    // --- Player starts on the square --------------------------------------
    canon.player_place_id = start_id;
    canon
}

fn build_world_lore(spec: &WorldSpec, seed: &str) -> WorldLore {
    let genre = spec.genre.trim();
    let tone = spec.tone.trim();
    let scale = spec.scale.trim();
    let normalized = format!(
        "{} {} {}",
        genre.to_lowercase(),
        tone.to_lowercase(),
        scale.to_lowercase()
    );
    let mut rng = ids::DetRng::from_parts(&[seed, "world_lore", genre, tone, scale]);

    if has_any(
        &normalized,
        &[
            "post",
            "апок",
            "машин",
            "machine",
            "robot",
            "робот",
            "искусственн",
            "нейросет",
            "синтет",
            "cyber",
            "кибер",
            "wasteland",
            "пустош",
        ],
    ) {
        machine_apocalypse_lore(spec, seed, &mut rng)
    } else if has_any(
        &normalized,
        &[
            "isekai",
            "исека",
            "попадан",
            "fantasy",
            "фэнт",
            "magic",
            "маг",
            "sword",
            "меч",
        ],
    ) {
        fantasy_isekai_lore(spec, seed, &mut rng)
    } else {
        frontier_lore(spec, seed, &mut rng)
    }
}

fn machine_apocalypse_lore(spec: &WorldSpec, seed: &str, rng: &mut ids::DetRng) -> WorldLore {
    let names = [
        "Ржавый Венец",
        "Поля После Сигнала",
        "Зона Тихих Машин",
        "Пепельная Сеть",
    ];
    WorldLore {
        lore_id: ids::stable_id(seed, "world", "lore", "0"),
        name: rng.pick(&names).to_string(),
        genre: spec.genre.clone(),
        tone: spec.tone.clone(),
        scale: spec.scale.clone(),
        public_premise: format!(
            "После краха автономных городов выжившие держатся за {} и старые маршруты. Машины не исчезли: часть служит людям, часть охраняет мёртвые зоны, часть давно испортилась и стала опасной.",
            readable_scale(&spec.scale)
        ),
        hidden_premise:
            "Главный управляющий протокол не погиб: он дробит себя на локальные узлы, проверяет поселения через аварии и слухи, и может принять игрока за полезную переменную."
                .to_string(),
        dogmas: strings(&[
            "работающая машина — это договор, а не вещь",
            "энергия, вода и исправные детали ценнее золота",
            "старые сигналы нельзя включать без причины",
            "человек, умеющий чинить узлы, опаснее солдата",
        ]),
        world_laws: strings(&[
            "электронные системы старого мира могут проснуться от питания, кода или радиосигнала",
            "многие руины опасны не ловушками, а автоматическими протоколами обслуживания",
            "чистая вода и стабильный ток ограничивают размер поселений",
            "дальняя дорога требует припасов, проводника или рабочего транспорта",
        ]),
        inhabitants: strings(&[
            "общины выживших",
            "утилизаторы и ремонтники",
            "караванные семьи",
            "сигнальные культы",
            "полусинтетические изгнанники",
        ]),
        creatures: strings(&[
            "ремонтные дроны",
            "боевые платформы без приказов",
            "железные звери-утилизаторы",
            "рои наблюдателей",
            "мутировавшие падальщики",
        ]),
        power_sources: strings(&[
            "аккумуляторные блоки",
            "солнечные фермы",
            "старые дата-узлы",
            "радиомаяки",
            "фрагменты управляющего ИИ",
        ]),
        technologies: strings(&[
            "самодельное оружие",
            "ремонтируемые дроны",
            "защитные маски",
            "караванные вездеходы",
            "ручные терминалы старого мира",
        ]),
        taboos: strings(&[
            "не будить закрытый узел без свидетелей",
            "не красть воду у каравана",
            "не продавать чужой код доступа",
            "не входить в машинный храм с включённым передатчиком",
        ]),
        conflicts: strings(&[
            "поселения спорят за батареи и воду",
            "караваны платят за безопасные дороги",
            "ремонтники скрывают, какие машины ещё слушаются людей",
            "культы считают старые протоколы волей богов",
        ]),
        hidden_secrets: strings(&[
            "некоторые поломки на дорогах устроены не людьми, а тестовыми сценариями управляющего протокола",
            "часть машин защищает людей, но не объясняет свои критерии отбора",
            "в старой сети есть карта безопасных зон, за которую фракции готовы убивать",
        ]),
        location_rules: strings(&[
            "каждая новая локация должна учитывать доступ к энергии, воде, деталям или сигналу",
            "опасность машин лучше показывать через следы, режимы, шумы, датчики и последствия",
            "социальные сцены должны вращаться вокруг ремонта, обмена, долга, доступа или маршрутов",
            "древние объекты могут быть непонятными, но должны иметь техническую, а не сказочную природу",
        ]),
        prohibited_elements: strings(&[
            "классическая магия без технологического объяснения",
            "эльфийские королевства, академии волшебства и драконы как обычная норма",
            "изобилие топлива, еды и чистой воды без источника",
        ]),
        provenance: Provenance::by("worldgen", "world lore", 0),
        ..Default::default()
    }
}

fn fantasy_isekai_lore(spec: &WorldSpec, seed: &str, rng: &mut ids::DetRng) -> WorldLore {
    let names = [
        "Порог Второго Неба",
        "Королевства Под Осколочной Луной",
        "Земли Призванных",
        "Карта Семи Клятв",
    ];
    WorldLore {
        lore_id: ids::stable_id(seed, "world", "lore", "0"),
        name: rng.pick(&names).to_string(),
        genre: spec.genre.clone(),
        tone: spec.tone.clone(),
        scale: spec.scale.clone(),
        public_premise: format!(
            "Мир держится на клятвах, землях под покровительством духов и редких людях, пришедших из другого мира. Даже маленькое {} связано с древними договорами, местной властью и слухами о чудесах.",
            readable_scale(&spec.scale)
        ),
        hidden_premise:
            "Призванные появляются не случайно: старые печати мира выбирают чужаков как ответ на нарушение древнего договора между коронами, духами и подземными силами."
                .to_string(),
        dogmas: strings(&[
            "имя, клятва и гостеприимство имеют реальную силу",
            "магия требует цены, посредника или права",
            "духи мест помнят старые договоры",
            "чужак из иного мира может нарушить предсказанный порядок",
        ]),
        world_laws: strings(&[
            "магия сильнее там, где есть имя, символ, кровь, долг или признанная власть",
            "местные духи могут благословлять, проклинать или запрещать путь",
            "монстры чаще появляются вокруг нарушенных границ, старых войн и забытых святынь",
            "дальняя дорога зависит от сезона, покровительства и репутации путника",
        ]),
        inhabitants: strings(&[
            "люди королевств и вольных земель",
            "призванные чужаки",
            "ремесленные гильдии",
            "жрецы местных духов",
            "пограничные кланы",
        ]),
        creatures: strings(&[
            "духи дорог и рек",
            "проклятые звери",
            "низшие демоны",
            "разумные фамильяры",
            "древние стражи руин",
        ]),
        power_sources: strings(&[
            "клятвы",
            "магические имена",
            "святыни",
            "артефакты",
            "долги перед духами",
        ]),
        technologies: strings(&[
            "средневековое ремесло",
            "алхимия",
            "рунические замки",
            "магические печати",
            "караванные повозки",
        ]),
        taboos: strings(&[
            "не нарушать клятву под именем рода",
            "не осквернять местный алтарь",
            "не называть истинное имя духа при чужих",
            "не скрывать статус призванного перед храмовым судом",
        ]),
        conflicts: strings(&[
            "короны спорят за право контролировать призванных",
            "гильдии скрывают монополию на руны",
            "местные духи требуют старые долги",
            "деревни боятся чудес не меньше, чем чудовищ",
        ]),
        hidden_secrets: strings(&[
            "часть пророчеств написали не пророки, а прошлые призванные",
            "нарушенный договор мира связан с исчезнувшей королевской линией",
            "некоторые монстры — бывшие хранители границ, лишённые имени",
        ]),
        location_rules: strings(&[
            "каждая новая локация должна учитывать власть, клятвы, местных духов или цену магии",
            "чудеса лучше показывать через обряды, следы, запреты и реакцию жителей",
            "монстры должны быть связаны с местом, долгом, проклятием или нарушенной границей",
            "обычный быт важен: рынки, дороги, ремёсла и слухи должны держать мир земным",
        ]),
        prohibited_elements: strings(&[
            "современные дроны, огнестрел и дата-центры без отдельного объяснения как чужеродный артефакт",
            "магия без цены и последствий",
            "случайные монстры, не связанные с местом или историей",
        ]),
        provenance: Provenance::by("worldgen", "world lore", 0),
        ..Default::default()
    }
}

fn frontier_lore(spec: &WorldSpec, seed: &str, rng: &mut ids::DetRng) -> WorldLore {
    let names = [
        "Край Старых Дорог",
        "Земли Долгих Сумерек",
        "Пограничье Пяти Знаков",
        "Долина Неспокойных Клятв",
    ];
    WorldLore {
        lore_id: ids::stable_id(seed, "world", "lore", "0"),
        name: rng.pick(&names).to_string(),
        genre: spec.genre.clone(),
        tone: spec.tone.clone(),
        scale: spec.scale.clone(),
        public_premise: format!(
            "{} — это окраина большого мира: дороги важнее карт, власть держится на договорах, а каждое {} знает больше слухов, чем готово признать.",
            rng.pick(&names),
            readable_scale(&spec.scale)
        ),
        hidden_premise:
            "Пограничье кажется обычным конфликтом за дороги и власть, но старые договоры местных фракций скрывают причину, по которой опасные места снова просыпаются."
                .to_string(),
        dogmas: strings(&[
            "дорога ценится выше стены",
            "слово при свидетелях почти равно закону",
            "старые места лучше не трогать без проводника",
            "чужак полезен, пока не влезает в чужие долги",
        ]),
        world_laws: strings(&[
            "сила власти быстро слабеет вдали от дорог и рынков",
            "опасные места оставляют следы в слухах, ценах и поведении жителей",
            "дальняя дорога меняет ситуацию в поселениях, пока игрок отсутствует",
            "любая редкая сила должна иметь источник, цену и свидетелей",
        ]),
        inhabitants: strings(&[
            "поселенцы",
            "купцы",
            "проводники",
            "местные стражи",
            "скрытые служители старых сил",
        ]),
        creatures: strings(&[
            "хищники окраин",
            "проклятые путники",
            "старые стражи руин",
            "болотные твари",
            "ночные падальщики",
        ]),
        power_sources: strings(&[
            "долги",
            "земельные права",
            "местные святыни",
            "редкие реликвии",
            "тайные договоры фракций",
        ]),
        technologies: strings(&[
            "ремесленные инструменты",
            "караванные припасы",
            "простые замки и ловушки",
            "местные карты",
            "сигнальные огни",
        ]),
        taboos: strings(&[
            "не нарушать дорожный мир",
            "не лгать на общем сходе",
            "не трогать метки проводников",
            "не продавать убежище преследователям",
        ]),
        conflicts: strings(&[
            "фракции спорят за дороги и сборы",
            "поселения зависят от редких ресурсов",
            "старые руины меняют баланс власти",
            "слухи бегут быстрее официальных приказов",
        ]),
        hidden_secrets: strings(&[
            "одна из местных фракций знает истинную причину пробуждения опасных мест",
            "старые карты намеренно искажены, чтобы скрыть путь к источнику власти",
            "часть публичных слухов запущена как прикрытие реального маршрута контрабанды",
        ]),
        location_rules: strings(&[
            "каждая новая локация должна иметь функцию: власть, ресурс, путь, долг, укрытие или след",
            "опасность должна быть связана с дорогой, фракцией, ресурсом или старым местом",
            "секреты нужно прятать в следах, свидетелях, предметах и противоречивых слухах",
            "не генерировать пустые красивые места без игрового рычага",
        ]),
        prohibited_elements: strings(&[
            "случайные жанровые элементы без связи с текущим миром",
            "абсолютно безопасные дальние дороги без причины",
            "поселения без экономики, власти, конфликта или маршрута",
        ]),
        provenance: Provenance::by("worldgen", "world lore", 0),
        ..Default::default()
    }
}

fn has_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn readable_scale(scale: &str) -> &str {
    match scale.trim() {
        "village" => "деревню",
        "town" => "городок",
        "city" => "город",
        "outpost" => "форпост",
        "region" => "регион",
        "" => "поселение",
        other => other,
    }
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
) -> usize {
    let mut made = 0usize;
    let fwd = ids::stable_id(seed, a, "transition", b);
    let fwd_time_cost = travel::infer_time_cost("path", label_ab, label_ba);
    let fwd_risk = travel::infer_risk("path", label_ab, label_ba);
    let back_time_cost = travel::infer_time_cost("path", label_ba, label_ab);
    let back_risk = travel::infer_risk("path", label_ba, label_ab);
    if !canon.transitions.contains_key(&fwd) {
        made += 1;
        canon.insert_transition(Transition {
            transition_id: fwd.clone(),
            source_exit_id: fwd.clone(),
            from_place: a.to_string(),
            to_place: b.to_string(),
            destination_hint: String::new(),
            label: label_ab.to_string(),
            kind: "path".to_string(),
            visible: true,
            passable: true,
            conditions: Vec::new(),
            blocked_by: String::new(),
            time_cost: fwd_time_cost,
            risk: fwd_risk,
            provenance: Provenance::by("worldgen", "two-way link", 0),
        });
    }
    let back = ids::stable_id(seed, b, "transition", a);
    if !canon.transitions.contains_key(&back) {
        made += 1;
        canon.insert_transition(Transition {
            transition_id: back.clone(),
            source_exit_id: back.clone(),
            from_place: b.to_string(),
            to_place: a.to_string(),
            destination_hint: String::new(),
            label: label_ba.to_string(),
            kind: "path".to_string(),
            visible: true,
            passable: true,
            conditions: Vec::new(),
            blocked_by: String::new(),
            time_cost: back_time_cost,
            risk: back_risk,
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
) {
    let mk = |kind: &str, salt: &str| -> String { ids::stable_id(seed, kind, "event", salt) };

    let founding_id = mk("founding", "0");
    canon.event_log.append(CanonEvent {
        event_id: founding_id.clone(),
        seq: 0,
        kind: "founding".to_string(),
        time_minutes: 0,
        time_label: "давно".to_string(),
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
        time_label: "недавно".to_string(),
        place_id: region_id.to_string(),
        actors: vec![faction_id.to_string()],
        causes: Vec::new(),
        effects: vec!["faction tightened control of the road".to_string()],
        visible_to_player: false,
        scope: Scope::GmPrivate,
        possible_traces: vec!["больше патрулей на тракте".to_string()],
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
        time_label: "сегодня".to_string(),
        place_id: start_id.to_string(),
        actors: Vec::new(),
        causes: Vec::new(),
        effects: vec!["на доске объявлений висит предупреждение о крипте".to_string()],
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
        text: "Старики говорят, поселение выросло на костях старого святилища.".to_string(),
        truth: Truthfulness::Partial,
        scope: Scope::Rumor,
    });
}
