//! World data model — faithful ports of the dataclasses in world.py.
//!
//! Field names and defaults match Python EXACTLY (persistence in gml-persistence
//! relies on these names). Loosely-typed fields (`ac:Any`, `abilities/skills/
//! hp:dict`, `metadata:dict`) map to `serde_json::Value` / `Map`. Tuples become
//! `Vec<String>`; `frozenset` witnesses become a sorted `Vec<String>` (Python
//! always serializes them via `sorted(...)`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use gml_types::{ContentLocale, NpcBeat};

fn default_true() -> bool {
    true
}

/// `Event` — PUBLIC observable scene event. Holds only speech/action; the class
/// docstring forbids reasoning/claims/secret/canon.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldEvent {
    pub seq: i64,
    pub turn: i64,
    /// Absolute game time in minutes when this observable beat was recorded.
    /// Older saves do not carry it; `0` means "unknown/legacy start".
    #[serde(default)]
    pub time_minutes: i64,
    pub actor: String,
    pub kind: String,
    #[serde(default)]
    pub response: String,
    #[serde(default)]
    pub beats: Vec<NpcBeat>,
    #[serde(default)]
    pub speech: String,
    #[serde(default)]
    pub action: String,
    /// `witnesses: frozenset` — stored sorted for deterministic output.
    #[serde(default)]
    pub witnesses: BTreeSet<String>,
}

/// `NPC` dataclass — every field, exact name & default. `secret`/`knowledge`
/// are SENSITIVE and never enter any player/GM/RAG projection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Npc {
    pub npc_id: String,
    pub name: String,
    pub persona: String,
    pub voice: String,
    pub goals: String,
    pub knowledge: String,
    pub secret: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub pronouns: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub public_label: String,
    #[serde(default)]
    pub age: String,
    #[serde(default)]
    pub physical_type: String,
    #[serde(default)]
    pub distinctive_features: String,
    /// Complete visible presentation right now: clothing, hair, dirt, blood,
    /// disguise, and other changeable details. Permanent traits stay in
    /// `physical_type` / `distinctive_features`.
    #[serde(default)]
    pub current_appearance: String,
    #[serde(default = "alive")]
    pub life_status: String,
    #[serde(default)]
    pub life_status_note: String,
    #[serde(default)]
    pub condition: String,
    #[serde(default)]
    pub personality: String,
    #[serde(default)]
    pub values: String,
    #[serde(default)]
    pub habits: String,
    #[serde(default)]
    pub pressure_response: String,
    #[serde(default)]
    pub boundaries: String,
    #[serde(default)]
    pub abilities: Map<String, Value>,
    #[serde(default)]
    pub skills: Map<String, Value>,
    #[serde(default)]
    pub saving_throws: Map<String, Value>,
    #[serde(default)]
    pub passive_perception: Option<i64>,
    #[serde(default)]
    pub ac: Value,
    #[serde(default)]
    pub hp: Map<String, Value>,
    #[serde(default)]
    pub speed: String,
    #[serde(default)]
    pub senses: String,
    #[serde(default)]
    pub languages: String,
    #[serde(default)]
    pub default_whereabouts: Option<Map<String, Value>>,
    #[serde(default)]
    pub card_revision: i64,
}

fn alive() -> String {
    "alive".to_string()
}

/// Фаза С spell record (`docs/ITEMS_AND_SPELLS_TZ.md` §С1). A DELIBERATELY thin
/// 5-field 5e subset: the engine reads only `name` (cast/slot matching),
/// `level` (0 = заговор/cantrip, spends no slot; else the base slot level),
/// `concentration` (a cast sets `pc.concentration`), and `ritual` (a display
/// mark; ritual casting stays narrative in v1 — no engine branch). Everything
/// else — school, casting time, range, duration, upcast text — lives as PROSE
/// inside `effect`; the panel cut those 5 dead free-string fields because the
/// engine never reads them (see §5: NO 10-field SpellEntry). All fields are
/// `#[serde(default)]` so old saves and partial packages deserialize cleanly.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpellEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub level: u8,
    #[serde(default)]
    pub concentration: bool,
    #[serde(default)]
    pub ritual: bool,
    #[serde(default)]
    pub effect: String,
}

/// `PlayerCharacter` dataclass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlayerCharacter {
    pub name: String,
    pub pronouns: String,
    pub class_role: String,
    pub level: Option<i64>,
    pub background: String,
    pub age: String,
    pub physical_type: String,
    #[serde(default)]
    pub distinctive_features: String,
    /// Complete visible presentation right now. This is intentionally one
    /// mutable field instead of separate outfit / hair / visible-state fields.
    #[serde(default)]
    pub current_appearance: String,
    pub life_status: String,
    #[serde(default)]
    pub life_status_note: String,
    #[serde(default)]
    pub condition: String,
    #[serde(default)]
    pub personality: String,
    #[serde(default)]
    pub values: String,
    #[serde(default)]
    pub gm_notes: String,
    pub abilities: Map<String, Value>,
    pub skills: Map<String, Value>,
    #[serde(default)]
    pub saving_throws: Map<String, Value>,
    pub passive_perception: Option<i64>,
    pub ac: Value,
    pub hp: Map<String, Value>,
    pub speed: String,
    pub senses: String,
    pub languages: String,
    pub inventory: Vec<String>,
    #[serde(default)]
    pub equipment: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
    // Фаза С (`docs/ITEMS_AND_SPELLS_TZ.md` §С1) — all additive `serde(default)`:
    // old saves/packages lacking these keys load with empty spells/slots.
    /// Known spells, objects (NOT a `Vec<String>` — bespoke apply/parse path).
    #[serde(default)]
    pub spells: Vec<SpellEntry>,
    /// FLAT «level → remaining slots» dict, e.g. `{"1": 3, "2": 1}`. Flat so it
    /// rides the K2.1 stat-dict numeric coercion (no nested {current,max} — §5).
    #[serde(default)]
    pub spell_slots: Map<String, Value>,
    /// FLAT «level → max slots» authored ceilings; an unwritten level = no slots.
    #[serde(default)]
    pub spell_slots_max: Map<String, Value>,
    /// Name of the active concentration spell; "" = none (a plain text field).
    #[serde(default)]
    pub concentration: String,
    #[serde(default)]
    pub card_revision: i64,
}

impl PlayerCharacter {
    pub fn for_locale(locale: ContentLocale) -> Self {
        let mut abilities = Map::new();
        for (k, v) in [
            ("STR", 10),
            ("DEX", 12),
            ("CON", 11),
            ("INT", 13),
            ("WIS", 14),
            ("CHA", 12),
        ] {
            abilities.insert(k.to_string(), Value::from(v));
        }
        let mut skills = Map::new();
        for (k, v) in [
            ("Investigation", 3),
            ("Perception", 4),
            ("Insight", 4),
            ("Persuasion", 3),
        ] {
            skills.insert(k.to_string(), Value::from(v));
        }
        let mut hp = Map::new();
        hp.insert("current".to_string(), Value::from(9));
        hp.insert("max".to_string(), Value::from(9));
        let (name, class_role, background, age, physical_type, senses, languages, inventory) =
            match locale {
                ContentLocale::Russian => (
                    "Искатель",
                    "сыщик-авантюрист",
                    "странствующий расследователь",
                    "Взрослый персонаж; точный возраст не задан.",
                    "обычный гуманоид среднего размера",
                    "обычное зрение",
                    "Общий",
                    ["дорожная одежда", "кинжал", "фонарь", "записная книжка"],
                ),
                ContentLocale::English => (
                    "Seeker",
                    "adventuring investigator",
                    "wandering investigator",
                    "Adult character; exact age is unspecified.",
                    "ordinary Medium humanoid",
                    "normal vision",
                    "Common",
                    ["traveler's clothes", "dagger", "lantern", "notebook"],
                ),
            };
        PlayerCharacter {
            name: name.to_string(),
            pronouns: "OTHER".to_string(),
            class_role: class_role.to_string(),
            level: Some(1),
            background: background.to_string(),
            age: age.to_string(),
            physical_type: physical_type.to_string(),
            distinctive_features: String::new(),
            current_appearance: String::new(),
            life_status: "alive".to_string(),
            life_status_note: String::new(),
            condition: String::new(),
            personality: String::new(),
            values: String::new(),
            gm_notes: String::new(),
            abilities,
            skills,
            saving_throws: Map::new(),
            passive_perception: Some(14),
            ac: Value::from(12),
            hp,
            speed: "30 ft".to_string(),
            senses: senses.to_string(),
            languages: languages.to_string(),
            inventory: inventory.into_iter().map(str::to_string).collect(),
            equipment: Vec::new(),
            features: Vec::new(),
            spells: Vec::new(),
            spell_slots: Map::new(),
            spell_slots_max: Map::new(),
            concentration: String::new(),
            card_revision: 0,
        }
    }
}

impl Default for PlayerCharacter {
    fn default() -> Self {
        Self::for_locale(ContentLocale::Russian)
    }
}

/// `SceneExit` dataclass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneExit {
    pub exit_id: String,
    pub name: String,
    pub destination: String,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub blocked_by: String,
}

/// `SceneItem` dataclass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneItem {
    pub item_id: String,
    pub name: String,
    pub location: String,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default)]
    pub portable: bool,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub details: String,
}

/// `Presence` dataclass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub npc_id: String,
    pub location: String,
    #[serde(default = "default_true")]
    pub visible: bool,
    #[serde(default = "default_true")]
    pub can_hear: bool,
    #[serde(default)]
    pub activity: String,
    #[serde(default)]
    pub attitude: String,
}

/// `NPCWhereabouts` dataclass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NpcWhereabouts {
    pub npc_id: String,
    #[serde(default)]
    pub location_id: String,
    #[serde(default)]
    pub location_name: String,
    #[serde(default = "unknown")]
    pub status: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub source: String,
}

fn unknown() -> String {
    "unknown".to_string()
}

impl NpcWhereabouts {
    pub fn new(npc_id: &str) -> Self {
        NpcWhereabouts {
            npc_id: npc_id.to_string(),
            location_id: String::new(),
            location_name: String::new(),
            status: "unknown".to_string(),
            details: String::new(),
            source: String::new(),
        }
    }
}

/// `FactRecord` dataclass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FactRecord {
    pub fact_id: String,
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default = "default_true")]
    pub confirmed: bool,
}

/// `Rumor` dataclass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rumor {
    #[serde(default)]
    pub rumor_id: String,
    pub seq: i64,
    pub turn: i64,
    pub speaker: String,
    pub text: String,
    #[serde(default)]
    pub witnesses: BTreeSet<String>,
    #[serde(default)]
    pub origin_scope: String,
    #[serde(default)]
    pub known_in: BTreeSet<String>,
    #[serde(default)]
    pub carriers: BTreeSet<String>,
    #[serde(default)]
    pub strength: i64,
    #[serde(default)]
    pub distortion: i64,
    #[serde(default)]
    pub created_minutes: i64,
    #[serde(default)]
    pub last_spread_minutes: i64,
    #[serde(default)]
    pub confirmed: bool,
}

/// `StateRecord` dataclass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateRecord {
    pub record_id: String,
    pub kind: String,
    pub text: String,
    #[serde(default = "public")]
    pub scope: String,
    #[serde(default = "default_true")]
    pub active: bool,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub source: String,
    #[serde(default = "known")]
    pub status: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub entity_id: String,
    #[serde(default)]
    pub source_npc: String,
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default)]
    pub location_id: String,
    #[serde(default)]
    pub location_name: String,
    #[serde(default)]
    pub region_id: String,
    #[serde(default)]
    pub region_name: String,
    #[serde(default)]
    pub scene_id: String,
    #[serde(default)]
    pub importance: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub metadata: Map<String, Value>,
}

fn public() -> String {
    "public".to_string()
}
fn known() -> String {
    "known".to_string()
}

/// `SceneState` dataclass. `present_npcs` is a set but iterated via `sorted()`
/// everywhere -> `BTreeSet` for free ordering. `presence` keyed by npc_id ->
/// `BTreeMap` for deterministic iteration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SceneState {
    pub scene_id: String,
    pub location_id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub present_npcs: BTreeSet<String>,
    #[serde(default)]
    pub presence: BTreeMap<String, Presence>,
    #[serde(default)]
    pub items: Vec<SceneItem>,
    #[serde(default)]
    pub exits: Vec<SceneExit>,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub tension: String,
    #[serde(default)]
    pub player_seen: Vec<String>,
}

/// Scene-local fields that are not part of the canonical place graph but must
/// follow their place when the player leaves and later returns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaceSceneContext {
    #[serde(default)]
    pub scene_id: String,
    #[serde(default)]
    pub constraints: Vec<String>,
    #[serde(default)]
    pub tension: String,
    #[serde(default)]
    pub player_seen: Vec<String>,
}

impl SceneState {
    pub fn visible_items(&self) -> Vec<&SceneItem> {
        self.items.iter().filter(|i| i.visible).collect()
    }
    pub fn visible_exits(&self) -> Vec<&SceneExit> {
        self.exits.iter().filter(|e| e.visible).collect()
    }
}

/// `WorldTime` dataclass.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldTime {
    #[serde(default)]
    pub calendar_name: String,
    #[serde(default)]
    pub absolute_minutes: i64,
    #[serde(default)]
    pub current_date_label: String,
    #[serde(default = "sixty")]
    pub minutes_per_hour: i64,
    #[serde(default = "twenty_four")]
    pub hours_per_day: i64,
    #[serde(default)]
    pub day_names: Vec<String>,
    #[serde(default)]
    pub month_names: Vec<String>,
    #[serde(default)]
    pub last_advance_minutes: i64,
    #[serde(default)]
    pub last_advance_reason: String,
}

fn sixty() -> i64 {
    60
}
fn twenty_four() -> i64 {
    24
}

impl Default for WorldTime {
    fn default() -> Self {
        WorldTime {
            calendar_name: String::new(),
            absolute_minutes: 0,
            current_date_label: String::new(),
            minutes_per_hour: 60,
            hours_per_day: 24,
            day_names: Vec::new(),
            month_names: Vec::new(),
            last_advance_minutes: 0,
            last_advance_reason: String::new(),
        }
    }
}

/// `WorldFact` frozen dataclass — return type of `World.fact()`.
#[derive(Clone, Debug, PartialEq)]
pub struct WorldFact {
    pub status: String,
    pub text: String,
    pub sources: Vec<Value>,
}

impl WorldFact {
    pub fn new(status: impl Into<String>, text: impl Into<String>, sources: Vec<Value>) -> Self {
        WorldFact {
            status: status.into(),
            text: text.into(),
            sources,
        }
    }

    /// `as_tool_payload()` — `{status, text}` plus `sources` only if non-empty.
    pub fn as_tool_payload(&self) -> Value {
        let mut m = Map::new();
        m.insert("status".to_string(), Value::String(self.status.clone()));
        m.insert("text".to_string(), Value::String(self.text.clone()));
        if !self.sources.is_empty() {
            m.insert("sources".to_string(), Value::Array(self.sources.clone()));
        }
        Value::Object(m)
    }
}
