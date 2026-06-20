"""Состояние мира, реестр NPC, кубы и код-детектор утечки секретов.

Принцип: правила и факты живут в КОДЕ, а не в голове модели. Кубы детерминированы,
секреты хранятся отдельно от контекста ГМ, утечку ловит дешёвая проверка кодом
(в дополнение к LLM-критику) — чтобы доп. раунд был виден гарантированно.
"""
from __future__ import annotations

import random
import re
import hashlib
import json
from dataclasses import dataclass, field
from typing import Any

import stories


@dataclass
class Event:
    """Публичное наблюдаемое событие сцены. ВНУТРИ — только speech/action:
    reasoning/claims/secret/canon сюда НЕ кладём (изоляция секретов)."""
    seq: int                                # глобальный монотонный порядок
    turn: int                               # номер хода
    actor: str                              # "player" | npc_id | "gm"
    kind: str                               # "speech" | "action" | "dice"
    speech: str = ""
    action: str = ""
    witnesses: frozenset = field(default_factory=frozenset)  # кто присутствовал


@dataclass
class NPC:
    npc_id: str
    name: str
    persona: str
    voice: str
    goals: str
    knowledge: str           # что персонаж вправе знать
    secret: str              # секрет; НЕ попадает в контекст ГМ
    role: str = ""           # короткая публичная роль (для ростера в инструменте ГМ)
    pronouns: str = ""       # grammatical gender marker for Russian output: M | F | N | PL | OTHER/custom
    color: str = ""          # player-facing accent hex (e.g. "#e6c08a"); empty -> theme token on the frontend
    public_label: str = ""   # player-facing label before/without identity, e.g. "трактирщик"
    age: str = ""            # free text: actual/apparent age notes if relevant
    physical_type: str = ""  # combined species/type/size/body impression
    distinctive_features: str = ""
    life_status: str = "alive"
    life_status_note: str = ""
    condition: str = ""
    personality: str = ""
    values: str = ""
    habits: str = ""
    pressure_response: str = ""
    boundaries: str = ""
    abilities: dict[str, Any] = field(default_factory=dict)
    skills: dict[str, Any] = field(default_factory=dict)
    saving_throws: dict[str, Any] = field(default_factory=dict)
    passive_perception: int | None = None
    ac: Any = None
    hp: dict[str, Any] = field(default_factory=dict)
    speed: str = ""
    senses: str = ""
    languages: str = ""
    default_whereabouts: dict | None = None  # optional card-defined offscreen whereabouts seed
    card_revision: int = 0   # bumped when a content card field changes; old snapshots default to 0


@dataclass
class PlayerCharacter:
    name: str = "Искатель"
    pronouns: str = "OTHER"
    class_role: str = "сыщик-авантюрист"
    level: int | None = 1
    background: str = "странствующий расследователь"
    age: str = "Взрослый персонаж; точный возраст не задан."
    physical_type: str = "обычный гуманоид среднего размера"
    distinctive_features: str = ""
    life_status: str = "alive"
    life_status_note: str = ""
    condition: str = ""
    personality: str = ""
    values: str = ""
    gm_notes: str = ""
    abilities: dict[str, Any] = field(default_factory=lambda: {
        "STR": 10, "DEX": 12, "CON": 11, "INT": 13, "WIS": 14, "CHA": 12,
    })
    skills: dict[str, Any] = field(default_factory=lambda: {
        "Investigation": 3, "Perception": 4, "Insight": 4, "Persuasion": 3,
    })
    saving_throws: dict[str, Any] = field(default_factory=dict)
    passive_perception: int | None = 14
    ac: Any = 12
    hp: dict[str, Any] = field(default_factory=lambda: {"current": 9, "max": 9})
    speed: str = "30 ft"
    senses: str = "обычное зрение"
    languages: str = "Общий"
    inventory: list[str] = field(default_factory=lambda: [
        "дорожная одежда", "кинжал", "фонарь", "записная книжка",
    ])
    equipment: list[str] = field(default_factory=list)
    features: list[str] = field(default_factory=list)
    card_revision: int = 0


@dataclass
class SceneExit:
    exit_id: str
    name: str
    destination: str
    visible: bool = True
    blocked_by: str = ""


@dataclass
class SceneItem:
    item_id: str
    name: str
    location: str
    visible: bool = True
    portable: bool = False
    owner: str = ""
    details: str = ""


@dataclass
class Presence:
    npc_id: str
    location: str
    visible: bool = True
    can_hear: bool = True
    activity: str = ""
    attitude: str = ""


@dataclass
class NPCWhereabouts:
    npc_id: str
    location_id: str = ""
    location_name: str = ""
    status: str = "unknown"     # present | known | likely | rumored | unknown
    details: str = ""
    source: str = ""


@dataclass
class FactRecord:
    fact_id: str
    kind: str                    # "public" | "truth" | "rumor"
    text: str
    keywords: list[str] = field(default_factory=list)
    source: str = ""
    confirmed: bool = True


@dataclass
class Rumor:
    seq: int
    turn: int
    speaker: str
    text: str
    witnesses: frozenset = field(default_factory=frozenset)
    confirmed: bool = False


@dataclass
class StateRecord:
    record_id: str
    kind: str
    text: str
    scope: str = "public"
    active: bool = True
    owner: str = ""
    subject: str = ""
    source: str = ""
    status: str = "known"
    tags: tuple[str, ...] = field(default_factory=tuple)
    entity_id: str = ""
    source_npc: str = ""
    location_id: str = ""
    location_name: str = ""
    region_id: str = ""
    region_name: str = ""
    scene_id: str = ""
    importance: str = ""
    aliases: tuple[str, ...] = field(default_factory=tuple)
    metadata: dict[str, Any] = field(default_factory=dict)


def state_record_hash(record: StateRecord) -> str:
    payload = {
        "id": record.record_id,
        "kind": _state_record_kind(record.kind),
        "text": _as_str(record.text),
        "scope": _state_record_scope(record.scope),
        "active": bool(record.active),
        "owner": _as_str(record.owner),
        "subject": _as_str(record.subject),
        "status": _as_str(record.status) or "known",
        "tags": list(record.tags or ()),
        "entity_id": _as_str(record.entity_id),
        "source_npc": _as_str(record.source_npc),
        "location_id": _as_str(record.location_id),
        "location_name": _as_str(record.location_name),
        "region_id": _as_str(record.region_id),
        "region_name": _as_str(record.region_name),
        "scene_id": _as_str(record.scene_id),
        "importance": _as_str(record.importance),
        "aliases": list(record.aliases or ()),
        "metadata": record.metadata or {},
    }
    raw = json.dumps(payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"), default=str)
    return hashlib.sha256(raw.encode("utf-8")).hexdigest()


@dataclass
class SceneState:
    scene_id: str
    location_id: str
    title: str
    description: str
    present_npcs: set[str] = field(default_factory=set)
    presence: dict[str, Presence] = field(default_factory=dict)
    items: list[SceneItem] = field(default_factory=list)
    exits: list[SceneExit] = field(default_factory=list)
    constraints: list[str] = field(default_factory=list)
    tension: str = ""
    player_seen: list[str] = field(default_factory=list)

    def visible_items(self) -> list[SceneItem]:
        return [item for item in self.items if item.visible]

    def visible_exits(self) -> list[SceneExit]:
        return [exit_ for exit_ in self.exits if exit_.visible]


@dataclass
class WorldTime:
    calendar_name: str = ""
    absolute_minutes: int = 0
    current_date_label: str = ""
    minutes_per_hour: int = 60
    hours_per_day: int = 24
    day_names: list[str] = field(default_factory=list)
    month_names: list[str] = field(default_factory=list)
    last_advance_minutes: int = 0
    last_advance_reason: str = ""


@dataclass(frozen=True)
class WorldFact:
    status: str
    text: str
    sources: list[dict] = field(default_factory=list)

    def as_tool_payload(self) -> dict:
        payload = {"status": self.status, "text": self.text}
        if self.sources:
            payload["sources"] = self.sources
        return payload


def _safe_id(raw: str, fallback: str) -> str:
    value = re.sub(r"[^a-zA-Z0-9_]+", "_", (raw or "").strip().lower()).strip("_")
    return value or fallback


def _as_list(value: Any) -> list:
    if value is None:
        return []
    if isinstance(value, list):
        return value
    if isinstance(value, tuple):
        return list(value)
    return [value]


def _as_dict(value: Any) -> dict:
    return dict(value) if isinstance(value, dict) else {}


def _as_str(value: Any) -> str:
    if value is None:
        return ""
    return str(value).strip()


def _as_joined_str(value: Any) -> str:
    if isinstance(value, list):
        return ", ".join(_as_str(item) for item in value if _as_str(item))
    return _as_str(value)


def _as_int_or_none(value: Any) -> int | None:
    if value is None or isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, float) and value.is_integer():
        return int(value)
    if isinstance(value, str):
        m = re.search(r"-?\d+", value)
        if m:
            return int(m.group(0))
    return None


def _match_words(text: str) -> set[str]:
    # Plain word tokenizer for the offline (non-RAG) rumor-search fallback.
    # NOT a keyword extractor: no length/count heuristics — facts no longer get
    # auto-tagged; what's worth keeping will be authored (later, by the model).
    return set(re.findall(r"[a-zа-яё0-9]+", _as_str(text).lower()))


def _actor_key(value: object) -> str:
    return _as_str(value).lower()


def _state_record_kind(value: object) -> str:
    raw = _safe_id(_as_str(value), "fact")
    return raw if raw in STATE_RECORD_KINDS else "fact"


def _state_record_scope(value: object) -> str:
    raw = _safe_id(_as_str(value), "public")
    raw = STATE_RECORD_SCOPE_ALIASES.get(raw, raw)
    return raw if raw in STATE_RECORD_SCOPES else "public"


def _state_record_tags(value: object) -> tuple[str, ...]:
    seen: set[str] = set()
    out: list[str] = []
    for item in _as_list(value):
        tag = _as_str(item)
        if tag and tag not in seen:
            seen.add(tag)
            out.append(tag)
    return tuple(out)


def _state_record_aliases(value: object) -> tuple[str, ...]:
    return _state_record_tags(value)


def _state_record_metadata(value: object) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    return {str(key): val for key, val in value.items() if key is not None}


def _anchor_label(name: str, identifier: str) -> str:
    name = _as_str(name)
    identifier = _as_str(identifier)
    if name and identifier and name != identifier:
        return f"{name} ({identifier})"
    return name or identifier


def _state_record_active(value: object, default: bool = True) -> bool:
    if isinstance(value, bool):
        return value
    if value is None:
        return default
    if isinstance(value, str):
        raw = value.strip().lower()
        if raw in {"1", "true", "yes", "on", "active"}:
            return True
        if raw in {"0", "false", "no", "off", "inactive"}:
            return False
    return bool(value)


ROLE_RU = {
    "innkeeper": "трактирщик",
    "serving girl": "служанка",
    "guard captain": "капитан стражи",
    "scene character": "персонаж сцены",
    "person in the starting scene": "персонаж стартовой сцены",
    "npc": "персонаж",
}

GENDER_LABELS_RU = {
    "m": "мужской род",
    "f": "женский род",
    "n": "средний род",
    "pl": "множественное число",
    "other": "другое",
}

# --- NPC whereabouts status: single source of truth ----------------------
# key -> player-facing RU label. The validator allowed-set and the /state payload
# both derive from this map; the frontend reads the labels from /state, so there
# is NO duplicated status table in the UI.
WHEREABOUTS_STATUS_LABELS = {
    "present": "в текущей сцене",
    "known": "известно",
    "likely": "вероятно",
    "rumored": "по слухам",
    "unknown": "неизвестно",
    "left_scene": "ушёл",
}
WHEREABOUTS_STATUSES = tuple(WHEREABOUTS_STATUS_LABELS)

STATE_RECORD_KINDS = ("fact", "rumor", "npc_memory", "relationship", "goal")
STATE_RECORD_SCOPES = ("public", "gm", "owner", "subject", "participants")
STATE_RECORD_SCOPE_ALIASES = {
    "private": "owner",
    "npc": "owner",
    "shared": "participants",
    "participant": "participants",
}
STATE_DEBUG_ACTORS = {"debug", "system"}
STATE_GM_ACTORS = {"gm", *STATE_DEBUG_ACTORS}

NPC_PROFILE_PRESETS = {
    "visible": (
        "public_label", "role", "physical_type", "distinctive_features",
        "condition", "life_status",
    ),
    "social": (
        "persona", "personality", "values", "habits", "pressure_response",
        "boundaries", "voice",
    ),
    "mechanics": (
        "abilities", "skills", "saving_throws", "passive_perception",
        "ac", "hp", "speed", "senses", "languages",
    ),
    "status": ("life_status", "life_status_note", "condition", "hp"),
    "identity": (
        "name", "public_label", "role", "age", "physical_type",
        "distinctive_features",
    ),
}
NPC_PROFILE_FIELDS = tuple(sorted({field for fields in NPC_PROFILE_PRESETS.values() for field in fields}))

PLAYER_CHARACTER_FIELDS = (
    "name", "pronouns", "class_role", "level", "background", "age",
    "physical_type", "distinctive_features", "life_status", "life_status_note",
    "condition", "personality", "values", "gm_notes", "abilities", "skills",
    "saving_throws", "passive_perception", "ac", "hp", "speed", "senses",
    "languages", "inventory", "equipment", "features",
)

# Machine source tags for whereabouts/facts. Named constants so the emit sites and
# the SOURCE_RU label map below cannot drift apart.
SOURCE_DEFAULT_LORE = "default public lore"
SOURCE_PREVIOUS_SCENE = "previous scene"
SOURCE_CURRENT_SCENE = "current scene"
SOURCE_NPC_ROSTER = "npc_roster"
SOURCE_SEED = "seed"
SOURCE_MOVE_NPC = "move_npc"
SOURCE_GM = "gm"

SOURCE_RU = {
    SOURCE_DEFAULT_LORE: "публичные сведения",
    SOURCE_PREVIOUS_SCENE: "предыдущая сцена",
    SOURCE_CURRENT_SCENE: "текущая сцена",
    "current_scene": "текущая сцена",
    SOURCE_NPC_ROSTER: "ростер персонажей",
    SOURCE_SEED: "стартовые данные",
    SOURCE_MOVE_NPC: "перемещение персонажа",
    SOURCE_GM: "гейм-мастер",
}


def _public_role(role: str) -> str:
    raw = _as_str(role)
    return ROLE_RU.get(raw.lower(), raw)


def _public_gender(value: str) -> str:
    raw = _as_str(value)
    return GENDER_LABELS_RU.get(raw.lower(), raw)


def _public_source(source: str) -> str:
    raw = _as_str(source)
    return SOURCE_RU.get(raw.lower(), raw)


def _normalize_seed(seed: dict) -> dict:
    """Accept the strict seed shape and the looser shape local models often produce."""
    if not isinstance(seed, dict):
        return {}
    raw_scene = seed.get("scene") if isinstance(seed.get("scene"), dict) else {}
    if (isinstance(seed.get("scene"), dict) and isinstance(seed.get("npcs"), list)
            and "items" in raw_scene and "exits" in raw_scene and "title" in raw_scene):
        return seed
    src = {**seed, **raw_scene}

    public_facts = [
        _as_str(item) for item in _as_list(src.get("public_facts")) if _as_str(item)
    ]
    npc_details = src.get("npc_details") if isinstance(src.get("npc_details"), dict) else {}
    if not npc_details and isinstance(seed.get("npcs"), dict):
        npc_details = seed["npcs"]
    if not npc_details and isinstance(seed.get("npcs"), list):
        npc_details = {
            _as_str(raw.get("id")): raw
            for raw in seed["npcs"]
            if isinstance(raw, dict) and _as_str(raw.get("id"))
        }
    present = [_as_str(item) for item in _as_list(src.get("present_npcs")) if _as_str(item)]
    if not present and npc_details:
        present = list(npc_details.keys())

    npcs = []
    npc_presence = {}
    for idx, npc_id in enumerate(present, start=1):
        raw = npc_details.get(npc_id, {}) if isinstance(npc_details.get(npc_id), dict) else {}
        name = _as_str(raw.get("name")) or npc_id
        safe_npc_id = _safe_id(npc_id, f"npc_{idx}")
        npcs.append({
            "id": safe_npc_id,
            "name": name,
            "role": _as_str(raw.get("role")) or "персонаж сцены",
            "pronouns": _as_str(raw.get("pronouns") or raw.get("gender")),
            "persona": _as_str(raw.get("persona")) or _as_str(raw.get("description"))
                       or f"{name} присутствует в стартовой сцене.",
            "voice": _as_str(raw.get("voice")) or "Естественно, кратко, в образе.",
            "goals": _as_str(raw.get("goals")) or "Реагировать правдоподобно и защищать свои интересы.",
            "knowledge": _as_str(raw.get("knowledge")) or (
                "Публичные факты сцены: " + "; ".join(public_facts)
                if public_facts else "Только то, что очевидно в текущей сцене."
            ),
            "secret": _as_str(raw.get("secret")) or "Личная тайна не задана.",
        })
        npc_presence[safe_npc_id] = {
            "location": _as_str(raw.get("location") or raw.get("position")) or "в сцене",
            "activity": _as_str(raw.get("state") or raw.get("activity")) or "",
            "attitude": _as_str(raw.get("mood") or raw.get("attitude")) or "",
        }

    location = src.get("location") if isinstance(src.get("location"), dict) else {}
    items = []
    for idx, raw in enumerate(_as_list(src.get("visible_objects") or src.get("objects")
                                       or src.get("items")), start=1):
        if isinstance(raw, dict):
            name = _as_str(raw.get("name")) or _as_str(raw.get("display_name")) \
                   or _as_str(raw.get("description")) \
                   or _as_str(raw.get("id")) or f"предмет {idx}"
            items.append({
                "id": _safe_id(_as_str(raw.get("id")), f"item_{idx}"),
                "name": name,
                "location": _as_str(raw.get("location")) or "в сцене",
                "visible": bool(raw.get("visible", True)),
                "portable": bool(raw.get("portable", False)),
                "details": _as_str(raw.get("details") or raw.get("description")),
            })
        elif _as_str(raw):
            name = _as_str(raw)
            items.append({"id": _safe_id(name, f"item_{idx}"), "name": name,
                          "location": "в сцене", "visible": True, "portable": False})

    exits = []
    for idx, raw in enumerate(_as_list(src.get("visible_exits") or src.get("exits")), start=1):
        if isinstance(raw, dict):
            name = _as_str(raw.get("name")) or _as_str(raw.get("display_name")) \
                   or _as_str(raw.get("description")) \
                   or _as_str(raw.get("id")) or f"выход {idx}"
            destination = _as_str(raw.get("destination")) \
                          or _as_str(raw.get("destination_scene_id")) \
                          or _as_str(raw.get("direction")) or name
            exits.append({
                "id": _safe_id(_as_str(raw.get("id")), f"exit_{idx}"),
                "name": name,
                "destination": destination,
                "visible": bool(raw.get("visible", True)),
                "blocked_by": _as_str(raw.get("blocked_by")),
            })
        elif _as_str(raw):
            name = _as_str(raw)
            exits.append({"id": _safe_id(name, f"exit_{idx}"), "name": name,
                          "destination": name, "visible": True})

    title = _as_str(src.get("location_name") or src.get("scene_title")
                    or src.get("title") or src.get("name")) \
            or "Стартовая сцена"
    description = _as_str(src.get("scene_description") or src.get("description")
                          or location.get("description")
                          or seed.get("public_intro")) or (
        "Новая сцена готова. Игрок видит место, людей рядом и ближайший источник конфликта."
    )
    proper_nouns = [_as_str(item) for item in _as_list(seed.get("proper_nouns")) if _as_str(item)]
    for raw in npcs:
        if raw["name"] and raw["name"] not in proper_nouns:
            proper_nouns.append(raw["name"])
    if title and title not in proper_nouns:
        proper_nouns.append(title)

    return {
        "public_intro": _as_str(seed.get("public_intro") or src.get("public_intro")) or description,
        "hidden_truth": _as_str(seed.get("hidden_truth") or seed.get("canon")),
        "proper_nouns": proper_nouns,
        "public_facts": public_facts,
        "state_records": _as_list(seed.get("state_records") or src.get("state_records")),
        "npcs": npcs,
        "scene": {
            "id": _as_str(seed.get("scene_id") or seed.get("id")) or "start_scene",
            "location_id": _safe_id(title, "start_location"),
            "title": title,
            "description": description,
            "present_npcs": [raw["id"] for raw in npcs],
            "items": items,
            "exits": exits,
            "constraints": _as_list(seed.get("constraints")) or [
                "Здесь существуют только перечисленные видимые предметы, видимые выходы и присутствующие именованные персонажи.",
                "Игрок может спрашивать о чём угодно, но неописанные факты остаются неизвестными, пока не будут установлены.",
            ],
            "tension": _as_str(seed.get("tension")),
            "npc_presence": npc_presence,
        },
    }


def _new_dice_seed() -> int:
    # Real OS entropy (not a seeded PRNG) so each new campaign gets a genuinely
    # random dice seed. Within a campaign rolls then follow this seed
    # deterministically, and the exact RNG state is persisted across save/restore.
    return random.SystemRandom().getrandbits(64)


class World:
    def __init__(self, seed: dict | None = None):
        self.dice_seed = _new_dice_seed()
        self._rng = random.Random(self.dice_seed)  # рандомный сид на кампанию -> дальше по сиду
        self.forced_die_next: int | None = None  # debug: одноразовый override значения кубика
        self.forced_die_all: int | None = None   # debug: постоянный override значения кубика
        self.hidden_events: list[str] = []  # события, которых NPC ещё не знают
        self.rumors: list[Rumor] = []
        self._rumor_seq = 0
        if seed:
            self._load_seed(seed)
        else:
            self._load_default()

    @classmethod
    def from_seed(cls, seed: dict) -> "World":
        return cls(seed=seed)

    @classmethod
    def from_story(cls, story_id: str) -> "World":
        return cls(seed=stories.story_seed(story_id))

    def _load_default(self) -> None:
        self._load_seed(stories.default_story_seed())

    def _load_seed(self, seed: dict) -> None:
        seed = _normalize_seed(seed)
        self.story_id = _as_str(seed.get("id")) or "custom"
        self.story_title = _as_str(seed.get("title")) or "Пользовательская история"
        self.public = _as_str(seed.get("public_intro")) or _as_str(seed.get("public")) or (
            "Новая сцена готова. Игрок видит место, людей рядом и ближайший источник конфликта."
        )
        self.canon = _as_str(seed.get("hidden_truth") or seed.get("canon"))
        self.time = self._seed_time(seed.get("time"))
        self.player_character = self._seed_player_character(
            seed.get("player_character") if "player_character" in seed else seed.get("player")
        )
        self.extra_proper_nouns = [
            _as_str(name) for name in _as_list(seed.get("proper_nouns")) if _as_str(name)
        ]
        self.npcs = self._seed_npcs(seed)
        self.scene = self._seed_scene(seed)
        self.constraints = self.scene.constraints
        self.fact_records = self._seed_facts(seed)
        self.state_records = self._seed_state_records(seed)
        self.npc_whereabouts = {}
        self._ensure_npc_whereabouts()

    def _seed_time(self, raw: object) -> WorldTime:
        data = raw if isinstance(raw, dict) else {}
        minutes_per_hour = _as_int_or_none(data.get("minutes_per_hour")) or 60
        hours_per_day = _as_int_or_none(data.get("hours_per_day")) or 24
        return WorldTime(
            calendar_name=_as_str(data.get("calendar_name")),
            absolute_minutes=max(0, _as_int_or_none(data.get("absolute_minutes")) or 0),
            current_date_label=_as_str(data.get("current_date_label")) or "День 1",
            minutes_per_hour=max(1, minutes_per_hour),
            hours_per_day=max(1, hours_per_day),
            day_names=[_as_str(item) for item in _as_list(data.get("day_names")) if _as_str(item)],
            month_names=[_as_str(item) for item in _as_list(data.get("month_names")) if _as_str(item)],
            last_advance_minutes=max(0, _as_int_or_none(data.get("last_advance_minutes")) or 0),
            last_advance_reason=_as_str(data.get("last_advance_reason")),
        )

    def _seed_player_character(self, raw: object) -> PlayerCharacter:
        if not isinstance(raw, dict):
            return PlayerCharacter()
        pc = PlayerCharacter()
        self._apply_player_character_fields(pc, raw)
        pc.card_revision = max(0, _as_int_or_none(raw.get("card_revision")) or 0)
        return pc

    def _seed_npcs(self, seed: dict) -> dict[str, NPC]:
        out: dict[str, NPC] = {}
        for idx, raw in enumerate(_as_list(seed.get("npcs")), start=1):
            if not isinstance(raw, dict):
                continue
            name = _as_str(raw.get("name")) or f"NPC {idx}"
            npc_id = _safe_id(_as_str(raw.get("id")), f"npc_{idx}")
            base_id, suffix = npc_id, 2
            while npc_id in out:
                npc_id = f"{base_id}_{suffix}"
                suffix += 1
            out[npc_id] = NPC(
                npc_id=npc_id,
                name=name,
                role=_as_str(raw.get("role")) or "персонаж сцены",
                pronouns=_as_str(raw.get("pronouns") or raw.get("gender")),
                color=_as_str(raw.get("color")),
                public_label=_as_str(raw.get("public_label")),
                age=_as_str(raw.get("age")),
                physical_type=_as_str(raw.get("physical_type")),
                distinctive_features=_as_str(raw.get("distinctive_features")),
                life_status=_as_str(raw.get("life_status")) or "alive",
                life_status_note=_as_str(raw.get("life_status_note")),
                condition=_as_str(raw.get("condition")),
                persona=_as_str(raw.get("persona")) or _as_str(raw.get("description")),
                personality=_as_str(raw.get("personality")),
                values=_as_str(raw.get("values")),
                habits=_as_str(raw.get("habits")),
                pressure_response=_as_str(raw.get("pressure_response")),
                boundaries=_as_str(raw.get("boundaries")),
                voice=_as_str(raw.get("voice")) or "Естественно, кратко, в образе.",
                goals=_as_str(raw.get("goals")) or "Реагировать правдоподобно и защищать свои интересы.",
                knowledge=_as_str(raw.get("knowledge")) or "Только то, что очевидно в текущей сцене.",
                secret=_as_str(raw.get("secret")) or "Личная тайна не задана.",
                abilities=_as_dict(raw.get("abilities")),
                skills=_as_dict(raw.get("skills")),
                saving_throws=_as_dict(raw.get("saving_throws")),
                passive_perception=_as_int_or_none(raw.get("passive_perception")),
                ac=raw.get("ac"),
                hp=_as_dict(raw.get("hp")),
                speed=_as_joined_str(raw.get("speed")),
                senses=_as_joined_str(raw.get("senses")),
                languages=_as_joined_str(raw.get("languages")),
                default_whereabouts=raw.get("default_whereabouts")
                if isinstance(raw.get("default_whereabouts"), dict) else None,
            )
            if name not in self.extra_proper_nouns:
                self.extra_proper_nouns.append(name)
        if out:
            return out
        return {
            "stranger": NPC(
                npc_id="stranger",
                name="Незнакомец",
                role="персонаж стартовой сцены",
                persona="Осторожный человек, присутствующий в новой сцене.",
                voice="Кратко, настороженно, естественно.",
                goals="Оставаться в безопасности и правдоподобно реагировать на игрока.",
                knowledge="Только то, что очевидно в стартовой сцене.",
                secret="Личная тайна не задана.",
            )
        }

    def _seed_scene(self, seed: dict) -> SceneState:
        raw = seed.get("scene") if isinstance(seed.get("scene"), dict) else {}
        present_raw = {_safe_id(_as_str(item), "") for item in _as_list(raw.get("present_npcs"))}
        present = {npc_id for npc_id in present_raw if npc_id in self.npcs}
        if not present:
            present = set(list(self.npcs)[:2])
        title = _as_str(raw.get("title")) or _as_str(raw.get("location")) or "Стартовая сцена"
        location_id = _safe_id(_as_str(raw.get("location_id")), "start_location")
        description = _as_str(raw.get("description")) or self.public
        constraints = [
            _as_str(item) for item in _as_list(raw.get("constraints")) if _as_str(item)
        ]
        if not constraints:
            constraints = ["Здесь существуют только описанные выходы, видимые предметы и присутствующие люди."]

        presence: dict[str, Presence] = {}
        presence_raw = raw.get("npc_presence") if isinstance(raw.get("npc_presence"), dict) else {}
        for npc_id in present:
            npc = self.npcs[npc_id]
            npc_presence = presence_raw.get(npc_id, {}) if isinstance(presence_raw.get(npc_id), dict) else {}
            presence[npc_id] = Presence(
                    npc_id=npc_id,
                    location=_as_str(npc_presence.get("location"))
                         or _as_str(raw.get("default_npc_location")) or "в сцене",
                visible=True,
                can_hear=True,
                activity=_as_str(npc_presence.get("activity"))
                         or _as_str(raw.get("npc_activity")) or f"present as {npc.role}",
                attitude=_as_str(npc_presence.get("attitude"))
                         or _as_str(raw.get("npc_attitude")) or "",
            )

        items = []
        for idx, item in enumerate(_as_list(raw.get("items")), start=1):
            if isinstance(item, dict):
                name = _as_str(item.get("name")) or f"предмет {idx}"
                items.append(SceneItem(
                    item_id=_safe_id(_as_str(item.get("id")), f"item_{idx}"),
                    name=name,
                    location=_as_str(item.get("location")) or "in the scene",
                    visible=bool(item.get("visible", True)),
                    portable=bool(item.get("portable", False)),
                    owner=_as_str(item.get("owner")),
                    details=_as_str(item.get("details")),
                ))
            elif _as_str(item):
                name = _as_str(item)
                items.append(SceneItem(_safe_id(name, f"item_{idx}"), name, "in the scene"))

        exits = []
        for idx, exit_ in enumerate(_as_list(raw.get("exits")), start=1):
            if isinstance(exit_, dict):
                name = _as_str(exit_.get("name")) or f"выход {idx}"
                exits.append(SceneExit(
                    exit_id=_safe_id(_as_str(exit_.get("id")), f"exit_{idx}"),
                    name=name,
                    destination=_as_str(exit_.get("destination")) or "unknown destination",
                    visible=bool(exit_.get("visible", True)),
                    blocked_by=_as_str(exit_.get("blocked_by")),
                ))
            elif _as_str(exit_):
                name = _as_str(exit_)
                exits.append(SceneExit(_safe_id(name, f"exit_{idx}"), name, "unknown destination"))

        return SceneState(
            scene_id=_safe_id(_as_str(raw.get("id")), "start_scene"),
            location_id=location_id,
            title=title,
            description=description,
            present_npcs=present,
            presence=presence,
            items=items,
            exits=exits,
            constraints=constraints,
            tension=_as_str(raw.get("tension")),
            player_seen=[description],
        )

    def _seed_facts(self, seed: dict) -> list[FactRecord]:
        records: list[FactRecord] = []
        for idx, raw in enumerate(_as_list(seed.get("public_facts")), start=1):
            if isinstance(raw, dict):
                text = _as_str(raw.get("text"))
                fact_id = _safe_id(_as_str(raw.get("id")), f"public_{idx}")
                kind = _as_str(raw.get("kind")).lower() or "public"
                if kind not in ("public", "truth", "rumor"):
                    kind = "public"
                keywords = [_as_str(item) for item in _as_list(raw.get("keywords")) if _as_str(item)]
                source = _as_str(raw.get("source"))
                confirmed = bool(raw.get("confirmed", True))
            else:
                text = _as_str(raw)
                fact_id = f"public_{idx}"
                kind = "public"
                keywords = []
                source = ""
                confirmed = True
            if text:
                records.append(FactRecord(
                    fact_id=fact_id,
                    kind=kind,
                    text=text,
                    keywords=keywords,
                    source=source,
                    confirmed=confirmed,
                ))
        if self.canon:
            records.append(FactRecord(
                fact_id="hidden_truth",
                kind="truth",
                text=self.canon,
                keywords=["hidden truth", "truth", "secret"],
                source="seed",
            ))
        return records

    def _seed_state_records(self, seed: dict) -> list[StateRecord]:
        records: list[StateRecord] = []
        existing: set[str] = set()
        for idx, raw in enumerate(_as_list(seed.get("state_records")), start=1):
            record = self._coerce_state_record(raw, f"seed_state_{idx}", existing)
            if record is not None:
                records.append(record)
                existing.add(record.record_id)
        return records

    def _coerce_state_record(
        self,
        raw: object,
        fallback_id: str,
        existing: set[str] | None = None,
    ) -> StateRecord | None:
        if isinstance(raw, StateRecord):
            data = {
                "id": raw.record_id,
                "kind": raw.kind,
                "text": raw.text,
                "scope": raw.scope,
                "active": raw.active,
                "owner": raw.owner,
                "subject": raw.subject,
                "source": raw.source,
                "status": raw.status,
                "tags": raw.tags,
                "entity_id": raw.entity_id,
                "source_npc": raw.source_npc,
                "location_id": raw.location_id,
                "location_name": raw.location_name,
                "region_id": raw.region_id,
                "region_name": raw.region_name,
                "scene_id": raw.scene_id,
                "importance": raw.importance,
                "aliases": raw.aliases,
                "metadata": raw.metadata,
            }
        elif isinstance(raw, dict):
            data = raw
        else:
            return None

        text = _as_str(data.get("text"))
        if not text:
            return None
        kind = _state_record_kind(data.get("kind"))
        preferred_id = _as_str(data.get("record_id") or data.get("id"))
        record_id = self._unique_state_record_id(
            preferred_id or fallback_id,
            kind,
            existing if existing is not None else {
                r.record_id for r in getattr(self, "state_records", [])
            },
        )
        return StateRecord(
            record_id=record_id,
            kind=kind,
            text=text,
            scope=_state_record_scope(data.get("scope")),
            active=_state_record_active(data.get("active"), True),
            owner=_as_str(data.get("owner") or data.get("owner_id")),
            subject=_as_str(data.get("subject") or data.get("subject_id")),
            source=_as_str(data.get("source")),
            status=_as_str(data.get("status")) or "known",
            tags=_state_record_tags(data.get("tags")),
            entity_id=_as_str(data.get("entity_id") or data.get("entity") or data.get("about")),
            source_npc=_as_str(data.get("source_npc") or data.get("source_npc_id")),
            location_id=_as_str(data.get("location_id")),
            location_name=_as_str(data.get("location_name")),
            region_id=_as_str(data.get("region_id")),
            region_name=_as_str(data.get("region_name")),
            scene_id=_as_str(data.get("scene_id")),
            importance=_as_str(data.get("importance")),
            aliases=_state_record_aliases(data.get("aliases")),
            metadata=_state_record_metadata(data.get("metadata")),
        )

    def _unique_state_record_id(
        self,
        preferred_id: str,
        kind: str,
        existing: set[str] | None = None,
    ) -> str:
        existing = existing if existing is not None else {
            r.record_id for r in getattr(self, "state_records", [])
        }
        base = _safe_id(preferred_id, "") or f"{kind}_{len(existing) + 1}"
        record_id = base
        idx = 2
        while record_id in existing:
            record_id = f"{base}_{idx}"
            idx += 1
        return record_id

    @staticmethod
    def _state_record_visible_to(record: StateRecord, actor_id: str) -> bool:
        actor = _actor_key(actor_id or "player")
        scope = _state_record_scope(record.scope)
        owner = _actor_key(record.owner)
        subject = _actor_key(record.subject)
        if scope == "public":
            return True
        if actor in STATE_GM_ACTORS:
            return True
        if scope == "gm":
            return actor in STATE_GM_ACTORS
        if scope == "owner":
            return bool(owner and actor == owner)
        if scope == "subject":
            return bool(subject and actor == subject)
        if scope == "participants":
            return bool((owner and actor == owner) or (subject and actor == subject))
        return False

    def add_state_records(self, records) -> list[StateRecord]:
        if not hasattr(self, "state_records"):
            self.state_records = []
        existing = {record.record_id for record in self.state_records}
        added: list[StateRecord] = []
        for idx, raw in enumerate(_as_list(records), start=1):
            record = self._coerce_state_record(raw, f"state_{len(existing) + idx}", existing)
            if record is None:
                continue
            self.state_records.append(record)
            existing.add(record.record_id)
            added.append(record)
        return added

    def update_state_records(self, updates) -> list[StateRecord]:
        if not hasattr(self, "state_records"):
            self.state_records = []
        by_id = {record.record_id: record for record in self.state_records}
        updated: list[StateRecord] = []
        for raw in _as_list(updates):
            if not isinstance(raw, dict):
                continue
            record_id = _as_str(raw.get("record_id") or raw.get("id"))
            record = by_id.get(record_id)
            if record is None:
                continue
            if "kind" in raw:
                record.kind = _state_record_kind(raw.get("kind"))
            if "text" in raw:
                text = _as_str(raw.get("text"))
                if text:
                    record.text = text
            if "scope" in raw:
                record.scope = _state_record_scope(raw.get("scope"))
            if "active" in raw:
                record.active = _state_record_active(raw.get("active"), record.active)
            if "owner" in raw or "owner_id" in raw:
                record.owner = _as_str(raw.get("owner") or raw.get("owner_id"))
            if "subject" in raw or "subject_id" in raw:
                record.subject = _as_str(raw.get("subject") or raw.get("subject_id"))
            if "source" in raw:
                record.source = _as_str(raw.get("source"))
            if "status" in raw:
                record.status = _as_str(raw.get("status")) or "known"
            if "tags" in raw:
                record.tags = _state_record_tags(raw.get("tags"))
            if "entity_id" in raw or "entity" in raw or "about" in raw:
                record.entity_id = _as_str(raw.get("entity_id") or raw.get("entity") or raw.get("about"))
            if "source_npc" in raw or "source_npc_id" in raw:
                record.source_npc = _as_str(raw.get("source_npc") or raw.get("source_npc_id"))
            if "location_id" in raw:
                record.location_id = _as_str(raw.get("location_id"))
            if "location_name" in raw:
                record.location_name = _as_str(raw.get("location_name"))
            if "region_id" in raw:
                record.region_id = _as_str(raw.get("region_id"))
            if "region_name" in raw:
                record.region_name = _as_str(raw.get("region_name"))
            if "scene_id" in raw:
                record.scene_id = _as_str(raw.get("scene_id"))
            if "importance" in raw:
                record.importance = _as_str(raw.get("importance"))
            if "aliases" in raw:
                record.aliases = _state_record_aliases(raw.get("aliases"))
            if "metadata" in raw:
                record.metadata = _state_record_metadata(raw.get("metadata"))
            updated.append(record)
        return updated

    def delete_state_records(self, record_ids, hard: bool = False) -> int:
        if not hasattr(self, "state_records"):
            self.state_records = []
        ids = {_as_str(record_id) for record_id in _as_list(record_ids) if _as_str(record_id)}
        if not ids:
            return 0
        if hard:
            before = len(self.state_records)
            self.state_records = [record for record in self.state_records if record.record_id not in ids]
            return before - len(self.state_records)

        count = 0
        for record in getattr(self, "state_records", []):
            if record.record_id in ids and record.active:
                record.active = False
                count += 1
        return count

    def apply_state_record_batch(
        self,
        *,
        add=None,
        update=None,
        delete=None,
        hard_delete: bool = False,
    ) -> dict:
        return {
            "added": self.add_state_records(add or []),
            "updated": self.update_state_records(update or []),
            "deleted": self.delete_state_records(delete or [], hard=hard_delete),
        }

    def state_records_for(
        self,
        actor_id: str = "player",
        *,
        kinds=None,
        active: bool | None = True,
        owner: str = "",
        subject: str = "",
        entity_id: str = "",
        source_npc: str = "",
        location_id: str = "",
        region_id: str = "",
        scene_id: str = "",
        scopes=None,
    ) -> list[StateRecord]:
        kind_filter = {_state_record_kind(kind) for kind in _as_list(kinds)} if kinds is not None else None
        scope_filter = {_state_record_scope(scope) for scope in _as_list(scopes)} if scopes is not None else None
        owner_filter = _actor_key(owner)
        subject_filter = _actor_key(subject)
        entity_filter = _actor_key(entity_id)
        source_npc_filter = _actor_key(source_npc)
        location_filter = _actor_key(location_id)
        region_filter = _actor_key(region_id)
        scene_filter = _actor_key(scene_id)

        out: list[StateRecord] = []
        for record in getattr(self, "state_records", []):
            if active is not None and record.active is not active:
                continue
            if kind_filter is not None and _state_record_kind(record.kind) not in kind_filter:
                continue
            if scope_filter is not None and _state_record_scope(record.scope) not in scope_filter:
                continue
            if owner_filter and _actor_key(record.owner) != owner_filter:
                continue
            if subject_filter and _actor_key(record.subject) != subject_filter:
                continue
            if entity_filter and _actor_key(record.entity_id) != entity_filter:
                continue
            if source_npc_filter and _actor_key(record.source_npc) != source_npc_filter:
                continue
            if location_filter and _actor_key(record.location_id) != location_filter:
                continue
            if region_filter and _actor_key(record.region_id) != region_filter:
                continue
            if scene_filter and _actor_key(record.scene_id) != scene_filter:
                continue
            if not self._state_record_visible_to(record, actor_id):
                continue
            out.append(record)
        return out

    def state_record_documents(self, actor_id: str = "player") -> list:
        from rag import RagDocument

        docs = []
        for record in self.state_records_for(actor_id):
            tags = tuple(
                value for value in (
                    _state_record_kind(record.kind),
                    record.owner,
                    record.subject,
                    record.entity_id,
                    record.source_npc,
                    record.location_id,
                    record.location_name,
                    record.region_id,
                    record.region_name,
                    record.scene_id,
                    record.importance,
                    _state_record_scope(record.scope),
                    *record.tags,
                    *record.aliases,
                )
                if value
            )
            context_bits = []
            if record.region_name or record.region_id:
                context_bits.append(f"region: {_anchor_label(record.region_name, record.region_id)}")
            if record.location_name or record.location_id:
                context_bits.append(f"location: {_anchor_label(record.location_name, record.location_id)}")
            if record.scene_id:
                context_bits.append(f"scene: {record.scene_id}")
            if record.aliases:
                context_bits.append("aliases: " + ", ".join(record.aliases))
            if record.importance:
                context_bits.append(f"importance: {record.importance}")
            doc_text = record.text
            if context_bits:
                doc_text = f"Memory context: {'; '.join(context_bits)}. {record.text}"
            docs.append(RagDocument(
                doc_id=f"state:{record.record_id}",
                kind=f"state_{_state_record_kind(record.kind)}",
                text=doc_text,
                status=record.status,
                source=record.source or record.record_id,
                visibility=_state_record_scope(record.scope),
                tags=tags,
                metadata={
                    **record.metadata,
                    "record_id": record.record_id,
                    "record_kind": _state_record_kind(record.kind),
                    "scope": _state_record_scope(record.scope),
                    "owner": record.owner,
                    "subject": record.subject,
                    "entity_id": record.entity_id,
                    "source_npc": record.source_npc,
                    "location_id": record.location_id,
                    "location_name": record.location_name,
                    "region_id": record.region_id,
                    "region_name": record.region_name,
                    "scene_id": record.scene_id,
                    "importance": record.importance,
                    "aliases": list(record.aliases or ()),
                    "active": record.active,
                },
            ))
        return docs

    def state_records_export(
        self,
        actor_id: str = "player",
        *,
        kinds=None,
        active: bool | None = True,
        owner: str = "",
        subject: str = "",
        entity_id: str = "",
        source_npc: str = "",
        location_id: str = "",
        region_id: str = "",
        scene_id: str = "",
        scopes=None,
    ) -> list[dict]:
        out = []
        for record in self.state_records_for(
            actor_id,
            kinds=kinds,
            active=active,
            owner=owner,
            subject=subject,
            entity_id=entity_id,
            source_npc=source_npc,
            location_id=location_id,
            region_id=region_id,
            scene_id=scene_id,
            scopes=scopes,
        ):
            row = vars(record).copy()
            row["hash"] = state_record_hash(record)
            out.append(row)
        return out

    def npc_known_name(self, npc_id: str, actor_id: str = "player") -> str:
        """Name/label explicitly revealed to this actor through durable state."""
        clean_id = _actor_key(npc_id)
        if not clean_id:
            return ""
        for record in reversed(self.state_records_for(actor_id, entity_id=clean_id)):
            metadata = record.metadata if isinstance(record.metadata, dict) else {}
            known_name = _as_str(metadata.get("known_name"))
            if known_name:
                return known_name
        return ""

    def npc_player_label(self, npc_id: str, actor_id: str = "player") -> str:
        npc = self.npcs.get(_actor_key(npc_id))
        if npc is None:
            return _as_str(npc_id)
        return self.npc_known_name(npc.npc_id, actor_id) or _as_str(npc.public_label) or npc.name

    def _ensure_npc_whereabouts(self) -> None:
        raw = getattr(self, "npc_whereabouts", {})
        raw = raw if isinstance(raw, dict) else {}
        cleaned: dict[str, NPCWhereabouts] = {}
        for npc_id in self.npcs:
            cleaned[npc_id] = self._coerce_whereabouts(npc_id, raw.get(npc_id))
        self.npc_whereabouts = cleaned
        self._apply_default_story_whereabouts()
        self._sync_present_npc_whereabouts()

    def _coerce_whereabouts(self, npc_id: str, raw: object) -> NPCWhereabouts:
        if isinstance(raw, NPCWhereabouts):
            return NPCWhereabouts(
                npc_id=npc_id,
                location_id=_safe_id(raw.location_id, ""),
                location_name=_as_str(raw.location_name),
                status=self._whereabouts_status(raw.status),
                details=_as_str(raw.details),
                source=_as_str(raw.source),
            )
        if isinstance(raw, dict):
            location_name = _as_str(raw.get("location_name") or raw.get("location"))
            return NPCWhereabouts(
                npc_id=npc_id,
                location_id=_safe_id(_as_str(raw.get("location_id")), ""),
                location_name=location_name,
                status=self._whereabouts_status(raw.get("status")),
                details=_as_str(raw.get("details") or raw.get("activity")),
                source=_as_str(raw.get("source")),
            )
        return NPCWhereabouts(npc_id=npc_id)

    def _whereabouts_status(self, raw: object, fallback: str = "unknown") -> str:
        status = _safe_id(_as_str(raw), fallback)
        return status if status in WHEREABOUTS_STATUSES else fallback

    def _apply_default_story_whereabouts(self) -> None:
        # Card-defined default offscreen whereabouts (NPC.default_whereabouts), seeded
        # generically. No per-name special-casing in engine logic: any card may carry it.
        for npc_id, npc in self.npcs.items():
            default = getattr(npc, "default_whereabouts", None)
            if not isinstance(default, dict) or not default:
                continue
            row = self.npc_whereabouts.get(npc_id)
            if row and (row.status != "unknown" or row.source):
                continue
            self.npc_whereabouts[npc_id] = NPCWhereabouts(
                npc_id=npc_id,
                location_id=_safe_id(_as_str(default.get("location_id")), ""),
                location_name=_as_str(default.get("location_name")),
                status=self._whereabouts_status(default.get("status"), "likely"),
                details=_as_str(default.get("details")),
                source=_as_str(default.get("source")) or SOURCE_DEFAULT_LORE,
            )

    def _sync_present_npc_whereabouts(self) -> None:
        for npc_id in sorted(self.scene.present_npcs):
            presence = self.scene.presence.get(npc_id)
            if npc_id not in self.npcs:
                continue
            details = ""
            if presence:
                details = presence.activity or presence.location
            self.npc_whereabouts[npc_id] = NPCWhereabouts(
                npc_id=npc_id,
                location_id=self.scene.location_id,
                location_name=self.scene.title,
                status="present",
                details=details,
                source="current scene",
            )

    def npc_whereabouts_export(self, npc_id: str | None = None) -> dict:
        self._ensure_npc_whereabouts()
        if npc_id is not None:
            row = self.npc_whereabouts.get(npc_id) or NPCWhereabouts(npc_id=npc_id)
            return vars(row).copy()
        return {key: vars(row).copy() for key, row in self.npc_whereabouts.items()}

    def npc_whereabouts_summary(self, npc_id: str) -> str:
        self._ensure_npc_whereabouts()
        npc = self.npcs.get(npc_id)
        row = self.npc_whereabouts.get(npc_id)
        if not npc or not row or row.status == "unknown":
            return ""
        bits = [
            f"{self.npc_player_label(npc_id)} ({npc_id}, {npc.role})",
            "NOT in current scene" if npc_id not in self.scene.present_npcs else "in current scene",
            f"status: {row.status}",
        ]
        if row.location_name:
            bits.append(f"location: {row.location_name}")
        elif row.location_id:
            bits.append(f"location_id: {row.location_id}")
        if row.details:
            bits.append(f"details: {row.details}")
        return "; ".join(bits)

    def retrieval_documents(self, actor_id: str = "player") -> list:
        """Actor-safe RAG corpus for the GM/player-facing world memory.

        Hidden canon and NPC secrets deliberately do not enter this corpus. Private NPC
        beliefs need an actor-filtered retrieval path; default player retrieval is for
        get_world_fact and player-facing narration support.
        """
        from rag import RagDocument

        docs = []
        docs.extend(self.state_record_documents(actor_id))
        for record in self.fact_records:
            if record.kind == "truth":
                continue
            status = "known" if record.kind == "public" and record.confirmed else "unconfirmed"
            docs.append(RagDocument(
                doc_id=f"fact:{record.fact_id}",
                kind="public_fact" if status == "known" else "claim",
                text=record.text,
                status=status,
                source=record.source or record.fact_id,
                visibility="player",
                tags=tuple(record.keywords),
                metadata={"fact_id": record.fact_id, "record_kind": record.kind},
            ))

        present_labels = [
            self.npc_player_label(npc_id, actor_id)
            for npc_id in sorted(self.scene.present_npcs)
        ]
        docs.append(RagDocument(
            doc_id=f"scene:{self.scene.scene_id}",
            kind="scene_state",
            text=(
                f"Текущая сцена: {self.scene.title}. {self.scene.description} "
                f"В сцене: {', '.join(present_labels) or 'нет именованных NPC'}. "
                f"Выходы: {', '.join(exit_.name + ' -> ' + exit_.destination for exit_ in self.scene.visible_exits()) or 'нет известных выходов'}."
            ),
            status="current",
            source="current_scene",
            visibility="player",
            tags=(self.scene.scene_id, self.scene.location_id, self.scene.title),
            metadata={"scene_id": self.scene.scene_id, "location_id": self.scene.location_id},
        ))

        for item in self.scene.visible_items():
            docs.append(RagDocument(
                doc_id=f"scene_item:{self.scene.scene_id}:{item.item_id}",
                kind="scene_item",
                text=(
                    f"В текущей сцене виден предмет: {item.name}; место: {item.location}."
                    + (f" Детали: {item.details}." if item.details else "")
                    + (f" Владелец: {item.owner}." if item.owner else "")
                ),
                status="current",
                source="current_scene",
                visibility="player",
                tags=(item.item_id, item.name, item.owner),
                metadata={"scene_id": self.scene.scene_id, "item_id": item.item_id},
            ))

        self._ensure_npc_whereabouts()
        for npc_id, npc in self.npcs.items():
            label = self.npc_player_label(npc_id, actor_id)
            known_name = self.npc_known_name(npc_id, actor_id)
            appearance = ""
            if npc.physical_type:
                appearance += f" Тип/внешнее впечатление: {npc.physical_type}."
            if npc.distinctive_features:
                appearance += f" Приметы: {npc.distinctive_features}."
            docs.append(RagDocument(
                doc_id=f"npc_public:{npc_id}",
                kind="npc_public",
                text=(
                    f"{label} ({npc_id}) — {npc.role}."
                    + (f" Род: {_public_gender(npc.pronouns)} ({npc.pronouns})." if npc.pronouns else "")
                    + appearance
                ),
                status="known",
                source="npc_roster",
                visibility="player",
                tags=(npc_id, label, npc.role, npc.pronouns, npc.physical_type),
                metadata={"npc_id": npc_id, "known_name": known_name},
            ))
            where = self.npc_whereabouts.get(npc_id)
            if where and where.status != "unknown":
                present_text = "присутствует в текущей сцене" if npc_id in self.scene.present_npcs else "не в текущей сцене"
                docs.append(RagDocument(
                    doc_id=f"npc_whereabouts:{npc_id}",
                    kind="npc_whereabouts",
                    text=(
                        f"{label} сейчас {present_text}. Статус местонахождения: {where.status}. "
                        f"Где искать: {where.location_name or where.location_id or 'неизвестно'}."
                        + (f" Детали: {where.details}." if where.details else "")
                    ),
                    status="present" if where.status == "present" else (
                        "known" if where.status in ("known", "likely") else "unconfirmed"
                    ),
                    source=where.source or "world_state",
                    visibility="player",
                    tags=(npc_id, label, where.location_id, where.location_name, where.status),
                    metadata={"npc_id": npc_id, "location_id": where.location_id},
                ))

        for rumor in self.rumors:
            if "player" not in rumor.witnesses:
                continue
            speaker = self.npcs.get(rumor.speaker)
            speaker_name = self.npc_player_label(rumor.speaker, actor_id) if speaker else rumor.speaker
            docs.append(RagDocument(
                doc_id=f"testimony:{rumor.seq}",
                kind="testimony",
                text=f"{speaker_name} сказал: «{rumor.text}»",
                status="known" if rumor.confirmed else "unconfirmed",
                source=f"event:{rumor.seq}",
                visibility="player",
                tags=(rumor.speaker, speaker_name),
                metadata={
                    "seq": rumor.seq,
                    "turn": rumor.turn,
                    "speaker": rumor.speaker,
                    "witnesses": sorted(rumor.witnesses),
                    "confirmed": rumor.confirmed,
                },
            ))
        return docs

    def proper_nouns(self) -> list[str]:
        names = [npc.name for npc in self.npcs.values()]
        scene_names = [self.scene.title]
        scene_names.extend(item.name for item in self.scene.items)
        scene_names.extend(exit_.name for exit_ in self.scene.exits)
        seen = set()
        out = []
        for name in names + self.extra_proper_nouns + scene_names:
            if name and name not in seen:
                seen.add(name)
                out.append(name)
        return out

    def scene_context(self) -> str:
        present = []
        for npc_id in sorted(self.scene.present_npcs):
            npc = self.npcs.get(npc_id)
            if not npc:
                continue
            p = self.scene.presence.get(npc_id)
            detail = f"{self.npc_player_label(npc_id)} ({npc_id}, {npc.role})"
            if npc.physical_type:
                detail += f", {npc.physical_type}"
            if npc.condition:
                detail += f", condition: {npc.condition}"
            if npc.pronouns:
                detail += f", род: {_public_gender(npc.pronouns)}"
            if p and p.visible:
                detail += f" at {p.location}"
            present.append(detail)
        offscreen = []
        for npc_id in sorted(self.npcs):
            if npc_id in self.scene.present_npcs:
                continue
            line = self.npc_whereabouts_summary(npc_id)
            if line:
                offscreen.append(line)
        items = [item.name for item in self.scene.visible_items()]
        exits = []
        for exit_ in self.scene.visible_exits():
            line = f"{exit_.name} -> {exit_.destination}"
            if exit_.blocked_by:
                line += f" (blocked by {exit_.blocked_by})"
            exits.append(line)
        parts = [
            f"Scene: {self.scene.title}",
            f"Location: {self.scene.location_id}",
            "World time: " + self.time_summary(),
            f"Description: {self.scene.description}",
            "Present named NPCs: " + (", ".join(present) if present else "(none)"),
            "Known offscreen NPC whereabouts: " + (
                "\n".join(f"- {line}" for line in offscreen) if offscreen else "(none established)"
            ),
            "Visible objects: " + (", ".join(items) if items else "(none listed)"),
            "Visible exits: " + (", ".join(exits) if exits else "(none listed)"),
        ]
        if self.scene.tension:
            parts.append("Tension: " + self.scene.tension)
        return "\n".join(parts)

    def entity_refs(self) -> dict:
        """Player-facing entity registry for markdown refs.

        Secrets, private goals, and NPC knowledge are intentionally excluded.
        """
        self._ensure_npc_whereabouts()
        entities: list[dict] = []

        def add_entity(kind: str, entity_id: str, label: str, title: str = "",
                       subtitle: str = "", description: str = "",
                       meta: list[dict] | None = None, color: str = "") -> None:
            clean_id = _as_str(entity_id)
            clean_label = _as_str(label)
            if not clean_id or not clean_label:
                return
            entities.append({
                "key": f"{kind}:{clean_id}",
                "kind": kind,
                "id": clean_id,
                "label": clean_label,
                "title": _as_str(title) or clean_label,
                "subtitle": _as_str(subtitle),
                "description": _as_str(description),
                "color": _as_str(color),
                "meta": meta or [],
            })

        status_labels = WHEREABOUTS_STATUS_LABELS

        def public_npc_description(npc: NPC) -> str:
            visible_bits = []
            if npc.physical_type:
                visible_bits.append(npc.physical_type)
            if npc.distinctive_features:
                visible_bits.append(npc.distinctive_features)
            if npc.condition:
                visible_bits.append(npc.condition)
            text = ". ".join(visible_bits)
            if text:
                return text
            role = f" Публичная роль: {_public_role(npc.role)}." if npc.role else ""
            return f"Конкретный персонаж текущего мира.{role} Подробности появятся, когда игрок их узнает."

        for npc_id in sorted(self.npcs):
            npc = self.npcs[npc_id]
            present = npc_id in self.scene.present_npcs
            presence = self.scene.presence.get(npc_id)
            whereabouts = self.npc_whereabouts.get(npc_id) or NPCWhereabouts(npc_id=npc_id)
            role = _public_role(npc.role)
            pronouns = _public_gender(npc.pronouns)
            label = self.npc_player_label(npc_id)
            where = ""
            if present:
                where = presence.location if presence else self.scene.title
            else:
                where = whereabouts.location_name or whereabouts.location_id
            meta = [
                {"label": "роль", "value": role or "персонаж"},
                {"label": "статус", "value": "в сцене" if present else status_labels.get(whereabouts.status, whereabouts.status)},
            ]
            if pronouns:
                meta.append({"label": "род", "value": pronouns})
            if where:
                meta.append({"label": "где", "value": where})
            if present and presence and presence.activity:
                meta.append({"label": "занят", "value": presence.activity})
            add_entity(
                "npc",
                npc_id,
                label,
                title=label,
                subtitle="персонаж" + (f" · {role}" if role else ""),
                description=public_npc_description(npc),
                meta=meta,
                color=npc.color,
            )

        seen_locs: set[str] = set()

        def add_location(location_id: str, label: str, description: str = "",
                         meta: list[dict] | None = None) -> None:
            fallback_id = "loc_" + str(sum(ord(ch) for ch in (_as_str(label) or _as_str(location_id))) % 100000)
            clean_id = _safe_id(location_id, fallback_id)
            if not clean_id:
                return
            if clean_id in seen_locs:
                return
            seen_locs.add(clean_id)
            add_entity(
                "loc",
                clean_id,
                label,
                title=label,
                subtitle="локация",
                description=description,
                meta=meta or [],
            )

        current_meta = []
        if self.scene.present_npcs:
            current_meta.append({
                "label": "в сцене",
                "value": ", ".join(self.npcs[n].name for n in sorted(self.scene.present_npcs) if n in self.npcs),
            })
        visible_exits = [exit_.name for exit_ in self.scene.visible_exits()]
        if visible_exits:
            current_meta.append({"label": "выходы", "value": ", ".join(visible_exits)})
        add_location(
            self.scene.location_id,
            self.scene.title,
            self.scene.description,
            current_meta,
        )

        for exit_ in self.scene.visible_exits():
            destination = _as_str(exit_.destination)
            if not destination or destination.lower() == "unknown destination":
                continue
            add_location(
                f"{exit_.exit_id}_destination",
                destination,
                f"Видимый выход из текущей сцены: {exit_.name}.",
                [{"label": "через", "value": exit_.name}],
            )

        for row in self.npc_whereabouts.values():
            if row.status == "unknown":
                continue
            label = row.location_name or row.location_id
            if not label:
                continue
            add_location(
                row.location_id or label,
                label,
                row.details,
                [{"label": "источник", "value": _public_source(row.source) or status_labels.get(row.status, row.status)}],
            )

        return {"version": 1, "entities": entities}

    def entity_reference_context(self) -> str:
        registry = self.entity_refs().get("entities", [])
        npcs = [e for e in registry if e.get("kind") == "npc"]
        locs = [e for e in registry if e.get("kind") == "loc"]
        npc_refs = ", ".join(f"[[npc:{e['id']}|{e['label']}]]" for e in npcs[:12]) or "(none)"
        loc_refs = ", ".join(f"[[loc:{e['id']}|{e['label']}]]" for e in locs[:12]) or "(none)"
        return (
            "Available player-safe entity refs (use exact labels for specific listed entities):\n"
            "NPCs: " + npc_refs + "\nLocations: " + loc_refs
        )

    def npc_scene_slice(self, npc_id: str) -> str:
        npc = self.npcs.get(npc_id)
        presence = self.scene.presence.get(npc_id)
        if not npc or not presence or npc_id not in self.scene.present_npcs:
            return "You are not present in the current scene."
        others = []
        for other_id in sorted(self.scene.present_npcs):
            if other_id == npc_id:
                continue
            other = self.npcs.get(other_id)
            other_presence = self.scene.presence.get(other_id)
            if not other or not other_presence or not other_presence.visible:
                continue
            label = f"{other.name} ({other.role}"
            if other.pronouns:
                label += f"; род: {_public_gender(other.pronouns)}"
            label += f") at {other_presence.location}"
            others.append(label)
        items = [
            f"{item.name} at {item.location}" + (f", owner: {item.owner}" if item.owner else "")
            for item in self.scene.visible_items()
        ]
        exits = [exit_.name for exit_ in self.scene.visible_exits()]
        parts = [
            f"You are in: {self.scene.title}",
            f"Your name/gender marker: {npc.name}"
            + (f" ({npc.pronouns} = {_public_gender(npc.pronouns)})" if npc.pronouns else ""),
            f"Your position: {presence.location}",
            f"Your current activity: {presence.activity or '(none specified)'}",
            f"Your attitude right now: {presence.attitude or '(none specified)'}",
            "Other visible named NPCs: " + (", ".join(others) if others else "(none)"),
            "Visible objects: " + (", ".join(items) if items else "(none listed)"),
            "Visible exits: " + (", ".join(exits) if exits else "(none listed)"),
        ]
        public_facts = [record.text for record in self.fact_records if record.kind == "public"]
        if public_facts:
            parts.append("Public facts you may know:\n" + "\n".join(f"- {fact}" for fact in public_facts[:8]))
        memory_records = self.state_records_for(
            npc_id,
            kinds=("fact", "rumor", "npc_memory", "relationship", "goal"),
        )
        if memory_records:
            lines = []
            for record in memory_records[:12]:
                subject = f" about {record.subject}" if record.subject else ""
                lines.append(f"- {record.kind}{subject}: {record.text}")
            parts.append("Actor-visible state memory:\n" + "\n".join(lines))
        if self.scene.constraints:
            parts.append("Physical limits:\n" + "\n".join(f"- {c}" for c in self.scene.constraints))
        parts.append("Entity refs for visible text:\n" + self.entity_reference_context())
        return "\n".join(parts)

    def present_witnesses(self) -> frozenset:
        return frozenset(sorted(self.scene.present_npcs | {"player"}))

    def npc_can_react(self, npc_id: str) -> bool:
        presence = self.scene.presence.get(npc_id)
        return bool(npc_id in self.scene.present_npcs and presence and presence.visible and presence.can_hear)

    def set_npc_presence(self, npc_id: str, present: bool, location: str = "",
                         visible: bool = True, can_hear: bool = True,
                         activity: str = "", attitude: str = "") -> dict:
        self._ensure_npc_whereabouts()
        npc = self.resolve(npc_id)
        if present:
            old = self.scene.presence.get(npc.npc_id)
            self.scene.present_npcs.add(npc.npc_id)
            self.scene.presence[npc.npc_id] = Presence(
                npc_id=npc.npc_id,
                location=_as_str(location) or (old.location if old else "in the scene"),
                visible=bool(visible),
                can_hear=bool(can_hear),
                activity=_as_str(activity) or (old.activity if old else f"present as {npc.role}"),
                attitude=_as_str(attitude) or (old.attitude if old else ""),
            )
            self._sync_present_npc_whereabouts()
        else:
            self.scene.present_npcs.discard(npc.npc_id)
            if npc.npc_id in self.scene.presence:
                old = self.scene.presence[npc.npc_id]
                self.scene.presence[npc.npc_id] = Presence(
                    npc_id=npc.npc_id,
                    location=_as_str(location) or old.location,
                    visible=False,
                    can_hear=False,
                    activity=_as_str(activity) or "not present in the current scene",
                    attitude=_as_str(attitude) or old.attitude,
                )
            location_text = _as_str(location)
            if location_text:
                self.npc_whereabouts[npc.npc_id] = NPCWhereabouts(
                    npc_id=npc.npc_id,
                    location_id=_safe_id(location_text, ""),
                    location_name=location_text,
                    status="known",
                    details=_as_str(activity) or "вне текущей сцены",
                    source="move_npc",
                )
            else:
                self.npc_whereabouts[npc.npc_id] = NPCWhereabouts(
                    npc_id=npc.npc_id,
                    status="unknown",
                    details=_as_str(activity) or "покинул текущую сцену; куда именно, не установлено",
                    source="move_npc",
                )
        return {
            "npc_id": npc.npc_id,
            "name": npc.name,
            "present": npc.npc_id in self.scene.present_npcs,
            "scene": self.scene.title,
            "present_npcs": sorted(self.scene.present_npcs),
            "whereabouts": self.npc_whereabouts_export(npc.npc_id),
        }

    def set_scene(self, title: str, description: str, location_id: str = "",
                  present_npcs=None, items=None, exits=None, constraints=None,
                  tension: str = "") -> dict:
        self._ensure_npc_whereabouts()
        title = _as_str(title) or "Новая сцена"
        description = _as_str(description) or title
        fallback_id = "scene_" + str(sum(ord(ch) for ch in title) % 100000)
        location_id = _safe_id(_as_str(location_id) or title, fallback_id)
        old_scene = self.scene
        old_present = set(old_scene.present_npcs)

        present = set()
        presence: dict[str, Presence] = {}
        dropped_present_npcs: list[str] = []
        for raw_id in _as_list(present_npcs):
            npc_id = _safe_id(_as_str(raw_id), "")
            if npc_id not in self.npcs:
                raw_label = _as_str(raw_id)
                if raw_label:
                    dropped_present_npcs.append(raw_label)
                continue
            old = self.scene.presence.get(npc_id)
            npc = self.npcs[npc_id]
            present.add(npc_id)
            presence[npc_id] = Presence(
                npc_id=npc_id,
                location=old.location if old else "в сцене",
                visible=True,
                can_hear=True,
                activity=old.activity if old else f"присутствует как {npc.role}",
                attitude=old.attitude if old else "",
            )

        scene_items = []
        for idx, raw in enumerate(_as_list(items), start=1):
            if isinstance(raw, dict):
                name = _as_str(raw.get("name")) or f"предмет {idx}"
                scene_items.append(SceneItem(
                    item_id=_safe_id(_as_str(raw.get("id")), f"item_{idx}"),
                    name=name,
                    location=_as_str(raw.get("location")) or "в сцене",
                    visible=bool(raw.get("visible", True)),
                    portable=bool(raw.get("portable", False)),
                    owner=_as_str(raw.get("owner")),
                    details=_as_str(raw.get("details")),
                ))
            elif _as_str(raw):
                name = _as_str(raw)
                scene_items.append(SceneItem(_safe_id(name, f"item_{idx}"), name, "в сцене"))

        scene_exits = []
        for idx, raw in enumerate(_as_list(exits), start=1):
            if isinstance(raw, dict):
                name = _as_str(raw.get("name")) or f"выход {idx}"
                scene_exits.append(SceneExit(
                    exit_id=_safe_id(_as_str(raw.get("id")), f"exit_{idx}"),
                    name=name,
                    destination=_as_str(raw.get("destination")) or "неизвестное направление",
                    visible=bool(raw.get("visible", True)),
                    blocked_by=_as_str(raw.get("blocked_by")),
                ))
            elif _as_str(raw):
                name = _as_str(raw)
                scene_exits.append(SceneExit(_safe_id(name, f"exit_{idx}"), name, "unknown destination"))

        self.scene = SceneState(
            scene_id=location_id,
            location_id=location_id,
            title=title,
            description=description,
            present_npcs=present,
            presence=presence,
            items=scene_items,
            exits=scene_exits,
            constraints=[_as_str(item) for item in _as_list(constraints) if _as_str(item)],
            tension=_as_str(tension),
            player_seen=[description],
        )
        for old_npc_id in sorted(old_present - present):
            if old_npc_id not in self.npcs:
                continue
            old_presence = old_scene.presence.get(old_npc_id)
            self.npc_whereabouts[old_npc_id] = NPCWhereabouts(
                npc_id=old_npc_id,
                location_id=old_scene.location_id,
                location_name=old_scene.title,
                status="known",
                details=(old_presence.activity if old_presence else "") or "оставался в прежней сцене",
                source="previous scene",
            )
        self._sync_present_npc_whereabouts()
        result = self.scene_export()
        if dropped_present_npcs:
            result["dropped_present_npcs"] = dropped_present_npcs
            result["repair_hint"] = (
                "Ignored unknown present_npcs ids: "
                + ", ".join(dropped_present_npcs)
                + ". Use npc_ids from the current roster in CURRENT TURN CONTEXT."
            )
        return result

    def set_npc_whereabouts(self, npc_id: str, location_id: str = "",
                            location_name: str = "", status: str = "",
                            details: str = "", source: str = "") -> dict:
        self._ensure_npc_whereabouts()
        npc = self.resolve(npc_id)
        if npc.npc_id in self.scene.present_npcs:
            self._sync_present_npc_whereabouts()
        else:
            clean_location_name = _as_str(location_name)
            clean_location_id = _safe_id(_as_str(location_id), "")
            if not clean_location_name and clean_location_id:
                clean_location_name = clean_location_id
            clean_status = self._whereabouts_status(status, "known")
            if not clean_location_name and not clean_location_id and not _as_str(details):
                clean_status = "unknown"
            self.npc_whereabouts[npc.npc_id] = NPCWhereabouts(
                npc_id=npc.npc_id,
                location_id=clean_location_id,
                location_name=clean_location_name,
                status=clean_status,
                details=_as_str(details),
                source=_as_str(source) or "gm",
            )
        return {
            "npc_id": npc.npc_id,
            "name": npc.name,
            "present": npc.npc_id in self.scene.present_npcs,
            "current_scene": self.scene.title,
            "whereabouts": self.npc_whereabouts_export(npc.npc_id),
        }

    def record_rumor(self, seq: int, turn: int, speaker: str, text: str, witnesses: frozenset) -> None:
        import config
        text = _as_str(text)
        if not text:
            return
        self._rumor_seq += 1
        self.rumors.append(Rumor(seq=seq, turn=turn, speaker=speaker, text=text,
                                 witnesses=witnesses, confirmed=False))
        self.rumors = self.rumors[-config.RUMORS_CAP:]

    def scene_export(self) -> dict:
        return {
            "scene_id": self.scene.scene_id,
            "location_id": self.scene.location_id,
            "title": self.scene.title,
            "description": self.scene.description,
            "present_npcs": sorted(self.scene.present_npcs),
            "presence": {k: vars(v) for k, v in self.scene.presence.items()},
            "items": [vars(item) for item in self.scene.items],
            "exits": [vars(exit_) for exit_ in self.scene.exits],
            "constraints": list(self.scene.constraints),
            "tension": self.scene.tension,
            "npc_whereabouts": self.npc_whereabouts_export(),
        }

    def npc(self, npc_id: str) -> NPC:
        if npc_id not in self.npcs:
            raise KeyError(f"No such NPC: {npc_id}. Available: {list(self.npcs)}")
        return self.npcs[npc_id]

    def resolve(self, npc_id: str) -> NPC:
        """Lenient NPC lookup: by id or by name, case-insensitive."""
        key = (npc_id or "").strip().lower()
        if key in self.npcs:
            return self.npcs[key]
        for npc in self.npcs.values():
            if key == npc.name.lower() or key in npc.name.lower():
                return npc
        raise KeyError(f"No such NPC: '{npc_id}'. Available: {list(self.npcs)}")

    def time_export(self) -> dict:
        time = getattr(self, "time", WorldTime())
        minutes_per_hour = max(1, int(getattr(time, "minutes_per_hour", 60) or 60))
        hours_per_day = max(1, int(getattr(time, "hours_per_day", 24) or 24))
        day_minutes = minutes_per_hour * hours_per_day
        absolute = max(0, int(getattr(time, "absolute_minutes", 0) or 0))
        minute_of_day = absolute % day_minutes
        hour = minute_of_day // minutes_per_hour
        minute = minute_of_day % minutes_per_hour
        return {
            "calendar_name": _as_str(getattr(time, "calendar_name", "")),
            "absolute_minutes": absolute,
            "current_date_label": _as_str(getattr(time, "current_date_label", "")) or "День 1",
            "day_number": absolute // day_minutes + 1,
            "time_of_day": f"{hour:02d}:{minute:02d}",
            "minutes_per_hour": minutes_per_hour,
            "hours_per_day": hours_per_day,
            "day_names": list(getattr(time, "day_names", []) or []),
            "month_names": list(getattr(time, "month_names", []) or []),
            "last_advance_minutes": max(0, int(getattr(time, "last_advance_minutes", 0) or 0)),
            "last_advance_reason": _as_str(getattr(time, "last_advance_reason", "")),
        }

    def time_summary(self) -> str:
        payload = self.time_export()
        calendar = payload.get("calendar_name")
        date = payload.get("current_date_label") or f"День {payload.get('day_number')}"
        prefix = f"{calendar}, " if calendar else ""
        return f"{prefix}{date}, {payload.get('time_of_day')}"

    def time_context(self) -> str:
        payload = self.time_export()
        lines = [
            "Current world time: " + self.time_summary(),
            f"Previous player turn elapsed: {payload.get('last_advance_minutes', 0)} minutes",
        ]
        reason = _as_str(payload.get("last_advance_reason"))
        if reason:
            lines.append("Previous time reason: " + reason)
        return "\n".join(lines)

    def advance_time(self, minutes: Any, reason: str = "") -> dict:
        amount = _as_int_or_none(minutes)
        if amount is None or amount < 0:
            raise ValueError("minutes must be a non-negative integer")
        if not hasattr(self, "time"):
            self.time = WorldTime()
        before = self.time_export()
        self.time.absolute_minutes = before["absolute_minutes"] + amount
        self.time.last_advance_minutes = amount
        self.time.last_advance_reason = _as_str(reason)
        after = self.time_export()
        return {
            "ok": True,
            "elapsed_minutes": amount,
            "reason": _as_str(reason),
            "before": before,
            "current": after,
            "summary": self.time_summary(),
        }

    def _apply_player_character_fields(
        self,
        pc: PlayerCharacter,
        fields: dict,
    ) -> set[str]:
        if not isinstance(fields, dict):
            return set()
        text_fields = (
            "name", "pronouns", "class_role", "background", "age",
            "physical_type", "distinctive_features", "life_status",
            "life_status_note", "condition", "personality", "values",
            "gm_notes", "speed", "senses", "languages",
        )
        dict_fields = ("abilities", "skills", "saving_throws", "hp")
        list_fields = ("inventory", "equipment", "features")
        changed: set[str] = set()
        for key in PLAYER_CHARACTER_FIELDS:
            if key not in fields:
                continue
            if key in dict_fields:
                new_value = _as_dict(fields[key])
            elif key in list_fields:
                new_value = [_as_str(item) for item in _as_list(fields[key]) if _as_str(item)]
            elif key in {"level", "passive_perception"}:
                new_value = _as_int_or_none(fields[key])
            elif key == "ac":
                new_value = fields[key]
            elif key in text_fields:
                new_value = _as_joined_str(fields[key]) if key in {"speed", "senses", "languages"} else _as_str(fields[key])
            else:
                continue
            if new_value != getattr(pc, key, None):
                setattr(pc, key, new_value)
                changed.add(key)
        return changed

    def update_player_character(self, fields: dict, reason: str = "") -> dict:
        if not hasattr(self, "player_character"):
            self.player_character = PlayerCharacter()
        pc = self.player_character
        changed = self._apply_player_character_fields(pc, fields if isinstance(fields, dict) else {})
        if changed:
            pc.card_revision = int(getattr(pc, "card_revision", 0) or 0) + 1
        return {
            "ok": True,
            "updated": sorted(changed),
            "reason": _as_str(reason),
            "card_revision": int(getattr(pc, "card_revision", 0) or 0),
            "player_character": self.player_character_export(public=False),
        }

    def player_character_export(self, public: bool = True) -> dict:
        pc = getattr(self, "player_character", None) or PlayerCharacter()
        payload = {
            "name": pc.name,
            "pronouns": pc.pronouns,
            "class_role": pc.class_role,
            "level": pc.level,
            "background": pc.background,
            "age": pc.age,
            "physical_type": pc.physical_type,
            "distinctive_features": pc.distinctive_features,
            "life_status": pc.life_status,
            "life_status_note": pc.life_status_note,
            "condition": pc.condition,
            "personality": pc.personality,
            "values": pc.values,
            "abilities": dict(pc.abilities),
            "skills": dict(pc.skills),
            "saving_throws": dict(pc.saving_throws),
            "passive_perception": pc.passive_perception,
            "ac": pc.ac,
            "hp": dict(pc.hp),
            "speed": pc.speed,
            "senses": pc.senses,
            "languages": pc.languages,
            "inventory": list(pc.inventory),
            "equipment": list(pc.equipment),
            "features": list(pc.features),
            "card_revision": int(getattr(pc, "card_revision", 0) or 0),
        }
        if not public:
            payload["gm_notes"] = pc.gm_notes
        return payload

    @staticmethod
    def _context_value_empty(value: Any) -> bool:
        return value is None or value == "" or value == {} or value == []

    def player_character_context(self) -> str:
        pc = getattr(self, "player_character", None) or PlayerCharacter()
        mechanics = {
            "abilities": pc.abilities,
            "skills": pc.skills,
            "saving_throws": pc.saving_throws,
            "passive_perception": pc.passive_perception,
            "ac": pc.ac,
            "hp": pc.hp,
            "speed": pc.speed,
            "senses": pc.senses,
            "languages": pc.languages,
        }
        mechanics = {
            key: value for key, value in mechanics.items()
            if not self._context_value_empty(value)
        }
        lines = [
            f"Name: {pc.name}",
            f"Pronouns: {pc.pronouns}",
        ]
        for label, value in (
            ("Class/role", pc.class_role),
            ("Level", pc.level),
            ("Background", pc.background),
            ("Age", pc.age),
            ("Type/size/appearance", pc.physical_type),
            ("Distinctive features", pc.distinctive_features),
            ("Life status", pc.life_status),
            ("Life status note", pc.life_status_note),
            ("Condition", pc.condition),
            ("Personality", pc.personality),
            ("Values", pc.values),
        ):
            if not self._context_value_empty(value):
                lines.append(f"{label}: {value}")
        if mechanics:
            lines.append(
                "Mechanics: " + json.dumps(
                    mechanics, ensure_ascii=False, sort_keys=True, separators=(",", ":")
                )
            )
        for label, items in (
            ("Inventory", pc.inventory),
            ("Equipment", pc.equipment),
            ("Features", pc.features),
        ):
            values = [_as_str(item) for item in items if _as_str(item)]
            if values:
                lines.append(f"{label}: " + "; ".join(values))
        if pc.gm_notes:
            lines.append("GM notes: " + pc.gm_notes)
        lines.append(f"Card revision: {int(getattr(pc, 'card_revision', 0) or 0)}")
        return "\n".join(lines)

    @staticmethod
    def _profile_empty(value: Any) -> bool:
        return value is None or value == "" or value == {} or value == []

    def npc_profile(self, npc_id: str, preset: str = "visible", fields=None) -> dict:
        npc = self.resolve(npc_id)
        clean_preset = _safe_id(_as_str(preset), "visible")
        if clean_preset not in NPC_PROFILE_PRESETS:
            clean_preset = "visible"

        wanted: list[str] = list(NPC_PROFILE_PRESETS[clean_preset])
        ignored: list[str] = []
        for raw in _as_list(fields):
            field_name = _safe_id(_as_str(raw), "")
            if not field_name:
                continue
            if field_name in NPC_PROFILE_FIELDS and field_name not in wanted:
                wanted.append(field_name)
            elif field_name not in NPC_PROFILE_FIELDS:
                ignored.append(_as_str(raw))

        profile = {}
        for field_name in wanted:
            value = getattr(npc, field_name, None)
            if self._profile_empty(value):
                continue
            profile[field_name] = value

        label = self.npc_player_label(npc.npc_id)
        return {
            "status": "known",
            "npc_id": npc.npc_id,
            "label": label,
            "preset": clean_preset,
            "card_revision": int(getattr(npc, "card_revision", 0) or 0),
            "profile": profile,
            "ignored_fields": ignored,
        }

    # --- Dice tool (deterministic, in code) -------------------------------
    @staticmethod
    def _coerce_int(value: Any) -> int | None:
        if value is None or isinstance(value, bool):
            return None
        if isinstance(value, int):
            return value
        if isinstance(value, float) and value.is_integer():
            return int(value)
        if isinstance(value, str):
            m = re.search(r"-?\d+", value)
            if m:
                return int(m.group(0))
        return None

    @staticmethod
    def _roll_kind(value: Any) -> str:
        raw = str(value or "").strip().lower().replace("-", "_").replace(" ", "_")
        aliases = {
            "ability_check": "check",
            "saving_throw": "save",
            "random": "chance",
            "opposed": "contest",
        }
        return aliases.get(raw, raw)

    @staticmethod
    def _target_label(target_kind: Any, roll_kind: str) -> str:
        raw = str(target_kind or "").strip()
        if raw and raw.lower() != "none":
            return raw
        if roll_kind == "attack":
            return "AC"
        if roll_kind in ("check", "save"):
            return "DC"
        if roll_kind == "contest":
            return "opposed_total"
        return "target"

    @staticmethod
    def _grade_from_margin(margin: int) -> str:
        if margin >= 15:
            return "overwhelming_success"
        if margin >= 10:
            return "critical_success"
        if margin >= 5:
            return "strong_success"
        if margin >= 0:
            return "success"
        if margin >= -2:
            return "near_miss"
        if margin >= -4:
            return "weak_failure"
        if margin >= -9:
            return "failure"
        if margin >= -14:
            return "major_failure"
        return "critical_failure"

    def _roll_data(self, notation: str) -> dict:
        raw = str(notation or "")
        m = re.fullmatch(
            r"\s*(\d*)d(\d+)\s*(k[hl]\s*\d+)?\s*([+-]\s*\d+)?\s*",
            raw.lower(),
        )
        if not m:
            return {"ok": False, "total": 0, "detail": f"invalid notation '{notation}'"}
        count = int(m.group(1) or 1)
        sides = int(m.group(2))
        keep_raw = (m.group(3) or "").replace(" ", "")
        mod = int((m.group(4) or "0").replace(" ", ""))
        if count <= 0 or sides <= 0:
            return {"ok": False, "total": 0, "detail": f"invalid notation '{notation}'"}
        forced = self.forced_die_next if self.forced_die_next is not None else self.forced_die_all
        if forced is not None:
            face = max(1, min(sides, int(forced)))
            rolls = [face for _ in range(count)]
        else:
            rolls = [self._rng.randint(1, sides) for _ in range(count)]
        if self.forced_die_next is not None:
            self.forced_die_next = None  # одноразовый override израсходован этим броском
        kept = rolls
        keep_note = ""
        if keep_raw:
            keep_count = int(keep_raw[2:])
            if keep_count <= 0 or keep_count > count:
                return {"ok": False, "total": 0, "detail": f"invalid notation '{notation}'"}
            if keep_raw.startswith("kh"):
                kept = sorted(rolls, reverse=True)[:keep_count]
                keep_note = f" keep highest {keep_count}: {kept}"
            else:
                kept = sorted(rolls)[:keep_count]
                keep_note = f" keep lowest {keep_count}: {kept}"
        total = sum(kept) + mod
        natural = kept[0] if sides == 20 and len(kept) == 1 else None
        detail = f"{notation} -> {rolls}{keep_note}{f' {mod:+d}' if mod else ''} = {total}"
        return {
            "ok": True,
            "notation": notation,
            "sides": sides,
            "count": count,
            "keep": keep_raw,
            "rolls": rolls,
            "kept": kept,
            "modifier": mod,
            "total": total,
            "natural": natural,
            "forced": forced is not None,
            "detail": detail,
        }

    def roll(self, notation: str) -> tuple[int, str]:
        data = self._roll_data(notation)
        return int(data["total"]), str(data["detail"])

    def roll_for_outcome(
        self,
        notation: str,
        target_number: Any = None,
        target_kind: str = "",
        roll_kind: str = "",
    ) -> tuple[int, str]:
        payload = self.roll_outcome_payload(notation, target_number, target_kind, roll_kind)
        return int(payload.get("total", 0) or 0), str(payload.get("detail", ""))

    def roll_outcome_payload(
        self,
        notation: str,
        target_number: Any = None,
        target_kind: str = "",
        roll_kind: str = "",
    ) -> dict:
        data = self._roll_data(notation)
        total = int(data["total"])
        detail = str(data["detail"])
        if not data.get("ok"):
            return {
                "ok": False,
                "notation": notation,
                "total": total,
                "grade": "invalid",
                "detail": detail,
            }

        # Physical dice geometry (faces actually rolled) so a renderer can show the
        # real result instead of re-rolling or re-parsing the notation. The model-facing
        # compact payload whitelists fields, so these never reach the model.
        geometry = {
            "sides": data.get("sides"),
            "count": data.get("count"),
            "keep": data.get("keep"),
            "rolls": data.get("rolls"),
            "kept": data.get("kept"),
            "modifier": data.get("modifier"),
            "forced": data.get("forced"),
        }

        kind = self._roll_kind(roll_kind)
        target = self._coerce_int(target_number)
        if kind not in {"check", "save", "attack", "contest"} or target is None:
            return {
                "ok": True,
                "notation": data.get("notation", notation),
                "roll_kind": kind or "roll",
                "total": total,
                "grade": "ungraded",
                "natural": data.get("natural"),
                "detail": f"{detail}: grade=ungraded",
                **geometry,
            }

        margin = total - target
        grade = self._grade_from_margin(margin)
        natural = data.get("natural")
        natural_note = f", natural={natural}" if natural is not None else ""
        if kind == "attack" and natural == 20:
            grade = "critical_success"
        elif kind == "attack" and natural == 1:
            grade = "critical_failure"

        target_label = self._target_label(target_kind, kind)
        return {
            "ok": True,
            "notation": data.get("notation", notation),
            "roll_kind": kind,
            "target_kind": target_label,
            "target_number": target,
            "total": total,
            "grade": grade,
            "margin": margin,
            "natural": natural,
            "detail": f"{detail} vs {target_label} {target}: grade={grade}, margin={margin:+d}{natural_note}",
            **geometry,
        }

    # --- Debug / authoring mutators ---------------------------------------
    def update_npc(self, npc_id: str, fields: dict) -> bool:
        npc = self.npcs.get(npc_id)
        if npc is None or not isinstance(fields, dict):
            return False
        text_fields = (
            "name", "color", "role", "pronouns", "public_label", "age",
            "physical_type", "distinctive_features", "life_status",
            "life_status_note", "condition", "persona", "personality", "values",
            "habits", "pressure_response", "boundaries", "voice", "goals",
            "knowledge", "secret", "speed", "senses", "languages",
        )
        dict_fields = ("abilities", "skills", "saving_throws", "hp")
        scalar_fields = ("passive_perception", "ac")
        editable = text_fields + dict_fields + scalar_fields
        # Content fields bump card_revision when they actually change. "color" is
        # editable but cosmetic, so color-only edits must not bump the revision.
        content = tuple(field for field in editable if field != "color")
        content_changed = False
        for key in editable:
            if key not in fields:
                continue
            if key in dict_fields:
                new_value = _as_dict(fields[key])
            elif key == "passive_perception":
                new_value = _as_int_or_none(fields[key])
            elif key == "ac":
                new_value = fields[key]
            elif key in {"speed", "senses", "languages"}:
                new_value = _as_joined_str(fields[key])
            else:
                new_value = _as_str(fields[key])
            if key in content and new_value != getattr(npc, key, ""):
                content_changed = True
            setattr(npc, key, new_value)
        if content_changed:
            npc.card_revision = int(getattr(npc, "card_revision", 0) or 0) + 1
        return True

    def add_fact(self, text: str, kind: str = "public") -> "FactRecord | None":
        text = _as_str(text)
        if not text:
            return None
        kind = _as_str(kind).lower() or "public"
        if kind not in ("public", "truth", "rumor"):
            kind = "public"
        existing = {record.fact_id for record in self.fact_records}
        base, idx = f"{kind}_dbg", 1
        while f"{base}_{idx}" in existing:
            idx += 1
        record = FactRecord(fact_id=f"{base}_{idx}", kind=kind, text=text,
                            source="debug", confirmed=(kind != "rumor"))
        self.fact_records.append(record)
        return record

    def remove_fact(self, fact_id: str) -> bool:
        fid = _as_str(fact_id)
        before = len(self.fact_records)
        self.fact_records = [record for record in self.fact_records if record.fact_id != fid]
        return len(self.fact_records) < before

    # --- World-fact tool (pull pattern) -----------------------------------
    def fact(self, query: str, actor_id: str = "player") -> WorldFact:
        """Honest actor-safe lookup. Hidden truth is stored, but not returned to player lookup."""
        q = (query or "").lower()
        try:
            import config
            if config.RAG_ENABLED:
                from rag import retrieve_world_fact
                rag_payload = retrieve_world_fact(query, self.retrieval_documents(actor_id))
                if rag_payload:
                    return WorldFact(
                        str(rag_payload.get("status") or "unknown"),
                        str(rag_payload.get("text") or ""),
                        list(rag_payload.get("sources") or []),
                    )
        except Exception:
            # RAG is an accuracy layer, not a hard dependency for running the game.
            pass

        matches = []
        q_words = _match_words(q)
        for record in self.fact_records:
            if record.kind == "truth":
                continue
            haystack = " ".join([record.text, *record.keywords]).lower()
            hay_words = _match_words(haystack)
            if (q and q in haystack) or (q_words and q_words & hay_words):
                label = "rumor" if record.kind == "rumor" or not record.confirmed else "known"
                matches.append(f"{label}: {record.text}")
        for record in self.state_records_for(actor_id, kinds=("fact", "rumor")):
            metadata = record.metadata if isinstance(record.metadata, dict) else {}
            haystack = " ".join([
                record.text,
                *record.tags,
                record.owner,
                record.subject,
                record.entity_id,
                record.source_npc,
                record.location_id,
                record.location_name,
                record.region_id,
                record.region_name,
                record.scene_id,
                record.importance,
                *record.aliases,
                _as_str(metadata.get("known_name")),
            ]).lower()
            hay_words = _match_words(haystack)
            if (q and q in haystack) or (q_words and q_words & hay_words):
                label = "rumor" if record.kind == "rumor" else record.status
                matches.append(f"{label}: {record.text}")
        if matches:
            return WorldFact("known", " ".join(matches[:3]))

        rumor_matches = []
        for rumor in self.rumors:
            text_words = _match_words(rumor.text)
            if q_words and q_words & text_words:
                speaker = self.npcs.get(rumor.speaker)
                name = self.npc_player_label(rumor.speaker, actor_id) if speaker else rumor.speaker
                rumor_matches.append(f"{name} said: «{rumor.text}»")
        if rumor_matches:
            return WorldFact("unknown", "Unconfirmed statements only: "
                             + " ".join(rumor_matches[-3:]))
        # hidden_events are GM-author-only; they must never surface through this
        # public lookup. No public hidden-events fallback here by design.
        return WorldFact("unknown", "Nothing is reliably known about this in town.")
