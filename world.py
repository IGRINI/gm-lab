"""Состояние мира, реестр NPC, кубы и код-детектор утечки секретов.

Принцип: правила и факты живут в КОДЕ, а не в голове модели. Кубы детерминированы,
секреты хранятся отдельно от контекста ГМ, утечку ловит дешёвая проверка кодом
(в дополнение к LLM-критику) — чтобы доп. раунд был виден гарантированно.
"""
from __future__ import annotations

import random
import re
from dataclasses import dataclass, field
from typing import Any


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
    default_whereabouts: dict | None = None  # optional card-defined offscreen whereabouts seed
    card_revision: int = 0   # bumped when a content card field changes; old snapshots default to 0


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


# --- The world's hidden truth (known only to the GM-as-author and critic, NOT the player) --
# In English (internal/model-facing); proper nouns stay in their original Russian form.
WORLD_CANON = (
    "Прошлой ночью в городе Тёрнвейл убили купца Алдрика. На самом деле его убила "
    "Гильдия воров, заметая следы контрабанды специй. Трактирщик Борин — осведомитель "
    "гильдии и знает, чья это работа. Городская стража подозревает гильдию, но "
    "доказательств у неё нет."
)

# The public scene — this the GM is allowed to show. Kept in RUSSIAN: it is the
# player-facing scene banner.
WORLD_PUBLIC = (
    "Городок Тёрнвейл, утро. Трактир «Серый грифон». По городу слух: вчера ночью "
    "нашли мёртвым купца Алдрика. Народ напуган и шепчется."
)

# Publicly known lore for get_world_fact. Only what the GM may tell TRUTHFULLY. Secrets
# (who killed, who is an informant) do NOT go here — they live in canon / NPC cards.
# Values are Russian because tool results are visible in the lab/debug UI. Keys: the
# original Russian proper-noun substrings ARE kept (queries may include Russian names), AND
# English common-noun keys are added so older English queries still match (lookup is a
# lowercase substring test against the query).
WORLD_LORE = {
    # Russian proper-noun keys (matched when the query carries the Russian name).
    "алдрик": "Алдрик был небогатым купцом: торговал специями и сушёными травами, время от "
              "времени возил товары через Тёрнвейл. Его считали скрытным и нелюдимым. "
              "Прошлой ночью его нашли мёртвым.",
    "тёрнвейл": "Тёрнвейл — небольшой торговый городок на большой дороге; он живёт за счёт "
                "проезжих купцов и сезонной ярмарки.",
    "грифон": "«Серый грифон» — главный трактир Тёрнвейла; его держит трактирщик Борин.",
    "трактир": "«Серый грифон» — главный трактир Тёрнвейла; его держит трактирщик Борин.",
    "гильди": "Говорят, на большой дороге действует гильдия воров, но это только слух: "
              "доказательств нет.",
    "стража": "Городская стража небольшая; расследованием убийства занимается Капитан Марет.",
    # English common-noun keys (matched when the GM queries in English). Same values.
    "aldrik": "Алдрик был небогатым купцом: торговал специями и сушёными травами, время от "
              "времени возил товары через Тёрнвейл. Его считали скрытным и нелюдимым. "
              "Прошлой ночью его нашли мёртвым.",
    "turnvale": "Тёрнвейл — небольшой торговый городок на большой дороге; он живёт за счёт "
                "проезжих купцов и сезонной ярмарки.",
    "town": "Тёрнвейл — небольшой торговый городок на большой дороге; он живёт за счёт "
            "проезжих купцов и сезонной ярмарки.",
    "griffon": "«Серый грифон» — главный трактир Тёрнвейла; его держит трактирщик Борин.",
    "tavern": "«Серый грифон» — главный трактир Тёрнвейла; его держит трактирщик Борин.",
    "inn": "«Серый грифон» — главный трактир Тёрнвейла; его держит трактирщик Борин.",
    "guild": "Говорят, на большой дороге действует гильдия воров, но это только слух: "
             "доказательств нет.",
    "guard": "Городская стража небольшая; расследованием убийства занимается Капитан Марет.",
    "mareth": "Городская стража небольшая; расследованием убийства занимается Капитан Марет.",
}


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


def _as_str(value: Any) -> str:
    if value is None:
        return ""
    return str(value).strip()


def _match_words(text: str) -> set[str]:
    # Plain word tokenizer for the offline (non-RAG) rumor-search fallback.
    # NOT a keyword extractor: no length/count heuristics — facts no longer get
    # auto-tagged; what's worth keeping will be authored (later, by the model).
    return set(re.findall(r"[a-zа-яё0-9]+", _as_str(text).lower()))


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


def _default_facts() -> list[FactRecord]:
    return [
        FactRecord(
            fact_id="aldrik_public",
            kind="public",
            text=WORLD_LORE["алдрик"],
            keywords=["алдрик", "aldrik"],
        ),
        FactRecord(
            fact_id="turnvale_public",
            kind="public",
            text=WORLD_LORE["тёрнвейл"],
            keywords=["тёрнвейл", "turnvale"],
        ),
        FactRecord(
            fact_id="griffon_public",
            kind="public",
            text=WORLD_LORE["грифон"],
            keywords=["серый грифон", "грифон", "grey griffon", "griffon"],
        ),
        FactRecord(
            fact_id="guild_rumor",
            kind="public",
            text=WORLD_LORE["гильди"],
            keywords=["гильди", "guild", "thieves"],
        ),
        FactRecord(
            fact_id="guard_public",
            kind="public",
            text=WORLD_LORE["стража"],
            keywords=["капитан марет", "марет", "mareth", "town guard", "guard captain"],
        ),
        FactRecord(
            fact_id="murder_truth",
            kind="truth",
            text=WORLD_CANON,
            keywords=["killer", "murder truth", "who killed"],
            source="скрытый канон",
        ),
    ]


def _default_scene() -> SceneState:
    constraints = [
        "Единственный выход из «Серого грифона» — через главную дверь; её видно из всего "
        "общего зала.",
        "В общем зале полно посетителей: пересечь зал, выйти, драться или незаметно "
        "возиться с предметами почти невозможно. Тихие слова, сказанные вплотную, по "
        "умолчанию остаются приватными; другие могут заметить жесты, позу или близость, "
        "но не содержание разговора, если явно не подслушивают.",
    ]
    return SceneState(
        scene_id="griffon_common_room",
        location_id="grey_griffon",
        title="Трактир «Серый грифон»",
        description=(
            "Утро в общем зале трактира. Гости говорят вполголоса об убийстве Алдрика; "
            "стойка, столы и главный вход хорошо видны игроку."
        ),
        present_npcs={"borin", "lysa"},
        presence={
            "borin": Presence(
                npc_id="borin",
                location="за стойкой",
                visible=True,
                can_hear=True,
                activity="разливает эль и следит за залом",
                attitude="насторожен, если спрашивают про Алдрика или гильдию",
            ),
            "lysa": Presence(
                npc_id="lysa",
                location="между столами",
                visible=True,
                can_hear=True,
                activity="разносит кружки между столами",
                attitude="любопытна, но боится привлечь внимание",
            ),
        },
        items=[
            SceneItem("counter", "стойка", "общий зал", visible=True, portable=False),
            SceneItem("mugs", "кружки", "на стойке", visible=True, portable=True,
                      owner="borin"),
            SceneItem("ale_barrels", "бочки с элем", "за стойкой", visible=True,
                      portable=False, owner="borin"),
        ],
        exits=[
            SceneExit("main_door", "главная дверь", "улица Тёрнвейла", visible=True),
        ],
        constraints=constraints,
        tension="В зале тесно и нервно.",
        player_seen=[
            "Игрок видит общий зал, стойку, Борина за стойкой, Лизу между столами и "
            "главную дверь."
        ],
    )


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


def _npcs() -> dict[str, NPC]:
    return {
        "borin": NPC(
            npc_id="borin",
            name="Борин",
            role="трактирщик",
            pronouns="M",
            color="#e6c08a",
            persona="Крупный трактирщик «Серого грифона», за пятьдесят; осторожный, хитрый "
                    "и себе на уме. На людях грубоват, но умеет выглядеть гостеприимным.",
            voice="Коротко, хрипловато, с прибаутками. Часто называет гостей «дружище».",
            goals="Держать трактир на плаву и не давать людям слишком глубоко копать тему гильдии.",
            knowledge="Местные слухи, кто заходил в трактир, кто выходил, сколько стоит эль. "
                      "Официально про убийство знаешь только слух: Алдрика нашли мёртвым.",
            secret="Ты осведомитель Гильдии воров и точно знаешь, что гильдия убила Алдрика, "
                   "прикрывая контрабанду. Если это всплывёт, тебя убьют.",
        ),
        "lysa": NPC(
            npc_id="lysa",
            name="Лиза",
            role="служанка",
            pronouns="F",
            color="#c4a7e7",
            persona="Молодая служанка в трактире: быстрая, разговорчивая, любопытная, но "
                    "робеет, когда дело становится серьёзным.",
            voice="Живо, эмоционально, быстро тараторит; когда страшно, сбивается на шёпот.",
            goals="Делиться слухами, но не нажить себе беды.",
            knowledge="Прошлой ночью ты видела, как из лавки Алдрика вышла фигура в капюшоне. "
                      "Боишься говорить об этом вслух. Знаешь обычные трактирные слухи.",
            secret="Своей большой тайны у тебя нет, но ты боишься: если слишком громко говорить "
                   "о фигуре в капюшоне, тебе навредят.",
        ),
        "mareth": NPC(
            npc_id="mareth",
            name="Капитан Марет",
            role="капитан стражи",
            pronouns="F",
            color="#9ccfd8",
            default_whereabouts={
                "location_id": "town_guard_duty",
                "location_name": "служба городской стражи",
                "status": "likely",
                "details": (
                    "расследует убийство Алдрика; если точное место не установлено, её логично "
                    "искать в караульной, у городских ворот или у места находки тела"
                ),
                "source": "default public lore",
            },
            persona="Капитан городской стражи: собранная, подозрительная, строго держится "
                    "закона. Ведёт расследование убийства.",
            voice="Сухо, по делу, с лёгким нажимом. Не любит, когда посторонние лезут в дела стражи.",
            goals="Раскрыть убийство Алдрика и не допустить паники.",
            knowledge="Тело Алдрика, место преступления, показания нескольких горожан. "
                      "Подозреваешь гильдию, но доказательств нет.",
            secret="У тебя есть собственный информатор в городе, чьё имя ты никому не раскрываешь.",
        ),
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

    def _load_default(self) -> None:
        self.npcs = _npcs()
        self.public = WORLD_PUBLIC
        self.canon = WORLD_CANON
        # Story data owns proper nouns. The engine must not hardcode them.
        self.extra_proper_nouns: list[str] = ["Алдрик", "Тёрнвейл", "«Серый грифон»"]
        self.scene = _default_scene()
        # Backward-compatible alias: старый код добавляет ограничения сюда.
        self.constraints = self.scene.constraints
        self.fact_records = _default_facts()
        self.npc_whereabouts: dict[str, NPCWhereabouts] = {}
        self._ensure_npc_whereabouts()

    def _load_seed(self, seed: dict) -> None:
        seed = _normalize_seed(seed)
        self.public = _as_str(seed.get("public_intro")) or _as_str(seed.get("public")) or (
            "Новая сцена готова. Игрок видит место, людей рядом и ближайший источник конфликта."
        )
        self.canon = _as_str(seed.get("hidden_truth") or seed.get("canon"))
        self.extra_proper_nouns = [
            _as_str(name) for name in _as_list(seed.get("proper_nouns")) if _as_str(name)
        ]
        self.npcs = self._seed_npcs(seed)
        self.scene = self._seed_scene(seed)
        self.constraints = self.scene.constraints
        self.fact_records = self._seed_facts(seed)
        self.npc_whereabouts = {}
        self._ensure_npc_whereabouts()

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
                persona=_as_str(raw.get("persona")) or _as_str(raw.get("description")),
                voice=_as_str(raw.get("voice")) or "Естественно, кратко, в образе.",
                goals=_as_str(raw.get("goals")) or "Реагировать правдоподобно и защищать свои интересы.",
                knowledge=_as_str(raw.get("knowledge")) or "Только то, что очевидно в текущей сцене.",
                secret=_as_str(raw.get("secret")) or "Личная тайна не задана.",
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
        for idx, text in enumerate(_as_list(seed.get("public_facts")), start=1):
            text = _as_str(text)
            if text:
                records.append(FactRecord(
                    fact_id=f"public_{idx}",
                    kind="public",
                    text=text,
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
            f"{npc.name} ({npc_id}, {npc.role})",
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

    def retrieval_documents(self) -> list:
        """Actor-safe RAG corpus for the GM/player-facing world memory.

        Hidden canon and NPC secrets deliberately do not enter this corpus. Private NPC
        beliefs need a separate actor-filtered retrieval path; this public path is for
        get_world_fact and player-facing narration support.
        """
        from rag import RagDocument

        docs = []
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

        docs.append(RagDocument(
            doc_id=f"scene:{self.scene.scene_id}",
            kind="scene_state",
            text=(
                f"Текущая сцена: {self.scene.title}. {self.scene.description} "
                f"В сцене: {', '.join(sorted(self.scene.present_npcs)) or 'нет именованных NPC'}. "
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
            docs.append(RagDocument(
                doc_id=f"npc_public:{npc_id}",
                kind="npc_public",
                text=(
                    f"{npc.name} ({npc_id}) — {npc.role}."
                    + (f" Род: {_public_gender(npc.pronouns)} ({npc.pronouns})." if npc.pronouns else "")
                ),
                status="known",
                source="npc_roster",
                visibility="player",
                tags=(npc_id, npc.name, npc.role, npc.pronouns),
                metadata={"npc_id": npc_id},
            ))
            where = self.npc_whereabouts.get(npc_id)
            if where and where.status != "unknown":
                present_text = "присутствует в текущей сцене" if npc_id in self.scene.present_npcs else "не в текущей сцене"
                docs.append(RagDocument(
                    doc_id=f"npc_whereabouts:{npc_id}",
                    kind="npc_whereabouts",
                    text=(
                        f"{npc.name} сейчас {present_text}. Статус местонахождения: {where.status}. "
                        f"Где искать: {where.location_name or where.location_id or 'неизвестно'}."
                        + (f" Детали: {where.details}." if where.details else "")
                    ),
                    status="present" if where.status == "present" else (
                        "known" if where.status in ("known", "likely") else "unconfirmed"
                    ),
                    source=where.source or "world_state",
                    visibility="player",
                    tags=(npc_id, npc.name, where.location_id, where.location_name, where.status),
                    metadata={"npc_id": npc_id, "location_id": where.location_id},
                ))

        for rumor in self.rumors:
            if "player" not in rumor.witnesses:
                continue
            speaker = self.npcs.get(rumor.speaker)
            speaker_name = speaker.name if speaker else rumor.speaker
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
            detail = f"{npc.name} ({npc.role})"
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
            text = _as_str(npc.persona)
            cyrillic = len(re.findall(r"[а-яА-ЯёЁ]", text))
            latin = len(re.findall(r"[a-zA-Z]", text))
            if text and cyrillic >= latin:
                return text
            role = f" Публичная роль: {_public_role(npc.role)}." if npc.role else ""
            return f"Именованный персонаж текущего мира.{role} Подробности держатся в состоянии сцены."

        for npc_id in sorted(self.npcs):
            npc = self.npcs[npc_id]
            present = npc_id in self.scene.present_npcs
            presence = self.scene.presence.get(npc_id)
            whereabouts = self.npc_whereabouts.get(npc_id) or NPCWhereabouts(npc_id=npc_id)
            role = _public_role(npc.role)
            pronouns = _public_gender(npc.pronouns)
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
                npc.name,
                title=npc.name,
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
        return "Available entity refs:\nNPCs: " + npc_refs + "\nLocations: " + loc_refs

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
        data = self._roll_data(notation)
        total = int(data["total"])
        detail = str(data["detail"])
        if not data.get("ok"):
            return total, detail

        kind = self._roll_kind(roll_kind)
        target = self._coerce_int(target_number)
        if kind not in {"check", "save", "attack", "contest"} or target is None:
            return total, f"{detail}: grade=ungraded"

        margin = total - target
        grade = self._grade_from_margin(margin)
        natural = data.get("natural")
        natural_note = f", natural={natural}" if natural is not None else ""
        if kind == "attack" and natural == 20:
            grade = "critical_success"
        elif kind == "attack" and natural == 1:
            grade = "critical_failure"

        target_label = self._target_label(target_kind, kind)
        return total, f"{detail} vs {target_label} {target}: grade={grade}, margin={margin:+d}{natural_note}"

    # --- Debug / authoring mutators ---------------------------------------
    def update_npc(self, npc_id: str, fields: dict) -> bool:
        npc = self.npcs.get(npc_id)
        if npc is None or not isinstance(fields, dict):
            return False
        editable = ("name", "color", "role", "pronouns", "persona", "voice",
                    "goals", "knowledge", "secret")
        # Content fields bump card_revision when they actually change. "color" is
        # editable but cosmetic, so color-only edits must not bump the revision.
        content = ("name", "role", "pronouns", "persona", "voice",
                   "goals", "knowledge", "secret")
        content_changed = False
        for key in editable:
            if key not in fields:
                continue
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
    def fact(self, query: str) -> WorldFact:
        """Honest public lookup. Hidden truth is stored, but not returned to the GM tool."""
        q = (query or "").lower()
        try:
            import config
            if config.RAG_ENABLED:
                from rag import retrieve_world_fact
                rag_payload = retrieve_world_fact(query, self.retrieval_documents())
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
        for record in self.fact_records:
            if record.kind == "truth":
                continue
            haystack = [record.text.lower(), *[kw.lower() for kw in record.keywords]]
            if any(key and key in q for key in haystack):
                label = "rumor" if record.kind == "rumor" or not record.confirmed else "known"
                matches.append(f"{label}: {record.text}")
        if matches:
            return WorldFact("known", " ".join(matches[:3]))

        rumor_matches = []
        q_words = _match_words(q)
        for rumor in self.rumors:
            text_words = _match_words(rumor.text)
            if q_words and q_words & text_words:
                speaker = self.npcs.get(rumor.speaker)
                name = speaker.name if speaker else rumor.speaker
                rumor_matches.append(f"{name} said: «{rumor.text}»")
        if rumor_matches:
            return WorldFact("unknown", "Unconfirmed statements only: "
                             + " ".join(rumor_matches[-3:]))
        # hidden_events are GM-author-only; they must never surface through this
        # public lookup. No public hidden-events fallback here by design.
        return WorldFact("unknown", "Nothing is reliably known about this in town.")
