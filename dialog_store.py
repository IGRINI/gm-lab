"""SQLite-backed per-guest dialog persistence."""
from __future__ import annotations

import json
import os
import random
import secrets
import sqlite3
import threading
from contextlib import contextmanager
from dataclasses import dataclass, field
from typing import Callable

from orchestrator import Session
import world as world_mod

SCHEMA_VERSION = 1
DEFAULT_CHAT_TITLE = "Новый чат"


@dataclass
class DialogRuntime:
    guest_id: str
    chat_id: str
    session: Session
    transcript: list[dict] = field(default_factory=list)
    turn_count: int = 0
    title: str = ""
    preview: str = ""
    created_at: str = ""
    updated_at: str = ""
    lock: threading.RLock = field(default_factory=threading.RLock, repr=False)


class DialogStore:
    def __init__(self, db_path: str, client_factory: Callable[[], object]):
        self.db_path = os.path.abspath(db_path)
        self._client_factory = client_factory
        self._cache: dict[tuple[str, str], DialogRuntime] = {}
        self._cache_lock = threading.RLock()
        self._init_db()

    def get(self, guest_id: str, chat_id: str | None = None) -> DialogRuntime:
        if chat_id is None:
            return self.get_active(guest_id)
        runtime = self._get_chat(guest_id, chat_id)
        if runtime is None:
            raise KeyError(f"chat not found: {chat_id}")
        return runtime

    def get_active(self, guest_id: str) -> DialogRuntime:
        with self._cache_lock:
            active_chat_id = self._active_chat_id(guest_id)
            if active_chat_id:
                runtime = self._get_chat_locked(guest_id, active_chat_id)
                if runtime is not None:
                    return runtime

            latest_chat_id = self._latest_chat_id(guest_id)
            if latest_chat_id:
                self._set_active_chat(guest_id, latest_chat_id)
                runtime = self._get_chat_locked(guest_id, latest_chat_id)
                if runtime is not None:
                    return runtime

            return self.create_chat(guest_id, activate=True)

    def list_chats(self, guest_id: str) -> list[dict]:
        with self._cache_lock:
            with self._connection() as con:
                rows = con.execute(
                    """
                    SELECT chat_id, title, preview, turn_count, created_at, updated_at
                    FROM dialog_chats
                    WHERE guest_id = ?
                    ORDER BY updated_at DESC, created_at DESC, chat_id DESC
                    """,
                    (guest_id,),
                ).fetchall()
                active_chat_id = self._active_chat_id_with_connection(con, guest_id)
                chat_ids = {row[0] for row in rows}
                if rows and active_chat_id not in chat_ids:
                    active_chat_id = rows[0][0]
                    self._set_active_chat_with_connection(con, guest_id, active_chat_id)
            return [
                {
                    "id": row[0],
                    "title": row[1] or DEFAULT_CHAT_TITLE,
                    "preview": row[2] or "",
                    "turn_count": int(row[3] or 0),
                    "created_at": row[4] or "",
                    "updated_at": row[5] or "",
                    "active": row[0] == active_chat_id,
                }
                for row in rows
            ]

    def active_chat_id(self, guest_id: str) -> str | None:
        with self._cache_lock:
            active_chat_id = self._active_chat_id(guest_id)
            if active_chat_id and self._chat_exists(guest_id, active_chat_id):
                return active_chat_id
            latest_chat_id = self._latest_chat_id(guest_id)
            if latest_chat_id:
                self._set_active_chat(guest_id, latest_chat_id)
            return latest_chat_id

    def create_chat(
        self,
        guest_id: str,
        *,
        session: Session | None = None,
        transcript: list[dict] | None = None,
        turn_count: int = 0,
        title: str | None = None,
        preview: str | None = None,
        activate: bool = True,
    ) -> DialogRuntime:
        with self._cache_lock:
            chat_id = self._new_chat_id(guest_id)
            runtime = DialogRuntime(
                guest_id=guest_id,
                chat_id=chat_id,
                session=session or Session(None),
                transcript=list(transcript or []),
                turn_count=int(turn_count or 0),
                title=_clean_metadata_text(title, 80) or DEFAULT_CHAT_TITLE,
                preview=_clean_metadata_text(preview, 180),
            )
            self.save(runtime)
            if activate:
                self._set_active_chat(guest_id, chat_id)
            return runtime

    def activate_chat(self, guest_id: str, chat_id: str) -> DialogRuntime | None:
        chat_id = str(chat_id or "").strip()
        if not chat_id:
            return None
        with self._cache_lock:
            runtime = self._get_chat_locked(guest_id, chat_id)
            if runtime is None:
                return None
            self._set_active_chat(guest_id, chat_id)
            return runtime

    def _get_chat(self, guest_id: str, chat_id: str) -> DialogRuntime | None:
        with self._cache_lock:
            return self._get_chat_locked(guest_id, chat_id)

    def _get_chat_locked(self, guest_id: str, chat_id: str) -> DialogRuntime | None:
        cache_key = (guest_id, chat_id)
        cached = self._cache.get(cache_key)
        if cached is not None:
            return cached

        row = self._load_chat_row(guest_id, chat_id)
        if row is None:
            return None
        payload, title, preview, turn_count, created_at, updated_at = row
        runtime = self._runtime_from_payload(
            guest_id,
            chat_id,
            payload,
            title=title,
            preview=preview,
            created_at=created_at,
            updated_at=updated_at,
        )

        self._cache[cache_key] = runtime
        return runtime

    def save(self, runtime: DialogRuntime) -> None:
        runtime.title = _title_for_save(runtime)
        runtime.preview = _derive_preview(runtime)
        runtime.turn_count = int(runtime.turn_count or 0)
        payload = json.dumps(
            _runtime_to_payload(runtime),
            ensure_ascii=False,
            separators=(",", ":"),
        )
        with self._connection() as con:
            con.execute(
                """
                INSERT INTO dialog_chats (
                    guest_id, chat_id, title, preview, turn_count,
                    payload, created_at, updated_at
                )
                VALUES (?, ?, ?, ?, ?, ?, datetime('now'), datetime('now'))
                ON CONFLICT(guest_id, chat_id) DO UPDATE SET
                    title = excluded.title,
                    preview = excluded.preview,
                    turn_count = excluded.turn_count,
                    payload = excluded.payload,
                    updated_at = datetime('now')
                """,
                (
                    runtime.guest_id,
                    runtime.chat_id,
                    runtime.title,
                    runtime.preview,
                    runtime.turn_count,
                    payload,
                ),
            )
            saved = con.execute(
                """
                SELECT created_at, updated_at
                FROM dialog_chats
                WHERE guest_id = ? AND chat_id = ?
                """,
                (runtime.guest_id, runtime.chat_id),
            ).fetchone()
            if saved:
                runtime.created_at = saved[0] or runtime.created_at
                runtime.updated_at = saved[1] or runtime.updated_at
        with self._cache_lock:
            self._cache[(runtime.guest_id, runtime.chat_id)] = runtime

    def _runtime_from_payload(
        self,
        guest_id: str,
        chat_id: str,
        payload: str,
        *,
        title: str = "",
        preview: str = "",
        created_at: str = "",
        updated_at: str = "",
    ) -> DialogRuntime:
        data = json.loads(payload)
        if int(data.get("schema_version", 0)) != SCHEMA_VERSION:
            raise ValueError(f"unsupported schema version: {data.get('schema_version')}")
        session = _session_from_payload(data.get("session") or {}, self._client_factory)
        transcript = _json_list(data.get("transcript"))
        turn_count = int(data.get("turn_count") or 0)
        return DialogRuntime(
            guest_id=guest_id,
            chat_id=chat_id,
            session=session,
            transcript=transcript,
            turn_count=turn_count,
            title=title or "",
            preview=preview or "",
            created_at=created_at or "",
            updated_at=updated_at or "",
        )

    def _load_chat_row(self, guest_id: str, chat_id: str) -> tuple | None:
        with self._connection() as con:
            return con.execute(
                """
                SELECT payload, title, preview, turn_count, created_at, updated_at
                FROM dialog_chats
                WHERE guest_id = ? AND chat_id = ?
                """,
                (guest_id, chat_id),
            ).fetchone()

    def _new_chat_id(self, guest_id: str) -> str:
        for _ in range(32):
            chat_id = secrets.token_urlsafe(12)
            if not self._chat_exists(guest_id, chat_id):
                return chat_id
        raise RuntimeError("could not allocate unique chat id")

    def _chat_exists(self, guest_id: str, chat_id: str) -> bool:
        with self._connection() as con:
            return self._chat_exists_with_connection(con, guest_id, chat_id)

    def _chat_exists_with_connection(
        self,
        con: sqlite3.Connection,
        guest_id: str,
        chat_id: str,
    ) -> bool:
        row = con.execute(
            """
            SELECT 1
            FROM dialog_chats
            WHERE guest_id = ? AND chat_id = ?
            LIMIT 1
            """,
            (guest_id, chat_id),
        ).fetchone()
        return row is not None

    def _active_chat_id(self, guest_id: str) -> str | None:
        with self._connection() as con:
            return self._active_chat_id_with_connection(con, guest_id)

    def _active_chat_id_with_connection(
        self,
        con: sqlite3.Connection,
        guest_id: str,
    ) -> str | None:
        row = con.execute(
            "SELECT active_chat_id FROM guest_dialog_state WHERE guest_id = ?",
            (guest_id,),
        ).fetchone()
        return str(row[0]) if row and row[0] else None

    def _latest_chat_id(self, guest_id: str) -> str | None:
        with self._connection() as con:
            row = con.execute(
                """
                SELECT chat_id
                FROM dialog_chats
                WHERE guest_id = ?
                ORDER BY updated_at DESC, created_at DESC, chat_id DESC
                LIMIT 1
                """,
                (guest_id,),
            ).fetchone()
        return str(row[0]) if row and row[0] else None

    def _set_active_chat(self, guest_id: str, chat_id: str) -> None:
        with self._connection() as con:
            self._set_active_chat_with_connection(con, guest_id, chat_id)

    def _set_active_chat_with_connection(
        self,
        con: sqlite3.Connection,
        guest_id: str,
        chat_id: str,
    ) -> None:
        con.execute(
            """
            INSERT INTO guest_dialog_state (
                guest_id, active_chat_id, created_at, updated_at
            )
            VALUES (?, ?, datetime('now'), datetime('now'))
            ON CONFLICT(guest_id) DO UPDATE SET
                active_chat_id = excluded.active_chat_id,
                updated_at = datetime('now')
            """,
            (guest_id, chat_id),
        )

    def _connect(self) -> sqlite3.Connection:
        con = sqlite3.connect(self.db_path, timeout=10.0)
        con.execute("PRAGMA busy_timeout = 10000")
        con.execute("PRAGMA journal_mode = WAL")
        con.execute("PRAGMA synchronous = NORMAL")
        return con

    @contextmanager
    def _connection(self):
        con = self._connect()
        try:
            yield con
            con.commit()
        finally:
            con.close()

    def _init_db(self) -> None:
        parent = os.path.dirname(self.db_path)
        if parent:
            os.makedirs(parent, exist_ok=True)
        with self._connection() as con:
            con.execute(
                """
                CREATE TABLE IF NOT EXISTS dialog_chats (
                    guest_id TEXT NOT NULL,
                    chat_id TEXT NOT NULL,
                    title TEXT NOT NULL,
                    preview TEXT NOT NULL,
                    turn_count INTEGER NOT NULL DEFAULT 0,
                    payload TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (guest_id, chat_id)
                )
                """
            )
            con.execute(
                """
                CREATE INDEX IF NOT EXISTS idx_dialog_chats_guest_updated
                ON dialog_chats(guest_id, updated_at)
                """
            )
            con.execute(
                """
                CREATE TABLE IF NOT EXISTS guest_dialog_state (
                    guest_id TEXT PRIMARY KEY,
                    active_chat_id TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )
                """
            )


def _title_for_save(runtime: DialogRuntime) -> str:
    title = _clean_metadata_text(runtime.title, 80)
    if title:
        return title
    return _derive_missing_title(runtime) or DEFAULT_CHAT_TITLE


def _derive_missing_title(runtime: DialogRuntime) -> str:
    scene_title = _clean_metadata_text(
        getattr(getattr(runtime.session, "world", None), "scene", None)
        and getattr(runtime.session.world.scene, "title", ""),
        80,
    )
    if scene_title:
        return scene_title
    first_player = _first_player_event_text(runtime.transcript)
    if first_player:
        return _clean_metadata_text(first_player, 80)
    return ""


def _derive_preview(runtime: DialogRuntime) -> str:
    last_event = _last_transcript_text(runtime.transcript)
    if last_event:
        return _clean_metadata_text(last_event, 180)
    last_action = _clean_metadata_text(getattr(runtime.session, "last_player_action", ""), 180)
    if last_action:
        return last_action
    scene = getattr(getattr(runtime.session, "world", None), "scene", None)
    scene_description = _clean_metadata_text(getattr(scene, "description", ""), 180)
    if scene_description:
        return scene_description
    return _clean_metadata_text(runtime.title, 180)


def _first_player_event_text(transcript: list[dict]) -> str:
    for row in transcript:
        event = row.get("event") if isinstance(row, dict) else None
        if not isinstance(event, dict):
            continue
        kind = str(event.get("kind") or "").lower()
        agent = str(event.get("agent") or "").lower()
        if kind == "player" or agent in {"player", "игрок"}:
            text = _event_text(event)
            if text:
                return text
    return ""


def _last_transcript_text(transcript: list[dict]) -> str:
    for row in reversed(transcript):
        event = row.get("event") if isinstance(row, dict) else None
        if not isinstance(event, dict):
            continue
        text = _event_text(event)
        if text:
            return text
    return ""


def _event_text(event: dict) -> str:
    for key in ("data", "text", "speech", "action"):
        value = event.get(key)
        if isinstance(value, str):
            text = _clean_metadata_text(value)
            if text:
                return text
    return ""


def _clean_metadata_text(value: object, limit: int = 160) -> str:
    if not isinstance(value, str):
        return ""
    text = " ".join(value.split())
    if len(text) <= limit:
        return text
    return text[: max(0, limit - 3)].rstrip() + "..."


def _runtime_to_payload(runtime: DialogRuntime) -> dict:
    return {
        "schema_version": SCHEMA_VERSION,
        "turn_count": int(runtime.turn_count),
        "session": _session_to_payload(runtime.session),
        "transcript": _json_list(runtime.transcript),
    }


def _session_to_payload(session: Session) -> dict:
    return {
        "client_model": str(
            getattr(session, "client_model", "")
            or getattr(getattr(session, "client", None), "model", "")
            or ""
        ),
        "client_backend": str(getattr(session, "client_backend", "") or ""),
        "client_session_id": str(
            getattr(session, "client_session_id", "")
            or getattr(getattr(session, "client", None), "session_id", "")
            or ""
        ),
        "client_thread_id": str(
            getattr(session, "client_thread_id", "")
            or getattr(getattr(session, "client", None), "thread_id", "")
            or ""
        ),
        "world": _world_to_payload(session.world),
        "gm_messages": [_json_value(msg) for msg in session.gm_messages],
        "gm_summary": session.gm_summary,
        "npc_messages": {
            str(npc_id): [_json_value(msg) for msg in messages]
            for npc_id, messages in getattr(session, "npc_messages", {}).items()
        },
        "npc_summaries": {
            str(npc_id): str(summary)
            for npc_id, summary in getattr(session, "npc_summaries", {}).items()
        },
        "run_usage": getattr(session, "run_usage", {}),
        "npc_client_state": {
            str(npc_id): {
                "model": str((state or {}).get("model") or ""),
                "session_id": str((state or {}).get("session_id") or ""),
                "thread_id": str((state or {}).get("thread_id") or ""),
            }
            for npc_id, state in getattr(session, "npc_client_state", {}).items()
            if isinstance(state, dict)
        },
        "last_player_action": session.last_player_action,
        "sid": int(getattr(session, "_sid", 0)),
        "events": [_event_to_payload(event) for event in session.events],
        "seq": int(getattr(session, "_seq", 0)),
        "turn": int(session.turn),
        "delivered": {str(k): int(v) for k, v in session.delivered.items()},
        "shown": {str(k): int(v) for k, v in getattr(session, "_shown", {}).items()},
        "pending": _pending_to_payload(session.pending),
        "commitments": {
            str(npc_id): [str(item) for item in blocks]
            for npc_id, blocks in session.commitments.items()
        },
    }


def _session_from_payload(data: dict, client_factory: Callable[[], object]) -> Session:
    session = Session(None, _world_from_payload(data.get("world") or {}))
    session.client_model = str(data.get("client_model") or "")
    stored_backend = str(data.get("client_backend") or "")
    session.client_backend = stored_backend
    session.client_session_id = str(data.get("client_session_id") or "")
    session.client_thread_id = str(data.get("client_thread_id") or "")
    session.gm_messages = _json_list(data.get("gm_messages"))
    session.gm_summary = str(data.get("gm_summary") or "")
    session.npc_messages = {
        str(k): [_json_value(msg) for msg in _json_list(v)]
        for k, v in _json_dict(data.get("npc_messages")).items()
    }
    session.npc_summaries = {
        str(k): str(v)
        for k, v in _json_dict(data.get("npc_summaries")).items()
    }
    session.set_run_usage(_json_dict(data.get("run_usage")))
    session.npc_client_state = {
        str(k): {
            "model": str(_json_dict(v).get("model") or ""),
            "session_id": str(_json_dict(v).get("session_id") or ""),
            "thread_id": str(_json_dict(v).get("thread_id") or ""),
        }
        for k, v in _json_dict(data.get("npc_client_state")).items()
    }
    session.last_player_action = str(data.get("last_player_action") or "")
    session._sid = int(data.get("sid") or 0)
    session.events = [_event_from_payload(row) for row in _json_list(data.get("events"))]
    session._seq = int(data.get("seq") or 0)
    session.turn = int(data.get("turn") or 0)
    session.delivered = {
        str(k): int(v) for k, v in _json_dict(data.get("delivered")).items()
    }
    session._shown = {
        str(k): int(v) for k, v in _json_dict(data.get("shown")).items()
    }
    session.pending = _pending_from_payload(data.get("pending"))
    session.commitments = {
        str(k): [str(item) for item in _json_list(v)]
        for k, v in _json_dict(data.get("commitments")).items()
    }
    return session


def _world_to_payload(world: world_mod.World) -> dict:
    return {
        "story_id": str(getattr(world, "story_id", "") or ""),
        "story_title": str(getattr(world, "story_title", "") or ""),
        "dice_seed": int(getattr(world, "dice_seed", 0)),
        "forced_die_next": getattr(world, "forced_die_next", None),
        "forced_die_all": getattr(world, "forced_die_all", None),
        "rng_state": _rng_state_to_payload(world._rng.getstate()),
        "hidden_events": [str(item) for item in world.hidden_events],
        "rumors": [_rumor_to_payload(rumor) for rumor in world.rumors],
        "rumor_seq": int(getattr(world, "_rumor_seq", 0)),
        "npcs": {
            str(npc_id): _npc_to_payload(npc)
            for npc_id, npc in world.npcs.items()
        },
        "public": world.public,
        "canon": world.canon,
        "time": _time_to_payload(getattr(world, "time", world_mod.WorldTime())),
        "player_character": _player_character_to_payload(
            getattr(world, "player_character", world_mod.PlayerCharacter())
        ),
        "extra_proper_nouns": [str(name) for name in world.extra_proper_nouns],
        "scene": _scene_to_payload(world.scene),
        "npc_whereabouts": world.npc_whereabouts_export(),
        "fact_records": [_fact_to_payload(record) for record in world.fact_records],
        "state_records": [
            _state_record_to_payload(record)
            for record in getattr(world, "state_records", [])
        ],
    }


def _world_from_payload(data: dict) -> world_mod.World:
    if not isinstance(data, dict):
        raise ValueError("invalid world payload")
    required = ("story_id", "story_title", "npcs", "public", "canon", "scene", "fact_records")
    missing = [key for key in required if key not in data]
    if missing:
        raise ValueError("unsupported world payload: missing " + ", ".join(missing))

    world = world_mod.World.__new__(world_mod.World)
    world.story_id = str(data.get("story_id") or "")
    world.story_title = str(data.get("story_title") or "")
    world.hidden_events = [str(item) for item in _json_list(data.get("hidden_events"))]
    world.rumors = [_rumor_from_payload(row) for row in _json_list(data.get("rumors"))]
    world._rumor_seq = int(data.get("rumor_seq") or 0)

    npcs = _json_dict(data.get("npcs"))
    if not npcs:
        raise ValueError("unsupported world payload: npcs is required")
    world.npcs = {
        str(npc_id): _npc_from_payload(npc)
        for npc_id, npc in npcs.items()
        if isinstance(npc, dict)
    }
    world.public = str(data.get("public") or "")
    world.canon = str(data.get("canon") or "")
    world.time = _time_from_payload(data.get("time"))
    world.player_character = _player_character_from_payload(data.get("player_character"))
    world.extra_proper_nouns = [
        str(name) for name in _json_list(data.get("extra_proper_nouns"))
    ]
    if not isinstance(data.get("scene"), dict):
        raise ValueError("unsupported world payload: scene is required")
    world.scene = _scene_from_payload(data["scene"])
    world.constraints = world.scene.constraints
    whereabouts = _json_dict(data.get("npc_whereabouts"))
    if whereabouts:
        world.npc_whereabouts = {
            str(npc_id): _whereabouts_from_payload(str(npc_id), row)
            for npc_id, row in whereabouts.items()
            if isinstance(row, dict)
        }
    facts = _json_list(data.get("fact_records"))
    if not facts:
        raise ValueError("unsupported world payload: fact_records is required")
    world.fact_records = [_fact_from_payload(row) for row in facts]
    world.state_records = [
        _state_record_from_payload(row)
        for row in _json_list(data.get("state_records"))
        if isinstance(row, dict)
    ]
    world._ensure_npc_whereabouts()

    if data.get("dice_seed") is None:
        raise ValueError("unsupported world payload: dice_seed is required")
    world.dice_seed = int(data.get("dice_seed") or 0)
    _fn = data.get("forced_die_next")
    world.forced_die_next = int(_fn) if _fn is not None else None
    _fa = data.get("forced_die_all")
    world.forced_die_all = int(_fa) if _fa is not None else None
    rng_state = _rng_state_from_payload(data.get("rng_state"))
    if rng_state is None:
        raise ValueError("unsupported world payload: rng_state is required")
    world._rng = random.Random()
    world._rng.setstate(rng_state)
    return world


def _player_character_to_payload(pc: world_mod.PlayerCharacter) -> dict:
    return {
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
        "gm_notes": pc.gm_notes,
        "abilities": _json_dict(getattr(pc, "abilities", {})),
        "skills": _json_dict(getattr(pc, "skills", {})),
        "saving_throws": _json_dict(getattr(pc, "saving_throws", {})),
        "passive_perception": getattr(pc, "passive_perception", None),
        "ac": _json_value(getattr(pc, "ac", None)),
        "hp": _json_dict(getattr(pc, "hp", {})),
        "speed": pc.speed,
        "senses": pc.senses,
        "languages": pc.languages,
        "inventory": [str(item) for item in _json_list(getattr(pc, "inventory", []))],
        "equipment": [str(item) for item in _json_list(getattr(pc, "equipment", []))],
        "features": [str(item) for item in _json_list(getattr(pc, "features", []))],
        "card_revision": int(getattr(pc, "card_revision", 0) or 0),
    }


def _player_character_from_payload(data: object) -> world_mod.PlayerCharacter:
    if not isinstance(data, dict):
        return world_mod.PlayerCharacter()
    try:
        card_revision = int(data.get("card_revision") or 0)
    except (TypeError, ValueError):
        card_revision = 0
    return world_mod.PlayerCharacter(
        name=str(data.get("name") or "Искатель"),
        pronouns=str(data.get("pronouns") or "OTHER"),
        class_role=str(data.get("class_role") or ""),
        level=_int_or_none(data.get("level")),
        background=str(data.get("background") or ""),
        age=str(data.get("age") or ""),
        physical_type=str(data.get("physical_type") or ""),
        distinctive_features=str(data.get("distinctive_features") or ""),
        life_status=str(data.get("life_status") or "alive"),
        life_status_note=str(data.get("life_status_note") or ""),
        condition=str(data.get("condition") or ""),
        personality=str(data.get("personality") or ""),
        values=str(data.get("values") or ""),
        gm_notes=str(data.get("gm_notes") or ""),
        abilities=_json_dict(data.get("abilities")),
        skills=_json_dict(data.get("skills")),
        saving_throws=_json_dict(data.get("saving_throws")),
        passive_perception=_int_or_none(data.get("passive_perception")),
        ac=_json_value(data.get("ac")),
        hp=_json_dict(data.get("hp")),
        speed=str(data.get("speed") or ""),
        senses=str(data.get("senses") or ""),
        languages=str(data.get("languages") or ""),
        inventory=[str(item) for item in _json_list(data.get("inventory"))],
        equipment=[str(item) for item in _json_list(data.get("equipment"))],
        features=[str(item) for item in _json_list(data.get("features"))],
        card_revision=card_revision,
    )


def _time_to_payload(time: world_mod.WorldTime) -> dict:
    return {
        "calendar_name": str(getattr(time, "calendar_name", "") or ""),
        "absolute_minutes": int(getattr(time, "absolute_minutes", 0) or 0),
        "current_date_label": str(getattr(time, "current_date_label", "") or ""),
        "minutes_per_hour": int(getattr(time, "minutes_per_hour", 60) or 60),
        "hours_per_day": int(getattr(time, "hours_per_day", 24) or 24),
        "day_names": [str(item) for item in _json_list(getattr(time, "day_names", []))],
        "month_names": [str(item) for item in _json_list(getattr(time, "month_names", []))],
        "last_advance_minutes": int(getattr(time, "last_advance_minutes", 0) or 0),
        "last_advance_reason": str(getattr(time, "last_advance_reason", "") or ""),
    }


def _time_from_payload(data: object) -> world_mod.WorldTime:
    row = data if isinstance(data, dict) else {}
    return world_mod.WorldTime(
        calendar_name=str(row.get("calendar_name") or ""),
        absolute_minutes=max(0, _int_or_none(row.get("absolute_minutes")) or 0),
        current_date_label=str(row.get("current_date_label") or "День 1"),
        minutes_per_hour=max(1, _int_or_none(row.get("minutes_per_hour")) or 60),
        hours_per_day=max(1, _int_or_none(row.get("hours_per_day")) or 24),
        day_names=[str(item) for item in _json_list(row.get("day_names"))],
        month_names=[str(item) for item in _json_list(row.get("month_names"))],
        last_advance_minutes=max(0, _int_or_none(row.get("last_advance_minutes")) or 0),
        last_advance_reason=str(row.get("last_advance_reason") or ""),
    )


def _event_to_payload(event: world_mod.Event) -> dict:
    return {
        "seq": int(event.seq),
        "turn": int(event.turn),
        "actor": event.actor,
        "kind": event.kind,
        "speech": event.speech,
        "action": event.action,
        "witnesses": sorted(event.witnesses),
    }


def _event_from_payload(data: dict) -> world_mod.Event:
    return world_mod.Event(
        seq=int(data.get("seq") or 0),
        turn=int(data.get("turn") or 0),
        actor=str(data.get("actor") or ""),
        kind=str(data.get("kind") or ""),
        speech=str(data.get("speech") or ""),
        action=str(data.get("action") or ""),
        witnesses=frozenset(str(item) for item in _json_list(data.get("witnesses"))),
    )


def _npc_to_payload(npc: world_mod.NPC) -> dict:
    return {
        "npc_id": npc.npc_id,
        "name": npc.name,
        "persona": npc.persona,
        "voice": npc.voice,
        "goals": npc.goals,
        "knowledge": npc.knowledge,
        "secret": npc.secret,
        "role": npc.role,
        "pronouns": npc.pronouns,
        "color": getattr(npc, "color", ""),
        "public_label": getattr(npc, "public_label", ""),
        "age": getattr(npc, "age", ""),
        "physical_type": getattr(npc, "physical_type", ""),
        "distinctive_features": getattr(npc, "distinctive_features", ""),
        "life_status": getattr(npc, "life_status", "alive"),
        "life_status_note": getattr(npc, "life_status_note", ""),
        "condition": getattr(npc, "condition", ""),
        "personality": getattr(npc, "personality", ""),
        "values": getattr(npc, "values", ""),
        "habits": getattr(npc, "habits", ""),
        "pressure_response": getattr(npc, "pressure_response", ""),
        "boundaries": getattr(npc, "boundaries", ""),
        "abilities": _json_dict(getattr(npc, "abilities", {})),
        "skills": _json_dict(getattr(npc, "skills", {})),
        "saving_throws": _json_dict(getattr(npc, "saving_throws", {})),
        "passive_perception": getattr(npc, "passive_perception", None),
        "ac": _json_value(getattr(npc, "ac", None)),
        "hp": _json_dict(getattr(npc, "hp", {})),
        "speed": getattr(npc, "speed", ""),
        "senses": getattr(npc, "senses", ""),
        "languages": getattr(npc, "languages", ""),
        "default_whereabouts": getattr(npc, "default_whereabouts", None),
        "card_revision": int(getattr(npc, "card_revision", 0) or 0),
    }


def _npc_from_payload(data: dict) -> world_mod.NPC:
    try:
        card_revision = int(data.get("card_revision") or 0)
    except (TypeError, ValueError):
        card_revision = 0
    npc_id = str(data.get("npc_id") or "")
    color = str(data.get("color") or "")
    dw = data.get("default_whereabouts")
    dw = dw if isinstance(dw, dict) else None
    return world_mod.NPC(
        npc_id=npc_id,
        name=str(data.get("name") or ""),
        persona=str(data.get("persona") or ""),
        voice=str(data.get("voice") or ""),
        goals=str(data.get("goals") or ""),
        knowledge=str(data.get("knowledge") or ""),
        secret=str(data.get("secret") or ""),
        role=str(data.get("role") or ""),
        pronouns=str(data.get("pronouns") or ""),
        color=color,
        public_label=str(data.get("public_label") or ""),
        age=str(data.get("age") or ""),
        physical_type=str(data.get("physical_type") or ""),
        distinctive_features=str(data.get("distinctive_features") or ""),
        life_status=str(data.get("life_status") or "alive"),
        life_status_note=str(data.get("life_status_note") or ""),
        condition=str(data.get("condition") or ""),
        personality=str(data.get("personality") or ""),
        values=str(data.get("values") or ""),
        habits=str(data.get("habits") or ""),
        pressure_response=str(data.get("pressure_response") or ""),
        boundaries=str(data.get("boundaries") or ""),
        abilities=_json_dict(data.get("abilities")),
        skills=_json_dict(data.get("skills")),
        saving_throws=_json_dict(data.get("saving_throws")),
        passive_perception=_int_or_none(data.get("passive_perception")),
        ac=_json_value(data.get("ac")),
        hp=_json_dict(data.get("hp")),
        speed=str(data.get("speed") or ""),
        senses=str(data.get("senses") or ""),
        languages=str(data.get("languages") or ""),
        default_whereabouts=dw,
        card_revision=card_revision,
    )


def _scene_to_payload(scene: world_mod.SceneState) -> dict:
    return {
        "scene_id": scene.scene_id,
        "location_id": scene.location_id,
        "title": scene.title,
        "description": scene.description,
        "present_npcs": sorted(scene.present_npcs),
        "presence": {
            str(npc_id): _presence_to_payload(presence)
            for npc_id, presence in scene.presence.items()
        },
        "items": [_item_to_payload(item) for item in scene.items],
        "exits": [_exit_to_payload(exit_) for exit_ in scene.exits],
        "constraints": [str(item) for item in scene.constraints],
        "tension": scene.tension,
        "player_seen": [str(item) for item in scene.player_seen],
    }


def _scene_from_payload(data: dict) -> world_mod.SceneState:
    return world_mod.SceneState(
        scene_id=str(data.get("scene_id") or ""),
        location_id=str(data.get("location_id") or ""),
        title=str(data.get("title") or ""),
        description=str(data.get("description") or ""),
        present_npcs=set(str(item) for item in _json_list(data.get("present_npcs"))),
        presence={
            str(npc_id): _presence_from_payload(presence)
            for npc_id, presence in _json_dict(data.get("presence")).items()
            if isinstance(presence, dict)
        },
        items=[_item_from_payload(item) for item in _json_list(data.get("items"))],
        exits=[_exit_from_payload(exit_) for exit_ in _json_list(data.get("exits"))],
        constraints=[str(item) for item in _json_list(data.get("constraints"))],
        tension=str(data.get("tension") or ""),
        player_seen=[str(item) for item in _json_list(data.get("player_seen"))],
    )


def _presence_to_payload(presence: world_mod.Presence) -> dict:
    return {
        "npc_id": presence.npc_id,
        "location": presence.location,
        "visible": bool(presence.visible),
        "can_hear": bool(presence.can_hear),
        "activity": presence.activity,
        "attitude": presence.attitude,
    }


def _presence_from_payload(data: dict) -> world_mod.Presence:
    return world_mod.Presence(
        npc_id=str(data.get("npc_id") or ""),
        location=str(data.get("location") or ""),
        visible=bool(data.get("visible", True)),
        can_hear=bool(data.get("can_hear", True)),
        activity=str(data.get("activity") or ""),
        attitude=str(data.get("attitude") or ""),
    )


def _whereabouts_from_payload(npc_id: str, data: dict) -> world_mod.NPCWhereabouts:
    return world_mod.NPCWhereabouts(
        npc_id=str(data.get("npc_id") or npc_id),
        location_id=str(data.get("location_id") or ""),
        location_name=str(data.get("location_name") or ""),
        status=str(data.get("status") or "unknown"),
        details=str(data.get("details") or ""),
        source=str(data.get("source") or ""),
    )


def _item_to_payload(item: world_mod.SceneItem) -> dict:
    return {
        "item_id": item.item_id,
        "name": item.name,
        "location": item.location,
        "visible": bool(item.visible),
        "portable": bool(item.portable),
        "owner": item.owner,
        "details": item.details,
    }


def _item_from_payload(data: dict) -> world_mod.SceneItem:
    return world_mod.SceneItem(
        item_id=str(data.get("item_id") or ""),
        name=str(data.get("name") or ""),
        location=str(data.get("location") or ""),
        visible=bool(data.get("visible", True)),
        portable=bool(data.get("portable", False)),
        owner=str(data.get("owner") or ""),
        details=str(data.get("details") or ""),
    )


def _exit_to_payload(exit_: world_mod.SceneExit) -> dict:
    return {
        "exit_id": exit_.exit_id,
        "name": exit_.name,
        "destination": exit_.destination,
        "visible": bool(exit_.visible),
        "blocked_by": exit_.blocked_by,
    }


def _exit_from_payload(data: dict) -> world_mod.SceneExit:
    return world_mod.SceneExit(
        exit_id=str(data.get("exit_id") or ""),
        name=str(data.get("name") or ""),
        destination=str(data.get("destination") or ""),
        visible=bool(data.get("visible", True)),
        blocked_by=str(data.get("blocked_by") or ""),
    )


def _fact_to_payload(record: world_mod.FactRecord) -> dict:
    return {
        "fact_id": record.fact_id,
        "kind": record.kind,
        "text": record.text,
        "keywords": [str(item) for item in record.keywords],
        "source": record.source,
        "confirmed": bool(record.confirmed),
    }


def _fact_from_payload(data: dict) -> world_mod.FactRecord:
    return world_mod.FactRecord(
        fact_id=str(data.get("fact_id") or ""),
        kind=str(data.get("kind") or ""),
        text=str(data.get("text") or ""),
        keywords=[str(item) for item in _json_list(data.get("keywords"))],
        source=str(data.get("source") or ""),
        confirmed=bool(data.get("confirmed", True)),
    )


def _state_record_to_payload(record: world_mod.StateRecord) -> dict:
    metadata = _json_value(record.metadata)
    return {
        "record_id": record.record_id,
        "kind": record.kind,
        "text": record.text,
        "scope": record.scope,
        "active": bool(record.active),
        "owner": record.owner,
        "subject": record.subject,
        "source": record.source,
        "status": record.status,
        "tags": [str(item) for item in record.tags],
        "entity_id": getattr(record, "entity_id", ""),
        "source_npc": getattr(record, "source_npc", ""),
        "location_id": getattr(record, "location_id", ""),
        "location_name": getattr(record, "location_name", ""),
        "region_id": getattr(record, "region_id", ""),
        "region_name": getattr(record, "region_name", ""),
        "scene_id": getattr(record, "scene_id", ""),
        "importance": getattr(record, "importance", ""),
        "aliases": [str(item) for item in getattr(record, "aliases", ())],
        "metadata": metadata if isinstance(metadata, dict) else {},
    }


def _state_record_from_payload(data: dict) -> world_mod.StateRecord:
    return world_mod.StateRecord(
        record_id=str(data.get("record_id") or data.get("id") or ""),
        kind=str(data.get("kind") or "fact"),
        text=str(data.get("text") or ""),
        scope=str(data.get("scope") or "public"),
        active=bool(data.get("active", True)),
        owner=str(data.get("owner") or data.get("owner_id") or ""),
        subject=str(data.get("subject") or data.get("subject_id") or ""),
        source=str(data.get("source") or ""),
        status=str(data.get("status") or "known"),
        tags=tuple(str(item) for item in _json_list(data.get("tags"))),
        entity_id=str(data.get("entity_id") or data.get("entity") or data.get("about") or ""),
        source_npc=str(data.get("source_npc") or data.get("source_npc_id") or ""),
        location_id=str(data.get("location_id") or ""),
        location_name=str(data.get("location_name") or ""),
        region_id=str(data.get("region_id") or ""),
        region_name=str(data.get("region_name") or ""),
        scene_id=str(data.get("scene_id") or ""),
        importance=str(data.get("importance") or ""),
        aliases=tuple(str(item) for item in _json_list(data.get("aliases"))),
        metadata=_json_dict(data.get("metadata")),
    )


def _rumor_to_payload(rumor: world_mod.Rumor) -> dict:
    return {
        "seq": int(rumor.seq),
        "turn": int(rumor.turn),
        "speaker": rumor.speaker,
        "text": rumor.text,
        "witnesses": sorted(rumor.witnesses),
        "confirmed": bool(rumor.confirmed),
    }


def _rumor_from_payload(data: dict) -> world_mod.Rumor:
    return world_mod.Rumor(
        seq=int(data.get("seq") or 0),
        turn=int(data.get("turn") or 0),
        speaker=str(data.get("speaker") or ""),
        text=str(data.get("text") or ""),
        witnesses=frozenset(str(item) for item in _json_list(data.get("witnesses"))),
        confirmed=bool(data.get("confirmed", False)),
    )


def _pending_to_payload(pending: dict[str, dict]) -> dict:
    out = {}
    for npc_id, row in pending.items():
        if not isinstance(row, dict):
            continue
        # Persist the LLM turn (user_message/assistant_message) too: commit_turn() only
        # appends a turn to npc_messages when BOTH are present, so dropping them here loses
        # the NPC's private history if a save lands between draft() and commit_turn().
        out[str(npc_id)] = {
            "seq": int(row.get("seq") or 0),
            "speech": str(row.get("speech") or ""),
            "action": str(row.get("action") or ""),
            "claims": [str(item) for item in _json_list(row.get("claims"))],
            "witnesses": [str(item) for item in _json_list(row.get("witnesses"))],
            "user_message": _json_value(row.get("user_message")),
            "assistant_message": _json_value(row.get("assistant_message")),
        }
    return out


def _pending_from_payload(data: object) -> dict[str, dict]:
    out = {}
    for npc_id, row in _json_dict(data).items():
        if not isinstance(row, dict):
            continue
        user_message = row.get("user_message")
        assistant_message = row.get("assistant_message")
        out[str(npc_id)] = {
            "seq": int(row.get("seq") or 0),
            "speech": str(row.get("speech") or ""),
            "action": str(row.get("action") or ""),
            "claims": [str(item) for item in _json_list(row.get("claims"))],
            "witnesses": frozenset(str(item) for item in _json_list(row.get("witnesses"))),
            "user_message": user_message if isinstance(user_message, dict) else None,
            "assistant_message": assistant_message if isinstance(assistant_message, dict) else None,
        }
    return out


def _rng_state_to_payload(state: tuple) -> dict:
    version, internal, gauss = state
    return {
        "version": int(version),
        "internal": [int(item) for item in internal],
        "gauss": gauss,
    }


def _rng_state_from_payload(data: object) -> tuple | None:
    if not isinstance(data, dict):
        return None
    internal = data.get("internal")
    if not isinstance(internal, list):
        return None
    return (
        int(data.get("version") or 3),
        tuple(int(item) for item in internal),
        data.get("gauss"),
    )


def _json_value(value):
    if hasattr(value, "model_dump"):
        value = value.model_dump()
    try:
        json.dumps(value, ensure_ascii=False)
        return value
    except TypeError:
        return str(value)


def _int_or_none(value):
    if value is None or isinstance(value, bool):
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def _json_list(value: object) -> list:
    return value if isinstance(value, list) else []


def _json_dict(value: object) -> dict:
    return value if isinstance(value, dict) else {}
