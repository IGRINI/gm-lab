//! The `World` aggregate — faithful port of `class World` in world.py.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::dice;
use crate::helpers::{
    actor_key, anchor_label, as_dict, as_int_or_none, as_joined_str, as_list, as_str, get_str,
    match_words, safe_id,
};
use crate::model::*;
use crate::rng::{MersenneTwister, RngState};
use crate::seed::normalize_seed;
use crate::state_record::{
    self, state_record_hash, state_record_kind, state_record_scope, RagDocument,
};

// =========================================================================
// Static RU label / alias / source maps (load-bearing UI contract data —
// ported verbatim, shared with the frontend via /state). DO NOT translate.
// =========================================================================

fn role_ru(role: &str) -> Option<&'static str> {
    match role {
        "innkeeper" => Some("трактирщик"),
        "serving girl" => Some("служанка"),
        "guard captain" => Some("капитан стражи"),
        "scene character" => Some("персонаж сцены"),
        "person in the starting scene" => Some("персонаж стартовой сцены"),
        "npc" => Some("персонаж"),
        _ => None,
    }
}

fn gender_label_ru(value: &str) -> Option<&'static str> {
    match value {
        "m" => Some("мужской род"),
        "f" => Some("женский род"),
        "n" => Some("средний род"),
        "pl" => Some("множественное число"),
        "other" => Some("другое"),
        _ => None,
    }
}

/// `WHEREABOUTS_STATUS_LABELS` — key -> player-facing RU label, in insertion
/// order (present, known, likely, rumored, unknown, left_scene).
pub const WHEREABOUTS_STATUS_LABELS: [(&str, &str); 6] = [
    ("present", "в текущей сцене"),
    ("known", "известно"),
    ("likely", "вероятно"),
    ("rumored", "по слухам"),
    ("unknown", "неизвестно"),
    ("left_scene", "ушёл"),
];

/// `WHEREABOUTS_STATUSES = tuple(WHEREABOUTS_STATUS_LABELS)`.
pub fn whereabouts_statuses() -> [&'static str; 6] {
    [
        "present", "known", "likely", "rumored", "unknown", "left_scene",
    ]
}

fn whereabouts_status_label(status: &str) -> Option<&'static str> {
    WHEREABOUTS_STATUS_LABELS
        .iter()
        .find(|(k, _)| *k == status)
        .map(|(_, v)| *v)
}

// Source tag constants.
pub const SOURCE_DEFAULT_LORE: &str = "default public lore";
pub const SOURCE_PREVIOUS_SCENE: &str = "previous scene";
pub const SOURCE_CURRENT_SCENE: &str = "current scene";
pub const SOURCE_NPC_ROSTER: &str = "npc_roster";
pub const SOURCE_SEED: &str = "seed";
pub const SOURCE_MOVE_NPC: &str = "move_npc";
pub const SOURCE_GM: &str = "gm";

fn source_ru(source: &str) -> Option<&'static str> {
    match source {
        "default public lore" => Some("публичные сведения"),
        "previous scene" => Some("предыдущая сцена"),
        "current scene" => Some("текущая сцена"),
        "current_scene" => Some("текущая сцена"),
        "npc_roster" => Some("ростер персонажей"),
        "seed" => Some("стартовые данные"),
        "move_npc" => Some("перемещение персонажа"),
        "gm" => Some("гейм-мастер"),
        _ => None,
    }
}

/// `_public_role(role)`.
fn public_role(role: &str) -> String {
    let raw = as_str(&Value::String(role.to_string()));
    role_ru(&raw.to_lowercase()).map(|s| s.to_string()).unwrap_or(raw)
}

/// `_public_gender(value)`.
fn public_gender(value: &str) -> String {
    let raw = value.trim().to_string();
    gender_label_ru(&raw.to_lowercase())
        .map(|s| s.to_string())
        .unwrap_or(raw)
}

/// `_public_source(source)`.
fn public_source(source: &str) -> String {
    let raw = source.trim().to_string();
    source_ru(&raw.to_lowercase())
        .map(|s| s.to_string())
        .unwrap_or(raw)
}

/// `NPC_PROFILE_PRESETS` and `NPC_PROFILE_FIELDS`.
fn npc_profile_preset(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "visible" => Some(&[
            "public_label",
            "role",
            "physical_type",
            "distinctive_features",
            "condition",
            "life_status",
        ]),
        "social" => Some(&[
            "persona",
            "personality",
            "values",
            "habits",
            "pressure_response",
            "boundaries",
            "voice",
        ]),
        "mechanics" => Some(&[
            "abilities",
            "skills",
            "saving_throws",
            "passive_perception",
            "ac",
            "hp",
            "speed",
            "senses",
            "languages",
        ]),
        "status" => Some(&["life_status", "life_status_note", "condition", "hp"]),
        "identity" => Some(&[
            "name",
            "public_label",
            "role",
            "age",
            "physical_type",
            "distinctive_features",
        ]),
        _ => None,
    }
}

/// sorted union of all preset fields.
fn npc_profile_fields() -> Vec<&'static str> {
    let mut set: BTreeSet<&'static str> = BTreeSet::new();
    for preset in ["visible", "social", "mechanics", "status", "identity"] {
        for f in npc_profile_preset(preset).unwrap() {
            set.insert(f);
        }
    }
    set.into_iter().collect()
}

/// `PLAYER_CHARACTER_FIELDS`.
const PLAYER_CHARACTER_FIELDS: [&str; 26] = [
    "name",
    "pronouns",
    "class_role",
    "level",
    "background",
    "age",
    "physical_type",
    "distinctive_features",
    "life_status",
    "life_status_note",
    "condition",
    "personality",
    "values",
    "gm_notes",
    "abilities",
    "skills",
    "saving_throws",
    "passive_perception",
    "ac",
    "hp",
    "speed",
    "senses",
    "languages",
    "inventory",
    "equipment",
    "features",
];

// =========================================================================
// The World aggregate.
// =========================================================================

/// `class World` — every public field/method world.py exposes, ported.
///
/// Construction:
/// - [`World::from_seed`] mirrors `World(seed)` (fresh OS-entropy dice_seed).
/// - [`World::from_seed_with_dice_seed`] pins the dice seed (tests / determinism).
/// - [`World::empty_with_rng`] reproduces the `World.__new__` bypass the dice
///   fixture uses (no seed loaded, RNG pinned).
pub struct World {
    pub dice_seed: u128,
    rng: MersenneTwister,
    pub forced_die_next: Option<i64>,
    pub forced_die_all: Option<i64>,
    pub hidden_events: Vec<String>,
    pub rumors: Vec<Rumor>,
    pub rumor_seq: i64,

    pub story_id: String,
    pub story_title: String,
    pub public: String,
    pub canon: String,
    pub time: WorldTime,
    pub player_character: PlayerCharacter,
    pub extra_proper_nouns: Vec<String>,
    pub npcs: BTreeMap<String, Npc>,
    pub scene: SceneState,
    pub constraints: Vec<String>,
    pub fact_records: Vec<FactRecord>,
    pub state_records: Vec<StateRecord>,
    pub npc_whereabouts: BTreeMap<String, NpcWhereabouts>,

    /// True when the cacheable system prefix must be recomputed — set by
    /// `set_public_intro` (the single prefix-mutating world method). Mirrors the
    /// PROMPT-CACHE PREFIX DISCIPLINE invariant for the orchestrator port.
    pub prefix_dirty: bool,
}

impl World {
    /// `_new_dice_seed()` — `random.SystemRandom().getrandbits(64)`.
    pub fn new_dice_seed() -> u128 {
        let mut buf = [0u8; 8];
        getrandom::getrandom(&mut buf).expect("OS entropy for dice seed");
        u64::from_le_bytes(buf) as u128
    }

    /// `World(seed=seed)` — fresh OS-entropy dice seed.
    pub fn from_seed(seed: &Value) -> Self {
        Self::from_seed_with_dice_seed(seed, Self::new_dice_seed())
    }

    /// Construct with an explicit dice seed (deterministic tests; persistence
    /// restore wires RNG separately via [`World::set_rng_state`]).
    pub fn from_seed_with_dice_seed(seed: &Value, dice_seed: u128) -> Self {
        let mut world = World::skeleton(dice_seed);
        world.load_seed(seed);
        world
    }

    /// Reproduce Python's `World.__new__(World)` bypass with a pinned RNG: no
    /// seed loaded, just the fields the dice path needs. The dice fixture
    /// builds the World this way (`World.__new__` + `random.Random(424242)`).
    pub fn empty_with_rng(rng: MersenneTwister) -> Self {
        World {
            dice_seed: 0,
            rng,
            forced_die_next: None,
            forced_die_all: None,
            hidden_events: Vec::new(),
            rumors: Vec::new(),
            rumor_seq: 0,
            story_id: String::new(),
            story_title: String::new(),
            public: String::new(),
            canon: String::new(),
            time: WorldTime::default(),
            player_character: PlayerCharacter::default(),
            extra_proper_nouns: Vec::new(),
            npcs: BTreeMap::new(),
            scene: SceneState {
                scene_id: String::new(),
                location_id: String::new(),
                title: String::new(),
                description: String::new(),
                present_npcs: BTreeSet::new(),
                presence: BTreeMap::new(),
                items: Vec::new(),
                exits: Vec::new(),
                constraints: Vec::new(),
                tension: String::new(),
                player_seen: Vec::new(),
            },
            constraints: Vec::new(),
            fact_records: Vec::new(),
            state_records: Vec::new(),
            npc_whereabouts: BTreeMap::new(),
            prefix_dirty: false,
        }
    }

    fn skeleton(dice_seed: u128) -> Self {
        let mut w = World::empty_with_rng(MersenneTwister::from_u128_seed(dice_seed));
        w.dice_seed = dice_seed;
        w
    }

    // --- RNG accessors for persistence -----------------------------------

    /// `self._rng.getstate()` — for snapshot persistence.
    pub fn rng_state(&self) -> RngState {
        self.rng.getstate()
    }

    /// `self._rng.setstate(...)` — restore exact RNG state (never reseed).
    pub fn set_rng_state(&mut self, state: &RngState) -> Result<(), String> {
        self.rng.setstate(state)
    }

    /// Direct mutable RNG access (orchestrator routes campaign rolls here).
    pub fn rng_mut(&mut self) -> &mut MersenneTwister {
        &mut self.rng
    }

    // =====================================================================
    // Seeding (_load_seed and friends)
    // =====================================================================

    fn load_seed(&mut self, seed: &Value) {
        let seed = normalize_seed(seed);
        self.story_id = {
            let v = get_str(&seed, "id");
            if v.is_empty() {
                "custom".to_string()
            } else {
                v
            }
        };
        self.story_title = {
            let v = get_str(&seed, "title");
            if v.is_empty() {
                "Пользовательская история".to_string()
            } else {
                v
            }
        };
        self.public = {
            let intro = get_str(&seed, "public_intro");
            if !intro.is_empty() {
                intro
            } else {
                let p = get_str(&seed, "public");
                if !p.is_empty() {
                    p
                } else {
                    "Новая сцена готова. Игрок видит место, людей рядом и ближайший источник конфликта.".to_string()
                }
            }
        };
        self.canon = {
            let ht = get_str(&seed, "hidden_truth");
            if !ht.is_empty() {
                ht
            } else {
                get_str(&seed, "canon")
            }
        };
        self.time = seed_time(seed.get("time"));
        let pc_raw = if seed.contains_key("player_character") {
            seed.get("player_character")
        } else {
            seed.get("player")
        };
        self.player_character = seed_player_character(pc_raw);
        self.extra_proper_nouns = as_list(seed.get("proper_nouns").unwrap_or(&Value::Null))
            .iter()
            .map(as_str)
            .filter(|s| !s.is_empty())
            .collect();
        self.npcs = self.seed_npcs(&seed);
        self.scene = self.seed_scene(&seed);
        self.constraints = self.scene.constraints.clone();
        self.fact_records = self.seed_facts(&seed);
        self.state_records = self.seed_state_records(&seed);
        self.npc_whereabouts = BTreeMap::new();
        self.ensure_npc_whereabouts();
    }

    fn seed_npcs(&mut self, seed: &Map<String, Value>) -> BTreeMap<String, Npc> {
        let mut out: BTreeMap<String, Npc> = BTreeMap::new();
        // Preserve insertion order for dedup suffixing AND extra_proper_nouns.
        let mut order: Vec<String> = Vec::new();
        for (idx, raw) in as_list(seed.get("npcs").unwrap_or(&Value::Null))
            .iter()
            .enumerate()
        {
            let raw = match raw {
                Value::Object(m) => m,
                _ => continue,
            };
            let i = idx + 1;
            let name = {
                let n = get_str(raw, "name");
                if n.is_empty() {
                    format!("NPC {i}")
                } else {
                    n
                }
            };
            let mut npc_id = safe_id(&get_str(raw, "id"), &format!("npc_{i}"));
            let base_id = npc_id.clone();
            let mut suffix = 2;
            while out.contains_key(&npc_id) {
                npc_id = format!("{base_id}_{suffix}");
                suffix += 1;
            }
            let pronouns = {
                let p = get_str(raw, "pronouns");
                if p.is_empty() {
                    get_str(raw, "gender")
                } else {
                    p
                }
            };
            let persona = {
                let p = get_str(raw, "persona");
                if p.is_empty() {
                    get_str(raw, "description")
                } else {
                    p
                }
            };
            let npc = Npc {
                npc_id: npc_id.clone(),
                name: name.clone(),
                role: nonempty_or(get_str(raw, "role"), "персонаж сцены"),
                pronouns,
                color: get_str(raw, "color"),
                public_label: get_str(raw, "public_label"),
                age: get_str(raw, "age"),
                physical_type: get_str(raw, "physical_type"),
                distinctive_features: get_str(raw, "distinctive_features"),
                life_status: nonempty_or(get_str(raw, "life_status"), "alive"),
                life_status_note: get_str(raw, "life_status_note"),
                condition: get_str(raw, "condition"),
                persona,
                personality: get_str(raw, "personality"),
                values: get_str(raw, "values"),
                habits: get_str(raw, "habits"),
                pressure_response: get_str(raw, "pressure_response"),
                boundaries: get_str(raw, "boundaries"),
                voice: nonempty_or(get_str(raw, "voice"), "Естественно, кратко, в образе."),
                goals: nonempty_or(
                    get_str(raw, "goals"),
                    "Реагировать правдоподобно и защищать свои интересы.",
                ),
                knowledge: nonempty_or(
                    get_str(raw, "knowledge"),
                    "Только то, что очевидно в текущей сцене.",
                ),
                secret: nonempty_or(get_str(raw, "secret"), "Личная тайна не задана."),
                abilities: as_dict(raw.get("abilities").unwrap_or(&Value::Null)),
                skills: as_dict(raw.get("skills").unwrap_or(&Value::Null)),
                saving_throws: as_dict(raw.get("saving_throws").unwrap_or(&Value::Null)),
                passive_perception: as_int_or_none(
                    raw.get("passive_perception").unwrap_or(&Value::Null),
                ),
                ac: raw.get("ac").cloned().unwrap_or(Value::Null),
                hp: as_dict(raw.get("hp").unwrap_or(&Value::Null)),
                speed: as_joined_str(raw.get("speed").unwrap_or(&Value::Null)),
                senses: as_joined_str(raw.get("senses").unwrap_or(&Value::Null)),
                languages: as_joined_str(raw.get("languages").unwrap_or(&Value::Null)),
                default_whereabouts: match raw.get("default_whereabouts") {
                    Some(Value::Object(m)) => Some(m.clone()),
                    _ => None,
                },
                card_revision: 0,
            };
            out.insert(npc_id.clone(), npc);
            order.push(npc_id);
            if !name.is_empty() && !self.extra_proper_nouns.contains(&name) {
                self.extra_proper_nouns.push(name);
            }
        }
        if !out.is_empty() {
            return out;
        }
        // default stranger
        let mut def = BTreeMap::new();
        def.insert(
            "stranger".to_string(),
            Npc {
                npc_id: "stranger".to_string(),
                name: "Незнакомец".to_string(),
                role: "персонаж стартовой сцены".to_string(),
                persona: "Осторожный человек, присутствующий в новой сцене.".to_string(),
                voice: "Кратко, настороженно, естественно.".to_string(),
                goals: "Оставаться в безопасности и правдоподобно реагировать на игрока."
                    .to_string(),
                knowledge: "Только то, что очевидно в стартовой сцене.".to_string(),
                secret: "Личная тайна не задана.".to_string(),
                pronouns: String::new(),
                color: String::new(),
                public_label: String::new(),
                age: String::new(),
                physical_type: String::new(),
                distinctive_features: String::new(),
                life_status: "alive".to_string(),
                life_status_note: String::new(),
                condition: String::new(),
                personality: String::new(),
                values: String::new(),
                habits: String::new(),
                pressure_response: String::new(),
                boundaries: String::new(),
                abilities: Map::new(),
                skills: Map::new(),
                saving_throws: Map::new(),
                passive_perception: None,
                ac: Value::Null,
                hp: Map::new(),
                speed: String::new(),
                senses: String::new(),
                languages: String::new(),
                default_whereabouts: None,
                card_revision: 0,
            },
        );
        def
    }

    fn seed_scene(&self, seed: &Map<String, Value>) -> SceneState {
        let raw = match seed.get("scene") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        let present_raw: BTreeSet<String> =
            as_list(raw.get("present_npcs").unwrap_or(&Value::Null))
                .iter()
                .map(|item| safe_id(&as_str(item), ""))
                .collect();
        let mut present: BTreeSet<String> = present_raw
            .into_iter()
            .filter(|id| self.npcs.contains_key(id))
            .collect();
        if present.is_empty() {
            // set(list(self.npcs)[:2]) — first two npc ids (insertion order).
            for id in self.npcs.keys().take(2) {
                present.insert(id.clone());
            }
        }
        let title = {
            let t = get_str(&raw, "title");
            if !t.is_empty() {
                t
            } else {
                let l = get_str(&raw, "location");
                if !l.is_empty() {
                    l
                } else {
                    "Стартовая сцена".to_string()
                }
            }
        };
        let location_id = safe_id(&get_str(&raw, "location_id"), "start_location");
        let description = nonempty_or(get_str(&raw, "description"), &self.public);
        let mut constraints: Vec<String> =
            as_list(raw.get("constraints").unwrap_or(&Value::Null))
                .iter()
                .map(as_str)
                .filter(|s| !s.is_empty())
                .collect();
        if constraints.is_empty() {
            constraints = vec![
                "Здесь существуют только описанные выходы, видимые предметы и присутствующие люди."
                    .to_string(),
            ];
        }

        let mut presence: BTreeMap<String, Presence> = BTreeMap::new();
        let presence_raw = match raw.get("npc_presence") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        for npc_id in &present {
            let npc = &self.npcs[npc_id];
            let np = match presence_raw.get(npc_id) {
                Some(Value::Object(m)) => m.clone(),
                _ => Map::new(),
            };
            presence.insert(
                npc_id.clone(),
                Presence {
                    npc_id: npc_id.clone(),
                    location: {
                        let loc = get_str(&np, "location");
                        if !loc.is_empty() {
                            loc
                        } else {
                            let dl = get_str(&raw, "default_npc_location");
                            if !dl.is_empty() {
                                dl
                            } else {
                                "в сцене".to_string()
                            }
                        }
                    },
                    visible: true,
                    can_hear: true,
                    activity: {
                        let a = get_str(&np, "activity");
                        if !a.is_empty() {
                            a
                        } else {
                            let na = get_str(&raw, "npc_activity");
                            if !na.is_empty() {
                                na
                            } else {
                                format!("present as {}", npc.role)
                            }
                        }
                    },
                    attitude: {
                        let at = get_str(&np, "attitude");
                        if !at.is_empty() {
                            at
                        } else {
                            get_str(&raw, "npc_attitude")
                        }
                    },
                },
            );
        }

        let items = coerce_scene_items(raw.get("items"), "in the scene");
        let exits = coerce_scene_exits(raw.get("exits"), "unknown destination");

        SceneState {
            scene_id: safe_id(&get_str(&raw, "id"), "start_scene"),
            location_id,
            title,
            description: description.clone(),
            present_npcs: present,
            presence,
            items,
            exits,
            constraints,
            tension: get_str(&raw, "tension"),
            player_seen: vec![description],
        }
    }

    fn seed_facts(&self, seed: &Map<String, Value>) -> Vec<FactRecord> {
        let mut records: Vec<FactRecord> = Vec::new();
        for (idx, raw) in as_list(seed.get("public_facts").unwrap_or(&Value::Null))
            .iter()
            .enumerate()
        {
            let i = idx + 1;
            let (text, fact_id, kind, keywords, source, confirmed) = match raw {
                Value::Object(m) => {
                    let text = get_str(m, "text");
                    let fid = safe_id(&get_str(m, "id"), &format!("public_{i}"));
                    let mut kind = get_str(m, "kind").to_lowercase();
                    if kind.is_empty() {
                        kind = "public".to_string();
                    }
                    if !matches!(kind.as_str(), "public" | "truth" | "rumor") {
                        kind = "public".to_string();
                    }
                    let kw: Vec<String> = as_list(m.get("keywords").unwrap_or(&Value::Null))
                        .iter()
                        .map(as_str)
                        .filter(|s| !s.is_empty())
                        .collect();
                    let source = get_str(m, "source");
                    let confirmed = m
                        .get("confirmed")
                        .map(|v| as_bool_pyish(v))
                        .unwrap_or(true);
                    (text, fid, kind, kw, source, confirmed)
                }
                _ => {
                    let text = as_str(raw);
                    (
                        text,
                        format!("public_{i}"),
                        "public".to_string(),
                        Vec::new(),
                        String::new(),
                        true,
                    )
                }
            };
            if !text.is_empty() {
                records.push(FactRecord {
                    fact_id,
                    kind,
                    text,
                    keywords,
                    source,
                    confirmed,
                });
            }
        }
        if !self.canon.is_empty() {
            records.push(FactRecord {
                fact_id: "hidden_truth".to_string(),
                kind: "truth".to_string(),
                text: self.canon.clone(),
                keywords: vec![
                    "hidden truth".to_string(),
                    "truth".to_string(),
                    "secret".to_string(),
                ],
                source: "seed".to_string(),
                confirmed: true,
            });
        }
        records
    }

    fn seed_state_records(&self, seed: &Map<String, Value>) -> Vec<StateRecord> {
        let mut records: Vec<StateRecord> = Vec::new();
        let mut existing: BTreeSet<String> = BTreeSet::new();
        for (idx, raw) in as_list(seed.get("state_records").unwrap_or(&Value::Null))
            .iter()
            .enumerate()
        {
            let i = idx + 1;
            if let Some(rec) =
                coerce_state_record(raw, &format!("seed_state_{i}"), &existing)
            {
                existing.insert(rec.record_id.clone());
                records.push(rec);
            }
        }
        records
    }

    // =====================================================================
    // State-record CRUD / queries
    // =====================================================================

    pub fn add_state_records(&mut self, records: &Value) -> Vec<StateRecord> {
        let mut existing: BTreeSet<String> =
            self.state_records.iter().map(|r| r.record_id.clone()).collect();
        let mut added: Vec<StateRecord> = Vec::new();
        for (idx, raw) in as_list(records).iter().enumerate() {
            let fallback = format!("state_{}", existing.len() + idx + 1);
            if let Some(rec) = coerce_state_record(raw, &fallback, &existing) {
                existing.insert(rec.record_id.clone());
                self.state_records.push(rec.clone());
                added.push(rec);
            }
        }
        added
    }

    pub fn update_state_records(&mut self, updates: &Value) -> Vec<StateRecord> {
        let mut updated: Vec<StateRecord> = Vec::new();
        for raw in as_list(updates) {
            let m = match raw {
                Value::Object(ref m) => m.clone(),
                _ => continue,
            };
            let record_id = {
                let r = get_str(&m, "record_id");
                if !r.is_empty() {
                    r
                } else {
                    get_str(&m, "id")
                }
            };
            let pos = match self.state_records.iter().position(|r| r.record_id == record_id) {
                Some(p) => p,
                None => continue,
            };
            let rec = &mut self.state_records[pos];
            if m.contains_key("kind") {
                rec.kind = state_record_kind(&get_str(&m, "kind"));
            }
            if m.contains_key("text") {
                let text = get_str(&m, "text");
                if !text.is_empty() {
                    rec.text = text;
                }
            }
            if m.contains_key("scope") {
                rec.scope = state_record_scope(&get_str(&m, "scope"));
            }
            if m.contains_key("active") {
                rec.active =
                    state_record::state_record_active(m.get("active").unwrap(), rec.active);
            }
            if m.contains_key("owner") || m.contains_key("owner_id") {
                rec.owner = first_nonempty(&m, &["owner", "owner_id"]);
            }
            if m.contains_key("subject") || m.contains_key("subject_id") {
                rec.subject = first_nonempty(&m, &["subject", "subject_id"]);
            }
            if m.contains_key("source") {
                rec.source = get_str(&m, "source");
            }
            if m.contains_key("status") {
                rec.status = nonempty_or(get_str(&m, "status"), "known");
            }
            if m.contains_key("tags") {
                rec.tags = state_record::state_record_tags(m.get("tags").unwrap());
            }
            if m.contains_key("entity_id") || m.contains_key("entity") || m.contains_key("about") {
                rec.entity_id = first_nonempty(&m, &["entity_id", "entity", "about"]);
            }
            if m.contains_key("source_npc") || m.contains_key("source_npc_id") {
                rec.source_npc = first_nonempty(&m, &["source_npc", "source_npc_id"]);
            }
            if m.contains_key("participants") {
                rec.participants =
                    state_record::state_record_participants(m.get("participants").unwrap());
            }
            if m.contains_key("location_id") {
                rec.location_id = get_str(&m, "location_id");
            }
            if m.contains_key("location_name") {
                rec.location_name = get_str(&m, "location_name");
            }
            if m.contains_key("region_id") {
                rec.region_id = get_str(&m, "region_id");
            }
            if m.contains_key("region_name") {
                rec.region_name = get_str(&m, "region_name");
            }
            if m.contains_key("scene_id") {
                rec.scene_id = get_str(&m, "scene_id");
            }
            if m.contains_key("importance") {
                rec.importance = get_str(&m, "importance");
            }
            if m.contains_key("aliases") {
                rec.aliases = state_record::state_record_aliases(m.get("aliases").unwrap());
            }
            if m.contains_key("metadata") {
                rec.metadata = state_record::state_record_metadata(m.get("metadata").unwrap());
            }
            updated.push(rec.clone());
        }
        updated
    }

    pub fn delete_state_records(&mut self, record_ids: &Value, hard: bool) -> i64 {
        let ids: BTreeSet<String> = as_list(record_ids)
            .iter()
            .map(as_str)
            .filter(|s| !s.is_empty())
            .collect();
        if ids.is_empty() {
            return 0;
        }
        if hard {
            let before = self.state_records.len();
            self.state_records.retain(|r| !ids.contains(&r.record_id));
            return (before - self.state_records.len()) as i64;
        }
        let mut count = 0;
        for rec in self.state_records.iter_mut() {
            if ids.contains(&rec.record_id) && rec.active {
                rec.active = false;
                count += 1;
            }
        }
        count
    }

    pub fn apply_state_record_batch(
        &mut self,
        add: &Value,
        update: &Value,
        delete: &Value,
        hard_delete: bool,
    ) -> Value {
        let added = self.add_state_records(add);
        let updated = self.update_state_records(update);
        let deleted = self.delete_state_records(delete, hard_delete);
        json!({
            "added": added.iter().map(state_record_to_value).collect::<Vec<_>>(),
            "updated": updated.iter().map(state_record_to_value).collect::<Vec<_>>(),
            "deleted": deleted,
        })
    }

    /// `state_records_for(actor_id, ...)`.
    #[allow(clippy::too_many_arguments)]
    pub fn state_records_for(&self, query: &StateRecordQuery) -> Vec<&StateRecord> {
        let kind_filter: Option<BTreeSet<String>> = query
            .kinds
            .as_ref()
            .map(|ks| ks.iter().map(|k| state_record_kind(k)).collect());
        let scope_filter: Option<BTreeSet<String>> = query
            .scopes
            .as_ref()
            .map(|ss| ss.iter().map(|s| state_record_scope(s)).collect());
        let owner_filter = actor_key(query.owner);
        let subject_filter = actor_key(query.subject);
        let entity_filter = actor_key(query.entity_id);
        let source_npc_filter = actor_key(query.source_npc);
        let location_filter = actor_key(query.location_id);
        let region_filter = actor_key(query.region_id);
        let scene_filter = actor_key(query.scene_id);

        let mut out: Vec<&StateRecord> = Vec::new();
        for record in &self.state_records {
            if let Some(active) = query.active {
                if record.active != active {
                    continue;
                }
            }
            if let Some(ref kf) = kind_filter {
                if !kf.contains(&state_record_kind(&record.kind)) {
                    continue;
                }
            }
            if let Some(ref sf) = scope_filter {
                if !sf.contains(&state_record_scope(&record.scope)) {
                    continue;
                }
            }
            if !owner_filter.is_empty() && actor_key(&record.owner) != owner_filter {
                continue;
            }
            if !subject_filter.is_empty() && actor_key(&record.subject) != subject_filter {
                continue;
            }
            if !entity_filter.is_empty() && actor_key(&record.entity_id) != entity_filter {
                continue;
            }
            if !source_npc_filter.is_empty() && actor_key(&record.source_npc) != source_npc_filter {
                continue;
            }
            if !location_filter.is_empty() && actor_key(&record.location_id) != location_filter {
                continue;
            }
            if !region_filter.is_empty() && actor_key(&record.region_id) != region_filter {
                continue;
            }
            if !scene_filter.is_empty() && actor_key(&record.scene_id) != scene_filter {
                continue;
            }
            if !state_record::state_record_visible_to(record, query.actor_id) {
                continue;
            }
            out.push(record);
        }
        out
    }

    pub fn state_record_documents(&self, actor_id: &str) -> Vec<RagDocument> {
        let mut docs = Vec::new();
        let query = StateRecordQuery::new(actor_id);
        for record in self.state_records_for(&query) {
            let mut tags: Vec<String> = Vec::new();
            let candidate_tags: Vec<String> = {
                let mut v: Vec<String> = Vec::new();
                v.push(state_record_kind(&record.kind));
                v.push(record.owner.clone());
                v.push(record.subject.clone());
                v.push(record.entity_id.clone());
                v.push(record.source_npc.clone());
                v.extend(record.participants.iter().cloned());
                v.push(record.location_id.clone());
                v.push(record.location_name.clone());
                v.push(record.region_id.clone());
                v.push(record.region_name.clone());
                v.push(record.scene_id.clone());
                v.push(record.importance.clone());
                v.push(state_record_scope(&record.scope));
                v.extend(record.tags.iter().cloned());
                v.extend(record.aliases.iter().cloned());
                v
            };
            for t in candidate_tags {
                if !t.is_empty() {
                    tags.push(t);
                }
            }

            let mut context_bits: Vec<String> = Vec::new();
            if !record.region_name.is_empty() || !record.region_id.is_empty() {
                context_bits.push(format!(
                    "region: {}",
                    anchor_label(&record.region_name, &record.region_id)
                ));
            }
            if !record.location_name.is_empty() || !record.location_id.is_empty() {
                context_bits.push(format!(
                    "location: {}",
                    anchor_label(&record.location_name, &record.location_id)
                ));
            }
            if !record.scene_id.is_empty() {
                context_bits.push(format!("scene: {}", record.scene_id));
            }
            if !record.aliases.is_empty() {
                context_bits.push(format!("aliases: {}", record.aliases.join(", ")));
            }
            if !record.importance.is_empty() {
                context_bits.push(format!("importance: {}", record.importance));
            }
            let doc_text = if context_bits.is_empty() {
                record.text.clone()
            } else {
                format!("Memory context: {}. {}", context_bits.join("; "), record.text)
            };

            let mut metadata = record.metadata.clone();
            metadata.insert("record_id".to_string(), json!(record.record_id));
            metadata.insert("record_kind".to_string(), json!(state_record_kind(&record.kind)));
            metadata.insert("scope".to_string(), json!(state_record_scope(&record.scope)));
            metadata.insert("owner".to_string(), json!(record.owner));
            metadata.insert("subject".to_string(), json!(record.subject));
            metadata.insert("entity_id".to_string(), json!(record.entity_id));
            metadata.insert("source_npc".to_string(), json!(record.source_npc));
            metadata.insert("participants".to_string(), json!(record.participants));
            metadata.insert("location_id".to_string(), json!(record.location_id));
            metadata.insert("location_name".to_string(), json!(record.location_name));
            metadata.insert("region_id".to_string(), json!(record.region_id));
            metadata.insert("region_name".to_string(), json!(record.region_name));
            metadata.insert("scene_id".to_string(), json!(record.scene_id));
            metadata.insert("importance".to_string(), json!(record.importance));
            metadata.insert("aliases".to_string(), json!(record.aliases));
            metadata.insert("active".to_string(), json!(record.active));

            docs.push(RagDocument::new(
                format!("state:{}", record.record_id),
                format!("state_{}", state_record_kind(&record.kind)),
                doc_text,
                record.status.clone(),
                nonempty_or(record.source.clone(), &record.record_id),
                state_record_scope(&record.scope),
                tags,
                metadata,
            ));
        }
        docs
    }

    pub fn state_records_export(&self, query: &StateRecordQuery) -> Vec<Value> {
        let mut out = Vec::new();
        for record in self.state_records_for(query) {
            let mut row = state_record_to_value(record);
            if let Value::Object(ref mut m) = row {
                m.insert("hash".to_string(), json!(state_record_hash(record)));
            }
            out.push(row);
        }
        out
    }

    /// `npc_known_name(npc_id, actor_id)`.
    pub fn npc_known_name(&self, npc_id: &str, actor_id: &str) -> String {
        let clean_id = actor_key(npc_id);
        if clean_id.is_empty() {
            return String::new();
        }
        let mut query = StateRecordQuery::new(actor_id);
        let entity_owned = clean_id.clone();
        query.entity_id = &entity_owned;
        let records = self.state_records_for(&query);
        for record in records.iter().rev() {
            let known_name = get_str(&record.metadata, "known_name");
            if !known_name.is_empty() {
                return known_name;
            }
        }
        String::new()
    }

    /// `npc_player_label(npc_id, actor_id)`.
    pub fn npc_player_label(&self, npc_id: &str, actor_id: &str) -> String {
        let npc = self.npcs.get(&actor_key(npc_id));
        match npc {
            None => npc_id.trim().to_string(),
            Some(npc) => {
                let known = self.npc_known_name(&npc.npc_id, actor_id);
                if !known.is_empty() {
                    known
                } else if !npc.public_label.is_empty() {
                    npc.public_label.clone()
                } else {
                    npc.name.clone()
                }
            }
        }
    }

    // =====================================================================
    // Whereabouts
    // =====================================================================

    pub fn ensure_npc_whereabouts(&mut self) {
        let raw = std::mem::take(&mut self.npc_whereabouts);
        let mut cleaned: BTreeMap<String, NpcWhereabouts> = BTreeMap::new();
        for npc_id in self.npcs.keys().cloned().collect::<Vec<_>>() {
            let existing = raw.get(&npc_id);
            cleaned.insert(npc_id.clone(), self.coerce_whereabouts(&npc_id, existing));
        }
        self.npc_whereabouts = cleaned;
        self.apply_default_story_whereabouts();
        self.sync_present_npc_whereabouts();
    }

    fn coerce_whereabouts(&self, npc_id: &str, raw: Option<&NpcWhereabouts>) -> NpcWhereabouts {
        match raw {
            Some(w) => NpcWhereabouts {
                npc_id: npc_id.to_string(),
                location_id: safe_id(&w.location_id, ""),
                location_name: w.location_name.trim().to_string(),
                status: self.whereabouts_status(&w.status, "unknown"),
                details: w.details.trim().to_string(),
                source: w.source.trim().to_string(),
            },
            None => NpcWhereabouts::new(npc_id),
        }
    }

    /// Coerce from a loosely-typed JSON whereabouts payload (used by callers
    /// that hold raw dicts, e.g. persistence/orchestrator).
    pub fn coerce_whereabouts_value(&self, npc_id: &str, raw: &Value) -> NpcWhereabouts {
        match raw {
            Value::Object(m) => {
                let location_name = {
                    let ln = get_str(m, "location_name");
                    if !ln.is_empty() {
                        ln
                    } else {
                        get_str(m, "location")
                    }
                };
                NpcWhereabouts {
                    npc_id: npc_id.to_string(),
                    location_id: safe_id(&get_str(m, "location_id"), ""),
                    location_name,
                    status: self.whereabouts_status(&get_str(m, "status"), "unknown"),
                    details: {
                        let d = get_str(m, "details");
                        if !d.is_empty() {
                            d
                        } else {
                            get_str(m, "activity")
                        }
                    },
                    source: get_str(m, "source"),
                }
            }
            _ => NpcWhereabouts::new(npc_id),
        }
    }

    fn whereabouts_status(&self, raw: &str, fallback: &str) -> String {
        let status = safe_id(raw, fallback);
        if whereabouts_statuses().contains(&status.as_str()) {
            status
        } else {
            fallback.to_string()
        }
    }

    fn apply_default_story_whereabouts(&mut self) {
        let npc_ids: Vec<String> = self.npcs.keys().cloned().collect();
        for npc_id in npc_ids {
            let default = match &self.npcs[&npc_id].default_whereabouts {
                Some(m) if !m.is_empty() => m.clone(),
                _ => continue,
            };
            if let Some(row) = self.npc_whereabouts.get(&npc_id) {
                if row.status != "unknown" || !row.source.is_empty() {
                    continue;
                }
            }
            let status = self.whereabouts_status(&get_str(&default, "status"), "likely");
            self.npc_whereabouts.insert(
                npc_id.clone(),
                NpcWhereabouts {
                    npc_id: npc_id.clone(),
                    location_id: safe_id(&get_str(&default, "location_id"), ""),
                    location_name: get_str(&default, "location_name"),
                    status,
                    details: get_str(&default, "details"),
                    source: nonempty_or(get_str(&default, "source"), SOURCE_DEFAULT_LORE),
                },
            );
        }
    }

    fn sync_present_npc_whereabouts(&mut self) {
        let present: Vec<String> = self.scene.present_npcs.iter().cloned().collect();
        for npc_id in present {
            if !self.npcs.contains_key(&npc_id) {
                continue;
            }
            let details = match self.scene.presence.get(&npc_id) {
                Some(p) => {
                    if !p.activity.is_empty() {
                        p.activity.clone()
                    } else {
                        p.location.clone()
                    }
                }
                None => String::new(),
            };
            self.npc_whereabouts.insert(
                npc_id.clone(),
                NpcWhereabouts {
                    npc_id: npc_id.clone(),
                    location_id: self.scene.location_id.clone(),
                    location_name: self.scene.title.clone(),
                    status: "present".to_string(),
                    details,
                    source: "current scene".to_string(),
                },
            );
        }
    }

    pub fn npc_whereabouts_export(&mut self, npc_id: Option<&str>) -> Value {
        self.ensure_npc_whereabouts();
        match npc_id {
            Some(id) => {
                let row = self
                    .npc_whereabouts
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| NpcWhereabouts::new(id));
                whereabouts_to_value(&row)
            }
            None => {
                let mut m = Map::new();
                for (k, row) in &self.npc_whereabouts {
                    m.insert(k.clone(), whereabouts_to_value(row));
                }
                Value::Object(m)
            }
        }
    }

    pub fn npc_whereabouts_summary(&mut self, npc_id: &str) -> String {
        self.ensure_npc_whereabouts();
        let npc = match self.npcs.get(npc_id) {
            Some(n) => n.clone(),
            None => return String::new(),
        };
        let row = match self.npc_whereabouts.get(npc_id) {
            Some(r) => r.clone(),
            None => return String::new(),
        };
        if row.status == "unknown" {
            return String::new();
        }
        let mut bits = vec![
            format!(
                "{} ({}, {})",
                self.npc_player_label(npc_id, "player"),
                npc_id,
                npc.role
            ),
            if self.scene.present_npcs.contains(npc_id) {
                "in current scene".to_string()
            } else {
                "NOT in current scene".to_string()
            },
            format!("status: {}", row.status),
        ];
        if !row.location_name.is_empty() {
            bits.push(format!("location: {}", row.location_name));
        } else if !row.location_id.is_empty() {
            bits.push(format!("location_id: {}", row.location_id));
        }
        if !row.details.is_empty() {
            bits.push(format!("details: {}", row.details));
        }
        bits.join("; ")
    }

    // =====================================================================
    // Retrieval / projections
    // =====================================================================

    pub fn retrieval_documents(&mut self, actor_id: &str) -> Vec<RagDocument> {
        let mut docs: Vec<RagDocument> = Vec::new();
        docs.extend(self.state_record_documents(actor_id));

        for record in &self.fact_records {
            if record.kind == "truth" {
                continue;
            }
            let status = if record.kind == "public" && record.confirmed {
                "known"
            } else {
                "unconfirmed"
            };
            let mut metadata = Map::new();
            metadata.insert("fact_id".to_string(), json!(record.fact_id));
            metadata.insert("record_kind".to_string(), json!(record.kind));
            docs.push(RagDocument::new(
                format!("fact:{}", record.fact_id),
                if status == "known" {
                    "public_fact".to_string()
                } else {
                    "claim".to_string()
                },
                record.text.clone(),
                status.to_string(),
                nonempty_or(record.source.clone(), &record.fact_id),
                "player".to_string(),
                record.keywords.clone(),
                metadata,
            ));
        }

        let present_labels: Vec<String> = self
            .scene
            .present_npcs
            .iter()
            .map(|id| self.npc_player_label(id, actor_id))
            .collect();
        let exits_text = self
            .scene
            .visible_exits()
            .iter()
            .map(|e| format!("{} -> {}", e.name, e.destination))
            .collect::<Vec<_>>()
            .join(", ");
        let scene_text = format!(
            "Текущая сцена: {}. {} В сцене: {}. Выходы: {}.",
            self.scene.title,
            self.scene.description,
            if present_labels.is_empty() {
                "нет именованных NPC".to_string()
            } else {
                present_labels.join(", ")
            },
            if exits_text.is_empty() {
                "нет известных выходов".to_string()
            } else {
                exits_text
            }
        );
        {
            let mut metadata = Map::new();
            metadata.insert("scene_id".to_string(), json!(self.scene.scene_id));
            metadata.insert("location_id".to_string(), json!(self.scene.location_id));
            docs.push(RagDocument::new(
                format!("scene:{}", self.scene.scene_id),
                "scene_state".to_string(),
                scene_text,
                "current".to_string(),
                "current_scene".to_string(),
                "player".to_string(),
                vec![
                    self.scene.scene_id.clone(),
                    self.scene.location_id.clone(),
                    self.scene.title.clone(),
                ],
                metadata,
            ));
        }

        for item in self
            .scene
            .visible_items()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
        {
            let mut text = format!("В текущей сцене виден предмет: {}; место: {}.", item.name, item.location);
            if !item.details.is_empty() {
                text.push_str(&format!(" Детали: {}.", item.details));
            }
            if !item.owner.is_empty() {
                text.push_str(&format!(" Владелец: {}.", item.owner));
            }
            let mut metadata = Map::new();
            metadata.insert("scene_id".to_string(), json!(self.scene.scene_id));
            metadata.insert("item_id".to_string(), json!(item.item_id));
            docs.push(RagDocument::new(
                format!("scene_item:{}:{}", self.scene.scene_id, item.item_id),
                "scene_item".to_string(),
                text,
                "current".to_string(),
                "current_scene".to_string(),
                "player".to_string(),
                vec![item.item_id.clone(), item.name.clone(), item.owner.clone()],
                metadata,
            ));
        }

        self.ensure_npc_whereabouts();
        let npc_ids: Vec<String> = self.npcs.keys().cloned().collect();
        for npc_id in &npc_ids {
            let npc = self.npcs[npc_id].clone();
            let label = self.npc_player_label(npc_id, actor_id);
            let known_name = self.npc_known_name(npc_id, actor_id);
            let mut appearance = String::new();
            if !npc.physical_type.is_empty() {
                appearance.push_str(&format!(" Тип/внешнее впечатление: {}.", npc.physical_type));
            }
            if !npc.distinctive_features.is_empty() {
                appearance.push_str(&format!(" Приметы: {}.", npc.distinctive_features));
            }
            let gender_part = if !npc.pronouns.is_empty() {
                format!(" Род: {} ({}).", public_gender(&npc.pronouns), npc.pronouns)
            } else {
                String::new()
            };
            let mut metadata = Map::new();
            metadata.insert("npc_id".to_string(), json!(npc_id));
            metadata.insert("known_name".to_string(), json!(known_name));
            docs.push(RagDocument::new(
                format!("npc_public:{npc_id}"),
                "npc_public".to_string(),
                format!("{label} ({npc_id}) — {}.{gender_part}{appearance}", npc.role),
                "known".to_string(),
                "npc_roster".to_string(),
                "player".to_string(),
                vec![
                    npc_id.clone(),
                    label.clone(),
                    npc.role.clone(),
                    npc.pronouns.clone(),
                    npc.physical_type.clone(),
                ],
                metadata,
            ));

            let where_row = self.npc_whereabouts.get(npc_id).cloned();
            if let Some(w) = where_row {
                if w.status != "unknown" {
                    let present_text = if self.scene.present_npcs.contains(npc_id) {
                        "присутствует в текущей сцене"
                    } else {
                        "не в текущей сцене"
                    };
                    let where_label = if !w.location_name.is_empty() {
                        w.location_name.clone()
                    } else if !w.location_id.is_empty() {
                        w.location_id.clone()
                    } else {
                        "неизвестно".to_string()
                    };
                    let details_part = if !w.details.is_empty() {
                        format!(" Детали: {}.", w.details)
                    } else {
                        String::new()
                    };
                    let status = if w.status == "present" {
                        "present"
                    } else if matches!(w.status.as_str(), "known" | "likely") {
                        "known"
                    } else {
                        "unconfirmed"
                    };
                    let mut metadata = Map::new();
                    metadata.insert("npc_id".to_string(), json!(npc_id));
                    metadata.insert("location_id".to_string(), json!(w.location_id));
                    docs.push(RagDocument::new(
                        format!("npc_whereabouts:{npc_id}"),
                        "npc_whereabouts".to_string(),
                        format!(
                            "{label} сейчас {present_text}. Статус местонахождения: {}. Где искать: {where_label}.{details_part}",
                            w.status
                        ),
                        status.to_string(),
                        nonempty_or(w.source.clone(), "world_state"),
                        "player".to_string(),
                        vec![
                            npc_id.clone(),
                            label.clone(),
                            w.location_id.clone(),
                            w.location_name.clone(),
                            w.status.clone(),
                        ],
                        metadata,
                    ));
                }
            }
        }

        for rumor in &self.rumors {
            if !rumor.witnesses.contains("player") {
                continue;
            }
            let speaker_exists = self.npcs.contains_key(&rumor.speaker);
            let speaker_name = if speaker_exists {
                self.npc_player_label(&rumor.speaker, actor_id)
            } else {
                rumor.speaker.clone()
            };
            let mut metadata = Map::new();
            metadata.insert("seq".to_string(), json!(rumor.seq));
            metadata.insert("turn".to_string(), json!(rumor.turn));
            metadata.insert("speaker".to_string(), json!(rumor.speaker));
            let witnesses_sorted: Vec<String> = rumor.witnesses.iter().cloned().collect();
            metadata.insert("witnesses".to_string(), json!(witnesses_sorted));
            metadata.insert("confirmed".to_string(), json!(rumor.confirmed));
            docs.push(RagDocument::new(
                format!("testimony:{}", rumor.seq),
                "testimony".to_string(),
                format!("{speaker_name} сказал: «{}»", rumor.text),
                if rumor.confirmed { "known" } else { "unconfirmed" }.to_string(),
                format!("event:{}", rumor.seq),
                "player".to_string(),
                vec![rumor.speaker.clone(), speaker_name.clone()],
                metadata,
            ));
        }
        docs
    }

    pub fn proper_nouns(&self) -> Vec<String> {
        let mut names: Vec<String> = self.npcs.values().map(|n| n.name.clone()).collect();
        let mut scene_names = vec![self.scene.title.clone()];
        scene_names.extend(self.scene.items.iter().map(|i| i.name.clone()));
        scene_names.extend(self.scene.exits.iter().map(|e| e.name.clone()));
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut out: Vec<String> = Vec::new();
        names.extend(self.extra_proper_nouns.iter().cloned());
        names.extend(scene_names);
        for name in names {
            if !name.is_empty() && !seen.contains(&name) {
                seen.insert(name.clone());
                out.push(name);
            }
        }
        out
    }

    pub fn scene_context(&self) -> String {
        let mut present: Vec<String> = Vec::new();
        for npc_id in &self.scene.present_npcs {
            let npc = match self.npcs.get(npc_id) {
                Some(n) => n,
                None => continue,
            };
            let p = self.scene.presence.get(npc_id);
            let mut detail = format!(
                "{} ({}, {})",
                self.npc_player_label(npc_id, "player"),
                npc_id,
                npc.role
            );
            if !npc.physical_type.is_empty() {
                detail.push_str(&format!(", {}", npc.physical_type));
            }
            if !npc.condition.is_empty() {
                detail.push_str(&format!(", condition: {}", npc.condition));
            }
            if !npc.pronouns.is_empty() {
                detail.push_str(&format!(", род: {}", public_gender(&npc.pronouns)));
            }
            if let Some(p) = p {
                if p.visible {
                    detail.push_str(&format!(" at {}", p.location));
                }
            }
            present.push(detail);
        }
        let mut offscreen: Vec<String> = Vec::new();
        for npc_id in self.npcs.keys() {
            if self.scene.present_npcs.contains(npc_id) {
                continue;
            }
            let line = self.npc_whereabouts_summary_ro(npc_id);
            if !line.is_empty() {
                offscreen.push(line);
            }
        }
        let items: Vec<String> = self
            .scene
            .visible_items()
            .iter()
            .map(|i| i.name.clone())
            .collect();
        let mut exits: Vec<String> = Vec::new();
        for exit_ in self.scene.visible_exits() {
            let mut line = format!("{} -> {}", exit_.name, exit_.destination);
            if !exit_.blocked_by.is_empty() {
                line.push_str(&format!(" (blocked by {})", exit_.blocked_by));
            }
            exits.push(line);
        }
        let mut parts = vec![
            format!("Scene: {}", self.scene.title),
            format!("Location: {}", self.scene.location_id),
            format!("World time: {}", self.time_summary()),
            format!("Description: {}", self.scene.description),
            format!(
                "Present named NPCs: {}",
                if present.is_empty() {
                    "(none)".to_string()
                } else {
                    present.join(", ")
                }
            ),
            format!(
                "Known offscreen NPC whereabouts: {}",
                if offscreen.is_empty() {
                    "(none established)".to_string()
                } else {
                    offscreen
                        .iter()
                        .map(|l| format!("- {l}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            ),
            format!(
                "Visible objects: {}",
                if items.is_empty() {
                    "(none listed)".to_string()
                } else {
                    items.join(", ")
                }
            ),
            format!(
                "Visible exits: {}",
                if exits.is_empty() {
                    "(none listed)".to_string()
                } else {
                    exits.join(", ")
                }
            ),
        ];
        if !self.scene.tension.is_empty() {
            parts.push(format!("Tension: {}", self.scene.tension));
        }
        parts.join("\n")
    }

    /// Read-only whereabouts summary (scene_context calls this without the
    /// `ensure_npc_whereabouts` re-coercion, matching world.py which assumes the
    /// rows are already synced — scene_context does not call _ensure first).
    fn npc_whereabouts_summary_ro(&self, npc_id: &str) -> String {
        let npc = match self.npcs.get(npc_id) {
            Some(n) => n,
            None => return String::new(),
        };
        let row = match self.npc_whereabouts.get(npc_id) {
            Some(r) => r,
            None => return String::new(),
        };
        if row.status == "unknown" {
            return String::new();
        }
        let mut bits = vec![
            format!(
                "{} ({}, {})",
                self.npc_player_label(npc_id, "player"),
                npc_id,
                npc.role
            ),
            if self.scene.present_npcs.contains(npc_id) {
                "in current scene".to_string()
            } else {
                "NOT in current scene".to_string()
            },
            format!("status: {}", row.status),
        ];
        if !row.location_name.is_empty() {
            bits.push(format!("location: {}", row.location_name));
        } else if !row.location_id.is_empty() {
            bits.push(format!("location_id: {}", row.location_id));
        }
        if !row.details.is_empty() {
            bits.push(format!("details: {}", row.details));
        }
        bits.join("; ")
    }

    pub fn entity_refs(&mut self) -> Value {
        self.ensure_npc_whereabouts();
        let mut entities: Vec<Value> = Vec::new();

        let npc_ids: Vec<String> = self.npcs.keys().cloned().collect();
        for npc_id in &npc_ids {
            let npc = self.npcs[npc_id].clone();
            let present = self.scene.present_npcs.contains(npc_id);
            let presence = self.scene.presence.get(npc_id).cloned();
            let whereabouts = self
                .npc_whereabouts
                .get(npc_id)
                .cloned()
                .unwrap_or_else(|| NpcWhereabouts::new(npc_id));
            let role = public_role(&npc.role);
            let pronouns = public_gender(&npc.pronouns);
            let label = self.npc_player_label(npc_id, "player");
            let where_str = if present {
                presence
                    .as_ref()
                    .map(|p| p.location.clone())
                    .unwrap_or_else(|| self.scene.title.clone())
            } else if !whereabouts.location_name.is_empty() {
                whereabouts.location_name.clone()
            } else {
                whereabouts.location_id.clone()
            };
            let mut meta: Vec<Value> = vec![
                json!({"label": "роль", "value": nonempty_or(role.clone(), "персонаж")}),
                json!({
                    "label": "статус",
                    "value": if present {
                        "в сцене".to_string()
                    } else {
                        whereabouts_status_label(&whereabouts.status)
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| whereabouts.status.clone())
                    }
                }),
            ];
            if !pronouns.is_empty() {
                meta.push(json!({"label": "род", "value": pronouns}));
            }
            if !where_str.is_empty() {
                meta.push(json!({"label": "где", "value": where_str}));
            }
            if present {
                if let Some(ref p) = presence {
                    if !p.activity.is_empty() {
                        meta.push(json!({"label": "занят", "value": p.activity}));
                    }
                }
            }
            let description = public_npc_description(&npc);
            let subtitle = format!(
                "персонаж{}",
                if role.is_empty() {
                    String::new()
                } else {
                    format!(" · {role}")
                }
            );
            push_entity(
                &mut entities,
                "npc",
                npc_id,
                &label,
                &label,
                &subtitle,
                &description,
                meta,
                &npc.color,
            );
        }

        let mut seen_locs: BTreeSet<String> = BTreeSet::new();

        // current scene location
        let mut current_meta: Vec<Value> = Vec::new();
        if !self.scene.present_npcs.is_empty() {
            let names: Vec<String> = self
                .scene
                .present_npcs
                .iter()
                .filter_map(|n| self.npcs.get(n).map(|npc| npc.name.clone()))
                .collect();
            current_meta.push(json!({"label": "в сцене", "value": names.join(", ")}));
        }
        let visible_exit_names: Vec<String> =
            self.scene.visible_exits().iter().map(|e| e.name.clone()).collect();
        if !visible_exit_names.is_empty() {
            current_meta.push(json!({"label": "выходы", "value": visible_exit_names.join(", ")}));
        }
        add_location(
            &mut entities,
            &mut seen_locs,
            &self.scene.location_id,
            &self.scene.title,
            &self.scene.description,
            current_meta,
        );

        for exit_ in self
            .scene
            .visible_exits()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>()
        {
            let destination = exit_.destination.trim().to_string();
            if destination.is_empty() || destination.to_lowercase() == "unknown destination" {
                continue;
            }
            add_location(
                &mut entities,
                &mut seen_locs,
                &format!("{}_destination", exit_.exit_id),
                &destination,
                &format!("Видимый выход из текущей сцены: {}.", exit_.name),
                vec![json!({"label": "через", "value": exit_.name})],
            );
        }

        for row in self.npc_whereabouts.values().cloned().collect::<Vec<_>>() {
            if row.status == "unknown" {
                continue;
            }
            let label = if !row.location_name.is_empty() {
                row.location_name.clone()
            } else {
                row.location_id.clone()
            };
            if label.is_empty() {
                continue;
            }
            let source_label = {
                let s = public_source(&row.source);
                if !s.is_empty() {
                    s
                } else {
                    whereabouts_status_label(&row.status)
                        .map(|x| x.to_string())
                        .unwrap_or_else(|| row.status.clone())
                }
            };
            let loc_id = if !row.location_id.is_empty() {
                row.location_id.clone()
            } else {
                label.clone()
            };
            add_location(
                &mut entities,
                &mut seen_locs,
                &loc_id,
                &label,
                &row.details,
                vec![json!({"label": "источник", "value": source_label})],
            );
        }

        json!({"version": 1, "entities": entities})
    }

    pub fn entity_reference_context(&mut self) -> String {
        let registry = self.entity_refs();
        let entities = registry
            .get("entities")
            .and_then(|e| e.as_array())
            .cloned()
            .unwrap_or_default();
        let npcs: Vec<&Value> = entities
            .iter()
            .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("npc"))
            .collect();
        let locs: Vec<&Value> = entities
            .iter()
            .filter(|e| e.get("kind").and_then(|k| k.as_str()) == Some("loc"))
            .collect();
        let npc_refs = if npcs.is_empty() {
            "(none)".to_string()
        } else {
            npcs.iter()
                .take(12)
                .map(|e| {
                    format!(
                        "[[npc:{}|{}]]",
                        e["id"].as_str().unwrap_or(""),
                        e["label"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let loc_refs = if locs.is_empty() {
            "(none)".to_string()
        } else {
            locs.iter()
                .take(12)
                .map(|e| {
                    format!(
                        "[[loc:{}|{}]]",
                        e["id"].as_str().unwrap_or(""),
                        e["label"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "Available player-safe entity refs (use exact labels for specific listed entities):\nNPCs: {npc_refs}\nLocations: {loc_refs}"
        )
    }

    pub fn npc_scene_slice(&mut self, npc_id: &str) -> String {
        let npc = match self.npcs.get(npc_id) {
            Some(n) => n.clone(),
            None => return "You are not present in the current scene.".to_string(),
        };
        let presence = match self.scene.presence.get(npc_id) {
            Some(p) => p.clone(),
            None => return "You are not present in the current scene.".to_string(),
        };
        if !self.scene.present_npcs.contains(npc_id) {
            return "You are not present in the current scene.".to_string();
        }
        let mut others: Vec<String> = Vec::new();
        for other_id in &self.scene.present_npcs {
            if other_id == npc_id {
                continue;
            }
            let other = match self.npcs.get(other_id) {
                Some(o) => o,
                None => continue,
            };
            let other_presence = match self.scene.presence.get(other_id) {
                Some(p) if p.visible => p,
                _ => continue,
            };
            let mut label = format!("{} ({}", other.name, other.role);
            if !other.pronouns.is_empty() {
                label.push_str(&format!("; род: {}", public_gender(&other.pronouns)));
            }
            label.push_str(&format!(") at {}", other_presence.location));
            others.push(label);
        }
        let items: Vec<String> = self
            .scene
            .visible_items()
            .iter()
            .map(|item| {
                let mut s = format!("{} at {}", item.name, item.location);
                if !item.owner.is_empty() {
                    s.push_str(&format!(", owner: {}", item.owner));
                }
                s
            })
            .collect();
        let exits: Vec<String> = self
            .scene
            .visible_exits()
            .iter()
            .map(|e| e.name.clone())
            .collect();
        let mut parts = vec![
            format!("You are in: {}", self.scene.title),
            format!(
                "Your name/gender marker: {}{}",
                npc.name,
                if !npc.pronouns.is_empty() {
                    format!(" ({} = {})", npc.pronouns, public_gender(&npc.pronouns))
                } else {
                    String::new()
                }
            ),
            format!("Your position: {}", presence.location),
            format!(
                "Your current activity: {}",
                if presence.activity.is_empty() {
                    "(none specified)".to_string()
                } else {
                    presence.activity.clone()
                }
            ),
            format!(
                "Your attitude right now: {}",
                if presence.attitude.is_empty() {
                    "(none specified)".to_string()
                } else {
                    presence.attitude.clone()
                }
            ),
            format!(
                "Other visible named NPCs: {}",
                if others.is_empty() {
                    "(none)".to_string()
                } else {
                    others.join(", ")
                }
            ),
            format!(
                "Visible objects: {}",
                if items.is_empty() {
                    "(none listed)".to_string()
                } else {
                    items.join(", ")
                }
            ),
            format!(
                "Visible exits: {}",
                if exits.is_empty() {
                    "(none listed)".to_string()
                } else {
                    exits.join(", ")
                }
            ),
        ];
        let public_facts: Vec<String> = self
            .fact_records
            .iter()
            .filter(|r| r.kind == "public")
            .map(|r| r.text.clone())
            .collect();
        if !public_facts.is_empty() {
            let listed: Vec<String> = public_facts
                .iter()
                .take(8)
                .map(|f| format!("- {f}"))
                .collect();
            parts.push(format!("Public facts you may know:\n{}", listed.join("\n")));
        }
        let mut query = StateRecordQuery::new(npc_id);
        let kinds = vec![
            "fact".to_string(),
            "rumor".to_string(),
            "npc_memory".to_string(),
            "relationship".to_string(),
            "goal".to_string(),
        ];
        query.kinds = Some(&kinds);
        let memory_records: Vec<StateRecord> =
            self.state_records_for(&query).into_iter().cloned().collect();
        if !memory_records.is_empty() {
            let lines: Vec<String> = memory_records
                .iter()
                .take(12)
                .map(|record| {
                    let subject = if record.subject.is_empty() {
                        String::new()
                    } else {
                        format!(" about {}", record.subject)
                    };
                    format!("- {}{subject}: {}", record.kind, record.text)
                })
                .collect();
            parts.push(format!("Actor-visible state memory:\n{}", lines.join("\n")));
        }
        if !self.scene.constraints.is_empty() {
            let lines: Vec<String> = self
                .scene
                .constraints
                .iter()
                .map(|c| format!("- {c}"))
                .collect();
            parts.push(format!("Physical limits:\n{}", lines.join("\n")));
        }
        parts.push(format!(
            "Entity refs for visible text:\n{}",
            self.entity_reference_context()
        ));
        parts.join("\n")
    }

    pub fn present_witnesses(&self) -> BTreeSet<String> {
        let mut s: BTreeSet<String> = self.scene.present_npcs.clone();
        s.insert("player".to_string());
        s
    }

    pub fn npc_can_react(&self, npc_id: &str) -> bool {
        if !self.scene.present_npcs.contains(npc_id) {
            return false;
        }
        match self.scene.presence.get(npc_id) {
            Some(p) => p.visible && p.can_hear,
            None => false,
        }
    }

    // =====================================================================
    // Presence / scene mutators
    // =====================================================================

    #[allow(clippy::too_many_arguments)]
    pub fn set_npc_presence(
        &mut self,
        npc_id: &str,
        present: bool,
        location: &str,
        visible: bool,
        can_hear: bool,
        activity: &str,
        attitude: &str,
    ) -> Result<Value, String> {
        self.ensure_npc_whereabouts();
        let resolved_id = self.resolve(npc_id)?;
        let role = self.npcs[&resolved_id].role.clone();
        if present {
            let old = self.scene.presence.get(&resolved_id).cloned();
            self.scene.present_npcs.insert(resolved_id.clone());
            self.scene.presence.insert(
                resolved_id.clone(),
                Presence {
                    npc_id: resolved_id.clone(),
                    location: nonempty_or(
                        location.trim().to_string(),
                        &old.as_ref().map(|o| o.location.clone()).unwrap_or_else(|| "in the scene".to_string()),
                    ),
                    visible,
                    can_hear,
                    activity: nonempty_or(
                        activity.trim().to_string(),
                        &old.as_ref().map(|o| o.activity.clone()).unwrap_or_else(|| format!("present as {role}")),
                    ),
                    attitude: nonempty_or(
                        attitude.trim().to_string(),
                        &old.as_ref().map(|o| o.attitude.clone()).unwrap_or_default(),
                    ),
                },
            );
            self.sync_present_npc_whereabouts();
        } else {
            self.scene.present_npcs.remove(&resolved_id);
            if let Some(old) = self.scene.presence.get(&resolved_id).cloned() {
                self.scene.presence.insert(
                    resolved_id.clone(),
                    Presence {
                        npc_id: resolved_id.clone(),
                        location: nonempty_or(location.trim().to_string(), &old.location),
                        visible: false,
                        can_hear: false,
                        activity: nonempty_or(
                            activity.trim().to_string(),
                            "not present in the current scene",
                        ),
                        attitude: nonempty_or(attitude.trim().to_string(), &old.attitude),
                    },
                );
            }
            let location_text = location.trim().to_string();
            if !location_text.is_empty() {
                self.npc_whereabouts.insert(
                    resolved_id.clone(),
                    NpcWhereabouts {
                        npc_id: resolved_id.clone(),
                        location_id: safe_id(&location_text, ""),
                        location_name: location_text,
                        status: "known".to_string(),
                        details: nonempty_or(activity.trim().to_string(), "вне текущей сцены"),
                        source: "move_npc".to_string(),
                    },
                );
            } else {
                self.npc_whereabouts.insert(
                    resolved_id.clone(),
                    NpcWhereabouts {
                        npc_id: resolved_id.clone(),
                        location_id: String::new(),
                        location_name: String::new(),
                        status: "unknown".to_string(),
                        details: nonempty_or(
                            activity.trim().to_string(),
                            "покинул текущую сцену; куда именно, не установлено",
                        ),
                        source: "move_npc".to_string(),
                    },
                );
            }
        }
        let name = self.npcs[&resolved_id].name.clone();
        let present_now = self.scene.present_npcs.contains(&resolved_id);
        let present_npcs: Vec<String> = self.scene.present_npcs.iter().cloned().collect();
        let whereabouts = self.npc_whereabouts_export(Some(&resolved_id));
        Ok(json!({
            "npc_id": resolved_id,
            "name": name,
            "present": present_now,
            "scene": self.scene.title,
            "present_npcs": present_npcs,
            "whereabouts": whereabouts,
        }))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_scene(
        &mut self,
        title: &str,
        description: &str,
        location_id: &str,
        present_npcs: &Value,
        items: &Value,
        exits: &Value,
        constraints: &Value,
        tension: &str,
    ) -> Value {
        self.ensure_npc_whereabouts();
        let title = nonempty_or(title.trim().to_string(), "Новая сцена");
        let description = nonempty_or(description.trim().to_string(), &title);
        let fallback_id = format!("scene_{}", ord_sum(&title) % 100000);
        let location_id = safe_id(
            &nonempty_or(location_id.trim().to_string(), &title),
            &fallback_id,
        );
        let old_scene = self.scene.clone();
        let old_present: BTreeSet<String> = old_scene.present_npcs.clone();

        let mut present: BTreeSet<String> = BTreeSet::new();
        let mut presence: BTreeMap<String, Presence> = BTreeMap::new();
        let mut dropped_present_npcs: Vec<String> = Vec::new();
        for raw_id in as_list(present_npcs) {
            let npc_id = safe_id(&as_str(&raw_id), "");
            if !self.npcs.contains_key(&npc_id) {
                let raw_label = as_str(&raw_id);
                if !raw_label.is_empty() {
                    dropped_present_npcs.push(raw_label);
                }
                continue;
            }
            let old = self.scene.presence.get(&npc_id).cloned();
            let npc = &self.npcs[&npc_id];
            present.insert(npc_id.clone());
            presence.insert(
                npc_id.clone(),
                Presence {
                    npc_id: npc_id.clone(),
                    location: old.as_ref().map(|o| o.location.clone()).unwrap_or_else(|| "в сцене".to_string()),
                    visible: true,
                    can_hear: true,
                    activity: old
                        .as_ref()
                        .map(|o| o.activity.clone())
                        .unwrap_or_else(|| format!("присутствует как {}", npc.role)),
                    attitude: old.as_ref().map(|o| o.attitude.clone()).unwrap_or_default(),
                },
            );
        }

        let scene_items = coerce_scene_items(Some(items), "в сцене");
        let scene_exits = coerce_scene_exits_setscene(Some(exits));

        self.scene = SceneState {
            scene_id: location_id.clone(),
            location_id: location_id.clone(),
            title,
            description: description.clone(),
            present_npcs: present.clone(),
            presence,
            items: scene_items,
            exits: scene_exits,
            constraints: as_list(constraints)
                .iter()
                .map(as_str)
                .filter(|s| !s.is_empty())
                .collect(),
            tension: tension.trim().to_string(),
            player_seen: vec![description],
        };

        let gone: Vec<String> = old_present.difference(&present).cloned().collect();
        for old_npc_id in gone {
            if !self.npcs.contains_key(&old_npc_id) {
                continue;
            }
            let old_presence = old_scene.presence.get(&old_npc_id);
            self.npc_whereabouts.insert(
                old_npc_id.clone(),
                NpcWhereabouts {
                    npc_id: old_npc_id.clone(),
                    location_id: old_scene.location_id.clone(),
                    location_name: old_scene.title.clone(),
                    status: "known".to_string(),
                    details: nonempty_or(
                        old_presence.map(|p| p.activity.clone()).unwrap_or_default(),
                        "оставался в прежней сцене",
                    ),
                    source: "previous scene".to_string(),
                },
            );
        }
        self.sync_present_npc_whereabouts();
        let mut result = self.scene_export();
        if !dropped_present_npcs.is_empty() {
            if let Value::Object(ref mut m) = result {
                m.insert(
                    "dropped_present_npcs".to_string(),
                    json!(dropped_present_npcs),
                );
                m.insert(
                    "repair_hint".to_string(),
                    json!(format!(
                        "Ignored unknown present_npcs ids: {}. Use npc_ids from the current roster in CURRENT TURN CONTEXT.",
                        dropped_present_npcs.join(", ")
                    )),
                );
            }
        }
        result
    }

    #[allow(clippy::too_many_arguments)]
    pub fn set_npc_whereabouts(
        &mut self,
        npc_id: &str,
        location_id: &str,
        location_name: &str,
        status: &str,
        details: &str,
        source: &str,
    ) -> Result<Value, String> {
        self.ensure_npc_whereabouts();
        let resolved_id = self.resolve(npc_id)?;
        if self.scene.present_npcs.contains(&resolved_id) {
            self.sync_present_npc_whereabouts();
        } else {
            let mut clean_location_name = location_name.trim().to_string();
            let clean_location_id = safe_id(location_id, "");
            if clean_location_name.is_empty() && !clean_location_id.is_empty() {
                clean_location_name = clean_location_id.clone();
            }
            let mut clean_status = self.whereabouts_status(status, "known");
            if clean_location_name.is_empty()
                && clean_location_id.is_empty()
                && details.trim().is_empty()
            {
                clean_status = "unknown".to_string();
            }
            self.npc_whereabouts.insert(
                resolved_id.clone(),
                NpcWhereabouts {
                    npc_id: resolved_id.clone(),
                    location_id: clean_location_id,
                    location_name: clean_location_name,
                    status: clean_status,
                    details: details.trim().to_string(),
                    source: nonempty_or(source.trim().to_string(), "gm"),
                },
            );
        }
        let name = self.npcs[&resolved_id].name.clone();
        let present_now = self.scene.present_npcs.contains(&resolved_id);
        let whereabouts = self.npc_whereabouts_export(Some(&resolved_id));
        Ok(json!({
            "npc_id": resolved_id,
            "name": name,
            "present": present_now,
            "current_scene": self.scene.title,
            "whereabouts": whereabouts,
        }))
    }

    /// `record_rumor(seq, turn, speaker, text, witnesses)`.
    pub fn record_rumor(
        &mut self,
        seq: i64,
        turn: i64,
        speaker: &str,
        text: &str,
        witnesses: BTreeSet<String>,
        rumors_cap: usize,
    ) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.rumor_seq += 1;
        self.rumors.push(Rumor {
            seq,
            turn,
            speaker: speaker.to_string(),
            text,
            witnesses,
            confirmed: false,
        });
        truncate_tail(&mut self.rumors, rumors_cap);
    }

    pub fn scene_export(&mut self) -> Value {
        let presence: Map<String, Value> = self
            .scene
            .presence
            .iter()
            .map(|(k, v)| (k.clone(), presence_to_value(v)))
            .collect();
        let items: Vec<Value> = self.scene.items.iter().map(scene_item_to_value).collect();
        let exits: Vec<Value> = self.scene.exits.iter().map(scene_exit_to_value).collect();
        let present_npcs: Vec<String> = self.scene.present_npcs.iter().cloned().collect();
        let whereabouts = self.npc_whereabouts_export(None);
        json!({
            "scene_id": self.scene.scene_id,
            "location_id": self.scene.location_id,
            "title": self.scene.title,
            "description": self.scene.description,
            "present_npcs": present_npcs,
            "presence": presence,
            "items": items,
            "exits": exits,
            "constraints": self.scene.constraints,
            "tension": self.scene.tension,
            "npc_whereabouts": whereabouts,
        })
    }

    /// `npc(npc_id)` — strict lookup (KeyError -> Err).
    pub fn npc(&self, npc_id: &str) -> Result<&Npc, String> {
        self.npcs.get(npc_id).ok_or_else(|| {
            format!(
                "No such NPC: {npc_id}. Available: {:?}",
                self.npcs.keys().collect::<Vec<_>>()
            )
        })
    }

    /// `resolve(npc_id)` — lenient lookup by id or name (case-insensitive),
    /// returning the resolved npc_id.
    pub fn resolve(&self, npc_id: &str) -> Result<String, String> {
        let key = npc_id.trim().to_lowercase();
        if self.npcs.contains_key(&key) {
            return Ok(key);
        }
        for npc in self.npcs.values() {
            let name_lower = npc.name.to_lowercase();
            if key == name_lower || name_lower.contains(&key) {
                return Ok(npc.npc_id.clone());
            }
        }
        Err(format!(
            "No such NPC: '{npc_id}'. Available: {:?}",
            self.npcs.keys().collect::<Vec<_>>()
        ))
    }

    // =====================================================================
    // Time
    // =====================================================================

    pub fn time_export(&self) -> Value {
        let time = &self.time;
        let minutes_per_hour = std::cmp::max(1, time.minutes_per_hour);
        let hours_per_day = std::cmp::max(1, time.hours_per_day);
        let day_minutes = minutes_per_hour * hours_per_day;
        let absolute = std::cmp::max(0, time.absolute_minutes);
        let minute_of_day = absolute % day_minutes;
        let hour = minute_of_day / minutes_per_hour;
        let minute = minute_of_day % minutes_per_hour;
        json!({
            "calendar_name": time.calendar_name,
            "absolute_minutes": absolute,
            "current_date_label": nonempty_or(time.current_date_label.clone(), "День 1"),
            "day_number": absolute / day_minutes + 1,
            "time_of_day": format!("{hour:02}:{minute:02}"),
            "minutes_per_hour": minutes_per_hour,
            "hours_per_day": hours_per_day,
            "day_names": time.day_names,
            "month_names": time.month_names,
            "last_advance_minutes": std::cmp::max(0, time.last_advance_minutes),
            "last_advance_reason": time.last_advance_reason,
        })
    }

    pub fn time_summary(&self) -> String {
        let payload = self.time_export();
        let calendar = payload["calendar_name"].as_str().unwrap_or("");
        let date = {
            let d = payload["current_date_label"].as_str().unwrap_or("");
            if !d.is_empty() {
                d.to_string()
            } else {
                format!("День {}", payload["day_number"])
            }
        };
        let prefix = if !calendar.is_empty() {
            format!("{calendar}, ")
        } else {
            String::new()
        };
        format!("{prefix}{date}, {}", payload["time_of_day"].as_str().unwrap_or(""))
    }

    pub fn time_context(&self) -> String {
        let payload = self.time_export();
        let mut lines = vec![
            format!("Current world time: {}", self.time_summary()),
            format!(
                "Previous player turn elapsed: {} minutes",
                payload["last_advance_minutes"]
            ),
        ];
        let reason = payload["last_advance_reason"].as_str().unwrap_or("").trim().to_string();
        if !reason.is_empty() {
            lines.push(format!("Previous time reason: {reason}"));
        }
        lines.join("\n")
    }

    pub fn advance_time(&mut self, minutes: &Value, reason: &str) -> Result<Value, String> {
        let amount = as_int_or_none(minutes);
        let amount = match amount {
            Some(a) if a >= 0 => a,
            _ => return Err("minutes must be a non-negative integer".to_string()),
        };
        let before = self.time_export();
        self.time.absolute_minutes =
            before["absolute_minutes"].as_i64().unwrap_or(0) + amount;
        self.time.last_advance_minutes = amount;
        self.time.last_advance_reason = reason.trim().to_string();
        let after = self.time_export();
        Ok(json!({
            "ok": true,
            "elapsed_minutes": amount,
            "reason": reason.trim(),
            "before": before,
            "current": after,
            "summary": self.time_summary(),
        }))
    }

    // =====================================================================
    // Player character
    // =====================================================================

    fn apply_player_character_fields(
        pc: &mut PlayerCharacter,
        fields: &Map<String, Value>,
    ) -> BTreeSet<String> {
        let text_fields: BTreeSet<&str> = [
            "name",
            "pronouns",
            "class_role",
            "background",
            "age",
            "physical_type",
            "distinctive_features",
            "life_status",
            "life_status_note",
            "condition",
            "personality",
            "values",
            "gm_notes",
            "speed",
            "senses",
            "languages",
        ]
        .into_iter()
        .collect();
        let dict_fields: BTreeSet<&str> =
            ["abilities", "skills", "saving_throws", "hp"].into_iter().collect();
        let list_fields: BTreeSet<&str> =
            ["inventory", "equipment", "features"].into_iter().collect();
        let joined: BTreeSet<&str> = ["speed", "senses", "languages"].into_iter().collect();

        let mut changed: BTreeSet<String> = BTreeSet::new();
        for key in PLAYER_CHARACTER_FIELDS {
            if !fields.contains_key(key) {
                continue;
            }
            let raw = &fields[key];
            // Compute the new value as a Value and compare to current, then set.
            let new_value: Value = if dict_fields.contains(key) {
                Value::Object(as_dict(raw))
            } else if list_fields.contains(key) {
                Value::Array(
                    as_list(raw)
                        .iter()
                        .map(as_str)
                        .filter(|s| !s.is_empty())
                        .map(Value::String)
                        .collect(),
                )
            } else if key == "level" || key == "passive_perception" {
                match as_int_or_none(raw) {
                    Some(i) => json!(i),
                    None => Value::Null,
                }
            } else if key == "ac" {
                raw.clone()
            } else if text_fields.contains(key) {
                if joined.contains(key) {
                    Value::String(as_joined_str(raw))
                } else {
                    Value::String(as_str(raw))
                }
            } else {
                continue;
            };
            let current = pc_field_value(pc, key);
            if new_value != current {
                set_pc_field(pc, key, new_value);
                changed.insert(key.to_string());
            }
        }
        changed
    }

    pub fn update_player_character(&mut self, fields: &Value, reason: &str) -> Value {
        let map = match fields {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        };
        let changed = Self::apply_player_character_fields(&mut self.player_character, &map);
        if !changed.is_empty() {
            self.player_character.card_revision += 1;
        }
        let changed_sorted: Vec<String> = changed.into_iter().collect();
        json!({
            "ok": true,
            "updated": changed_sorted,
            "reason": reason.trim(),
            "card_revision": self.player_character.card_revision,
            "player_character": self.player_character_export(false),
        })
    }

    pub fn player_character_export(&self, public: bool) -> Value {
        let pc = &self.player_character;
        let mut m = Map::new();
        m.insert("name".to_string(), json!(pc.name));
        m.insert("pronouns".to_string(), json!(pc.pronouns));
        m.insert("class_role".to_string(), json!(pc.class_role));
        m.insert("level".to_string(), opt_int(pc.level));
        m.insert("background".to_string(), json!(pc.background));
        m.insert("age".to_string(), json!(pc.age));
        m.insert("physical_type".to_string(), json!(pc.physical_type));
        m.insert("distinctive_features".to_string(), json!(pc.distinctive_features));
        m.insert("life_status".to_string(), json!(pc.life_status));
        m.insert("life_status_note".to_string(), json!(pc.life_status_note));
        m.insert("condition".to_string(), json!(pc.condition));
        m.insert("personality".to_string(), json!(pc.personality));
        m.insert("values".to_string(), json!(pc.values));
        m.insert("abilities".to_string(), Value::Object(pc.abilities.clone()));
        m.insert("skills".to_string(), Value::Object(pc.skills.clone()));
        m.insert("saving_throws".to_string(), Value::Object(pc.saving_throws.clone()));
        m.insert("passive_perception".to_string(), opt_int(pc.passive_perception));
        m.insert("ac".to_string(), pc.ac.clone());
        m.insert("hp".to_string(), Value::Object(pc.hp.clone()));
        m.insert("speed".to_string(), json!(pc.speed));
        m.insert("senses".to_string(), json!(pc.senses));
        m.insert("languages".to_string(), json!(pc.languages));
        m.insert("inventory".to_string(), json!(pc.inventory));
        m.insert("equipment".to_string(), json!(pc.equipment));
        m.insert("features".to_string(), json!(pc.features));
        m.insert("card_revision".to_string(), json!(pc.card_revision));
        if !public {
            m.insert("gm_notes".to_string(), json!(pc.gm_notes));
        }
        Value::Object(m)
    }

    pub fn player_character_context(&self) -> String {
        let pc = &self.player_character;
        // mechanics dict in fixed insertion order, filtering empties.
        let mechanics_pairs: Vec<(&str, Value)> = vec![
            ("abilities", Value::Object(pc.abilities.clone())),
            ("skills", Value::Object(pc.skills.clone())),
            ("saving_throws", Value::Object(pc.saving_throws.clone())),
            ("passive_perception", opt_int(pc.passive_perception)),
            ("ac", pc.ac.clone()),
            ("hp", Value::Object(pc.hp.clone())),
            ("speed", json!(pc.speed)),
            ("senses", json!(pc.senses)),
            ("languages", json!(pc.languages)),
        ];
        let mechanics: Vec<(&str, Value)> = mechanics_pairs
            .into_iter()
            .filter(|(_, v)| !context_value_empty(v))
            .collect();
        let mut lines = vec![
            format!("Name: {}", pc.name),
            format!("Pronouns: {}", pc.pronouns),
        ];
        let labelled: Vec<(&str, Value)> = vec![
            ("Class/role", json!(pc.class_role)),
            ("Level", opt_int(pc.level)),
            ("Background", json!(pc.background)),
            ("Age", json!(pc.age)),
            ("Type/size/appearance", json!(pc.physical_type)),
            ("Distinctive features", json!(pc.distinctive_features)),
            ("Life status", json!(pc.life_status)),
            ("Life status note", json!(pc.life_status_note)),
            ("Condition", json!(pc.condition)),
            ("Personality", json!(pc.personality)),
            ("Values", json!(pc.values)),
        ];
        for (label, value) in labelled {
            if !context_value_empty(&value) {
                lines.push(format!("{label}: {}", value_to_plain(&value)));
            }
        }
        if !mechanics.is_empty() {
            // json.dumps(mechanics, sort_keys=True, separators=(',',':')).
            let mut obj = Map::new();
            for (k, v) in &mechanics {
                obj.insert(k.to_string(), v.clone());
            }
            lines.push(format!(
                "Mechanics: {}",
                state_record::canonical_json(&Value::Object(obj))
            ));
        }
        for (label, items) in [
            ("Inventory", &pc.inventory),
            ("Equipment", &pc.equipment),
            ("Features", &pc.features),
        ] {
            let values: Vec<String> = items
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !values.is_empty() {
                lines.push(format!("{label}: {}", values.join("; ")));
            }
        }
        if !pc.gm_notes.is_empty() {
            lines.push(format!("GM notes: {}", pc.gm_notes));
        }
        lines.push(format!("Card revision: {}", pc.card_revision));
        lines.join("\n")
    }

    pub fn npc_profile(&self, npc_id: &str, preset: &str, fields: &Value) -> Result<Value, String> {
        let resolved_id = self.resolve(npc_id)?;
        let npc = &self.npcs[&resolved_id];
        let mut clean_preset = safe_id(preset, "visible");
        if npc_profile_preset(&clean_preset).is_none() {
            clean_preset = "visible".to_string();
        }
        let mut wanted: Vec<String> = npc_profile_preset(&clean_preset)
            .unwrap()
            .iter()
            .map(|s| s.to_string())
            .collect();
        let all_fields = npc_profile_fields();
        let mut ignored: Vec<String> = Vec::new();
        for raw in as_list(fields) {
            let field_name = safe_id(&as_str(&raw), "");
            if field_name.is_empty() {
                continue;
            }
            if all_fields.contains(&field_name.as_str()) {
                if !wanted.contains(&field_name) {
                    wanted.push(field_name);
                }
            } else {
                ignored.push(as_str(&raw));
            }
        }
        let mut profile = Map::new();
        for field_name in &wanted {
            let value = npc_field_value(npc, field_name);
            if profile_empty(&value) {
                continue;
            }
            profile.insert(field_name.clone(), value);
        }
        let label = self.npc_player_label(&resolved_id, "player");
        Ok(json!({
            "status": "known",
            "npc_id": resolved_id,
            "label": label,
            "preset": clean_preset,
            "card_revision": npc.card_revision,
            "profile": profile,
            "ignored_fields": ignored,
        }))
    }

    // =====================================================================
    // Dice
    // =====================================================================

    pub fn roll(&mut self, notation: &str) -> (i64, String) {
        let data = dice::roll_data(
            &mut self.rng,
            &mut self.forced_die_next,
            &self.forced_die_all,
            notation,
        );
        (data.total, data.detail)
    }

    pub fn roll_for_outcome(
        &mut self,
        notation: &str,
        target_number: Option<&Value>,
        target_kind: &str,
        roll_kind: &str,
    ) -> (i64, String) {
        let payload = self.roll_outcome_payload(notation, target_number, target_kind, roll_kind);
        let total = payload
            .get("total")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let detail = payload
            .get("detail")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        (total, detail)
    }

    pub fn roll_outcome_payload(
        &mut self,
        notation: &str,
        target_number: Option<&Value>,
        target_kind: &str,
        roll_kind: &str,
    ) -> Value {
        dice::roll_outcome_payload(
            &mut self.rng,
            &mut self.forced_die_next,
            &self.forced_die_all,
            notation,
            target_number,
            target_kind,
            roll_kind,
        )
    }

    // =====================================================================
    // Debug / authoring mutators
    // =====================================================================

    pub fn update_npc(&mut self, npc_id: &str, fields: &Value) -> bool {
        let map = match fields {
            Value::Object(m) => m.clone(),
            _ => return false,
        };
        let npc = match self.npcs.get_mut(npc_id) {
            Some(n) => n,
            None => return false,
        };
        let text_fields: [&str; 26] = [
            "name",
            "color",
            "role",
            "pronouns",
            "public_label",
            "age",
            "physical_type",
            "distinctive_features",
            "life_status",
            "life_status_note",
            "condition",
            "persona",
            "personality",
            "values",
            "habits",
            "pressure_response",
            "boundaries",
            "voice",
            "goals",
            "knowledge",
            "secret",
            "speed",
            "senses",
            "languages",
            "_pad1",
            "_pad2",
        ];
        // Build the actual editable list precisely (text + dict + scalar).
        let text_fields_real: [&str; 24] = [
            "name",
            "color",
            "role",
            "pronouns",
            "public_label",
            "age",
            "physical_type",
            "distinctive_features",
            "life_status",
            "life_status_note",
            "condition",
            "persona",
            "personality",
            "values",
            "habits",
            "pressure_response",
            "boundaries",
            "voice",
            "goals",
            "knowledge",
            "secret",
            "speed",
            "senses",
            "languages",
        ];
        let _ = text_fields; // (kept to mirror Python tuple; unused)
        let dict_fields = ["abilities", "skills", "saving_throws", "hp"];
        let scalar_fields = ["passive_perception", "ac"];
        let joined = ["speed", "senses", "languages"];

        let mut editable: Vec<&str> = Vec::new();
        editable.extend(text_fields_real.iter().copied());
        editable.extend(dict_fields.iter().copied());
        editable.extend(scalar_fields.iter().copied());

        let mut content_changed = false;
        for key in editable {
            if !map.contains_key(key) {
                continue;
            }
            let raw = &map[key];
            let new_value: Value = if dict_fields.contains(&key) {
                Value::Object(as_dict(raw))
            } else if key == "passive_perception" {
                match as_int_or_none(raw) {
                    Some(i) => json!(i),
                    None => Value::Null,
                }
            } else if key == "ac" {
                raw.clone()
            } else if joined.contains(&key) {
                Value::String(as_joined_str(raw))
            } else {
                Value::String(as_str(raw))
            };
            let is_content = key != "color";
            let current = npc_field_value(npc, key);
            if is_content && new_value != current {
                content_changed = true;
            }
            set_npc_field(npc, key, new_value);
        }
        if content_changed {
            npc.card_revision += 1;
        }
        true
    }

    pub fn add_fact(&mut self, text: &str, kind: &str) -> Option<FactRecord> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        let mut kind = kind.trim().to_lowercase();
        if kind.is_empty() {
            kind = "public".to_string();
        }
        if !matches!(kind.as_str(), "public" | "truth" | "rumor") {
            kind = "public".to_string();
        }
        let existing: BTreeSet<String> =
            self.fact_records.iter().map(|r| r.fact_id.clone()).collect();
        let base = format!("{kind}_dbg");
        let mut idx = 1;
        while existing.contains(&format!("{base}_{idx}")) {
            idx += 1;
        }
        let record = FactRecord {
            fact_id: format!("{base}_{idx}"),
            kind: kind.clone(),
            text,
            keywords: Vec::new(),
            source: "debug".to_string(),
            confirmed: kind != "rumor",
        };
        self.fact_records.push(record.clone());
        Some(record)
    }

    pub fn remove_fact(&mut self, fact_id: &str) -> bool {
        let fid = fact_id.trim().to_string();
        let before = self.fact_records.len();
        self.fact_records.retain(|r| r.fact_id != fid);
        self.fact_records.len() < before
    }

    /// `set_public_intro` — the ONLY prefix-mutating world method.
    pub fn set_public_intro(&mut self, text: &str) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() {
            return false;
        }
        self.public = text;
        self.prefix_dirty = true;
        true
    }

    pub fn set_story_title(&mut self, text: &str) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() {
            return false;
        }
        self.story_title = text;
        true
    }

    pub fn set_hidden_truth(&mut self, text: &str) {
        let text = text.trim().to_string();
        self.canon = text.clone();
        self.fact_records.retain(|r| r.fact_id != "hidden_truth");
        if !text.is_empty() {
            self.fact_records.push(FactRecord {
                fact_id: "hidden_truth".to_string(),
                kind: "truth".to_string(),
                text,
                keywords: vec![
                    "hidden truth".to_string(),
                    "truth".to_string(),
                    "secret".to_string(),
                ],
                source: "debug".to_string(),
                confirmed: true,
            });
        }
    }

    pub fn set_hidden_events(&mut self, events: &Value) -> Vec<String> {
        self.hidden_events = as_list(events)
            .iter()
            .map(as_str)
            .filter(|s| !s.is_empty())
            .collect();
        self.hidden_events.clone()
    }

    pub fn add_hidden_event(&mut self, text: &str) -> bool {
        let text = text.trim().to_string();
        if text.is_empty() {
            return false;
        }
        self.hidden_events.push(text);
        true
    }

    pub fn remove_hidden_event(&mut self, index: &Value) -> bool {
        let idx = match as_int_or_none(index) {
            Some(i) => i,
            None => return false,
        };
        if idx < 0 || idx as usize >= self.hidden_events.len() {
            return false;
        }
        self.hidden_events.remove(idx as usize);
        true
    }

    pub fn add_debug_rumor(
        &mut self,
        speaker: &str,
        text: &str,
        rumors_cap: usize,
    ) -> Option<Rumor> {
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.rumor_seq += 1;
        let rumor = Rumor {
            seq: self.rumor_seq,
            turn: 0,
            speaker: nonempty_or(speaker.trim().to_string(), "слух"),
            text,
            witnesses: BTreeSet::new(),
            confirmed: false,
        };
        self.rumors.push(rumor.clone());
        truncate_tail(&mut self.rumors, rumors_cap);
        // After truncation the returned rumor is still the last appended one.
        Some(rumor)
    }

    pub fn remove_rumor(&mut self, seq: &Value) -> bool {
        let target = match as_int_or_none(seq) {
            Some(s) => s,
            None => return false,
        };
        let before = self.rumors.len();
        self.rumors.retain(|r| r.seq != target);
        self.rumors.len() < before
    }

    pub fn set_rumor_confirmed(&mut self, seq: &Value, confirmed: bool) -> bool {
        let target = match as_int_or_none(seq) {
            Some(s) => s,
            None => return false,
        };
        for rumor in self.rumors.iter_mut() {
            if rumor.seq == target {
                rumor.confirmed = confirmed;
                return true;
            }
        }
        false
    }

    pub fn patch_scene(&mut self, patch: &Value) -> Value {
        let patch = match patch {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        };
        let scene = self.scene.clone();
        let items: Vec<Value> = scene
            .items
            .iter()
            .map(|item| {
                json!({
                    "id": item.item_id,
                    "name": item.name,
                    "location": item.location,
                    "visible": item.visible,
                    "portable": item.portable,
                    "owner": item.owner,
                    "details": item.details,
                })
            })
            .collect();
        let exits: Vec<Value> = scene
            .exits
            .iter()
            .map(|ex| {
                json!({
                    "id": ex.exit_id,
                    "name": ex.name,
                    "destination": ex.destination,
                    "visible": ex.visible,
                    "blocked_by": ex.blocked_by,
                })
            })
            .collect();
        let present: Vec<String> = scene.present_npcs.iter().cloned().collect();

        let pick = |key: &str, default: Value| -> Value {
            patch.get(key).cloned().unwrap_or(default)
        };

        let result = self.set_scene(
            &as_str(&pick("title", json!(scene.title))),
            &as_str(&pick("description", json!(scene.description))),
            &as_str(&pick("location_id", json!(scene.location_id))),
            &pick("present_npcs", json!(present)),
            &pick("items", json!(items)),
            &pick("exits", json!(exits)),
            &pick("constraints", json!(scene.constraints)),
            &as_str(&pick("tension", json!(scene.tension))),
        );
        self.constraints = self.scene.constraints.clone();
        result
    }

    // =====================================================================
    // World-fact tool (pull pattern)
    // =====================================================================

    /// `fact(query, actor_id)` — honest actor-safe lookup. `retriever` is the
    /// optional RAG path (world.py: `if config.RAG_ENABLED: retrieve_world_fact`).
    /// Pass `None` to use only the offline matcher (RAG disabled or unported).
    pub fn fact(
        &self,
        query: &str,
        actor_id: &str,
        retriever: Option<&dyn RagRetriever>,
    ) -> WorldFact {
        let q = query.to_lowercase();
        if let Some(r) = retriever {
            // RAG is an accuracy layer, not a hard dependency: errors are ignored.
            if let Ok(Some(payload)) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                r.retrieve_world_fact(query, actor_id)
            })) {
                return WorldFact::new(
                    nonempty_or(payload.status.clone(), "unknown"),
                    payload.text.clone(),
                    payload.sources.clone(),
                );
            }
        }

        let mut matches: Vec<String> = Vec::new();
        let q_words = match_words(&q);
        for record in &self.fact_records {
            if record.kind == "truth" {
                continue;
            }
            let mut hay = vec![record.text.clone()];
            hay.extend(record.keywords.iter().cloned());
            let haystack = hay.join(" ").to_lowercase();
            let hay_words = match_words(&haystack);
            if (!q.is_empty() && haystack.contains(&q)) || words_intersect(&q_words, &hay_words) {
                let label = if record.kind == "rumor" || !record.confirmed {
                    "rumor"
                } else {
                    "known"
                };
                matches.push(format!("{label}: {}", record.text));
            }
        }
        let mut query_obj = StateRecordQuery::new(actor_id);
        let kinds = vec!["fact".to_string(), "rumor".to_string()];
        query_obj.kinds = Some(&kinds);
        for record in self.state_records_for(&query_obj) {
            let known_name = get_str(&record.metadata, "known_name");
            let hay = vec![
                record.text.clone(),
                record.tags.join(" "),
                record.owner.clone(),
                record.subject.clone(),
                record.entity_id.clone(),
                record.source_npc.clone(),
                record.location_id.clone(),
                record.location_name.clone(),
                record.region_id.clone(),
                record.region_name.clone(),
                record.scene_id.clone(),
                record.importance.clone(),
                record.aliases.join(" "),
                known_name,
            ];
            let haystack = hay.join(" ").to_lowercase();
            let hay_words = match_words(&haystack);
            if (!q.is_empty() && haystack.contains(&q)) || words_intersect(&q_words, &hay_words) {
                let label = if record.kind == "rumor" {
                    "rumor".to_string()
                } else {
                    record.status.clone()
                };
                matches.push(format!("{label}: {}", record.text));
            }
        }
        if !matches.is_empty() {
            let joined = matches.iter().take(3).cloned().collect::<Vec<_>>().join(" ");
            return WorldFact::new("known", joined, Vec::new());
        }

        let mut rumor_matches: Vec<String> = Vec::new();
        for rumor in &self.rumors {
            let text_words = match_words(&rumor.text);
            if words_intersect(&q_words, &text_words) {
                let speaker_exists = self.npcs.contains_key(&rumor.speaker);
                let name = if speaker_exists {
                    self.npc_player_label(&rumor.speaker, actor_id)
                } else {
                    rumor.speaker.clone()
                };
                rumor_matches.push(format!("{name} said: «{}»", rumor.text));
            }
        }
        if !rumor_matches.is_empty() {
            let last3: Vec<String> = rumor_matches
                .iter()
                .rev()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return WorldFact::new(
                "unknown",
                format!("Unconfirmed statements only: {}", last3.join(" ")),
                Vec::new(),
            );
        }
        WorldFact::new(
            "unknown",
            "Nothing is reliably known about this in town.",
            Vec::new(),
        )
    }
}

/// Optional RAG retrieval seam — gml-world stays decoupled from the (unported)
/// gml-rag crate (subsystem map rustNotes: pass RAG as a trait object so it
/// remains optional and its errors degrade gracefully).
pub trait RagRetriever {
    fn retrieve_world_fact(&self, query: &str, actor_id: &str) -> Option<RetrievedFact>;
}

/// Shape returned by `rag.retrieve_world_fact` (status/text/sources).
#[derive(Clone, Debug)]
pub struct RetrievedFact {
    pub status: String,
    pub text: String,
    pub sources: Vec<Value>,
}

/// Query parameters for `state_records_for` (kwargs in Python).
pub struct StateRecordQuery<'a> {
    pub actor_id: &'a str,
    pub kinds: Option<&'a Vec<String>>,
    pub active: Option<bool>,
    pub owner: &'a str,
    pub subject: &'a str,
    pub entity_id: &'a str,
    pub source_npc: &'a str,
    pub location_id: &'a str,
    pub region_id: &'a str,
    pub scene_id: &'a str,
    pub scopes: Option<&'a Vec<String>>,
}

impl<'a> StateRecordQuery<'a> {
    pub fn new(actor_id: &'a str) -> Self {
        StateRecordQuery {
            actor_id,
            kinds: None,
            active: Some(true),
            owner: "",
            subject: "",
            entity_id: "",
            source_npc: "",
            location_id: "",
            region_id: "",
            scene_id: "",
            scopes: None,
        }
    }
}

// =========================================================================
// Free helper functions
// =========================================================================

fn nonempty_or(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_string()
    } else {
        value
    }
}

fn as_bool_pyish(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn truncate_tail<T>(v: &mut Vec<T>, cap: usize) {
    if v.len() > cap {
        let start = v.len() - cap;
        v.drain(0..start);
    }
}

/// `sum(ord(ch) for ch in s)` — Python uses Unicode code points.
fn ord_sum(s: &str) -> u64 {
    s.chars().map(|c| c as u64).sum()
}

fn words_intersect(a: &std::collections::BTreeSet<String>, b: &std::collections::BTreeSet<String>) -> bool {
    if a.is_empty() {
        return false;
    }
    a.iter().any(|w| b.contains(w))
}

fn first_nonempty(m: &Map<String, Value>, keys: &[&str]) -> String {
    for k in keys {
        if let Some(v) = m.get(*k) {
            let s = as_str(v);
            if !s.is_empty() {
                return s;
            }
        }
    }
    String::new()
}

fn context_value_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Object(o) => o.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

fn profile_empty(v: &Value) -> bool {
    context_value_empty(v)
}

/// Render a Value for `player_character_context` plain-text lines (Python f-string).
fn value_to_plain(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

fn opt_int(v: Option<i64>) -> Value {
    match v {
        Some(i) => json!(i),
        None => Value::Null,
    }
}

// --- seed-time / player-character / scene coercion -----------------------

fn seed_time(raw: Option<&Value>) -> WorldTime {
    let data = match raw {
        Some(Value::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    let minutes_per_hour = as_int_or_none(data.get("minutes_per_hour").unwrap_or(&Value::Null))
        .filter(|v| *v != 0)
        .unwrap_or(60);
    let hours_per_day = as_int_or_none(data.get("hours_per_day").unwrap_or(&Value::Null))
        .filter(|v| *v != 0)
        .unwrap_or(24);
    WorldTime {
        calendar_name: get_str(&data, "calendar_name"),
        absolute_minutes: std::cmp::max(
            0,
            as_int_or_none(data.get("absolute_minutes").unwrap_or(&Value::Null)).unwrap_or(0),
        ),
        current_date_label: nonempty_or(get_str(&data, "current_date_label"), "День 1"),
        minutes_per_hour: std::cmp::max(1, minutes_per_hour),
        hours_per_day: std::cmp::max(1, hours_per_day),
        day_names: as_list(data.get("day_names").unwrap_or(&Value::Null))
            .iter()
            .map(as_str)
            .filter(|s| !s.is_empty())
            .collect(),
        month_names: as_list(data.get("month_names").unwrap_or(&Value::Null))
            .iter()
            .map(as_str)
            .filter(|s| !s.is_empty())
            .collect(),
        last_advance_minutes: std::cmp::max(
            0,
            as_int_or_none(data.get("last_advance_minutes").unwrap_or(&Value::Null)).unwrap_or(0),
        ),
        last_advance_reason: get_str(&data, "last_advance_reason"),
    }
}

fn seed_player_character(raw: Option<&Value>) -> PlayerCharacter {
    let m = match raw {
        Some(Value::Object(m)) => m.clone(),
        _ => return PlayerCharacter::default(),
    };
    let mut pc = PlayerCharacter::default();
    World::apply_player_character_fields(&mut pc, &m);
    pc.card_revision = std::cmp::max(
        0,
        as_int_or_none(m.get("card_revision").unwrap_or(&Value::Null)).unwrap_or(0),
    );
    pc
}

fn coerce_scene_items(raw: Option<&Value>, default_location: &str) -> Vec<SceneItem> {
    let list = match raw {
        Some(v) => as_list(v),
        None => Vec::new(),
    };
    let mut items = Vec::new();
    for (idx, item) in list.iter().enumerate() {
        let i = idx + 1;
        match item {
            Value::Object(m) => {
                let name = nonempty_or(get_str(m, "name"), &format!("предмет {i}"));
                items.push(SceneItem {
                    item_id: safe_id(&get_str(m, "id"), &format!("item_{i}")),
                    name,
                    location: nonempty_or(get_str(m, "location"), default_location),
                    visible: m.get("visible").map(as_bool_pyish).unwrap_or(true),
                    portable: m.get("portable").map(as_bool_pyish).unwrap_or(false),
                    owner: get_str(m, "owner"),
                    details: get_str(m, "details"),
                });
            }
            _ => {
                let name = as_str(item);
                if !name.is_empty() {
                    items.push(SceneItem {
                        item_id: safe_id(&name, &format!("item_{i}")),
                        name,
                        location: default_location.to_string(),
                        visible: true,
                        portable: false,
                        owner: String::new(),
                        details: String::new(),
                    });
                }
            }
        }
    }
    items
}

fn coerce_scene_exits(raw: Option<&Value>, default_dest: &str) -> Vec<SceneExit> {
    let list = match raw {
        Some(v) => as_list(v),
        None => Vec::new(),
    };
    let mut exits = Vec::new();
    for (idx, exit_) in list.iter().enumerate() {
        let i = idx + 1;
        match exit_ {
            Value::Object(m) => {
                let name = nonempty_or(get_str(m, "name"), &format!("выход {i}"));
                exits.push(SceneExit {
                    exit_id: safe_id(&get_str(m, "id"), &format!("exit_{i}")),
                    name,
                    destination: nonempty_or(get_str(m, "destination"), default_dest),
                    visible: m.get("visible").map(as_bool_pyish).unwrap_or(true),
                    blocked_by: get_str(m, "blocked_by"),
                });
            }
            _ => {
                let name = as_str(exit_);
                if !name.is_empty() {
                    exits.push(SceneExit {
                        exit_id: safe_id(&name, &format!("exit_{i}")),
                        name: name.clone(),
                        destination: name,
                        visible: true,
                        blocked_by: String::new(),
                    });
                }
            }
        }
    }
    exits
}

/// set_scene's exit coercion uses a different fallback name for the
/// string-list branch ("unknown destination") and the dict branch
/// ("неизвестное направление") — mirror world.py exactly.
fn coerce_scene_exits_setscene(raw: Option<&Value>) -> Vec<SceneExit> {
    let list = match raw {
        Some(v) => as_list(v),
        None => Vec::new(),
    };
    let mut exits = Vec::new();
    for (idx, exit_) in list.iter().enumerate() {
        let i = idx + 1;
        match exit_ {
            Value::Object(m) => {
                let name = nonempty_or(get_str(m, "name"), &format!("выход {i}"));
                exits.push(SceneExit {
                    exit_id: safe_id(&get_str(m, "id"), &format!("exit_{i}")),
                    name,
                    destination: nonempty_or(get_str(m, "destination"), "неизвестное направление"),
                    visible: m.get("visible").map(as_bool_pyish).unwrap_or(true),
                    blocked_by: get_str(m, "blocked_by"),
                });
            }
            _ => {
                let name = as_str(exit_);
                if !name.is_empty() {
                    exits.push(SceneExit {
                        exit_id: safe_id(&name, &format!("exit_{i}")),
                        name: name.clone(),
                        destination: "unknown destination".to_string(),
                        visible: true,
                        blocked_by: String::new(),
                    });
                }
            }
        }
    }
    exits
}

// --- state-record coercion (module-level, no &self needed) ----------------

fn coerce_state_record(
    raw: &Value,
    fallback_id: &str,
    existing: &BTreeSet<String>,
) -> Option<StateRecord> {
    let data = match raw {
        Value::Object(m) => m.clone(),
        _ => return None,
    };
    let text = get_str(&data, "text");
    if text.is_empty() {
        return None;
    }
    let kind = state_record_kind(&get_str(&data, "kind"));
    let preferred_id = first_nonempty(&data, &["record_id", "id"]);
    let record_id = unique_state_record_id(
        &if preferred_id.is_empty() {
            fallback_id.to_string()
        } else {
            preferred_id
        },
        &kind,
        existing,
    );
    Some(StateRecord {
        record_id,
        kind,
        text,
        scope: state_record_scope(&get_str(&data, "scope")),
        active: state_record::state_record_active(
            data.get("active").unwrap_or(&Value::Null),
            true,
        ),
        owner: first_nonempty(&data, &["owner", "owner_id"]),
        subject: first_nonempty(&data, &["subject", "subject_id"]),
        source: get_str(&data, "source"),
        status: nonempty_or(get_str(&data, "status"), "known"),
        tags: state_record::state_record_tags(data.get("tags").unwrap_or(&Value::Null)),
        entity_id: first_nonempty(&data, &["entity_id", "entity", "about"]),
        source_npc: first_nonempty(&data, &["source_npc", "source_npc_id"]),
        participants: state_record::state_record_participants(
            data.get("participants").unwrap_or(&Value::Null),
        ),
        location_id: get_str(&data, "location_id"),
        location_name: get_str(&data, "location_name"),
        region_id: get_str(&data, "region_id"),
        region_name: get_str(&data, "region_name"),
        scene_id: get_str(&data, "scene_id"),
        importance: get_str(&data, "importance"),
        aliases: state_record::state_record_aliases(data.get("aliases").unwrap_or(&Value::Null)),
        metadata: state_record::state_record_metadata(
            data.get("metadata").unwrap_or(&Value::Null),
        ),
    })
}

fn unique_state_record_id(preferred_id: &str, kind: &str, existing: &BTreeSet<String>) -> String {
    let base = {
        let b = safe_id(preferred_id, "");
        if b.is_empty() {
            format!("{kind}_{}", existing.len() + 1)
        } else {
            b
        }
    };
    let mut record_id = base.clone();
    let mut idx = 2;
    while existing.contains(&record_id) {
        record_id = format!("{base}_{idx}");
        idx += 1;
    }
    record_id
}

// --- entity_refs helpers --------------------------------------------------

fn public_npc_description(npc: &Npc) -> String {
    let mut visible_bits: Vec<String> = Vec::new();
    if !npc.physical_type.is_empty() {
        visible_bits.push(npc.physical_type.clone());
    }
    if !npc.distinctive_features.is_empty() {
        visible_bits.push(npc.distinctive_features.clone());
    }
    if !npc.condition.is_empty() {
        visible_bits.push(npc.condition.clone());
    }
    let text = visible_bits.join(". ");
    if !text.is_empty() {
        return text;
    }
    let role = if !npc.role.is_empty() {
        format!(" Публичная роль: {}.", public_role(&npc.role))
    } else {
        String::new()
    };
    format!(
        "Конкретный персонаж текущего мира.{role} Подробности появятся, когда игрок их узнает."
    )
}

#[allow(clippy::too_many_arguments)]
fn push_entity(
    entities: &mut Vec<Value>,
    kind: &str,
    entity_id: &str,
    label: &str,
    title: &str,
    subtitle: &str,
    description: &str,
    meta: Vec<Value>,
    color: &str,
) {
    let clean_id = entity_id.trim();
    let clean_label = label.trim();
    if clean_id.is_empty() || clean_label.is_empty() {
        return;
    }
    entities.push(json!({
        "key": format!("{kind}:{clean_id}"),
        "kind": kind,
        "id": clean_id,
        "label": clean_label,
        "title": nonempty_or(title.trim().to_string(), clean_label),
        "subtitle": subtitle.trim(),
        "description": description.trim(),
        "color": color.trim(),
        "meta": meta,
    }));
}

fn add_location(
    entities: &mut Vec<Value>,
    seen_locs: &mut BTreeSet<String>,
    location_id: &str,
    label: &str,
    description: &str,
    meta: Vec<Value>,
) {
    let basis = {
        let l = label.trim();
        if !l.is_empty() {
            l.to_string()
        } else {
            location_id.trim().to_string()
        }
    };
    let fallback_id = format!("loc_{}", ord_sum(&basis) % 100000);
    let clean_id = safe_id(location_id, &fallback_id);
    if clean_id.is_empty() {
        return;
    }
    if seen_locs.contains(&clean_id) {
        return;
    }
    seen_locs.insert(clean_id.clone());
    push_entity(
        entities,
        "loc",
        &clean_id,
        label,
        label,
        "локация",
        description,
        meta,
        "",
    );
}

// --- export/serialization helpers ----------------------------------------

fn presence_to_value(p: &Presence) -> Value {
    json!({
        "npc_id": p.npc_id,
        "location": p.location,
        "visible": p.visible,
        "can_hear": p.can_hear,
        "activity": p.activity,
        "attitude": p.attitude,
    })
}

fn scene_item_to_value(i: &SceneItem) -> Value {
    json!({
        "item_id": i.item_id,
        "name": i.name,
        "location": i.location,
        "visible": i.visible,
        "portable": i.portable,
        "owner": i.owner,
        "details": i.details,
    })
}

fn scene_exit_to_value(e: &SceneExit) -> Value {
    json!({
        "exit_id": e.exit_id,
        "name": e.name,
        "destination": e.destination,
        "visible": e.visible,
        "blocked_by": e.blocked_by,
    })
}

fn whereabouts_to_value(w: &NpcWhereabouts) -> Value {
    json!({
        "npc_id": w.npc_id,
        "location_id": w.location_id,
        "location_name": w.location_name,
        "status": w.status,
        "details": w.details,
        "source": w.source,
    })
}

/// `vars(record).copy()` ordering for state_records_export.
fn state_record_to_value(r: &StateRecord) -> Value {
    json!({
        "record_id": r.record_id,
        "kind": r.kind,
        "text": r.text,
        "scope": r.scope,
        "active": r.active,
        "owner": r.owner,
        "subject": r.subject,
        "source": r.source,
        "status": r.status,
        "tags": r.tags,
        "entity_id": r.entity_id,
        "source_npc": r.source_npc,
        "participants": r.participants,
        "location_id": r.location_id,
        "location_name": r.location_name,
        "region_id": r.region_id,
        "region_name": r.region_name,
        "scene_id": r.scene_id,
        "importance": r.importance,
        "aliases": r.aliases,
        "metadata": r.metadata,
    })
}

// --- field get/set by name (player character / npc) ----------------------

fn pc_field_value(pc: &PlayerCharacter, key: &str) -> Value {
    match key {
        "name" => json!(pc.name),
        "pronouns" => json!(pc.pronouns),
        "class_role" => json!(pc.class_role),
        "level" => opt_int(pc.level),
        "background" => json!(pc.background),
        "age" => json!(pc.age),
        "physical_type" => json!(pc.physical_type),
        "distinctive_features" => json!(pc.distinctive_features),
        "life_status" => json!(pc.life_status),
        "life_status_note" => json!(pc.life_status_note),
        "condition" => json!(pc.condition),
        "personality" => json!(pc.personality),
        "values" => json!(pc.values),
        "gm_notes" => json!(pc.gm_notes),
        "abilities" => Value::Object(pc.abilities.clone()),
        "skills" => Value::Object(pc.skills.clone()),
        "saving_throws" => Value::Object(pc.saving_throws.clone()),
        "passive_perception" => opt_int(pc.passive_perception),
        "ac" => pc.ac.clone(),
        "hp" => Value::Object(pc.hp.clone()),
        "speed" => json!(pc.speed),
        "senses" => json!(pc.senses),
        "languages" => json!(pc.languages),
        "inventory" => json!(pc.inventory),
        "equipment" => json!(pc.equipment),
        "features" => json!(pc.features),
        _ => Value::Null,
    }
}

fn set_pc_field(pc: &mut PlayerCharacter, key: &str, value: Value) {
    match key {
        "name" => pc.name = value_as_string(&value),
        "pronouns" => pc.pronouns = value_as_string(&value),
        "class_role" => pc.class_role = value_as_string(&value),
        "level" => pc.level = value.as_i64(),
        "background" => pc.background = value_as_string(&value),
        "age" => pc.age = value_as_string(&value),
        "physical_type" => pc.physical_type = value_as_string(&value),
        "distinctive_features" => pc.distinctive_features = value_as_string(&value),
        "life_status" => pc.life_status = value_as_string(&value),
        "life_status_note" => pc.life_status_note = value_as_string(&value),
        "condition" => pc.condition = value_as_string(&value),
        "personality" => pc.personality = value_as_string(&value),
        "values" => pc.values = value_as_string(&value),
        "gm_notes" => pc.gm_notes = value_as_string(&value),
        "abilities" => pc.abilities = value_as_object(value),
        "skills" => pc.skills = value_as_object(value),
        "saving_throws" => pc.saving_throws = value_as_object(value),
        "passive_perception" => pc.passive_perception = value.as_i64(),
        "ac" => pc.ac = value,
        "hp" => pc.hp = value_as_object(value),
        "speed" => pc.speed = value_as_string(&value),
        "senses" => pc.senses = value_as_string(&value),
        "languages" => pc.languages = value_as_string(&value),
        "inventory" => pc.inventory = value_as_str_vec(value),
        "equipment" => pc.equipment = value_as_str_vec(value),
        "features" => pc.features = value_as_str_vec(value),
        _ => {}
    }
}

fn npc_field_value(npc: &Npc, key: &str) -> Value {
    match key {
        "npc_id" => json!(npc.npc_id),
        "name" => json!(npc.name),
        "persona" => json!(npc.persona),
        "voice" => json!(npc.voice),
        "goals" => json!(npc.goals),
        "knowledge" => json!(npc.knowledge),
        "secret" => json!(npc.secret),
        "role" => json!(npc.role),
        "pronouns" => json!(npc.pronouns),
        "color" => json!(npc.color),
        "public_label" => json!(npc.public_label),
        "age" => json!(npc.age),
        "physical_type" => json!(npc.physical_type),
        "distinctive_features" => json!(npc.distinctive_features),
        "life_status" => json!(npc.life_status),
        "life_status_note" => json!(npc.life_status_note),
        "condition" => json!(npc.condition),
        "personality" => json!(npc.personality),
        "values" => json!(npc.values),
        "habits" => json!(npc.habits),
        "pressure_response" => json!(npc.pressure_response),
        "boundaries" => json!(npc.boundaries),
        "abilities" => Value::Object(npc.abilities.clone()),
        "skills" => Value::Object(npc.skills.clone()),
        "saving_throws" => Value::Object(npc.saving_throws.clone()),
        "passive_perception" => opt_int(npc.passive_perception),
        "ac" => npc.ac.clone(),
        "hp" => Value::Object(npc.hp.clone()),
        "speed" => json!(npc.speed),
        "senses" => json!(npc.senses),
        "languages" => json!(npc.languages),
        _ => Value::Null,
    }
}

fn set_npc_field(npc: &mut Npc, key: &str, value: Value) {
    match key {
        "name" => npc.name = value_as_string(&value),
        "color" => npc.color = value_as_string(&value),
        "role" => npc.role = value_as_string(&value),
        "pronouns" => npc.pronouns = value_as_string(&value),
        "public_label" => npc.public_label = value_as_string(&value),
        "age" => npc.age = value_as_string(&value),
        "physical_type" => npc.physical_type = value_as_string(&value),
        "distinctive_features" => npc.distinctive_features = value_as_string(&value),
        "life_status" => npc.life_status = value_as_string(&value),
        "life_status_note" => npc.life_status_note = value_as_string(&value),
        "condition" => npc.condition = value_as_string(&value),
        "persona" => npc.persona = value_as_string(&value),
        "personality" => npc.personality = value_as_string(&value),
        "values" => npc.values = value_as_string(&value),
        "habits" => npc.habits = value_as_string(&value),
        "pressure_response" => npc.pressure_response = value_as_string(&value),
        "boundaries" => npc.boundaries = value_as_string(&value),
        "voice" => npc.voice = value_as_string(&value),
        "goals" => npc.goals = value_as_string(&value),
        "knowledge" => npc.knowledge = value_as_string(&value),
        "secret" => npc.secret = value_as_string(&value),
        "speed" => npc.speed = value_as_string(&value),
        "senses" => npc.senses = value_as_string(&value),
        "languages" => npc.languages = value_as_string(&value),
        "abilities" => npc.abilities = value_as_object(value),
        "skills" => npc.skills = value_as_object(value),
        "saving_throws" => npc.saving_throws = value_as_object(value),
        "passive_perception" => npc.passive_perception = value.as_i64(),
        "ac" => npc.ac = value,
        "hp" => npc.hp = value_as_object(value),
        _ => {}
    }
}

fn value_as_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn value_as_object(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

fn value_as_str_vec(v: Value) -> Vec<String> {
    match v {
        Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}
