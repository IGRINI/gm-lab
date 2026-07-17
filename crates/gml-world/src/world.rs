//! The `World` aggregate — faithful port of `class World` in world.py.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::canon::{
    canonical_scope, MemoryAccess, MemoryInjectionState, MemoryTier, MemoryTruthStatus, MemoryUnit,
    WorldCanon,
};
use crate::dice;
use crate::helpers::{
    actor_key, anchor_label, as_dict, as_int_or_none, as_joined_str, as_list, as_str, get_str,
    item_entry_string, item_head, item_tail, match_words, safe_id,
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
        "present",
        "known",
        "likely",
        "rumored",
        "unknown",
        "left_scene",
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
pub fn public_role(role: &str) -> String {
    let raw = as_str(&Value::String(role.to_string()));
    role_ru(&raw.to_lowercase())
        .map(|s| s.to_string())
        .unwrap_or(raw)
}

/// `_public_gender(value)`.
pub fn public_gender(value: &str) -> String {
    let raw = value.trim().to_string();
    gender_label_ru(&raw.to_lowercase())
        .map(|s| s.to_string())
        .unwrap_or(raw)
}

/// English grammatical-gender label for model-facing context. Custom notes are
/// preserved verbatim; the localized UI projection above remains unchanged.
pub fn model_gender_label(value: &str) -> String {
    let raw = value.trim();
    match raw.to_lowercase().as_str() {
        "m" => "masculine".to_string(),
        "f" => "feminine".to_string(),
        "n" => "neuter".to_string(),
        "pl" => "plural".to_string(),
        "other" => "other".to_string(),
        _ => raw.to_string(),
    }
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
            "current_appearance",
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
            "current_appearance",
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

/// `PLAYER_CHARACTER_FIELDS` — all patchable player-card fields.
/// quartet (`spells`/`spell_slots`/`spell_slots_max`/`concentration`,
/// `docs/ITEMS_AND_SPELLS_TZ.md` §С1) appended at the end. This drives the
/// full-rewrite loop in `apply_player_character_fields`, so a field absent here
/// is silently un-editable via `update_player_character`.
const PLAYER_CHARACTER_FIELDS: [&str; 31] = [
    "name",
    "pronouns",
    "class_role",
    "level",
    "background",
    "age",
    "physical_type",
    "distinctive_features",
    "current_appearance",
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
    "spells",
    "spell_slots",
    "spell_slots_max",
    "concentration",
];

/// NPC fields the live GM may patch through `update_character`. Private
/// knowledge/secrets and bookkeeping stay outside this surface.
const GM_NPC_CHARACTER_FIELDS: &[&str] = &[
    "name",
    "role",
    "pronouns",
    "public_label",
    "age",
    "physical_type",
    "current_appearance",
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
    "abilities",
    "skills",
    "saving_throws",
    "passive_perception",
    "ac",
    "hp",
    "speed",
    "senses",
    "languages",
];

/// Full debug/authoring NPC edit surface. The GM-facing tool deliberately uses
/// the narrower list above.
const DEBUG_NPC_CHARACTER_FIELDS: &[&str] = &[
    "name",
    "color",
    "role",
    "pronouns",
    "public_label",
    "age",
    "physical_type",
    "distinctive_features",
    "current_appearance",
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
    "abilities",
    "skills",
    "saving_throws",
    "passive_perception",
    "ac",
    "hp",
    "speed",
    "senses",
    "languages",
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
    pub story_brief: String,
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

    /// Canonical world graph (places + transitions) — the living-world layer
    /// (LIVING_WORLD_ARCHITECTURE_TZ.md). Phase 1: derived from the seeded
    /// scene, persisted additively, and projectable back to a `SceneState`, but
    /// not yet the owner of the live scene. Empty for worlds loaded from a
    /// pre-canon save. (Distinct from the legacy `canon: String` hidden-truth
    /// lore text above.)
    pub world_canon: WorldCanon,

    /// True when the cacheable system prefix must be recomputed — set by
    /// `set_public_intro` (the single prefix-mutating world method). Mirrors the
    /// PROMPT-CACHE PREFIX DISCIPLINE invariant for the orchestrator port.
    pub prefix_dirty: bool,

    /// Phase-4 package provenance (`docs/MODS_PACKAGES_TZ.md`): which saved
    /// WORLD package this session was launched from (`{id, version}`). `None`
    /// for sessions that were not launched from a saved world package (e.g. a
    /// catalog story or a brief-seeded chat). Persisted additively — a trailing
    /// `world_ref` key emitted only when `Some`, so pre-Phase-4 saves stay
    /// byte-identical.
    pub world_ref: Option<PackageRef>,
    /// Phase-4 package provenance: which saved STORY package this session was
    /// launched from (`{id, version}`). `None` for procedural-world and catalog
    /// launches. Persisted additively (trailing `story_ref` key when `Some`).
    pub story_ref: Option<PackageRef>,
    /// The world-package `version` the launching STORY was authored against,
    /// recorded at launch time (the story's `StoryWorldRef.version`). `None` when
    /// launched WITHOUT a pinned story ref — a direct saved-world play, a
    /// procedural/brief-seeded chat, or a story whose `world_ref.version` is `0`
    /// (unpinned). When set alongside a `world_ref` whose `version` differs, the
    /// session was launched against a world that has since moved on (version
    /// drift); the launch surfaces a warning but is allowed. Persisted additively
    /// (trailing `world_ref_authored_version` key emitted only when `Some`, so
    /// pre-existing saves stay byte-identical).
    pub world_ref_authored_version: Option<u64>,
    /// K1 CHARACTER package provenance (`docs/CHARACTERS_AND_STORY_TZ.md` §К1.3):
    /// which saved CHARACTER package's player-character was overlaid onto this
    /// session at launch (`{id, version}`). `None` when no character package was
    /// chosen (the story's/default hero is used). Provenance ONLY: the snapshot
    /// inside the save is self-sufficient, so this ref MAY dangle after the
    /// character package is deleted — loads NEVER break on a missing character.
    /// It is the FOURTH ref field. Persisted additively — a trailing `char_ref`
    /// key emitted only when `Some` (right after `world_ref_authored_version`),
    /// so pre-K1 saves stay byte-identical. NOT part of
    /// `player_character_to_payload` (that shape is shared with the character
    /// package and must not carry save-only provenance).
    pub char_ref: Option<PackageRef>,

    /// Phase-И per-place scene-item store (`docs/ITEMS_AND_SPELLS_TZ.md` §И2):
    /// the item bodies that belong to each canon place, keyed by `place_id`.
    /// Fixes the `view.rs` leak where the previous scene's `items` were cloned
    /// into every rebuild, so items "travelled" with the player. On a rebuild
    /// that CHANGES the player's place, [`Self::refresh_scene_from_canon`]
    /// STASHES the leaving place's live `scene.items` here and RESTORES the
    /// entered place's stored items (empty when unvisited). Same-place refreshes
    /// (the common case, including a current-place `set_scene` patch) leave the live items
    /// untouched. Persisted additively — a trailing `place_items` key emitted
    /// ONLY when non-empty (`BTreeMap` for deterministic bytes), parsed with a
    /// default, so pre-Phase-И saves stay byte-identical. Items are NOT canon
    /// entities in this phase (§0): this is a legacy-scene-side store, distinct
    /// from the canon place graph.
    pub place_items: BTreeMap<String, Vec<SceneItem>>,

    /// Per-place storage for live scene fields that are intentionally outside
    /// the canon graph. Without this store, constraints and tension from the
    /// place being left leaked into every subsequently entered place.
    pub place_scene_contexts: BTreeMap<String, PlaceSceneContext>,
}

/// A reference to a saved content package (a world or a story) by its stable id
/// and the package `version` that was launched, so a save is reproducible and
/// linked back to its package (`docs/MODS_PACKAGES_TZ.md` Phase 4).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PackageRef {
    /// The package id (folder name in the library).
    pub id: String,
    /// The package `version` at launch time (0 when unknown).
    pub version: u64,
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
            story_brief: String::new(),
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
            world_canon: WorldCanon::default(),
            prefix_dirty: false,
            world_ref: None,
            story_ref: None,
            world_ref_authored_version: None,
            char_ref: None,
            place_items: BTreeMap::new(),
            place_scene_contexts: BTreeMap::new(),
        }
    }

    fn skeleton(dice_seed: u128) -> Self {
        let mut w = World::empty_with_rng(MersenneTwister::from_u128_seed(dice_seed));
        w.dice_seed = dice_seed;
        w
    }

    /// Procedurally create a campaign from a [`crate::canon::WorldSpec`]
    /// (LOCKED DECISION #4): run worldgen, then DERIVE the legacy-facing `World`
    /// from the resulting canon. The canon is authoritative — `World.npcs`,
    /// `World.scene` and `npc_whereabouts` are all built from it.
    ///
    /// The dice seed is derived from the spec seed when it is a plain integer
    /// (so a numeric procedural seed is fully reproducible incl. the dice RNG),
    /// otherwise a fresh OS-entropy dice seed is used. Use
    /// [`World::from_worldgen_with_dice_seed`] to pin the dice seed in tests.
    pub fn from_worldgen(spec: &crate::canon::WorldSpec) -> Self {
        let dice_seed = spec
            .seed
            .parse::<u128>()
            .unwrap_or_else(|_| Self::new_dice_seed());
        Self::from_worldgen_with_lore_and_dice_seed(spec, None, dice_seed)
    }

    /// [`World::from_worldgen`] with a model-authored top-level world bible.
    pub fn from_worldgen_with_lore(
        spec: &crate::canon::WorldSpec,
        lore: crate::canon::WorldLore,
    ) -> Self {
        let dice_seed = spec
            .seed
            .parse::<u128>()
            .unwrap_or_else(|_| Self::new_dice_seed());
        Self::from_worldgen_with_lore_and_dice_seed(spec, Some(lore), dice_seed)
    }

    /// [`World::from_worldgen`] with an explicit dice seed (deterministic tests).
    pub fn from_worldgen_with_dice_seed(spec: &crate::canon::WorldSpec, dice_seed: u128) -> Self {
        Self::from_worldgen_with_lore_and_dice_seed(spec, None, dice_seed)
    }

    fn from_worldgen_with_lore_and_dice_seed(
        spec: &crate::canon::WorldSpec,
        lore: Option<crate::canon::WorldLore>,
        dice_seed: u128,
    ) -> Self {
        let mut world = World::skeleton(dice_seed);
        world.story_id = "procedural".to_string();
        world.story_title = "Процедурный мир".to_string();
        world.story_brief =
            "Ты начинаешь в живом, сгенерированном мире: рядом уже есть место, люди и первый источник напряжения. Осмотрись, выбери, кому верить, и реши, за какую нитку потянуть первым."
                .to_string();
        world.public =
            "Процедурно созданный мир. Игрок видит место, людей рядом и ближайший конфликт."
                .to_string();
        world.time.current_date_label = "День начала пути".to_string();
        world.time.absolute_minutes = 480;

        // Generate the canon — the source of truth — and derive everything else.
        world.world_canon = crate::canon::worldgen::generate_with_lore(spec, lore);
        world.world_canon.clock_minutes = world.time.absolute_minutes;
        world.derive_legacy_from_canon();
        world
    }

    /// Build the legacy-facing `World` surface (`npcs`, `npc_whereabouts`, the
    /// start `scene`) from `self.world_canon`. Called by [`World::from_worldgen`]
    /// after generation; idempotent.
    fn derive_legacy_from_canon(&mut self) {
        use crate::canon::Containment;

        // One NPC card per canon actor, so ask_npc / get_npc_profile / move_npc
        // work on the procedural roster. The rich card body is synthesised from
        // the actor's structural fields; the bodies stay editable later.
        self.npcs = BTreeMap::new();
        self.npc_whereabouts = BTreeMap::new();
        self.extra_proper_nouns = Vec::new();
        for actor in self.world_canon.actors.values() {
            let name = if actor.public_label.is_empty() {
                actor.actor_id.clone()
            } else {
                actor.public_label.clone()
            };
            let role = if actor.role.is_empty() {
                "персонаж мира".to_string()
            } else {
                actor.role.clone()
            };
            let goals = if actor.goals.is_empty() {
                "Реагировать правдоподобно и защищать свои интересы.".to_string()
            } else {
                actor.goals.join("; ")
            };
            let status = if actor.status.is_empty() {
                "alive".to_string()
            } else {
                actor.status.clone()
            };
            let npc = Npc {
                npc_id: actor.actor_id.clone(),
                name: name.clone(),
                role,
                pronouns: String::new(),
                color: String::new(),
                public_label: actor.public_label.clone(),
                age: String::new(),
                physical_type: String::new(),
                distinctive_features: String::new(),
                current_appearance: String::new(),
                life_status: status,
                life_status_note: String::new(),
                condition: String::new(),
                persona: if actor.agenda.is_empty() {
                    "Житель мира.".to_string()
                } else {
                    actor.agenda.clone()
                },
                personality: String::new(),
                values: String::new(),
                habits: String::new(),
                pressure_response: String::new(),
                boundaries: String::new(),
                voice: "Естественно, кратко, в образе.".to_string(),
                goals,
                knowledge: "Только то, что очевидно в текущей сцене.".to_string(),
                secret: "Личная тайна не задана.".to_string(),
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
            };
            self.npcs.insert(actor.actor_id.clone(), npc);
            if !name.is_empty() && !self.extra_proper_nouns.contains(&name) {
                self.extra_proper_nouns.push(name);
            }
        }
        // The start scene is DERIVED from the canon's player place first, so the
        // whereabouts sync below can read the present-NPC roster from it.
        self.refresh_scene_from_canon();
        self.ensure_npc_whereabouts();
        // Mirror each actor's canonical location into whereabouts so the legacy
        // whereabouts export agrees with the canon (for NPCs not marked present
        // by the scene sync above).
        for actor in self.world_canon.actors.values() {
            if let Containment::Place { place_id } = &actor.location {
                if let Some(wb) = self.npc_whereabouts.get_mut(&actor.actor_id) {
                    if wb.location_id.is_empty() {
                        wb.location_id = place_id.clone();
                    }
                }
            }
        }
        self.constraints = self.scene.constraints.clone();
    }

    /// Phase-4 composition (`docs/MODS_PACKAGES_TZ.md`): build a playable World
    /// from a world bible (`lore`) PLUS an authored plot overlay (`plot`).
    ///
    /// This is the engine-faithful realization of the TZ note
    /// "load(world.json.lore) + наложение story.json.plot": the world is first
    /// generated procedurally from the lore via [`World::from_worldgen_with_lore`]
    /// — so the resulting World carries the world's lore-derived content (the
    /// world `name`/`public_premise`, the procedural canon places, and one canon
    /// actor + NPC card per generated actor) — and then the authored plot is
    /// OVERLAID on top of that world WITHOUT discarding the worldgen canon:
    ///
    /// * `player_character`, `hidden_truth`, `story_brief`, `public_intro` and
    ///   the story `title` from the plot replace the procedural defaults;
    /// * authored `npcs` are merged into the worldgen roster (an authored npc id
    ///   that collides with a generated one overrides it);
    /// * authored `public_facts` / `proper_nouns` / `state_records` are merged;
    /// * the authored starting `scene` is applied via [`World::set_scene`], which
    ///   UPSERTS the scene as a canon place wired into the worldgen graph with a
    ///   transition from the procedural start place — so both the authored start
    ///   AND the rest of the generated world remain reachable in the canon.
    ///
    /// `spec` drives worldgen (seed/genre/tone/scale); a numeric `spec.seed`
    /// makes the generated world reproducible.
    pub fn compose_authored(
        spec: &crate::canon::WorldSpec,
        lore: crate::canon::WorldLore,
        plot: &Value,
    ) -> Self {
        let mut world = World::from_worldgen_with_lore(spec, lore);
        world.overlay_authored_plot(plot);
        world
    }

    /// Overlay an authored plot (a story-seed-shaped object) onto an already
    /// generated/loaded World, preserving the existing world canon. See
    /// [`World::compose_authored`] for the field-by-field contract.
    pub fn overlay_authored_plot(&mut self, plot: &Value) {
        // Story identity (title/brief/public/hidden_truth) and the player
        // character are read from the RAW plot: `normalize_seed` rebuilds a
        // loose seed and folds `title` into the derived scene title (dropping the
        // top-level key), so identity must be captured before normalization. The
        // normalized form is used only for the structural pieces (npcs / scene /
        // facts / state_records) that benefit from its lenient coercion.
        let raw = plot.as_object().cloned().unwrap_or_default();
        let plot = normalize_seed(plot);

        // --- story identity ------------------------------------------------
        let title = get_str(&raw, "title");
        if !title.is_empty() {
            self.set_story_title(&title);
        }
        let story_brief = {
            let v = get_str(&plot, "story_brief");
            if !v.is_empty() {
                v
            } else {
                let v = get_str(&plot, "player_brief");
                if !v.is_empty() {
                    v
                } else {
                    get_str(&plot, "brief")
                }
            }
        };
        if !story_brief.is_empty() {
            self.set_story_brief(&story_brief);
        }
        let public_intro = {
            let v = get_str(&plot, "public_intro");
            if !v.is_empty() {
                v
            } else {
                get_str(&plot, "public")
            }
        };
        if !public_intro.is_empty() {
            self.set_public_intro(&public_intro);
        }

        // --- hidden truth / canon -----------------------------------------
        let hidden_truth = {
            let v = get_str(&plot, "hidden_truth");
            if !v.is_empty() {
                v
            } else {
                get_str(&plot, "canon")
            }
        };
        if !hidden_truth.is_empty() {
            self.set_hidden_truth(&hidden_truth);
        }

        // --- player character (read from RAW: normalize_seed drops it) -----
        let pc_raw = if raw.contains_key("player_character") {
            raw.get("player_character")
        } else {
            raw.get("player")
        };
        if pc_raw.is_some() {
            self.player_character = seed_player_character(pc_raw);
        }

        // --- proper nouns --------------------------------------------------
        for noun in as_list(plot.get("proper_nouns").unwrap_or(&Value::Null))
            .iter()
            .map(as_str)
            .filter(|s| !s.is_empty())
        {
            if !self.extra_proper_nouns.contains(&noun) {
                self.extra_proper_nouns.push(noun);
            }
        }

        // --- authored npcs (merge into the worldgen roster) ---------------
        // `seed_npcs` returns the authored cards (and appends their names to
        // `extra_proper_nouns`); merge them over the generated roster so an
        // authored id overrides a generated one but the rest of the world's
        // actors remain.
        let authored_npcs = self.seed_npcs(&plot);
        // `seed_npcs` returns a single default "stranger" when the plot has no
        // npcs; do not inject it over a populated worldgen roster.
        let plot_has_npcs = !as_list(plot.get("npcs").unwrap_or(&Value::Null)).is_empty();
        if plot_has_npcs {
            for (id, npc) in authored_npcs {
                self.npcs.insert(id, npc);
            }
        }

        // --- authored public facts (merge) --------------------------------
        let authored_facts = self.seed_facts(&plot);
        for fact in authored_facts {
            self.fact_records.retain(|r| r.fact_id != fact.fact_id);
            self.fact_records.push(fact);
        }

        // --- authored state records (merge) -------------------------------
        let authored_state = self.seed_state_records(&plot);
        for record in authored_state {
            self.state_records.push(record.clone());
            self.sync_legacy_state_record_to_memory(&record);
        }

        // --- time (read from RAW: normalize_seed drops it) -----------------
        // The plot's `time` may be a full WorldTime object OR a bare integer of
        // absolute start minutes (the `story.json` shorthand in the TZ).
        match raw.get("time") {
            Some(Value::Object(_)) => {
                self.time = seed_time(raw.get("time"));
                self.world_canon.clock_minutes = self.time.absolute_minutes;
            }
            Some(num @ Value::Number(_)) => {
                if let Some(minutes) = as_int_or_none(num) {
                    self.time.absolute_minutes = std::cmp::max(0, minutes);
                    self.world_canon.clock_minutes = self.time.absolute_minutes;
                }
            }
            _ => {}
        }

        // --- authored starting scene (upsert into the worldgen canon) ------
        // Apply the scene via `set_scene`, which adds the authored place to the
        // existing canon (with a transition from the procedural start place)
        // rather than rebuilding the canon from scratch — keeping BOTH the
        // authored start AND the generated world reachable. Only when the AUTHOR
        // actually supplied a scene (or present npcs) — `normalize_seed` always
        // synthesizes a placeholder scene, which must not clobber the worldgen
        // start scene for a plot that left the scene unspecified.
        // Read the scene from the RAW plot so the author's explicit
        // `location_id` (and other ids) are honored verbatim — `normalize_seed`
        // re-derives the scene location id from the title, which would lose an
        // authored canonical id.
        let authored_scene = raw.contains_key("scene") || raw.contains_key("present_npcs");
        if let (true, Some(Value::Object(scene))) = (authored_scene, raw.get("scene")) {
            let location_id = get_str(scene, "location_id");
            let scene_title = {
                let t = get_str(scene, "title");
                if !t.is_empty() {
                    t
                } else {
                    get_str(scene, "location")
                }
            };
            let description = get_str(scene, "description");
            let present_npcs = scene.get("present_npcs").cloned().unwrap_or(Value::Null);
            let items = scene.get("items").cloned().unwrap_or(Value::Null);
            let exits = scene.get("exits").cloned().unwrap_or(Value::Null);
            let constraints = scene.get("constraints").cloned().unwrap_or(Value::Null);
            let tension = get_str(scene, "tension");
            self.set_initial_scene(
                &scene_title,
                &description,
                &location_id,
                &present_npcs,
                &items,
                &exits,
                &constraints,
                &tension,
            );
        }
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
        self.story_brief = {
            let v = get_str(&seed, "story_brief");
            if !v.is_empty() {
                v
            } else {
                let v = get_str(&seed, "player_brief");
                if !v.is_empty() {
                    v
                } else {
                    let v = get_str(&seed, "brief");
                    if !v.is_empty() {
                        v
                    } else {
                        get_str(&seed, "public_intro")
                    }
                }
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
        // Derive the Phase-1 canonical place graph from the seeded scene. This
        // consumes no RNG (ids are taken verbatim), so deterministic replay is
        // unaffected; the dice_seed is recorded only as provenance.
        self.world_canon = WorldCanon::from_scene(&self.scene, &self.dice_seed.to_string());
        self.world_canon.clock_minutes = self.time.absolute_minutes;
        self.populate_canon_actors();
        self.migrate_legacy_state_records_to_memory();
    }

    /// Populate `world_canon.actors` from the seeded NPC roster so an NPC exists
    /// as a world-side actor even outside the current scene (TZ §6.7). Consumes
    /// ZERO RNG: locations/roles/status are taken verbatim from the existing
    /// scene / whereabouts / NPC cards. One [`crate::canon::Actor`] per NPC, keyed
    /// by `npc_id`. Present NPCs are located at the start place; others sit at
    /// their whereabouts `location_id` when it names a known place, else off-scene
    /// (`OutOfPlay`) so the byte-identical view (occupant_ids) is unaffected.
    fn populate_canon_actors(&mut self) {
        use crate::canon::{Actor, Containment, Provenance};
        let start_place = self.scene.location_id.clone();
        for npc in self.npcs.values() {
            let present = self.scene.present_npcs.contains(&npc.npc_id);
            let wb_loc = self
                .npc_whereabouts
                .get(&npc.npc_id)
                .map(|w| w.location_id.clone())
                .unwrap_or_default();
            let location = if present && self.world_canon.places.contains_key(&start_place) {
                Containment::Place {
                    place_id: start_place.clone(),
                }
            } else if !wb_loc.is_empty() && self.world_canon.places.contains_key(&wb_loc) {
                Containment::Place {
                    place_id: wb_loc.clone(),
                }
            } else {
                Containment::OutOfPlay
            };
            let home = location.place().map(|s| s.to_string()).unwrap_or_default();
            let status = match npc.life_status.as_str() {
                "" => "alive".to_string(),
                other => other.to_string(),
            };
            self.world_canon.actors.insert(
                npc.npc_id.clone(),
                Actor {
                    actor_id: npc.npc_id.clone(),
                    public_label: if npc.public_label.is_empty() {
                        npc.name.clone()
                    } else {
                        npc.public_label.clone()
                    },
                    location,
                    home_place_id: home,
                    role: npc.role.clone(),
                    status,
                    provenance: Provenance::seed(),
                    ..Default::default()
                },
            );
        }
    }

    /// Move legacy `StateRecord` facts into the scoped living-world memory once,
    /// so old seeds/saves stay readable without letting the old flat store feed
    /// prompts or fact lookup directly.
    pub fn migrate_legacy_state_records_to_memory(&mut self) {
        let records = self.state_records.clone();
        for record in records {
            self.sync_legacy_state_record_to_memory(&record);
        }
    }

    fn sync_legacy_state_record_to_memory(&mut self, record: &StateRecord) {
        let _ = self.upsert_state_record_memory(record, "legacy_state_record_migration");
    }

    /// Store a `StateRecord`-shaped fact as scoped living-world memory.
    ///
    /// This keeps old seed/save import compatible while allowing live tools to
    /// stop appending to the legacy flat `state_records` vector.
    pub fn upsert_state_record_memory(
        &mut self,
        record: &StateRecord,
        created_by: &str,
    ) -> Option<String> {
        let normalized_record_id = actor_key(&record.record_id);
        if normalized_record_id.is_empty() {
            return None;
        }
        let existing_id = self
            .world_canon
            .memory
            .units
            .iter()
            .find(|(_, unit)| {
                unit.source_state_record_ids
                    .iter()
                    .any(|id| id == &normalized_record_id)
            })
            .map(|(id, _)| id.clone());

        if !record.active || record.text.trim().is_empty() {
            if let Some(id) = existing_id {
                if let Some(unit) = self.world_canon.memory.units.get_mut(&id) {
                    unit.injection_state = MemoryInjectionState::Archived;
                    unit.time_end = self.world_canon.clock_minutes;
                }
                return Some(id);
            }
            return None;
        }

        let owner_scope = legacy_state_record_owner_scope(record);
        let visibility_scopes = legacy_state_record_visibility_scopes(record, &owner_scope);
        let known_name = get_str(&record.metadata, "known_name");
        let mut metadata = record.metadata.clone();
        metadata.insert("record_id".to_string(), json!(record.record_id.clone()));
        metadata.insert("hash".to_string(), json!(state_record_hash(record)));
        metadata.insert(
            "legacy_kind".to_string(),
            json!(state_record_kind(&record.kind)),
        );
        metadata.insert(
            "legacy_scope".to_string(),
            json!(state_record_scope(&record.scope)),
        );
        metadata.insert("status".to_string(), json!(record.status.clone()));
        if created_by != "legacy_state_record_migration" {
            metadata.insert("source".to_string(), json!(record.source.clone()));
            metadata.insert("tags".to_string(), json!(record.tags.clone()));
        }
        metadata.insert("owner".to_string(), json!(record.owner.clone()));
        metadata.insert("subject".to_string(), json!(record.subject.clone()));
        metadata.insert("source_npc".to_string(), json!(record.source_npc.clone()));
        metadata.insert(
            "participants".to_string(),
            json!(record.participants.clone()),
        );
        metadata.insert("location_id".to_string(), json!(record.location_id.clone()));
        metadata.insert(
            "location_name".to_string(),
            json!(record.location_name.clone()),
        );
        metadata.insert("region_id".to_string(), json!(record.region_id.clone()));
        metadata.insert("region_name".to_string(), json!(record.region_name.clone()));
        metadata.insert("scene_id".to_string(), json!(record.scene_id.clone()));
        metadata.insert("importance".to_string(), json!(record.importance.clone()));
        metadata.insert("aliases".to_string(), json!(record.aliases.clone()));
        if !record.entity_id.trim().is_empty() {
            metadata.insert("entity_id".to_string(), json!(actor_key(&record.entity_id)));
        }
        if !known_name.is_empty() {
            metadata.insert("known_name".to_string(), json!(known_name));
        }
        let memory_id = existing_id.unwrap_or_else(|| {
            let prefix = if created_by == "legacy_state_record_migration" {
                "legacy_state_record"
            } else {
                "world_state"
            };
            format!("{prefix}_{normalized_record_id}")
        });
        let mut unit = MemoryUnit {
            memory_id: memory_id.clone(),
            tier: MemoryTier::Raw,
            owner_scope,
            visibility_scopes,
            summary: record.text.clone(),
            details: record.text.clone(),
            source_state_record_ids: vec![normalized_record_id],
            injection_state: MemoryInjectionState::Hot,
            time_start: self.world_canon.clock_minutes,
            time_end: self.world_canon.clock_minutes,
            place_ids: legacy_state_record_place_ids(record),
            actor_ids: legacy_state_record_actor_ids(record),
            topic_tags: legacy_state_record_topic_tags(record, &known_name),
            metadata,
            truth_status: legacy_state_record_truth_status(record),
            created_by: nonempty_or(created_by.trim().to_string(), "world_state_memory"),
            ..Default::default()
        };
        if unit.visibility_scopes.is_empty()
            && !matches!(unit.owner_scope.as_str(), "gm_private" | "true_canon")
        {
            unit.visibility_scopes.push(unit.owner_scope.clone());
        }
        unit.normalize();
        self.world_canon
            .memory
            .units
            .insert(unit.memory_id.clone(), unit);
        Some(memory_id)
    }

    pub fn archive_state_record_memory(&mut self, record_id: &str) -> bool {
        let normalized_record_id = actor_key(record_id);
        if normalized_record_id.is_empty() {
            return false;
        }
        let mut archived = false;
        for unit in self.world_canon.memory.units.values_mut() {
            if unit
                .source_state_record_ids
                .iter()
                .any(|id| id == &normalized_record_id)
            {
                unit.injection_state = MemoryInjectionState::Archived;
                unit.time_end = self.world_canon.clock_minutes;
                archived = true;
            }
        }
        archived
    }

    fn archive_legacy_state_record_memory(&mut self, record_id: &str) {
        let _ = self.archive_state_record_memory(record_id);
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
                current_appearance: get_str(raw, "current_appearance"),
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
                current_appearance: String::new(),
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
        let mut constraints: Vec<String> = as_list(raw.get("constraints").unwrap_or(&Value::Null))
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
                    let confirmed = m.get("confirmed").map(as_bool_pyish).unwrap_or(true);
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
            if let Some(rec) = coerce_state_record(raw, &format!("seed_state_{i}"), &existing) {
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
        let mut existing: BTreeSet<String> = self
            .state_records
            .iter()
            .map(|r| r.record_id.clone())
            .collect();
        let mut added: Vec<StateRecord> = Vec::new();
        for (idx, raw) in as_list(records).iter().enumerate() {
            let fallback = format!("state_{}", existing.len() + idx + 1);
            if let Some(rec) = coerce_state_record(raw, &fallback, &existing) {
                existing.insert(rec.record_id.clone());
                self.state_records.push(rec.clone());
                self.sync_legacy_state_record_to_memory(&rec);
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
            let pos = match self
                .state_records
                .iter()
                .position(|r| r.record_id == record_id)
            {
                Some(p) => p,
                None => continue,
            };
            let rec = &mut self.state_records[pos];
            apply_state_record_update_map(rec, &m);
            let synced = rec.clone();
            self.sync_legacy_state_record_to_memory(&synced);
            updated.push(synced);
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
            let removed: Vec<String> = self
                .state_records
                .iter()
                .filter(|r| ids.contains(&r.record_id))
                .map(|r| r.record_id.clone())
                .collect();
            self.state_records.retain(|r| !ids.contains(&r.record_id));
            for id in &removed {
                self.archive_legacy_state_record_memory(id);
            }
            return removed.len() as i64;
        }
        let mut count = 0;
        let mut archived = Vec::new();
        for rec in self.state_records.iter_mut() {
            if ids.contains(&rec.record_id) && rec.active {
                rec.active = false;
                archived.push(rec.record_id.clone());
                count += 1;
            }
        }
        for id in &archived {
            self.archive_legacy_state_record_memory(id);
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

    pub fn apply_state_memory_record_batch(
        &mut self,
        add: &Value,
        update: &Value,
        delete: &Value,
        _hard_delete: bool,
    ) -> Value {
        let mut existing: BTreeSet<String> = self
            .world_canon
            .memory
            .units
            .values()
            .filter_map(memory_unit_to_state_record)
            .map(|record| record.record_id)
            .collect();
        let mut added = Vec::new();
        for (idx, raw) in as_list(add).iter().enumerate() {
            let fallback = format!("state_{}", existing.len() + idx + 1);
            let Some(record) = coerce_state_record(raw, &fallback, &existing) else {
                continue;
            };
            existing.insert(record.record_id.clone());
            let _ = self.upsert_state_record_memory(&record, "world_state_memory");
            added.push(record);
        }

        let mut updated = Vec::new();
        for raw in as_list(update) {
            let Value::Object(m) = raw else {
                continue;
            };
            let record_id = {
                let r = get_str(&m, "record_id");
                if r.is_empty() {
                    get_str(&m, "id")
                } else {
                    r
                }
            };
            let Some(mut record) = self
                .world_canon
                .memory
                .units
                .values()
                .filter_map(memory_unit_to_state_record)
                .find(|record| record.record_id == record_id)
            else {
                continue;
            };
            apply_state_record_update_map(&mut record, &m);
            let _ = self.upsert_state_record_memory(&record, "world_state_memory");
            updated.push(record);
        }

        let mut deleted = 0;
        for id in as_list(delete)
            .iter()
            .map(as_str)
            .filter(|id| !id.is_empty())
        {
            if self.archive_state_record_memory(&id) {
                deleted += 1;
            }
        }

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
                let mut v: Vec<String> = vec![
                    state_record_kind(&record.kind),
                    record.owner.clone(),
                    record.subject.clone(),
                    record.entity_id.clone(),
                    record.source_npc.clone(),
                ];
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
                format!(
                    "Memory context: {}. {}",
                    context_bits.join("; "),
                    record.text
                )
            };

            let mut metadata = record.metadata.clone();
            metadata.insert("record_id".to_string(), json!(record.record_id));
            metadata.insert(
                "record_kind".to_string(),
                json!(state_record_kind(&record.kind)),
            );
            metadata.insert(
                "scope".to_string(),
                json!(state_record_scope(&record.scope)),
            );
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

    fn insert_place_scope_chain(&self, place_id: &str, scopes: &mut BTreeSet<String>) {
        let mut current = actor_key(place_id);
        let mut guard = 0usize;
        while !current.is_empty() && guard < 16 {
            guard += 1;
            scopes.insert(format!("place:{current}"));
            let Some(place) = self.world_canon.places.get(&current) else {
                break;
            };
            if !place.region_id.is_empty() {
                scopes.insert(format!("region:{}", actor_key(&place.region_id)));
            }
            if let Some(district) = self.world_canon.district(&place.district_id) {
                scopes.insert(format!("district:{}", actor_key(&district.district_id)));
                if let Some(settlement) = self.world_canon.settlement(&district.settlement_id) {
                    scopes.insert(format!(
                        "settlement:{}",
                        actor_key(&settlement.settlement_id)
                    ));
                    if !settlement.region_id.is_empty() {
                        scopes.insert(format!("region:{}", actor_key(&settlement.region_id)));
                    }
                }
            }
            if place.parent.is_empty() {
                break;
            }
            let parent = actor_key(&place.parent);
            if self.world_canon.districts.contains_key(&parent) {
                break;
            }
            if self.world_canon.settlements.contains_key(&parent) {
                scopes.insert(format!("settlement:{parent}"));
                if let Some(settlement) = self.world_canon.settlements.get(&parent) {
                    if !settlement.region_id.is_empty() {
                        scopes.insert(format!("region:{}", actor_key(&settlement.region_id)));
                    }
                }
                break;
            }
            if parent == current {
                break;
            }
            current = parent;
        }
    }

    fn insert_route_scopes_for_place(&self, place_id: &str, scopes: &mut BTreeSet<String>) {
        let place_id = actor_key(place_id);
        if place_id.is_empty() {
            return;
        }
        for transition in self.world_canon.transitions.values() {
            if actor_key(&transition.from_place) == place_id
                || actor_key(&transition.to_place) == place_id
            {
                scopes.extend(crate::canon::rumor::scopes_for_transition(transition));
            }
        }
    }

    pub fn memory_access_for_player(&self) -> MemoryAccess {
        let mut scopes = BTreeSet::new();
        scopes.insert("player".to_string());
        scopes.insert("actor:player".to_string());
        scopes.insert("public".to_string());
        scopes.insert("legacy_public".to_string());
        MemoryAccess::scoped(scopes)
    }

    pub fn memory_access_for_public(&self) -> MemoryAccess {
        let mut scopes = BTreeSet::new();
        scopes.insert("public".to_string());
        scopes.insert("legacy_public".to_string());
        MemoryAccess::scoped(scopes)
    }

    pub fn memory_access_for_actor(&self, actor_id: &str) -> MemoryAccess {
        let actor_id = actor_key(actor_id);
        if actor_id == "gm" || actor_id == "debug" || actor_id == "system" {
            return MemoryAccess::gm();
        }
        if actor_id == "public" {
            return self.memory_access_for_public();
        }
        if actor_id == "player" || actor_id.is_empty() {
            return self.memory_access_for_player();
        }
        let mut scopes = BTreeSet::new();
        scopes.insert("public".to_string());
        scopes.insert("legacy_public".to_string());
        scopes.insert(format!("actor:{actor_id}"));
        if let Some(actor) = self.world_canon.actors.get(&actor_id) {
            if let Some(place_id) = actor.location.place() {
                self.insert_place_scope_chain(place_id, &mut scopes);
                self.insert_route_scopes_for_place(place_id, &mut scopes);
            }
            if !actor.faction_id.is_empty() {
                scopes.insert(format!("faction:{}", actor_key(&actor.faction_id)));
            }
        }
        for faction in self.world_canon.factions.values() {
            if faction
                .member_ids
                .iter()
                .any(|id| actor_key(id) == actor_id)
            {
                scopes.insert(format!("faction:{}", actor_key(&faction.faction_id)));
            }
        }
        MemoryAccess::scoped(scopes)
    }

    pub fn memory_access_for_place(&self, place_id: &str) -> MemoryAccess {
        let mut scopes = BTreeSet::new();
        scopes.insert("public".to_string());
        scopes.insert("legacy_public".to_string());
        self.insert_place_scope_chain(place_id, &mut scopes);
        self.insert_route_scopes_for_place(place_id, &mut scopes);
        MemoryAccess::scoped(scopes)
    }

    pub fn memory_access_for_scope(&self, scope: &str, id: &str) -> Result<MemoryAccess, String> {
        let scope = canonical_scope(scope, "player");
        match scope.as_str() {
            "gm_private" | "true_canon" | "gm" => Ok(MemoryAccess::gm()),
            "player" => Ok(self.memory_access_for_player()),
            "public" | "legacy_public" => Ok(self.memory_access_for_public()),
            _ if scope.starts_with("actor:") => {
                let actor_id = scope.trim_start_matches("actor:");
                if !self.world_canon.actors.contains_key(actor_id)
                    && !self.npcs.contains_key(actor_id)
                {
                    return Err(format!("unknown actor scope: {actor_id}"));
                }
                Ok(self.memory_access_for_actor(actor_id))
            }
            _ if scope.starts_with("place:") => {
                let place_id = scope.trim_start_matches("place:");
                if !self.world_canon.places.contains_key(place_id) {
                    return Err(format!("unknown place scope: {place_id}"));
                }
                Ok(self.memory_access_for_place(place_id))
            }
            _ if !id.trim().is_empty() => {
                let scoped = canonical_scope(&format!("{scope}:{id}"), "");
                if scoped.starts_with("actor:") {
                    Ok(self.memory_access_for_actor(scoped.trim_start_matches("actor:")))
                } else if scoped.starts_with("place:") {
                    Ok(self.memory_access_for_place(scoped.trim_start_matches("place:")))
                } else {
                    let mut scopes = BTreeSet::new();
                    scopes.insert("public".to_string());
                    scopes.insert("legacy_public".to_string());
                    scopes.insert(scoped);
                    Ok(MemoryAccess::scoped(scopes))
                }
            }
            _ => Err(format!("unsupported memory scope: {scope}")),
        }
    }

    pub fn add_memory_unit(&mut self, unit: MemoryUnit) -> String {
        let seed = if self.world_canon.world_seed.is_empty() {
            self.dice_seed.to_string()
        } else {
            self.world_canon.world_seed.clone()
        };
        self.world_canon.memory.insert(unit, &seed)
    }

    pub fn consolidate_memory_unit(&mut self, mut unit: MemoryUnit) -> (String, Vec<String>) {
        if unit.tier == MemoryTier::Raw {
            unit.tier = MemoryTier::Episode;
        }
        let source_ids = unit.source_memory_ids.clone();
        let id = self.add_memory_unit(unit);
        let consumed = self.world_canon.memory.mark_consumed(&source_ids, &id);
        (id, consumed)
    }

    pub fn auto_consolidate_memory(&mut self) -> Vec<String> {
        let seed = if self.world_canon.world_seed.is_empty() {
            self.dice_seed.to_string()
        } else {
            self.world_canon.world_seed.clone()
        };
        self.world_canon.memory.auto_consolidate(&seed)
    }

    pub fn memory_rows_for_access(
        &self,
        access: &MemoryAccess,
        query: &str,
        limit: usize,
        include_cold: bool,
        include_details: bool,
    ) -> Vec<Value> {
        self.world_canon
            .memory
            .query_with_details(access, query, limit, include_cold, include_details)
            .into_iter()
            .map(|unit| unit.to_row(include_details))
            .collect()
    }

    pub fn memory_documents(&self, actor_id: &str) -> Vec<RagDocument> {
        let access = self.memory_access_for_actor(actor_id);
        self.memory_documents_for_access(&access, false, false)
    }

    pub fn memory_documents_for_access(
        &self,
        access: &MemoryAccess,
        include_cold: bool,
        include_details: bool,
    ) -> Vec<RagDocument> {
        self.world_canon
            .memory
            .query(access, "", usize::MAX, include_cold)
            .into_iter()
            .map(|unit| unit.to_rag_document_with_details(include_details))
            .collect()
    }

    pub fn npc_memory_brief(&self, npc_id: &str, query: &str, limit: usize) -> String {
        let access = self.memory_access_for_actor(npc_id);
        let rows = self
            .world_canon
            .memory
            .query_with_details(&access, query, limit, false, false);
        if rows.is_empty() {
            return String::new();
        }
        let lines: Vec<String> = rows
            .into_iter()
            .map(|unit| {
                format!(
                    "- [{}; {}; {}] {}",
                    unit.memory_id,
                    unit.tier.as_str(),
                    unit.truth_status.as_str(),
                    unit.summary
                )
            })
            .collect();
        format!(
            "Scoped memory recall (short summaries only; actor-visible):\n{}",
            lines.join("\n")
        )
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

    pub fn state_memory_records_export(&self, query: &StateRecordQuery) -> Vec<Value> {
        let mut out = Vec::new();
        for unit in self.world_canon.memory.units.values() {
            let Some(record) = memory_unit_to_state_record(unit) else {
                continue;
            };
            if !state_record_matches_query(&record, query) {
                continue;
            }
            let mut row = state_record_to_value(&record);
            if let Value::Object(ref mut m) = row {
                let hash = memory_meta_string(unit, "hash");
                m.insert(
                    "hash".to_string(),
                    json!(if hash.is_empty() {
                        state_record_hash(&record)
                    } else {
                        hash
                    }),
                );
                m.insert("memory_id".to_string(), json!(unit.memory_id.clone()));
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
        let access = self.memory_access_for_actor(actor_id);
        let mut memory_matches = self
            .world_canon
            .memory
            .units
            .values()
            .filter(|unit| unit.is_visible_to(&access))
            .filter_map(|unit| {
                let entity_id = unit
                    .metadata
                    .get("entity_id")
                    .and_then(Value::as_str)
                    .map(actor_key)
                    .unwrap_or_default();
                if entity_id != clean_id {
                    return None;
                }
                let known_name = unit
                    .metadata
                    .get("known_name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if known_name.is_empty() {
                    None
                } else {
                    Some((
                        unit.time_end,
                        unit.memory_id.clone(),
                        known_name.to_string(),
                    ))
                }
            })
            .collect::<Vec<_>>();
        memory_matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        if let Some((_, _, known_name)) = memory_matches.into_iter().next() {
            return known_name;
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

    /// Shared internal-roster line:
    /// `- id=…; internal_name=…; player_label=…; role=…[; gender=…]`.
    fn roster_line(&self, npc: &Npc) -> String {
        let mut line = format!(
            "- id={}; internal_name={}; player_label={}; role={}",
            npc.npc_id,
            npc.name,
            self.npc_player_label(&npc.npc_id, "player"),
            npc.role
        );
        if !npc.pronouns.is_empty() {
            line.push_str(&format!("; gender={}", model_gender_label(&npc.pronouns)));
        }
        line
    }

    /// Full internal NPC roster (every card). Used by `read_state(roster)` for
    /// the complete list. Pure read — consumes no RNG and mutates nothing.
    pub fn full_roster_context(&self) -> String {
        let lines: Vec<String> = self
            .npcs
            .values()
            .map(|npc| self.roster_line(npc))
            .collect();
        if lines.is_empty() {
            "(none)".to_string()
        } else {
            lines.join("\n")
        }
    }

    /// Dynamic, deterministically-filtered NPC roster for the world snapshot and
    /// `read_state(scene)`. Includes NPCs that are (1) present in the current
    /// scene, (2) located at the player's place or one transition away, (3) alive
    /// story-seed NPCs (canon `provenance.origin == "seed"`), or (4) recently
    /// contacted (`recent_contact_ids`, kept on the session). Deduped, capped at
    /// 15 lines with an offscreen-count note pointing at `read_state(roster)`.
    /// Consumes ZERO dice RNG and mutates nothing.
    pub fn dynamic_roster_context(&self, recent_contact_ids: &BTreeSet<String>) -> String {
        // Priority-ORDERED selection: when the cap truncates, the NPCs the GM is
        // most likely to reference next must survive — present first, then
        // recently contacted, then nearby, then story-seed. (A plain set would
        // truncate by lexicographic id instead.)
        let mut ordered: Vec<String> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        // (1) present in the current scene.
        for id in &self.scene.present_npcs {
            if self.npcs.contains_key(id) && seen.insert(id.clone()) {
                ordered.push(id.clone());
            }
        }
        // (2) recently contacted (session-tracked).
        for id in recent_contact_ids {
            if self.npcs.contains_key(id) && seen.insert(id.clone()) {
                ordered.push(id.clone());
            }
        }
        // (3) at the player's place or one transition away.
        let player_place = self.world_canon.player_place_id.clone();
        if !player_place.is_empty() {
            let mut nearby_places: BTreeSet<String> = BTreeSet::new();
            nearby_places.insert(player_place.clone());
            for t in self.world_canon.exits_from(&player_place) {
                if !t.to_place.is_empty() {
                    nearby_places.insert(t.to_place.clone());
                }
            }
            for place in &nearby_places {
                for actor in self.world_canon.actors_at(place) {
                    let id = actor.actor_id.clone();
                    if self.npcs.contains_key(&id) && seen.insert(id.clone()) {
                        ordered.push(id);
                    }
                }
            }
        }
        // (4) story-seed NPCs still in play (engine convention: only "dead"
        // removes an actor from play — wounded/missing statuses stay listed).
        for npc in self.npcs.values() {
            let is_seed = self
                .world_canon
                .actor(&npc.npc_id)
                .map(|a| a.provenance.origin == "seed")
                .unwrap_or(false);
            let alive = npc.life_status != "dead";
            if is_seed && alive && seen.insert(npc.npc_id.clone()) {
                ordered.push(npc.npc_id.clone());
            }
        }

        if ordered.is_empty() {
            return "(none)".to_string();
        }
        let cap = 15usize;
        let total = ordered.len();
        let mut lines: Vec<String> = Vec::new();
        for id in ordered.iter().take(cap) {
            if let Some(npc) = self.npcs.get(id) {
                lines.push(self.roster_line(npc));
            }
        }
        if total > cap {
            let offscreen = total - cap;
            lines.push(format!(
                "… (+{offscreen} offscreen — read_state(roster) for the full list)"
            ));
        }
        lines.join("\n")
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
        self.downgrade_stale_present_whereabouts();
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

    fn downgrade_stale_present_whereabouts(&mut self) {
        let current_scene_npcs = self.scene.present_npcs.clone();
        for (npc_id, row) in self.npc_whereabouts.iter_mut() {
            if row.status != "present" || current_scene_npcs.contains(npc_id) {
                continue;
            }
            row.status = if row.location_id.is_empty()
                && row.location_name.is_empty()
                && row.details.is_empty()
            {
                "unknown".to_string()
            } else {
                "known".to_string()
            };
            if row.source.trim().is_empty() || row.source == "current scene" {
                row.source = "stale current scene".to_string();
            }
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
        docs.extend(self.memory_documents(actor_id));

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
            "Current scene: {}. {} Present named NPCs: {}. Exits: {}.",
            self.scene.title,
            self.scene.description,
            if present_labels.is_empty() {
                "none".to_string()
            } else {
                present_labels.join(", ")
            },
            if exits_text.is_empty() {
                "none known".to_string()
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
            let mut text = format!(
                "Visible item in the current scene: {}; location: {}.",
                item.name, item.location
            );
            if !item.details.is_empty() {
                text.push_str(&format!(" Details: {}.", item.details));
            }
            if !item.owner.is_empty() {
                text.push_str(&format!(" Owner: {}.", item.owner));
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
                appearance.push_str(&format!(" Type/visible impression: {}.", npc.physical_type));
            }
            if !npc.distinctive_features.is_empty() {
                appearance.push_str(&format!(
                    " Distinctive features: {}.",
                    npc.distinctive_features
                ));
            }
            if !npc.current_appearance.is_empty() {
                appearance.push_str(&format!(" Current appearance: {}.", npc.current_appearance));
            }
            let gender_part = if !npc.pronouns.is_empty() {
                format!(
                    " Gender: {} ({}).",
                    model_gender_label(&npc.pronouns),
                    npc.pronouns
                )
            } else {
                String::new()
            };
            let mut metadata = Map::new();
            metadata.insert("npc_id".to_string(), json!(npc_id));
            metadata.insert("known_name".to_string(), json!(known_name));
            docs.push(RagDocument::new(
                format!("npc_public:{npc_id}"),
                "npc_public".to_string(),
                format!(
                    "{label} ({npc_id}) — {}.{gender_part}{appearance}",
                    npc.role
                ),
                "known".to_string(),
                "npc_roster".to_string(),
                "player".to_string(),
                vec![
                    npc_id.clone(),
                    label.clone(),
                    npc.role.clone(),
                    npc.pronouns.clone(),
                    npc.physical_type.clone(),
                    npc.current_appearance.clone(),
                ],
                metadata,
            ));

            let where_row = self.npc_whereabouts.get(npc_id).cloned();
            if let Some(w) = where_row {
                if w.status != "unknown" {
                    let present_text = if self.scene.present_npcs.contains(npc_id) {
                        "present in the current scene"
                    } else {
                        "not in the current scene"
                    };
                    let where_label = if !w.location_name.is_empty() {
                        w.location_name.clone()
                    } else if !w.location_id.is_empty() {
                        w.location_id.clone()
                    } else {
                        "unknown".to_string()
                    };
                    let details_part = if !w.details.is_empty() {
                        format!(" Details: {}.", w.details)
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
                            "{label} is currently {present_text}. Whereabouts status: {}. Where to look: {where_label}.{details_part}",
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
            if !npc.distinctive_features.is_empty() {
                detail.push_str(&format!(
                    ", distinctive features: {}",
                    npc.distinctive_features
                ));
            }
            if !npc.current_appearance.is_empty() {
                detail.push_str(&format!(", current appearance: {}", npc.current_appearance));
            }
            if !npc.condition.is_empty() {
                detail.push_str(&format!(", condition: {}", npc.condition));
            }
            if !npc.pronouns.is_empty() {
                detail.push_str(&format!(", gender: {}", model_gender_label(&npc.pronouns)));
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
        // Exits are rendered FROM the canon so the GM sees the real
        // `transition_id` it must pass to `move_player` (the canonical travel
        // mechanism). Falls back to the legacy scene exits only when the player
        // has no canonical place (a pre-canon save).
        let mut exits: Vec<String> = Vec::new();
        let here = &self.world_canon.player_place_id;
        if !here.is_empty() && self.world_canon.place(here).is_some() {
            for t in self.world_canon.exits_from(here) {
                if !t.visible {
                    continue;
                }
                let dest = if t.to_place.is_empty() {
                    t.destination_hint.clone()
                } else {
                    self.world_canon
                        .place(&t.to_place)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| t.to_place.clone())
                };
                let mut line = format!(
                    "{} -> {} [move_player transition_id={}; travel_minutes={}; road_risk={}]",
                    t.label, dest, t.transition_id, t.time_cost, t.risk
                );
                if !t.blocked_by.is_empty() {
                    line.push_str(&format!(" (blocked by {})", t.blocked_by));
                }
                exits.push(line);
            }
        } else {
            for exit_ in self.scene.visible_exits() {
                let mut line = format!("{} -> {}", exit_.name, exit_.destination);
                if !exit_.blocked_by.is_empty() {
                    line.push_str(&format!(" (blocked by {})", exit_.blocked_by));
                }
                exits.push(line);
            }
        }
        let mut parts = vec![
            format!("Scene: {}", self.scene.title),
            format!("Location: {}", self.scene.location_id),
            format!("World time: {}", self.model_time_summary()),
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
        let visible_exit_names: Vec<String> = self
            .scene
            .visible_exits()
            .iter()
            .map(|e| e.name.clone())
            .collect();
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
                label.push_str(&format!(
                    "; gender: {}",
                    model_gender_label(&other.pronouns)
                ));
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
                    format!(
                        " ({} = {})",
                        npc.pronouns,
                        model_gender_label(&npc.pronouns)
                    )
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

    /// Set a canon actor's physical location (creating a bridging actor for a
    /// legacy npc if it has none yet), keeping place occupant sets consistent.
    /// `place = None` means offscreen (`OutOfPlay`). This is how legacy presence
    /// mutators write through to the canonical source of truth.
    fn set_canon_actor_location(&mut self, npc_id: &str, place: Option<&str>) {
        use crate::canon::{Actor, Containment};
        if let Some(cur) = self
            .world_canon
            .actors
            .get(npc_id)
            .and_then(|a| a.location.place().map(|s| s.to_string()))
        {
            if let Some(p) = self.world_canon.places.get_mut(&cur) {
                p.occupant_ids.remove(npc_id);
            }
        }
        let containment = match place {
            Some(pid) if self.world_canon.places.contains_key(pid) => Containment::Place {
                place_id: pid.to_string(),
            },
            _ => Containment::OutOfPlay,
        };
        if let Some(a) = self.world_canon.actors.get_mut(npc_id) {
            a.location = containment;
        } else {
            let (label, role, status) = self
                .npcs
                .get(npc_id)
                .map(|n| {
                    (
                        n.public_label.clone(),
                        n.role.clone(),
                        n.life_status.clone(),
                    )
                })
                .unwrap_or_default();
            self.world_canon.actors.insert(
                npc_id.to_string(),
                Actor {
                    actor_id: npc_id.to_string(),
                    public_label: label,
                    location: containment,
                    role,
                    status: nonempty_or(status, "alive"),
                    ..Default::default()
                },
            );
        }
        if let Some(pid) = place {
            if let Some(p) = self.world_canon.places.get_mut(pid) {
                p.occupant_ids.insert(npc_id.to_string());
            }
        }
    }

    /// Reconcile one npc's canon actor with the resulting legacy presence /
    /// whereabouts so the canon stays authoritative after a legacy mutation
    /// (`move_npc`, `set_npc_whereabouts`). Present ⇒ at the player's place;
    /// otherwise at the canon place matching its recorded whereabouts, else
    /// offscreen.
    fn sync_canon_actor(&mut self, npc_id: &str) {
        if self.world_canon.player_place_id.is_empty() {
            return;
        }
        let target: Option<String> = if self.scene.present_npcs.contains(npc_id) {
            Some(self.world_canon.player_place_id.clone())
        } else {
            self.npc_whereabouts
                .get(npc_id)
                .map(|w| w.location_id.clone())
                .filter(|lid| !lid.is_empty() && self.world_canon.places.contains_key(lid))
        };
        self.set_canon_actor_location(npc_id, target.as_deref());
    }

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
                        &old.as_ref()
                            .map(|o| o.location.clone())
                            .unwrap_or_else(|| "in the scene".to_string()),
                    ),
                    visible,
                    can_hear,
                    activity: nonempty_or(
                        activity.trim().to_string(),
                        &old.as_ref()
                            .map(|o| o.activity.clone())
                            .unwrap_or_else(|| format!("present as {role}")),
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
        // Write the move through to the canon, then rebuild the live scene from
        // the canon so the change is the single source of truth (it would
        // otherwise be overwritten by the next refresh_scene_from_canon).
        self.sync_canon_actor(&resolved_id);
        self.refresh_scene_from_canon();
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

    /// Updates authored content for the current canonical place.
    ///
    /// Runtime location changes must go through the orchestrator's structured
    /// transition flow. This legacy API deliberately rejects a different
    /// `location_id`, so debug edits and old callers cannot create a route or
    /// teleport the player around the location creator.
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
        self.set_scene_inner(
            title,
            description,
            location_id,
            present_npcs,
            items,
            exits,
            constraints,
            tension,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn set_initial_scene(
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
        self.set_scene_inner(
            title,
            description,
            location_id,
            present_npcs,
            items,
            exits,
            constraints,
            tension,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn set_scene_inner(
        &mut self,
        title: &str,
        description: &str,
        location_id: &str,
        present_npcs: &Value,
        items: &Value,
        exits: &Value,
        constraints: &Value,
        tension: &str,
        allow_initial_relocation: bool,
    ) -> Value {
        use crate::canon::{Containment, Provenance, Transition};

        let title = nonempty_or(title.trim().to_string(), "Новая сцена");
        let description = nonempty_or(description.trim().to_string(), &title);
        let fallback_id = format!("scene_{}", ord_sum(&title) % 100000);
        let dest_id = safe_id(
            &nonempty_or(location_id.trim().to_string(), &title),
            &fallback_id,
        );

        // World seed for canon provenance/id derivation. If the canon is empty
        // (a pre-canon save), seed it from the current scene first so set_scene
        // becomes canon-authoritative from here on.
        if self.world_canon.is_empty() {
            let seed = self.dice_seed.to_string();
            self.world_canon = crate::canon::WorldCanon::from_scene(&self.scene, &seed);
        }
        let turn = self.world_canon.event_log.events.len() as i64;
        let from_place = self.world_canon.player_place_id.clone();
        if !allow_initial_relocation && !from_place.is_empty() && from_place != dest_id {
            return json!({
                "ok": false,
                "error": "set_scene cannot change location; use move_player or generate_location",
                "code": "location_change_requires_transition",
                "current_location_id": from_place,
                "requested_location_id": dest_id,
            });
        }
        self.ensure_npc_whereabouts();

        // Resolve present_npcs to known npc ids; collect unknowns to report back.
        let mut present: BTreeSet<String> = BTreeSet::new();
        let mut dropped_present_npcs: Vec<String> = Vec::new();
        for raw_id in as_list(present_npcs) {
            let npc_id = safe_id(&as_str(&raw_id), "");
            if npc_id.is_empty() || !self.npcs.contains_key(&npc_id) {
                let raw_label = as_str(&raw_id);
                if !raw_label.is_empty() {
                    dropped_present_npcs.push(raw_label);
                }
                continue;
            }
            present.insert(npc_id);
        }

        // --- upsert the destination Place ---------------------------------
        let coerced_items = coerce_scene_items(Some(items), "в сцене");
        let item_ids: Vec<String> = coerced_items.iter().map(|i| i.item_id.clone()).collect();
        match self.world_canon.places.get_mut(&dest_id) {
            Some(p) => {
                // Existing place: update its player-facing structural fields.
                p.name = title.clone();
                p.default_description = description.clone();
                p.mark_visited();
                p.item_ids = item_ids.clone();
            }
            None => {
                let mut flags = BTreeSet::new();
                flags.insert("visited".to_string());
                self.world_canon.insert_place(crate::canon::Place {
                    place_id: dest_id.clone(),
                    name: title.clone(),
                    kind: "scene".to_string(),
                    parent: String::new(),
                    region_id: String::new(),
                    district_id: String::new(),
                    default_description: description.clone(),
                    state_flags: flags,
                    features: Vec::new(),
                    transition_ids: Vec::new(),
                    occupant_ids: BTreeSet::new(),
                    item_ids: item_ids.clone(),
                    event_ids: Vec::new(),
                    fact_ids: Vec::new(),
                    provenance: Provenance::by("llm", "set_scene authored place", turn),
                });
            }
        }

        // Seed hydration may establish unresolved exit shells. Runtime scene
        // patches preserve canon exits; the dedicated location creator owns
        // every new route and its profile.
        if allow_initial_relocation {
            for exit in coerce_initial_scene_exits(Some(exits)) {
                let base = if exit.exit_id.is_empty() {
                    safe_id(&exit.name, "exit")
                } else {
                    exit.exit_id.clone()
                };
                let mut tid = format!("{dest_id}_{base}");
                let mut n = 2;
                while self.world_canon.transitions.contains_key(&tid) {
                    tid = format!("{dest_id}_{base}_{n}");
                    n += 1;
                }
                self.world_canon.insert_transition(Transition {
                    transition_id: tid.clone(),
                    source_exit_id: exit.exit_id.clone(),
                    passage_id: String::new(),
                    directionality: crate::canon::PassageDirectionality::Unspecified,
                    from_place: dest_id.clone(),
                    to_place: String::new(),
                    destination_hint: exit.destination.clone(),
                    label: exit.name.clone(),
                    kind: String::new(),
                    visible: exit.visible,
                    passable: exit.blocked_by.is_empty(),
                    conditions: Vec::new(),
                    blocked_by: exit.blocked_by.clone(),
                    time_cost: 0,
                    risk: String::new(),
                    provenance: Provenance::by("llm", "initial scene exit", turn),
                });
            }
        }

        // Seed hydration establishes the initial position; runtime callers can
        // only update the place the player already occupies.
        self.world_canon.player_place_id = dest_id.clone();

        // --- mirror present NPCs as canon actors AT the destination -------
        // The derived `present_npcs` is `actors_at(place)`, so each present NPC
        // must be a living actor located here; anyone previously here but no
        // longer listed is moved out (offscreen) and recorded in whereabouts.
        let previously_here: Vec<String> = self
            .world_canon
            .actors_at(&dest_id)
            .into_iter()
            .map(|a| a.actor_id.clone())
            .collect();
        for npc_id in &present {
            match self.world_canon.actors.get_mut(npc_id) {
                Some(a) => {
                    // Detach from any other place occupant set, then place here.
                    if let Some(old) = a.location.place().map(str::to_string) {
                        if old != dest_id {
                            if let Some(op) = self.world_canon.places.get_mut(&old) {
                                op.occupant_ids.remove(npc_id);
                            }
                        }
                    }
                    if let Some(a) = self.world_canon.actors.get_mut(npc_id) {
                        a.location = Containment::Place {
                            place_id: dest_id.clone(),
                        };
                        if a.status.is_empty() || a.status == "dead" {
                            a.status = "alive".to_string();
                        }
                    }
                }
                None => {
                    let npc = &self.npcs[npc_id];
                    self.world_canon.actors.insert(
                        npc_id.clone(),
                        crate::canon::Actor {
                            actor_id: npc_id.clone(),
                            public_label: npc.name.clone(),
                            location: Containment::Place {
                                place_id: dest_id.clone(),
                            },
                            home_place_id: dest_id.clone(),
                            role: npc.role.clone(),
                            status: "alive".to_string(),
                            provenance: Provenance::by("llm", "set_scene present npc", turn),
                            ..Default::default()
                        },
                    );
                }
            }
            if let Some(p) = self.world_canon.places.get_mut(&dest_id) {
                p.occupant_ids.insert(npc_id.clone());
            }
        }
        // Anyone here before but dropped from present_npcs leaves the place.
        for gone in previously_here.iter().filter(|id| !present.contains(*id)) {
            if let Some(a) = self.world_canon.actors.get_mut(gone) {
                a.location = Containment::OutOfPlay;
            }
            if let Some(p) = self.world_canon.places.get_mut(&dest_id) {
                p.occupant_ids.remove(gone);
            }
            if self.npcs.contains_key(gone) {
                self.npc_whereabouts.insert(
                    gone.clone(),
                    NpcWhereabouts {
                        npc_id: gone.clone(),
                        location_id: dest_id.clone(),
                        location_name: title.clone(),
                        status: "unknown".to_string(),
                        details: "покинул сцену".to_string(),
                        source: "set_scene".to_string(),
                    },
                );
            }
        }

        // --- record the current-scene update in the canon event log --------
        {
            let mut effects = vec![
                format!(
                    "{}:{dest_id}",
                    if allow_initial_relocation {
                        "initialized_place"
                    } else {
                        "updated_place"
                    }
                ),
                format!("player_at:{dest_id}"),
            ];
            if !present.is_empty() {
                effects.push(format!(
                    "present:{}",
                    present.iter().cloned().collect::<Vec<_>>().join(",")
                ));
            }
            let event_id = crate::canon::ids::stable_id(
                &self.world_canon.world_seed,
                "set_scene",
                "event",
                &format!("{turn}:{}", self.world_canon.event_log.events.len()),
            );
            self.world_canon.event_log.append(crate::canon::CanonEvent {
                event_id,
                seq: 0,
                kind: "set_scene".to_string(),
                time_minutes: self.world_canon.clock_minutes,
                time_label: String::new(),
                place_id: dest_id.clone(),
                actors: present.iter().cloned().collect(),
                causes: Vec::new(),
                effects,
                visible_to_player: true,
                scope: crate::canon::Scope::Player,
                possible_traces: Vec::new(),
                scheduled: false,
                due_minutes: 0,
                provenance: Provenance::by(
                    "llm",
                    if allow_initial_relocation {
                        "initial scene hydration"
                    } else {
                        "set_scene updated current place"
                    },
                    turn,
                ),
            });
        }

        // --- carry ephemeral view state, then rebuild scene from canon ----
        self.scene.scene_id = dest_id.clone();
        // §И2: pin the live location to the destination BEFORE staging its items
        // so refresh_scene_from_canon reads a same-place rebuild and keeps these
        // staged items (rather than stashing them under the old id and loading an
        // empty store). Mirrors run_set_scene in the orchestrator.
        self.scene.location_id = dest_id.clone();
        self.scene.items = coerced_items;
        self.scene.constraints = as_list(constraints)
            .iter()
            .map(as_str)
            .filter(|s| !s.is_empty())
            .collect();
        self.scene.tension = tension.trim().to_string();
        self.scene.player_seen = vec![description];

        self.refresh_scene_from_canon();
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
        // Reflect the offscreen whereabouts onto the canon actor + rebuild scene.
        self.sync_canon_actor(&resolved_id);
        self.refresh_scene_from_canon();
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
        let place_id = if !self.world_canon.player_place_id.is_empty() {
            self.world_canon.player_place_id.clone()
        } else {
            self.scene.location_id.clone()
        };
        let present_npcs = self.scene.present_npcs.clone();
        let audible_to_place = !present_npcs.is_empty()
            && witnesses.contains("player")
            && present_npcs.iter().all(|npc_id| witnesses.contains(npc_id))
            && (present_npcs.len() > 1 || witnesses.len() > 2);
        let mut known_in = BTreeSet::new();
        if audible_to_place && !place_id.is_empty() {
            known_in.insert(format!("place:{place_id}"));
        }
        if speaker.trim() == "player" {
            known_in.insert("player".to_string());
        } else if !speaker.trim().is_empty() {
            known_in.insert(format!("actor:{}", actor_key(speaker)));
        }
        for witness in &witnesses {
            if witness == "player" {
                known_in.insert("player".to_string());
            }
            known_in.insert(format!("actor:{}", actor_key(witness)));
        }
        let origin_scope = if audible_to_place && !place_id.is_empty() {
            format!("place:{place_id}")
        } else if speaker.trim().is_empty() {
            "public".to_string()
        } else if speaker.trim() == "player" {
            "player".to_string()
        } else {
            format!("actor:{}", actor_key(speaker))
        };
        let rumor_id = crate::canon::ids::stable_id(
            &self.world_canon.world_seed,
            &origin_scope,
            "rumor",
            &format!("{seq}:{turn}:{speaker}:{text}"),
        );
        let mut carriers = witnesses;
        if !speaker.trim().is_empty() {
            carriers.insert(actor_key(speaker));
        }
        self.rumors.push(Rumor {
            rumor_id,
            seq,
            turn,
            speaker: speaker.to_string(),
            text,
            witnesses: carriers.clone(),
            origin_scope,
            known_in,
            carriers,
            strength: 1,
            distortion: 0,
            created_minutes: self
                .time
                .absolute_minutes
                .max(self.world_canon.clock_minutes),
            last_spread_minutes: self
                .time
                .absolute_minutes
                .max(self.world_canon.clock_minutes),
            confirmed: false,
        });
        truncate_tail(&mut self.rumors, rumors_cap);
        self.prune_rumor_memory_to_live_ids();
        if let Some(rumor) = self.rumors.last().cloned() {
            self.sync_rumor_memory(&rumor);
        }
    }

    /// Record factual claims made by an NPC as scoped memory, without marking
    /// the claimed facts as objective canon.
    pub fn record_npc_claims(
        &mut self,
        seq: i64,
        turn: i64,
        speaker: &str,
        claims: &[String],
        witnesses: &BTreeSet<String>,
    ) {
        let speaker_id = actor_key(speaker);
        if speaker_id.is_empty() || claims.is_empty() {
            return;
        }

        let speaker_name = self
            .npcs
            .get(&speaker_id)
            .map(|npc| npc.name.trim())
            .filter(|name| !name.is_empty())
            .unwrap_or(speaker);
        let speaker_name = speaker_name.to_string();
        let place_id = if !self.world_canon.player_place_id.is_empty() {
            self.world_canon.player_place_id.clone()
        } else {
            self.scene.location_id.clone()
        };
        let source_event_id = format!("world_event_{seq}");

        let mut visibility = BTreeSet::new();
        visibility.insert(format!("actor:{speaker_id}"));
        for witness in witnesses {
            if witness == "player" {
                visibility.insert("player".to_string());
                continue;
            }
            let witness_id = actor_key(witness);
            if !witness_id.is_empty() {
                visibility.insert(format!("actor:{witness_id}"));
            }
        }

        let mut actor_ids = vec![speaker_id.clone()];
        for witness in witnesses {
            let witness_id = actor_key(witness);
            if !witness_id.is_empty() && witness_id != "player" && !actor_ids.contains(&witness_id)
            {
                actor_ids.push(witness_id);
            }
        }

        let mut seen_claims = BTreeSet::new();
        for claim in claims {
            let claim = claim.trim();
            if claim.is_empty() || !seen_claims.insert(claim.to_string()) {
                continue;
            }

            let mut unit = MemoryUnit {
                tier: MemoryTier::Raw,
                owner_scope: format!("actor:{speaker_id}"),
                visibility_scopes: visibility.iter().cloned().collect(),
                summary: format!("Claim by {speaker_name}: {claim}"),
                details: format!(
                    "NPC claim linked to speech event seq {seq}; turn {turn}; speaker {speaker_id}; witnesses: {}",
                    witnesses.iter().cloned().collect::<Vec<_>>().join(", ")
                ),
                facts_claimed: vec![claim.to_string()],
                source_event_ids: vec![source_event_id.clone()],
                time_start: self.time.absolute_minutes.max(self.world_canon.clock_minutes),
                time_end: self.time.absolute_minutes.max(self.world_canon.clock_minutes),
                actor_ids: actor_ids.clone(),
                topic_tags: vec!["claim".to_string(), "npc_claim".to_string()],
                truth_status: MemoryTruthStatus::Claim,
                created_by: "npc_claim".to_string(),
                ..Default::default()
            };
            if !place_id.is_empty() {
                unit.place_ids = vec![place_id.clone()];
            }
            self.add_memory_unit(unit);
        }
    }

    pub fn sync_rumor_memory(&mut self, rumor: &Rumor) {
        let mut unit = crate::canon::rumor::memory_unit_for_rumor(rumor, &self.world_canon);
        unit.normalize();
        self.world_canon
            .memory
            .units
            .insert(unit.memory_id.clone(), unit);
    }

    pub fn sync_all_rumor_memory(&mut self) {
        let rumors = self.rumors.clone();
        for rumor in &rumors {
            self.sync_rumor_memory(rumor);
        }
        self.prune_rumor_memory_to_live_ids();
    }

    pub fn prune_rumor_memory_to_live_ids(&mut self) {
        let live_ids: BTreeSet<String> = self
            .rumors
            .iter()
            .map(crate::canon::rumor::memory_id_for_rumor)
            .collect();
        self.world_canon.memory.units.retain(|id, unit| {
            unit.created_by != "rumor_graph" || !id.starts_with("rumor:") || live_ids.contains(id)
        });
    }

    pub fn rumor_visible_to_access(&self, rumor: &Rumor, access: &MemoryAccess) -> bool {
        if access.gm {
            return true;
        }
        rumor
            .known_in
            .iter()
            .any(|scope| access.scopes.contains(scope))
    }

    pub fn spread_rumors_on_transition(
        &mut self,
        actor_id: &str,
        transition_id: &str,
        elapsed_minutes: i64,
    ) -> Vec<String> {
        let actor_id = actor_key(actor_id);
        if actor_id.is_empty() || elapsed_minutes <= 0 {
            return Vec::new();
        }
        let now = self
            .time
            .absolute_minutes
            .max(self.world_canon.clock_minutes);
        let Some(transition) = self.world_canon.transition(transition_id).cloned() else {
            return Vec::new();
        };
        let route_scopes = crate::canon::rumor::scopes_for_transition(&transition);
        if route_scopes.is_empty() {
            return Vec::new();
        }
        let actor_scope = format!("actor:{actor_id}");
        let mut changed_ids = Vec::new();
        let mut changed_rumors = Vec::new();
        for rumor in &mut self.rumors {
            if rumor.strength <= 0 {
                continue;
            }
            if !rumor.carriers.contains(&actor_id) && !rumor.known_in.contains(&actor_scope) {
                continue;
            }
            let before = rumor.known_in.len();
            rumor.known_in.extend(route_scopes.iter().cloned());
            if rumor.known_in.len() != before {
                rumor.distortion = rumor.distortion.saturating_add(1).min(100);
                rumor.last_spread_minutes = now;
                changed_ids.push(rumor.rumor_id.clone());
                changed_rumors.push(rumor.clone());
            }
        }
        for rumor in &changed_rumors {
            self.sync_rumor_memory(rumor);
        }
        changed_ids
    }

    pub fn advance_rumors(&mut self, now_minutes: i64) -> Vec<String> {
        let mut changed_ids = Vec::new();
        let mut changed_rumors = Vec::new();
        for idx in 0..self.rumors.len() {
            let elapsed = (now_minutes - self.rumors[idx].last_spread_minutes).max(0);
            let decay = crate::canon::rumor::should_decay_rumor(elapsed);
            if decay > 0 {
                self.rumors[idx].strength = self.rumors[idx].strength.saturating_sub(decay);
            }
            if !crate::canon::rumor::should_spread_place_rumor(elapsed, self.rumors[idx].strength) {
                if decay > 0 {
                    self.rumors[idx].last_spread_minutes = now_minutes;
                    changed_ids.push(self.rumors[idx].rumor_id.clone());
                    changed_rumors.push(self.rumors[idx].clone());
                }
                continue;
            }

            let carriers: Vec<String> = self.rumors[idx].carriers.iter().cloned().collect();
            let mut added_any = false;
            for carrier in carriers {
                let carrier = actor_key(&carrier);
                let place_id = if carrier == "player" {
                    self.world_canon.player_place_id.clone()
                } else {
                    self.world_canon
                        .actors
                        .get(&carrier)
                        .and_then(|actor| actor.location.place().map(str::to_string))
                        .unwrap_or_default()
                };
                if place_id.is_empty() {
                    continue;
                }
                let scopes = crate::canon::rumor::scopes_added_by_carrier_at_place(
                    &self.world_canon,
                    &place_id,
                    elapsed,
                );
                let before = self.rumors[idx].known_in.len();
                self.rumors[idx].known_in.extend(scopes);
                if self.rumors[idx].known_in.len() != before {
                    added_any = true;
                }
            }
            if added_any {
                if elapsed >= crate::canon::rumor::WIDER_SPREAD_THRESHOLD_MINUTES {
                    self.rumors[idx].distortion =
                        self.rumors[idx].distortion.saturating_add(1).min(100);
                }
                self.rumors[idx].last_spread_minutes = now_minutes;
                changed_ids.push(self.rumors[idx].rumor_id.clone());
                changed_rumors.push(self.rumors[idx].clone());
            } else if decay > 0 {
                self.rumors[idx].last_spread_minutes = now_minutes;
                changed_ids.push(self.rumors[idx].rumor_id.clone());
                changed_rumors.push(self.rumors[idx].clone());
            }
        }
        for rumor in &changed_rumors {
            self.sync_rumor_memory(rumor);
        }
        changed_ids
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

    /// Project the current player-facing [`SceneState`] out of the canonical
    /// place graph (TZ §6.10 `Place + … -> CurrentView`). Phase 1 proves the
    /// scene is derivable from the canon; the live `scene_export` path is not
    /// yet routed through this (that is Phase 2). Falls back to the live scene
    /// when the current location has no canonical place (e.g. a pre-canon save).
    pub fn build_current_view(&self) -> SceneState {
        crate::canon::view::build_current_view(self)
    }

    /// Rebuild `self.scene` FROM the canon so the legacy-facing scene is a
    /// derived cache, not an independent owner of truth (LOCKED DECISION #1).
    ///
    /// Anchors on `world_canon.player_place_id`: title/description come from that
    /// [`crate::canon::Place`]; exits from its visible transitions; present NPCs
    /// from the living actors physically at the place; items/tension/constraints
    /// and other ephemeral view state are carried over from the previous scene.
    /// Call this after ANY canon mutation that can change the player's place,
    /// the place's structure, or actor locations, and after load.
    ///
    /// No-op when the canon has no player place (a pre-canon save): the legacy
    /// scene is left as-is so behaviour is never worse than before.
    ///
    /// Phase-И §И2 leak fix: when the rebuild anchor (the canon player place)
    /// DIFFERS from the live `scene.location_id`, the player is LEAVING that
    /// location — its live `scene.items` are STASHED into [`Self::place_items`]
    /// under the old id, and the entered place's stored items (empty when
    /// unvisited) become the new live items BEFORE the view is rebuilt (the view
    /// clones `scene.items`). Same-place rebuilds keep the live items untouched.
    /// Callers that stage a destination's items (`set_scene`) pin
    /// `scene.location_id` to the new place FIRST so this reads as a same-place
    /// refresh and their staged items are preserved (never clobbered/stashed
    /// under the old id).
    pub fn refresh_scene_from_canon(&mut self) {
        if self.world_canon.player_place_id.is_empty()
            || self
                .world_canon
                .place(&self.world_canon.player_place_id)
                .is_none()
        {
            return;
        }
        let anchor_id = self.world_canon.player_place_id.clone();
        let old_id = self.scene.location_id.clone();
        if !old_id.is_empty() && old_id != anchor_id {
            // Genuine place change: park all scene-local state under the place
            // being left, then restore the entered place's state. The canon
            // rebuild below owns structural fields such as title and exits.
            let leaving = std::mem::take(&mut self.scene.items);
            self.place_items.insert(old_id.clone(), leaving);
            self.place_scene_contexts.insert(
                old_id,
                PlaceSceneContext {
                    scene_id: self.scene.scene_id.clone(),
                    constraints: std::mem::take(&mut self.scene.constraints),
                    tension: std::mem::take(&mut self.scene.tension),
                    player_seen: std::mem::take(&mut self.scene.player_seen),
                },
            );
            self.scene.items = self
                .place_items
                .get(&anchor_id)
                .cloned()
                .unwrap_or_default();
            let entered = self
                .place_scene_contexts
                .get(&anchor_id)
                .cloned()
                .unwrap_or_default();
            self.scene.scene_id = if entered.scene_id.is_empty() {
                anchor_id.clone()
            } else {
                entered.scene_id
            };
            self.scene.constraints = entered.constraints;
            self.scene.tension = entered.tension;
            self.scene.player_seen = entered.player_seen;
        }
        self.scene = self.build_current_view();
        self.constraints = self.scene.constraints.clone();
    }

    /// A compact, structured summary of the canonical world for GM/generator
    /// context: high-level world lore, the current region/settlement, active
    /// factions and their goals, and the most recent PLAYER-VISIBLE canon
    /// events. This is what makes the rich worldgen (lore/region/settlement/
    /// faction/history) actually reach the GM — `scene_context` only covers the
    /// immediate place. Hidden lore is GM-only generation guidance; hidden event
    /// history is still gated, so only player-visible events are surfaced
    /// (TZ §11). Empty for a pre-canon / empty canon.
    pub fn canon_world_context(&self) -> String {
        let c = &self.world_canon;
        if c.is_empty() || c.player_place_id.is_empty() {
            return String::new();
        }
        let mut lines: Vec<String> = Vec::new();
        lines.extend(c.world_lore.gm_context_lines());
        if let Some(p) = c.place(&c.player_place_id) {
            if let Some(r) = c.region_for_place(&p.place_id) {
                lines.push(format!(
                    "Region: {} — climate {}, danger {}/5",
                    r.name, r.climate, r.danger_level
                ));
            }
            if let Some(s) = c.settlement_for_place(&p.place_id) {
                let mut s_line = format!("Settlement: {}", s.name);
                if !s.power.is_empty() {
                    s_line.push_str(&format!("; power: {}", s.power));
                }
                if !s.conflict.is_empty() {
                    s_line.push_str(&format!("; conflict: {}", s.conflict));
                }
                lines.push(s_line);
            }
            if let Some(district) = c.district_for_place(&p.place_id) {
                lines.push(format!(
                    "District: {} [{}]",
                    district.name, district.district_id
                ));
            }
        }
        let visited_places = c
            .places
            .values()
            .filter(|place| place.is_visited())
            .take(32)
            .map(|place| format!("{} [{}]", place.name, place.place_id))
            .collect::<Vec<_>>();
        if !visited_places.is_empty() {
            lines.push(format!(
                "Visited travel destinations: {}",
                visited_places.join(" | ")
            ));
        }
        let factions: Vec<String> = c
            .factions
            .values()
            .map(|f| {
                if f.goals.is_empty() {
                    f.name.clone()
                } else {
                    format!("{} (goals: {})", f.name, f.goals.join("; "))
                }
            })
            .collect();
        if !factions.is_empty() {
            lines.push(format!("Factions: {}", factions.join(" | ")));
        }
        let recent: Vec<String> = c
            .event_log
            .player_visible()
            .into_iter()
            .rev()
            .take(5)
            .map(|e| {
                if e.effects.is_empty() {
                    e.kind.clone()
                } else {
                    e.effects.join(", ")
                }
            })
            .collect();
        if !recent.is_empty() {
            lines.push(format!("Recent world events: {}", recent.join("; ")));
        }
        if lines.is_empty() {
            String::new()
        } else {
            lines.join("\n")
        }
    }

    /// Short access-gated continuity slice for the GM turn context.
    ///
    /// This is deliberately not a memory card dump: no ids, no details, no
    /// private NPC thoughts. Full drill-down remains behind memory/NPC tools.
    pub fn gm_memory_context(&self) -> String {
        if self.world_canon.memory.is_empty() {
            return String::new();
        }
        let place_id = if self.world_canon.player_place_id.is_empty() {
            self.scene.location_id.as_str()
        } else {
            self.world_canon.player_place_id.as_str()
        };
        let mut scopes = self.memory_access_for_player().scopes;
        if !place_id.trim().is_empty() {
            scopes.extend(self.memory_access_for_place(place_id).scopes);
        }
        let access = MemoryAccess::scoped(scopes);
        let rows = self.world_canon.memory.query(&access, "", 6, false);
        if rows.is_empty() {
            return String::new();
        }
        let lines: Vec<String> = rows
            .into_iter()
            .map(|unit| format!("- {}", gm_memory_snapshot_line(unit)))
            .collect();
        format!(
            "Access-gated short memory snapshot (player/local/public only; use get_memory or ask_npc for details):\n{}",
            lines.join("\n")
        )
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
        format!(
            "{prefix}{date}, {}",
            payload["time_of_day"].as_str().unwrap_or("")
        )
    }

    /// Time summary for model-facing context. Authored calendar/date values stay
    /// verbatim; only the empty-date fallback is an English instruction-layer label.
    pub fn model_time_summary(&self) -> String {
        let payload = self.time_export();
        let calendar = payload["calendar_name"].as_str().unwrap_or("");
        let date = if self.time.current_date_label.is_empty() {
            format!("Day {}", payload["day_number"])
        } else {
            self.time.current_date_label.clone()
        };
        let prefix = if calendar.is_empty() {
            String::new()
        } else {
            format!("{calendar}, ")
        };
        format!(
            "{prefix}{date}, {}",
            payload["time_of_day"].as_str().unwrap_or("")
        )
    }

    pub fn time_context(&self) -> String {
        let payload = self.time_export();
        let mut lines = vec![
            format!("Current world time: {}", self.model_time_summary()),
            format!(
                "Previous player turn elapsed: {} minutes",
                payload["last_advance_minutes"]
            ),
        ];
        let reason = payload["last_advance_reason"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
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
        self.time.absolute_minutes = before["absolute_minutes"].as_i64().unwrap_or(0) + amount;
        self.time.last_advance_minutes = amount;
        self.time.last_advance_reason = reason.trim().to_string();
        // Keep the canonical clock in lockstep and run the budgeted offscreen
        // tick so scheduled events / world simulation actually advance on a live
        // advance_time — not only on the engine's AdvanceClock action.
        if !self.world_canon.is_empty() {
            self.world_canon.clock_minutes = self.time.absolute_minutes;
            let now = self.world_canon.clock_minutes;
            crate::canon::engine::tick_offscreen(&mut self.world_canon, now, 0);
            self.advance_rumors(now);
            self.refresh_scene_from_canon();
        }
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

    /// K2.1 numeric normalization (`docs/CHARACTERS_AND_STORY_TZ.md` §К2.1):
    /// coerce the values of a stat map (`abilities`/`skills`/`saving_throws`/`hp`)
    /// so the notation-based dice path reads reliable numbers instead of stringy
    /// junk the model sometimes emits (`"14"` → `14`, `"3.5"` → `3.5`). This runs
    /// through the single choke point [`Self::apply_player_character_fields`], so
    /// BOTH the tool-edit path and the launch-seed path (`seed_player_character`,
    /// which delegates here) get the same coercion.
    ///
    /// Per-value rule (order matters — integers stay integers, exact-only):
    /// - a finite number is kept verbatim (an `i64` stays an `i64`); a non-finite
    ///   number (NaN/±inf, only reachable via an already-parsed float) is dropped;
    /// - a string that parses WHOLLY (after trim) as an integer becomes that
    ///   integer; else if it parses wholly as a finite float, it becomes that
    ///   float; otherwise the string is kept VERBATIM — legitimate textual notes
    ///   (e.g. an hp annotation) are never destroyed;
    /// - any other value kind (bool/null/array/object) is kept verbatim.
    ///
    /// This is deliberately value-only: keys are never touched, so all stat
    /// families (numeric-valued `abilities`/`saving_throws`/`skills` and the mixed
    /// `hp` map) share one pass — a stray textual entry simply survives untouched.
    fn normalize_stat_dict(raw: &Value) -> Map<String, Value> {
        let src = as_dict(raw);
        let mut out = Map::new();
        for (k, v) in src {
            out.insert(k, normalize_stat_value(v));
        }
        out
    }

    fn apply_npc_character_fields(
        npc: &mut Npc,
        fields: &Map<String, Value>,
        editable: &[&str],
    ) -> BTreeSet<String> {
        let dict_fields = ["abilities", "skills", "saving_throws", "hp"];
        let joined_fields = ["speed", "senses", "languages"];
        let mut changed = BTreeSet::new();

        for key in editable {
            if !fields.contains_key(*key) {
                continue;
            }
            let raw = &fields[*key];
            let new_value = if dict_fields.contains(key) {
                Value::Object(Self::normalize_stat_dict(raw))
            } else if *key == "passive_perception" {
                as_int_or_none(raw).map_or(Value::Null, Value::from)
            } else if *key == "ac" {
                raw.clone()
            } else if joined_fields.contains(key) {
                Value::String(as_joined_str(raw))
            } else {
                Value::String(as_str(raw))
            };
            if npc_field_value(npc, key) == new_value {
                continue;
            }
            set_npc_field(npc, key, new_value);
            changed.insert((*key).to_string());
        }
        changed
    }

    /// Append unique traits without requiring the model to read and rewrite the
    /// whole `distinctive_features` string. Existing cards commonly separate
    /// traits with commas, while model-authored additions use semicolons.
    fn append_distinctive_features(current: &mut String, additions: &Value) -> bool {
        let before = current.clone();
        for raw in as_list(additions) {
            let addition = as_str(&raw).trim().to_string();
            if addition.is_empty() {
                continue;
            }
            let addition_key = addition.to_lowercase();
            let already_present = current
                .split([';', ',', '\n', '\r'])
                .map(str::trim)
                .any(|existing| existing.to_lowercase() == addition_key);
            if already_present {
                continue;
            }
            if !current.trim().is_empty() {
                current.push_str("; ");
            }
            current.push_str(&addition);
        }
        *current != before
    }

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
            "current_appearance",
            "life_status",
            "life_status_note",
            "condition",
            "personality",
            "values",
            "gm_notes",
            "speed",
            "senses",
            "languages",
            // Фаза С §С1: active concentration is a plain text field (name of the
            // held spell; "" clears it). Documented in the tool schema.
            "concentration",
        ]
        .into_iter()
        .collect();
        // Фаза С §С1: spell_slots/spell_slots_max are FLAT «level → count» maps,
        // so they ride the same K2.1 numeric coercion as the stat dicts — a
        // stringy "3" becomes 3 for the cast_spell decrement path.
        let dict_fields: BTreeSet<&str> = [
            "abilities",
            "skills",
            "saving_throws",
            "hp",
            "spell_slots",
            "spell_slots_max",
        ]
        .into_iter()
        .collect();
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
            let new_value: Value = if key == "spells" {
                // Фаза С §С1: spells is a Vec of OBJECTS, NOT strings — it must
                // NOT go through the `as_str` list path (that would mangle each
                // record into a JSON string). Bespoke coercion: keep only object
                // entries and deserialize each into a SpellEntry (serde defaults
                // fill missing keys); non-object junk (strings/numbers/nested
                // arrays) is skipped, and a record that fails to parse is dropped
                // rather than poisoning the whole batch. Store the re-serialized
                // canonical array so change-detection compares like-for-like.
                let cleaned: Vec<SpellEntry> = as_list(raw)
                    .into_iter()
                    .filter(|v| v.is_object())
                    .filter_map(|v| serde_json::from_value::<SpellEntry>(v).ok())
                    .collect();
                serde_json::to_value(&cleaned).unwrap_or(Value::Array(Vec::new()))
            } else if dict_fields.contains(key) {
                // K2.1: coerce numeric-stringy stat values before storing so the
                // notation dice path reads real numbers; textual entries survive.
                Value::Object(Self::normalize_stat_dict(raw))
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

    /// K1 launch overlay (`docs/CHARACTERS_AND_STORY_TZ.md` §К1.3): FULLY REPLACE
    /// the world's player character from a character-package `player_character`
    /// object. Unlike [`Self::update_player_character`] (the tool path) this is a
    /// SEED, not an edit: it does NOT emit an event and does NOT bump
    /// `card_revision` — the package's `card_revision` travels VERBATIM (the
    /// hero's edu-counter rides with the package; the package `version` is a
    /// separate counter shown in the UI). Missing/absent fields fall back to the
    /// default hero via the shared `seed_player_character` coercion, so a partial
    /// package payload still yields a valid card. `raw` is the
    /// `payload.player_character` object; a non-object seeds the default hero.
    pub fn seed_player_character(&mut self, raw: Option<&Value>) {
        self.player_character = seed_player_character(raw);
    }

    /// K2.2 inventory/equipment delta ops (`docs/CHARACTERS_AND_STORY_TZ.md`
    /// §К2.2): apply `<field>_remove` then `<field>_add` to a `Vec<String>`
    /// list that the full-rewrite loop has ALREADY settled. `remove` deletes ALL
    /// trim-exact-matching occurrences; `add` appends trimmed entries, skipping
    /// any that already exist so a repeated tool call is idempotent.
    /// Empty/absent arrays are no-ops. Returns `true` iff the list actually
    /// changed, so the caller can fold it into the `changed` set exactly like a
    /// full rewrite would (revision/event fire identically). Precedence — the
    /// full-array field wins first, deltas mutate the result — is enforced by
    /// call order in [`Self::update_player_character`], not here.
    ///
    /// Matching is by the entry HEAD (the part before the first `« — »`
    /// description separator), trimmed + lowercased — the ONE matching rule the
    /// item convention mandates (ITEMS_AND_SPELLS_TZ §И1), shared with
    /// `take_item`/`drop_item`. `inventory_remove: ["кинжал"]` therefore removes
    /// `"кинжал — 1d4, скрыт в сапоге"`, and adding a second entry with an
    /// already-present head is skipped (one head = one item; distinct items need
    /// distinct names).
    fn apply_list_delta(list: &mut Vec<String>, adds: &Value, removes: &Value) -> bool {
        let before = list.clone();
        // Remove: every occurrence whose head matches the requested entry's head.
        for entry in as_list(removes) {
            let target = as_str(&entry);
            if target.is_empty() {
                continue;
            }
            let needle = item_head(&target).to_lowercase();
            list.retain(|existing| item_head(existing).to_lowercase() != needle);
        }
        // Add: append trimmed entries, skipping any whose head is already
        // present (including ones added earlier in this same batch).
        for entry in as_list(adds) {
            let value = as_str(&entry);
            if value.is_empty() {
                continue;
            }
            let needle = item_head(&value).to_lowercase();
            if list
                .iter()
                .any(|existing| item_head(existing).to_lowercase() == needle)
            {
                continue;
            }
            list.push(value);
        }
        *list != before
    }

    /// Validate the GM-facing player patch before applying it. Internal engine
    /// paths may keep using `update_player_character` directly.
    pub fn update_player_character_checked(
        &mut self,
        fields: &Value,
        reason: &str,
    ) -> Result<Value, String> {
        let map = match fields {
            Value::Object(map) => map,
            _ => return Err("update_character requires an object in `fields`.".to_string()),
        };
        let allowed: BTreeSet<&str> = PLAYER_CHARACTER_FIELDS.iter().copied().collect();
        let unsupported: Vec<String> = map
            .keys()
            .filter(|key| {
                !allowed.contains(key.as_str())
                    && ![
                        "inventory_add",
                        "inventory_remove",
                        "equipment_add",
                        "equipment_remove",
                        "distinctive_features_add",
                    ]
                    .contains(&key.as_str())
            })
            .cloned()
            .collect();
        if !unsupported.is_empty() {
            return Err(format!(
                "Fields are not editable for the player through update_character: {}.",
                unsupported.join(", ")
            ));
        }
        let mut payload = self.update_player_character(fields, reason);
        if let Value::Object(map) = &mut payload {
            map.insert("target".to_string(), Value::String("player".to_string()));
        }
        Ok(payload)
    }

    pub fn update_player_character(&mut self, fields: &Value, reason: &str) -> Value {
        let map = match fields {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        };
        let mut changed = Self::apply_player_character_fields(&mut self.player_character, &map);
        // K2.2: fold inventory/equipment deltas onto the just-rewritten arrays.
        // Order per spec: full rewrite (above) → remove → add. Each family's
        // resulting vec is compared to its post-rewrite value, so the change
        // detection feeding `card_revision`/the event fires exactly as a full
        // rewrite would. `features` is intentionally NOT delta-editable (spec
        // lists inventory + equipment only).
        for (field, add_key, remove_key) in [
            ("inventory", "inventory_add", "inventory_remove"),
            ("equipment", "equipment_add", "equipment_remove"),
        ] {
            let adds = map.get(add_key).cloned().unwrap_or(Value::Null);
            let removes = map.get(remove_key).cloned().unwrap_or(Value::Null);
            if adds.is_null() && removes.is_null() {
                continue;
            }
            let list = match field {
                "inventory" => &mut self.player_character.inventory,
                _ => &mut self.player_character.equipment,
            };
            if Self::apply_list_delta(list, &adds, &removes) {
                changed.insert(field.to_string());
            }
        }
        if let Some(additions) = map.get("distinctive_features_add") {
            if Self::append_distinctive_features(
                &mut self.player_character.distinctive_features,
                additions,
            ) {
                changed.insert("distinctive_features".to_string());
            }
        }
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

    /// §И3 `take_item` — move a scene item's BODY into the player's inventory.
    ///
    /// Matching (§И3.1-3): a non-empty `item_id` is an EXACT id match (the only
    /// path that can pick up an INVISIBLE item — the GM-trusted route); otherwise
    /// `name` matches VISIBLE scene items by `trim().to_lowercase()`. Zero matches
    /// → `item_not_here` (with the visible-item names as a hint); more than one →
    /// `ambiguous_item` (never silently take the first). A matched but
    /// `portable == false` item → `not_portable`. On success the [`SceneItem`] is
    /// removed from `scene.items` and `"{name} — {details}"` (just `name` when
    /// details are empty — the §И1 convention) is appended to `pc.inventory`
    /// through the same dedup-by-head machinery [`apply_list_delta`] uses, and
    /// `card_revision` bumps. NO canon event is written (§0/§5: items are not
    /// canon entities in this phase). Returns `Ok(payload)` mirroring
    /// `update_player_character` (so the orchestrator can emit
    /// PLAYER_CHARACTER_UPDATE) plus scene-item fields, or `Err(payload)` carrying
    /// the structured rejection so the handler renders it as a tool error.
    pub fn take_item(&mut self, item_id: &str, name: &str, reason: &str) -> Result<Value, Value> {
        let item_id = item_id.trim();
        let name = name.trim();
        let idx = if !item_id.is_empty() {
            match self.scene.items.iter().position(|it| it.item_id == item_id) {
                Some(i) => i,
                None => {
                    return Err(json!({
                        "code": "unknown_item",
                        "error": format!("no scene item has item_id '{item_id}'"),
                        "item_id": item_id,
                    }));
                }
            }
        } else if !name.is_empty() {
            let needle = name.to_lowercase();
            // Candidates: VISIBLE scene items whose name matches (trim+lowercase).
            let candidates: Vec<usize> = self
                .scene
                .items
                .iter()
                .enumerate()
                .filter(|(_, it)| it.visible && it.name.trim().to_lowercase() == needle)
                .map(|(i, _)| i)
                .collect();
            match candidates.as_slice() {
                [] => {
                    let visible: Vec<String> = self
                        .scene
                        .visible_items()
                        .iter()
                        .map(|it| it.name.clone())
                        .collect();
                    return Err(json!({
                        "code": "item_not_here",
                        "error": format!("no visible item named '{name}' is in the current scene"),
                        "name": name,
                        "visible_items": visible,
                    }));
                }
                [only] => *only,
                _ => {
                    let cands: Vec<Value> = candidates
                        .iter()
                        .map(|&i| {
                            let it = &self.scene.items[i];
                            json!({"item_id": it.item_id, "name": it.name, "location": it.location})
                        })
                        .collect();
                    return Err(json!({
                        "code": "ambiguous_item",
                        "error": format!("more than one visible item named '{name}'; pass item_id"),
                        "name": name,
                        "candidates": cands,
                    }));
                }
            }
        } else {
            return Err(json!({
                "code": "missing_item_ref",
                "error": "take_item requires item_id or name",
            }));
        };

        if !self.scene.items[idx].portable {
            let it = &self.scene.items[idx];
            return Err(json!({
                "code": "not_portable",
                "error": format!("'{}' cannot be picked up (not portable)", it.name),
                "item_id": it.item_id,
                "name": it.name,
            }));
        }

        let item = self.scene.items.remove(idx);
        let entry = item_entry_string(&item.name, &item.details);
        // Dedup by head like apply_list_delta (trim + lowercase — the one §И1
        // matching rule): skip when an inventory entry already has the same
        // head; still a success (idempotent re-take).
        let head = item_head(&entry).to_lowercase();
        let already = self
            .player_character
            .inventory
            .iter()
            .any(|e| item_head(e).to_lowercase() == head);
        if !already {
            self.player_character.inventory.push(entry.clone());
        }
        self.player_character.card_revision += 1;
        Ok(json!({
            "ok": true,
            "status": "taken",
            "item_id": item.item_id,
            "name": item.name,
            "inventory_entry": entry,
            "reason": reason.trim(),
            "updated": ["inventory"],
            "card_revision": self.player_character.card_revision,
            "player_character": self.player_character_export(false),
        }))
    }

    /// §И3 `drop_item` — move an inventory entry back into the CURRENT scene.
    ///
    /// Matches an inventory entry by its §И1 HEAD (`trim().to_lowercase()` of the
    /// part before « — »); zero matches → `unknown_item`. The entry is removed and
    /// a fresh [`SceneItem`] is inserted into `scene.items` with a generated id
    /// (mirroring the `safe_id(name, "item_N")` scheme used elsewhere, uniquified
    /// against the current scene), `name` = head, `details` = tail, `portable` +
    /// `visible` = true, `location` = `location` arg or `"рядом"`. `card_revision`
    /// bumps. NO canon event (§0). `Ok`/`Err` payload shape matches `take_item`.
    pub fn drop_item(&mut self, name: &str, location: &str, reason: &str) -> Result<Value, Value> {
        let needle = name.trim().to_lowercase();
        if needle.is_empty() {
            return Err(json!({
                "code": "unknown_item",
                "error": "drop_item requires a non-empty name",
                "name": name.trim(),
            }));
        }
        let inv_idx = self
            .player_character
            .inventory
            .iter()
            .position(|e| item_head(e).to_lowercase() == needle);
        let inv_idx = match inv_idx {
            Some(i) => i,
            None => {
                let inventory = self.player_character.inventory.clone();
                return Err(json!({
                    "code": "unknown_item",
                    "error": format!("no inventory entry named '{}'", name.trim()),
                    "name": name.trim(),
                    "inventory": inventory,
                }));
            }
        };
        let entry = self.player_character.inventory.remove(inv_idx);
        let head = item_head(&entry).to_string();
        let details = item_tail(&entry).to_string();
        let location = {
            let loc = location.trim();
            if loc.is_empty() {
                "рядом".to_string()
            } else {
                loc.to_string()
            }
        };
        // Generate a scene-unique item_id from the head (mirrors coerce items:
        // `safe_id(name, "item_N")`), suffixing on collision with a live id.
        let base = safe_id(&head, &format!("item_{}", self.scene.items.len() + 1));
        let mut item_id = base.clone();
        let mut n = 2;
        while self.scene.items.iter().any(|it| it.item_id == item_id) {
            item_id = format!("{base}_{n}");
            n += 1;
        }
        let scene_item = SceneItem {
            item_id: item_id.clone(),
            name: head.clone(),
            location: location.clone(),
            visible: true,
            portable: true,
            owner: String::new(),
            details: details.clone(),
        };
        self.scene.items.push(scene_item);
        self.player_character.card_revision += 1;
        Ok(json!({
            "ok": true,
            "status": "dropped",
            "item_id": item_id,
            "name": head,
            "details": details,
            "location": location,
            "reason": reason.trim(),
            "updated": ["inventory"],
            "card_revision": self.player_character.card_revision,
            "player_character": self.player_character_export(false),
        }))
    }

    /// §С2 `cast_spell` — spend a slot / set concentration for a known spell.
    ///
    /// Pure STATE bookkeeping (`docs/ITEMS_AND_SPELLS_TZ.md` §С2), NO dice/DC/
    /// damage math — attack/save/damage stay on the existing `roll_dice`
    /// notation contract. Steps EXACTLY per §С2:
    /// 1. find the spell by `name.trim().to_lowercase()` in `pc.spells`; a miss →
    ///    `Err(unknown_spell)` carrying the known-spell names as a hint.
    /// 2. a level-0 spell (заговор) spends NO slot.
    /// 3. otherwise the effective level is `max(spell.level, requested)` (an
    ///    upcast never drops below the spell's own level); `spell_slots[lvl]` must
    ///    coerce to an int `> 0` — decrement it (written back as a NUMBER) — else
    ///    `Err(no_slots)` for that level.
    /// 4. a concentration spell replaces `pc.concentration`; the PREVIOUS value
    ///    (if any) is returned as `concentration_ended` so the GM can narrate the
    ///    dropped effect. A non-concentration cast leaves `pc.concentration` as-is.
    /// 5. `card_revision` bumps; the `Ok` payload mirrors take_item/drop_item so
    ///    the orchestrator emits PLAYER_CHARACTER_UPDATE. `Err` carries a
    ///    validator-style `{code,error,…}` the handler renders as a tool error.
    pub fn cast_spell(
        &mut self,
        name: &str,
        slot_level: Option<i64>,
        reason: &str,
    ) -> Result<Value, Value> {
        let needle = name.trim().to_lowercase();
        if needle.is_empty() {
            return Err(json!({
                "code": "unknown_spell",
                "error": "cast_spell requires a non-empty spell name",
                "name": name.trim(),
            }));
        }
        let idx = self
            .player_character
            .spells
            .iter()
            .position(|sp| sp.name.trim().to_lowercase() == needle);
        let idx = match idx {
            Some(i) => i,
            None => {
                let known: Vec<String> = self
                    .player_character
                    .spells
                    .iter()
                    .map(|sp| sp.name.clone())
                    .filter(|n| !n.trim().is_empty())
                    .collect();
                return Err(json!({
                    "code": "unknown_spell",
                    "error": format!("персонаж не знает заклинания '{}'", name.trim()),
                    "name": name.trim(),
                    "known_spells": known,
                }));
            }
        };
        // Clone the fields we need before mutating the slot map (borrow split).
        let spell = self.player_character.spells[idx].clone();
        let base_level = spell.level as i64;

        // §С2.2/3: level 0 spends no slot; else effective level = max(base,
        // requested). A requested level below the spell's own is clamped up.
        let slot_spent_level: Option<i64> = if base_level == 0 {
            None
        } else {
            let lvl = slot_level.map(|r| r.max(base_level)).unwrap_or(base_level);
            let key = lvl.to_string();
            let remaining = slot_int(self.player_character.spell_slots.get(&key));
            if remaining <= 0 {
                return Err(json!({
                    "code": "no_slots",
                    "error": format!("нет свободных слотов уровня {lvl}"),
                    "name": spell.name,
                    "level": lvl,
                }));
            }
            // Decrement, writing the remainder back as a NUMBER so the flat map
            // stays numeric for the next cast (no stringy residue).
            self.player_character
                .spell_slots
                .insert(key, json!(remaining - 1));
            Some(lvl)
        };

        // §С2.4: concentration spells replace the held effect; the prior value
        // (non-empty only) surfaces as concentration_ended.
        let mut concentration_started: Option<String> = None;
        let mut concentration_ended: Option<String> = None;
        if spell.concentration {
            let prev = self.player_character.concentration.trim().to_string();
            if !prev.is_empty() && prev != spell.name.trim() {
                concentration_ended = Some(prev);
            }
            self.player_character.concentration = spell.name.trim().to_string();
            concentration_started = Some(spell.name.trim().to_string());
        }

        self.player_character.card_revision += 1;
        let slots_remaining = Value::Object(self.player_character.spell_slots.clone());
        Ok(json!({
            "ok": true,
            "status": "cast",
            "spell": spell.name.trim(),
            "level": base_level,
            "slot_spent_level": slot_spent_level,
            "slots_remaining": slots_remaining,
            "concentration_started": concentration_started,
            "concentration_ended": concentration_ended,
            "reason": reason.trim(),
            "updated": ["spell_slots", "concentration"],
            "card_revision": self.player_character.card_revision,
            "player_character": self.player_character_export(false),
        }))
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
        m.insert(
            "distinctive_features".to_string(),
            json!(pc.distinctive_features),
        );
        m.insert(
            "current_appearance".to_string(),
            json!(pc.current_appearance),
        );
        m.insert("life_status".to_string(), json!(pc.life_status));
        m.insert("life_status_note".to_string(), json!(pc.life_status_note));
        m.insert("condition".to_string(), json!(pc.condition));
        m.insert("personality".to_string(), json!(pc.personality));
        m.insert("values".to_string(), json!(pc.values));
        m.insert("abilities".to_string(), Value::Object(pc.abilities.clone()));
        m.insert("skills".to_string(), Value::Object(pc.skills.clone()));
        m.insert(
            "saving_throws".to_string(),
            Value::Object(pc.saving_throws.clone()),
        );
        m.insert(
            "passive_perception".to_string(),
            opt_int(pc.passive_perception),
        );
        m.insert("ac".to_string(), pc.ac.clone());
        m.insert("hp".to_string(), Value::Object(pc.hp.clone()));
        m.insert("speed".to_string(), json!(pc.speed));
        m.insert("senses".to_string(), json!(pc.senses));
        m.insert("languages".to_string(), json!(pc.languages));
        m.insert("inventory".to_string(), json!(pc.inventory));
        m.insert("equipment".to_string(), json!(pc.equipment));
        m.insert("features".to_string(), json!(pc.features));
        // Фаза С §С1: spells/slots/concentration ride the same UI/tool export so
        // the server /state payload and update_player_character results carry them.
        m.insert(
            "spells".to_string(),
            serde_json::to_value(&pc.spells).unwrap_or(Value::Array(Vec::new())),
        );
        m.insert(
            "spell_slots".to_string(),
            Value::Object(pc.spell_slots.clone()),
        );
        m.insert(
            "spell_slots_max".to_string(),
            Value::Object(pc.spell_slots_max.clone()),
        );
        m.insert("concentration".to_string(), json!(pc.concentration));
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
            ("Current appearance", json!(pc.current_appearance)),
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
        // Phase C §C1 spells block: one line per known spell (name, level or
        // `cantrip`, concentration/ritual marks, effect prose), then a compact
        // slots line `Slots: level 1: 3/4, level 2: 1/2` computed from spell_slots vs
        // spell_slots_max, then the active concentration line. All engine-facing
        // context text so the GM sees exactly what the card holds.
        let spells: Vec<String> = pc
            .spells
            .iter()
            .filter(|sp| !sp.name.trim().is_empty())
            .map(|sp| {
                let level = if sp.level == 0 {
                    "cantrip".to_string()
                } else {
                    format!("level {}", sp.level)
                };
                let mut marks = Vec::new();
                if sp.concentration {
                    marks.push("conc.");
                }
                if sp.ritual {
                    marks.push("ritual");
                }
                let mut parts = vec![format!("{} ({level}", sp.name.trim())];
                if !marks.is_empty() {
                    parts.push(format!(", {}", marks.join(", ")));
                }
                let mut head = parts.join("");
                head.push(')');
                let effect = sp.effect.trim();
                if effect.is_empty() {
                    head
                } else {
                    format!("{head}: {effect}")
                }
            })
            .collect();
        if !spells.is_empty() {
            lines.push(format!("Spells: {}", spells.join("; ")));
        }
        let slots = spell_slots_line(&pc.spell_slots, &pc.spell_slots_max);
        if !slots.is_empty() {
            lines.push(format!("Slots: {slots}"));
        }
        if !pc.concentration.trim().is_empty() {
            lines.push(format!("Concentration: {}", pc.concentration.trim()));
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
        let total = payload.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
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
        let changed = Self::apply_npc_character_fields(npc, &map, DEBUG_NPC_CHARACTER_FIELDS);
        if changed.iter().any(|field| field != "color") {
            npc.card_revision += 1;
        }
        true
    }

    /// Apply a live-GM partial patch to one existing NPC card. This deliberately
    /// excludes private `knowledge`/`secret`/`goals` and debug-only bookkeeping.
    pub fn update_npc_character(
        &mut self,
        npc_id: &str,
        fields: &Value,
        reason: &str,
    ) -> Result<Value, String> {
        let resolved_id = npc_id.trim().to_string();
        if resolved_id.is_empty() || !self.npcs.contains_key(&resolved_id) {
            return Err(format!(
                "no such NPC id: {}. Use an exact npc_id from the current roster.",
                npc_id.trim()
            ));
        }
        let map = match fields {
            Value::Object(map) => map.clone(),
            _ => return Err("update_character requires an object in `fields`.".to_string()),
        };
        let allowed: BTreeSet<&str> = GM_NPC_CHARACTER_FIELDS.iter().copied().collect();
        let unsupported: Vec<String> = map
            .keys()
            .filter(|key| {
                !allowed.contains(key.as_str()) && key.as_str() != "distinctive_features_add"
            })
            .cloned()
            .collect();
        if !unsupported.is_empty() {
            return Err(format!(
                "Fields are not editable for an NPC through update_character: {}.",
                unsupported.join(", ")
            ));
        }

        let npc = self
            .npcs
            .get_mut(&resolved_id)
            .expect("resolved NPC must exist");
        let mut changed = Self::apply_npc_character_fields(npc, &map, GM_NPC_CHARACTER_FIELDS);
        if let Some(additions) = map.get("distinctive_features_add") {
            if Self::append_distinctive_features(&mut npc.distinctive_features, additions) {
                changed.insert("distinctive_features".to_string());
            }
        }
        if !changed.is_empty() {
            npc.card_revision += 1;
        }
        let revision = npc.card_revision;
        let updated: Vec<String> = changed.into_iter().collect();
        let label = if npc.public_label.trim().is_empty() {
            npc.name.clone()
        } else {
            npc.public_label.clone()
        };
        Ok(json!({
            "ok": true,
            "target": "npc",
            "npc_id": resolved_id,
            "label": label,
            "updated": updated,
            "reason": reason.trim(),
            "card_revision": revision,
        }))
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
        let existing: BTreeSet<String> = self
            .fact_records
            .iter()
            .map(|r| r.fact_id.clone())
            .collect();
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

    pub fn set_story_brief(&mut self, text: &str) {
        self.story_brief = text.trim().to_string();
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
        let mut known_in = BTreeSet::new();
        known_in.insert("gm_private".to_string());
        let rumor = Rumor {
            rumor_id: crate::canon::ids::stable_id(
                &self.world_canon.world_seed,
                "debug",
                "rumor",
                &format!("{}:{text}", self.rumor_seq),
            ),
            seq: self.rumor_seq,
            turn: 0,
            speaker: nonempty_or(speaker.trim().to_string(), "слух"),
            text,
            witnesses: BTreeSet::new(),
            origin_scope: "gm_private".to_string(),
            known_in,
            carriers: BTreeSet::new(),
            strength: 1,
            distortion: 0,
            created_minutes: self
                .time
                .absolute_minutes
                .max(self.world_canon.clock_minutes),
            last_spread_minutes: self
                .time
                .absolute_minutes
                .max(self.world_canon.clock_minutes),
            confirmed: false,
        };
        self.rumors.push(rumor.clone());
        truncate_tail(&mut self.rumors, rumors_cap);
        self.prune_rumor_memory_to_live_ids();
        if self.rumors.iter().any(|r| r.rumor_id == rumor.rumor_id) {
            self.sync_rumor_memory(&rumor);
        }
        // After truncation the returned rumor is still the last appended one.
        Some(rumor)
    }

    pub fn remove_rumor(&mut self, seq: &Value) -> bool {
        let target = match as_int_or_none(seq) {
            Some(s) => s,
            None => return false,
        };
        let before = self.rumors.len();
        let removed_ids: Vec<String> = self
            .rumors
            .iter()
            .filter(|r| r.seq == target)
            .map(crate::canon::rumor::memory_id_for_rumor)
            .collect();
        self.rumors.retain(|r| r.seq != target);
        let changed = self.rumors.len() < before;
        if changed {
            for id in removed_ids {
                self.world_canon.memory.units.remove(&id);
            }
        }
        changed
    }

    pub fn set_rumor_confirmed(&mut self, seq: &Value, confirmed: bool) -> bool {
        let target = match as_int_or_none(seq) {
            Some(s) => s,
            None => return false,
        };
        let mut changed = None;
        for rumor in self.rumors.iter_mut() {
            if rumor.seq == target {
                rumor.confirmed = confirmed;
                changed = Some(rumor.clone());
                break;
            }
        }
        if let Some(rumor) = changed {
            self.sync_rumor_memory(&rumor);
            return true;
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

        let pick =
            |key: &str, default: Value| -> Value { patch.get(key).cloned().unwrap_or(default) };

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
            if let Ok(Some(payload)) =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    r.retrieve_world_fact(query, actor_id)
                }))
            {
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
        let memory_access = self.memory_access_for_actor(actor_id);
        for unit in self
            .world_canon
            .memory
            .query(&memory_access, query, 3, false)
        {
            matches.push(format!("{}: {}", unit.truth_status.as_str(), unit.summary));
        }
        if !matches.is_empty() {
            let joined = matches
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ");
            return WorldFact::new("known", joined, Vec::new());
        }

        let mut rumor_matches: Vec<String> = Vec::new();
        for rumor in &self.rumors {
            if rumor.strength <= 0 {
                continue;
            }
            if !self.rumor_visible_to_access(rumor, &memory_access) {
                continue;
            }
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

fn gm_memory_snapshot_line(unit: &MemoryUnit) -> String {
    let prefix = match unit.truth_status.as_str() {
        "actual" => "Known: ",
        "claim" => "Claim: ",
        "rumor" => "Rumor: ",
        "belief" => "Belief: ",
        "lie" => "Disputed: ",
        _ => "Uncertain: ",
    };
    let summary = unit.summary.trim();
    let mut line = if summary.is_empty() {
        format!("{prefix}(empty memory summary)")
    } else {
        format!("{prefix}{summary}")
    };
    if !unit.uncertainties.is_empty() {
        line.push_str(" Uncertainty: ");
        line.push_str(&unit.uncertainties.join("; "));
    }
    truncate_text_chars(&line, 240)
}

fn truncate_text_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let mut out: String = text.chars().take(keep).collect();
    out.push_str("...");
    out
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

fn words_intersect(
    a: &std::collections::BTreeSet<String>,
    b: &std::collections::BTreeSet<String>,
) -> bool {
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

/// K2.1 per-value stat coercion — see [`World::normalize_stat_dict`] for the
/// contract this implements. Kept as a free fn so unit tests can exercise every
/// value shape directly without a `World`.
fn normalize_stat_value(v: Value) -> Value {
    match v {
        Value::Number(n) => {
            // Reject non-finite floats (NaN/±inf); serde only yields these via an
            // f64 that failed the finite check on construction, but guard anyway.
            match n.as_f64() {
                Some(f) if f.is_finite() => Value::Number(n),
                Some(_) => Value::Null,
                // Big integers report `None` from as_f64 but are still finite and
                // exact — keep them verbatim.
                None => Value::Number(n),
            }
        }
        Value::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                return Value::String(s);
            }
            // Integers first so exact whole values never round-trip through f64.
            if let Ok(i) = t.parse::<i64>() {
                return json!(i);
            }
            // Then a finite float; a NaN/inf literal string is left textual.
            if let Ok(f) = t.parse::<f64>() {
                if f.is_finite() {
                    if let Some(num) = serde_json::Number::from_f64(f) {
                        return Value::Number(num);
                    }
                }
            }
            // Genuine text (e.g. "13 (кожаный доспех)"-style notes) stays verbatim.
            Value::String(s)
        }
        // Bool/null/array/object are not numeric stats — keep verbatim.
        other => other,
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
                    destination: crate::helpers::normalize_slug_like(&nonempty_or(
                        get_str(m, "destination"),
                        default_dest,
                    )),
                    visible: m.get("visible").map(as_bool_pyish).unwrap_or(true),
                    blocked_by: get_str(m, "blocked_by"),
                });
            }
            _ => {
                let name = as_str(exit_);
                if !name.is_empty() {
                    // "label -> location_id" (story/world-architect convention)
                    // splits into name/destination; a plain string keeps the
                    // legacy name=destination shape byte-identically.
                    let (label, target) = crate::helpers::split_exit_label(&name);
                    exits.push(SceneExit {
                        exit_id: safe_id(&name, &format!("exit_{i}")),
                        name: label,
                        destination: if target.is_empty() {
                            name
                        } else {
                            crate::helpers::normalize_slug_like(&target)
                        },
                        visible: true,
                        blocked_by: String::new(),
                    });
                }
            }
        }
    }
    exits
}

/// Initial seed exit coercion kept for compatibility with legacy story seeds.
fn coerce_initial_scene_exits(raw: Option<&Value>) -> Vec<SceneExit> {
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
                    destination: crate::helpers::normalize_slug_like(&nonempty_or(
                        get_str(m, "destination"),
                        "неизвестное направление",
                    )),
                    visible: m.get("visible").map(as_bool_pyish).unwrap_or(true),
                    blocked_by: get_str(m, "blocked_by"),
                });
            }
            _ => {
                let name = as_str(exit_);
                if !name.is_empty() {
                    // "label -> location_id" (architect convention) carries a
                    // real destination; only a plain string keeps the legacy
                    // "unknown destination" placeholder.
                    let (label, target) = crate::helpers::split_exit_label(&name);
                    exits.push(SceneExit {
                        exit_id: safe_id(&name, &format!("exit_{i}")),
                        name: label,
                        destination: if target.is_empty() {
                            "unknown destination".to_string()
                        } else {
                            crate::helpers::normalize_slug_like(&target)
                        },
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
        active: state_record::state_record_active(data.get("active").unwrap_or(&Value::Null), true),
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
        metadata: state_record::state_record_metadata(data.get("metadata").unwrap_or(&Value::Null)),
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
    if !npc.current_appearance.is_empty() {
        visible_bits.push(npc.current_appearance.clone());
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
    format!("Конкретный персонаж текущего мира.{role} Подробности появятся, когда игрок их узнает.")
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

fn legacy_actor_scope(raw: &str) -> Option<String> {
    let id = actor_key(raw);
    match id.as_str() {
        "" => None,
        "player" | "pc" => Some("player".to_string()),
        "public" => Some("legacy_public".to_string()),
        "gm" | "debug" | "system" => Some("gm_private".to_string()),
        _ => Some(format!("actor:{id}")),
    }
}

fn legacy_state_record_owner_scope(record: &StateRecord) -> String {
    match state_record_scope(&record.scope).as_str() {
        "public" => "legacy_public".to_string(),
        "gm" => "gm_private".to_string(),
        "owner" => legacy_actor_scope(&record.owner)
            .or_else(|| legacy_actor_scope(&record.source_npc))
            .or_else(|| legacy_actor_scope(&record.entity_id))
            .unwrap_or_else(|| "gm_private".to_string()),
        "subject" => legacy_actor_scope(&record.subject)
            .or_else(|| legacy_actor_scope(&record.entity_id))
            .unwrap_or_else(|| "gm_private".to_string()),
        "participants" => record
            .participants
            .iter()
            .find_map(|id| legacy_actor_scope(id))
            .or_else(|| legacy_actor_scope(&record.owner))
            .or_else(|| legacy_actor_scope(&record.source_npc))
            .or_else(|| legacy_actor_scope(&record.subject))
            .unwrap_or_else(|| "legacy_public".to_string()),
        _ => "legacy_public".to_string(),
    }
}

fn push_scope(out: &mut Vec<String>, seen: &mut BTreeSet<String>, scope: String) {
    if !scope.is_empty() && seen.insert(scope.clone()) {
        out.push(scope);
    }
}

fn legacy_state_record_visibility_scopes(record: &StateRecord, owner_scope: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    match state_record_scope(&record.scope).as_str() {
        "public" => push_scope(&mut out, &mut seen, "legacy_public".to_string()),
        "gm" => {}
        "owner" | "subject" => {
            push_scope(&mut out, &mut seen, owner_scope.to_string());
        }
        "participants" => {
            push_scope(&mut out, &mut seen, owner_scope.to_string());
            for participant in &record.participants {
                if let Some(scope) = legacy_actor_scope(participant) {
                    push_scope(&mut out, &mut seen, scope);
                }
            }
            for id in [&record.owner, &record.source_npc, &record.subject] {
                if let Some(scope) = legacy_actor_scope(id) {
                    push_scope(&mut out, &mut seen, scope);
                }
            }
        }
        _ => push_scope(&mut out, &mut seen, "legacy_public".to_string()),
    }
    out
}

fn legacy_state_record_truth_status(record: &StateRecord) -> MemoryTruthStatus {
    let kind = state_record_kind(&record.kind);
    if kind == "rumor" {
        return MemoryTruthStatus::Rumor;
    }
    match record.status.trim().to_lowercase().as_str() {
        "known" | "current" | "present" | "confirmed" | "actual" | "true" => {
            MemoryTruthStatus::Actual
        }
        "rumor" | "rumoured" | "unconfirmed" => MemoryTruthStatus::Rumor,
        "belief" | "believed" | "opinion" | "suspected" => MemoryTruthStatus::Belief,
        "lie" | "false" | "deception" => MemoryTruthStatus::Lie,
        "" | "unknown" => MemoryTruthStatus::Unknown,
        _ => MemoryTruthStatus::Claim,
    }
}

fn push_actor_id(out: &mut Vec<String>, seen: &mut BTreeSet<String>, id: &str) {
    let id = actor_key(id);
    if !id.is_empty() && seen.insert(id.clone()) {
        out.push(id);
    }
}

fn legacy_state_record_actor_ids(record: &StateRecord) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for id in [
        record.owner.as_str(),
        record.subject.as_str(),
        record.entity_id.as_str(),
        record.source_npc.as_str(),
    ] {
        push_actor_id(&mut out, &mut seen, id);
    }
    for id in &record.participants {
        push_actor_id(&mut out, &mut seen, id);
    }
    out
}

fn legacy_state_record_place_ids(record: &StateRecord) -> Vec<String> {
    let id = actor_key(&record.location_id);
    if id.is_empty() {
        Vec::new()
    } else {
        vec![id]
    }
}

fn legacy_state_record_topic_tags(record: &StateRecord, known_name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for tag in [
        "legacy_state_record".to_string(),
        state_record_kind(&record.kind),
        record.status.clone(),
        record.importance.clone(),
    ] {
        push_actor_id(&mut out, &mut seen, &tag);
    }
    if !known_name.trim().is_empty() {
        push_actor_id(&mut out, &mut seen, "known_name");
    }
    for tag in record.tags.iter().chain(record.aliases.iter()) {
        push_actor_id(&mut out, &mut seen, tag);
    }
    out
}

fn apply_state_record_update_map(rec: &mut StateRecord, m: &Map<String, Value>) {
    if m.contains_key("kind") {
        rec.kind = state_record_kind(&get_str(m, "kind"));
    }
    if m.contains_key("text") {
        let text = get_str(m, "text");
        if !text.is_empty() {
            rec.text = text;
        }
    }
    if m.contains_key("scope") {
        rec.scope = state_record_scope(&get_str(m, "scope"));
    }
    if m.contains_key("active") {
        rec.active = state_record::state_record_active(m.get("active").unwrap(), rec.active);
    }
    if m.contains_key("owner") || m.contains_key("owner_id") {
        rec.owner = first_nonempty(m, &["owner", "owner_id"]);
    }
    if m.contains_key("subject") || m.contains_key("subject_id") {
        rec.subject = first_nonempty(m, &["subject", "subject_id"]);
    }
    if m.contains_key("source") {
        rec.source = get_str(m, "source");
    }
    if m.contains_key("status") {
        rec.status = nonempty_or(get_str(m, "status"), "known");
    }
    if m.contains_key("tags") {
        rec.tags = state_record::state_record_tags(m.get("tags").unwrap());
    }
    if m.contains_key("entity_id") || m.contains_key("entity") || m.contains_key("about") {
        rec.entity_id = first_nonempty(m, &["entity_id", "entity", "about"]);
    }
    if m.contains_key("source_npc") || m.contains_key("source_npc_id") {
        rec.source_npc = first_nonempty(m, &["source_npc", "source_npc_id"]);
    }
    if m.contains_key("participants") {
        rec.participants = state_record::state_record_participants(m.get("participants").unwrap());
    }
    if m.contains_key("location_id") {
        rec.location_id = get_str(m, "location_id");
    }
    if m.contains_key("location_name") {
        rec.location_name = get_str(m, "location_name");
    }
    if m.contains_key("region_id") {
        rec.region_id = get_str(m, "region_id");
    }
    if m.contains_key("region_name") {
        rec.region_name = get_str(m, "region_name");
    }
    if m.contains_key("scene_id") {
        rec.scene_id = get_str(m, "scene_id");
    }
    if m.contains_key("importance") {
        rec.importance = get_str(m, "importance");
    }
    if m.contains_key("aliases") {
        rec.aliases = state_record::state_record_aliases(m.get("aliases").unwrap());
    }
    if m.contains_key("metadata") {
        rec.metadata = state_record::state_record_metadata(m.get("metadata").unwrap());
    }
}

fn memory_meta_string(unit: &MemoryUnit, key: &str) -> String {
    unit.metadata
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string()
}

fn memory_meta_array(unit: &MemoryUnit, key: &str) -> Vec<String> {
    unit.metadata
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn memory_unit_to_state_record(unit: &MemoryUnit) -> Option<StateRecord> {
    if unit.source_state_record_ids.is_empty()
        || !matches!(
            unit.created_by.as_str(),
            "legacy_state_record_migration" | "world_state_memory"
        )
    {
        return None;
    }
    let mut record_id = memory_meta_string(unit, "record_id");
    if record_id.is_empty() {
        record_id = unit.source_state_record_ids.first()?.clone();
    }
    let known_name = memory_meta_string(unit, "known_name");
    let mut metadata = Map::new();
    if !known_name.is_empty() {
        metadata.insert("known_name".to_string(), json!(known_name));
    }
    Some(StateRecord {
        record_id,
        kind: nonempty_or(memory_meta_string(unit, "legacy_kind"), "fact"),
        text: unit.summary.clone(),
        scope: nonempty_or(memory_meta_string(unit, "legacy_scope"), "public"),
        active: unit.injection_state != MemoryInjectionState::Archived,
        owner: memory_meta_string(unit, "owner"),
        subject: memory_meta_string(unit, "subject"),
        source: memory_meta_string(unit, "source"),
        status: nonempty_or(memory_meta_string(unit, "status"), "known"),
        tags: memory_meta_array(unit, "tags"),
        entity_id: memory_meta_string(unit, "entity_id"),
        source_npc: memory_meta_string(unit, "source_npc"),
        participants: memory_meta_array(unit, "participants"),
        location_id: memory_meta_string(unit, "location_id"),
        location_name: memory_meta_string(unit, "location_name"),
        region_id: memory_meta_string(unit, "region_id"),
        region_name: memory_meta_string(unit, "region_name"),
        scene_id: memory_meta_string(unit, "scene_id"),
        importance: memory_meta_string(unit, "importance"),
        aliases: memory_meta_array(unit, "aliases"),
        metadata,
    })
}

fn state_record_matches_query(record: &StateRecord, query: &StateRecordQuery) -> bool {
    if let Some(active) = query.active {
        if record.active != active {
            return false;
        }
    }
    if let Some(kinds) = query.kinds.as_ref() {
        let allowed: BTreeSet<String> = kinds.iter().map(|k| state_record_kind(k)).collect();
        if !allowed.contains(&state_record_kind(&record.kind)) {
            return false;
        }
    }
    if let Some(scopes) = query.scopes.as_ref() {
        let allowed: BTreeSet<String> = scopes.iter().map(|s| state_record_scope(s)).collect();
        if !allowed.contains(&state_record_scope(&record.scope)) {
            return false;
        }
    }
    let owner_filter = actor_key(query.owner);
    let subject_filter = actor_key(query.subject);
    let entity_filter = actor_key(query.entity_id);
    let source_npc_filter = actor_key(query.source_npc);
    let location_filter = actor_key(query.location_id);
    let region_filter = actor_key(query.region_id);
    let scene_filter = actor_key(query.scene_id);
    if !owner_filter.is_empty() && actor_key(&record.owner) != owner_filter {
        return false;
    }
    if !subject_filter.is_empty() && actor_key(&record.subject) != subject_filter {
        return false;
    }
    if !entity_filter.is_empty() && actor_key(&record.entity_id) != entity_filter {
        return false;
    }
    if !source_npc_filter.is_empty() && actor_key(&record.source_npc) != source_npc_filter {
        return false;
    }
    if !location_filter.is_empty() && actor_key(&record.location_id) != location_filter {
        return false;
    }
    if !region_filter.is_empty() && actor_key(&record.region_id) != region_filter {
        return false;
    }
    if !scene_filter.is_empty() && actor_key(&record.scene_id) != scene_filter {
        return false;
    }
    state_record::state_record_visible_to(record, query.actor_id)
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

// --- Фаза С spell-slot helpers (`docs/ITEMS_AND_SPELLS_TZ.md` §С) ---------

/// Coerce a flat-slot-map value to a non-negative slot count. The apply path
/// already runs K2.1 numeric coercion over `spell_slots`, but the maps can also
/// arrive raw (seed/back-compat), so accept an int, an integral float, or a
/// numeric string; anything else (text, bool, null, missing) reads as 0. NO
/// nested {current,max} lookup — the panel forbade that shape (§5).
fn slot_int(v: Option<&Value>) -> i64 {
    match v {
        Some(Value::Number(n)) => n
            .as_i64()
            .or_else(|| {
                n.as_f64().and_then(|f| {
                    if f.fract() == 0.0 {
                        Some(f as i64)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().ok().unwrap_or(0),
        _ => 0,
    }
}

/// Render the compact `level 1: 3/4, level 2: 1/2` slots line for GM context. The
/// key set is the UNION of `spell_slots` and `spell_slots_max` keys that parse
/// as a positive integer level, sorted ascending; each entry shows the remaining
/// count over the authored max (missing max → «?»). Empty when no levels exist.
fn spell_slots_line(slots: &Map<String, Value>, max: &Map<String, Value>) -> String {
    let mut levels: BTreeSet<i64> = BTreeSet::new();
    for m in [slots, max] {
        for k in m.keys() {
            if let Ok(lvl) = k.trim().parse::<i64>() {
                if lvl > 0 {
                    levels.insert(lvl);
                }
            }
        }
    }
    let parts: Vec<String> = levels
        .into_iter()
        .map(|lvl| {
            let key = lvl.to_string();
            let cur = slot_int(slots.get(&key));
            let cap = match max.get(&key) {
                Some(v) if !v.is_null() => slot_int(Some(v)).to_string(),
                _ => "?".to_string(),
            };
            format!("level {lvl}: {cur}/{cap}")
        })
        .collect();
    parts.join(", ")
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
        "current_appearance" => json!(pc.current_appearance),
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
        // Фаза С §С1: spells serialize to the canonical object array; the flat
        // slot maps and concentration string round-trip verbatim.
        "spells" => serde_json::to_value(&pc.spells).unwrap_or(Value::Array(Vec::new())),
        "spell_slots" => Value::Object(pc.spell_slots.clone()),
        "spell_slots_max" => Value::Object(pc.spell_slots_max.clone()),
        "concentration" => json!(pc.concentration),
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
        "current_appearance" => pc.current_appearance = value_as_string(&value),
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
        // Фаза С §С1: the apply path hands `spells` an already-cleaned canonical
        // object array (see apply_player_character_fields); deserialize it back
        // into Vec<SpellEntry>, defaulting to empty on any shape surprise.
        "spells" => pc.spells = serde_json::from_value(value).unwrap_or_default(),
        "spell_slots" => pc.spell_slots = value_as_object(value),
        "spell_slots_max" => pc.spell_slots_max = value_as_object(value),
        "concentration" => pc.concentration = value_as_string(&value),
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
        "current_appearance" => json!(npc.current_appearance),
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
        "current_appearance" => npc.current_appearance = value_as_string(&value),
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

#[cfg(test)]
mod compose_authored_tests {
    use super::*;
    use crate::canon::{WorldLore, WorldSpec};
    use serde_json::json;

    /// A deterministic worldgen base (fixed numeric seed => reproducible canon).
    fn base_spec() -> WorldSpec {
        WorldSpec {
            seed: "20260622".to_string(),
            genre: "fantasy".to_string(),
            tone: "tense".to_string(),
            scale: "town".to_string(),
        }
    }

    /// Alias keys are folded into the canonical world fields: `player_brief` /
    /// `brief` -> story_brief, `canon` -> hidden_truth, `player` ->
    /// player_character, and `public` -> public_intro (through
    /// `normalize_seed`, which maps the `public` alias onto the scene/intro).
    #[test]
    fn overlay_alias_keys_populate_canonical_fields() {
        // `brief` alias (story_brief absent and player_brief absent), `canon`
        // alias (hidden_truth absent), `player` alias (player_character absent),
        // and the canonical `public_intro` key.
        let plot = json!({
            "brief": "Краткое описание для игрока.",
            "public_intro": "Публичное вступление в сцену.",
            "canon": "Скрытая истина, известная только мастеру.",
            "player": {"name": "Тестовый Герой", "class_role": "следователь"},
        });
        let world = World::compose_authored(&base_spec(), WorldLore::default(), &plot);

        assert_eq!(world.story_brief, "Краткое описание для игрока.");
        assert_eq!(world.public, "Публичное вступление в сцену.");
        assert_eq!(world.canon, "Скрытая истина, известная только мастеру.");
        assert_eq!(world.player_character.name, "Тестовый Герой");

        // `player_brief` alias takes precedence over `brief` (matches the
        // story_brief precedence ladder: story_brief > player_brief > brief).
        let plot2 = json!({
            "player_brief": "Бриф из player_brief.",
            "brief": "Бриф из brief.",
        });
        let world2 = World::compose_authored(&base_spec(), WorldLore::default(), &plot2);
        assert_eq!(world2.story_brief, "Бриф из player_brief.");
    }

    /// A plot with NO npcs must NOT inject the authored "stranger" default over
    /// the procedural base — the roster stays exactly as worldgen left it (now
    /// empty; actors are generated lazily at play time, not hardcoded).
    #[test]
    fn empty_plot_preserves_worldgen_roster() {
        let generated = World::from_worldgen_with_dice_seed(&base_spec(), 20260622);
        let generated_ids: BTreeSet<String> = generated.npcs.keys().cloned().collect();
        assert!(
            generated_ids.is_empty(),
            "procedural worldgen now seeds no actors"
        );

        // An empty plot (no `npcs` key) overlaid on the same deterministic base.
        let plot = json!({"brief": "Только текст, без ролей."});
        let composed = World::compose_authored(&base_spec(), WorldLore::default(), &plot);
        let composed_ids: BTreeSet<String> = composed.npcs.keys().cloned().collect();

        assert_eq!(
            composed_ids, generated_ids,
            "empty plot must not inject a default NPC over the worldgen roster"
        );
    }

    /// An authored scene upserts its place into the canon while the generated
    /// places remain reachable (set_scene adds, it does not rebuild).
    #[test]
    fn authored_scene_upserts_place_keeping_generated_places() {
        let generated = World::from_worldgen_with_dice_seed(&base_spec(), 20260622);
        let generated_place_ids: BTreeSet<String> =
            generated.world_canon.places.keys().cloned().collect();
        assert!(
            generated_place_ids.len() >= 2,
            "worldgen must produce several places for this test to be meaningful"
        );

        let plot = json!({
            "brief": "Сцена с авторским местом.",
            "scene": {
                "location_id": "authored_start_room",
                "title": "Авторская комната",
                "description": "Тесная комната, которой не было в процедурном мире.",
            },
        });
        let composed = World::compose_authored(&base_spec(), WorldLore::default(), &plot);

        // The authored place was upserted into the canon.
        assert!(
            composed
                .world_canon
                .places
                .contains_key("authored_start_room"),
            "authored scene place must be present in the canon"
        );
        // The current scene points at the authored location.
        assert_eq!(composed.scene.location_id, "authored_start_room");

        // Every generated place still exists (set_scene added, did not replace).
        for id in &generated_place_ids {
            assert!(
                composed.world_canon.places.contains_key(id),
                "generated place {id} must remain after authored scene upsert"
            );
        }
        assert!(
            composed.world_canon.places.len() > generated_place_ids.len(),
            "authored scene must ADD a place, keeping the generated ones"
        );
    }
}

#[cfg(test)]
mod from_seed_normalize_tests {
    use super::*;
    use serde_json::json;

    /// A NON-strict-shape seed (missing the `scene`-object + `npcs`-list +
    /// items/exits/title strict shape) must keep its custom `player_character`
    /// through `normalize_seed`'s rebuild path. Before the fix the rebuild
    /// emitted a fixed key set that dropped `player_character`, so the world
    /// silently launched the default hero "Искатель".
    #[test]
    fn non_strict_seed_keeps_custom_player_character() {
        let seed = json!({
            "id": "loose_story",
            "title": "Свободная история",
            "player_character": {"name": "Тест", "class_role": "маг"},
        });
        // Sanity: this seed must NOT satisfy the strict-shape short-circuit,
        // otherwise the test would pass trivially without exercising the fix.
        assert!(
            !crate::seed::normalize_seed(&seed).is_empty(),
            "normalize_seed must produce output for this seed"
        );

        let world = World::from_seed(&seed);
        assert_eq!(
            world.player_character.name, "Тест",
            "custom player_character name must survive normalization (not default hero)"
        );
        assert_ne!(
            world.player_character.name,
            PlayerCharacter::default().name,
            "player character must not fall back to the default hero"
        );
        assert_eq!(world.player_character.class_role, "маг");
    }

    /// The `player` alias on a non-strict-shape seed must be honored the same
    /// way (folded onto the canonical `player_character` field).
    #[test]
    fn non_strict_seed_honors_player_alias() {
        let seed = json!({
            "id": "loose_story_alias",
            "title": "История с алиасом",
            "player": {"name": "Алиас", "class_role": "воин"},
        });
        let world = World::from_seed(&seed);
        assert_eq!(
            world.player_character.name, "Алиас",
            "`player` alias must seed the player character on a non-strict seed"
        );
        assert_eq!(world.player_character.class_role, "воин");
    }
}
