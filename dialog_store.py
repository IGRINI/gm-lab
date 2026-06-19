"""SQLite-backed per-guest dialog persistence."""
from __future__ import annotations

import json
import os
import random
import sqlite3
import threading
from contextlib import contextmanager
from dataclasses import dataclass, field
from typing import Callable

from orchestrator import Session
import world as world_mod

SCHEMA_VERSION = 1


@dataclass
class DialogRuntime:
    guest_id: str
    session: Session
    transcript: list[dict] = field(default_factory=list)
    turn_count: int = 0
    lock: threading.RLock = field(default_factory=threading.RLock, repr=False)


class DialogStore:
    def __init__(self, db_path: str, client_factory: Callable[[], object]):
        self.db_path = os.path.abspath(db_path)
        self._client_factory = client_factory
        self._cache: dict[str, DialogRuntime] = {}
        self._cache_lock = threading.RLock()
        self._init_db()

    def get(self, guest_id: str) -> DialogRuntime:
        with self._cache_lock:
            cached = self._cache.get(guest_id)
            if cached is not None:
                return cached

            payload = self._load_payload(guest_id)
            if payload:
                try:
                    runtime = self._runtime_from_payload(guest_id, payload)
                except Exception as exc:
                    print(f"Dialog store: failed to load {guest_id}, starting fresh: {exc}")
                    runtime = self._fresh_runtime(guest_id)
                    self.save(runtime)
            else:
                runtime = self._fresh_runtime(guest_id)
                self.save(runtime)

            self._cache[guest_id] = runtime
            return runtime

    def save(self, runtime: DialogRuntime) -> None:
        payload = json.dumps(
            _runtime_to_payload(runtime),
            ensure_ascii=False,
            separators=(",", ":"),
        )
        with self._connection() as con:
            con.execute(
                """
                INSERT INTO guest_dialogs (guest_id, payload, created_at, updated_at)
                VALUES (?, ?, datetime('now'), datetime('now'))
                ON CONFLICT(guest_id) DO UPDATE SET
                    payload = excluded.payload,
                    updated_at = datetime('now')
                """,
                (runtime.guest_id, payload),
            )

    def _fresh_runtime(self, guest_id: str) -> DialogRuntime:
        return DialogRuntime(guest_id=guest_id, session=Session(None))

    def _runtime_from_payload(self, guest_id: str, payload: str) -> DialogRuntime:
        data = json.loads(payload)
        if int(data.get("schema_version", 0)) != SCHEMA_VERSION:
            raise ValueError(f"unsupported schema version: {data.get('schema_version')}")
        session = _session_from_payload(data.get("session") or {}, self._client_factory)
        transcript = _json_list(data.get("transcript"))
        turn_count = int(data.get("turn_count") or 0)
        return DialogRuntime(
            guest_id=guest_id,
            session=session,
            transcript=transcript,
            turn_count=turn_count,
        )

    def _load_payload(self, guest_id: str) -> str | None:
        with self._connection() as con:
            row = con.execute(
                "SELECT payload FROM guest_dialogs WHERE guest_id = ?",
                (guest_id,),
            ).fetchone()
        return row[0] if row else None

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
                CREATE TABLE IF NOT EXISTS guest_dialogs (
                    guest_id TEXT PRIMARY KEY,
                    payload TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                )
                """
            )
            con.execute(
                """
                CREATE INDEX IF NOT EXISTS idx_guest_dialogs_updated_at
                ON guest_dialogs(updated_at)
                """
            )


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
    if not stored_backend:
        stored_backend = "codex" if session.client_model.startswith("gpt-") else "legacy"
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
        "extra_proper_nouns": [str(name) for name in world.extra_proper_nouns],
        "scene": _scene_to_payload(world.scene),
        "npc_whereabouts": world.npc_whereabouts_export(),
        "fact_records": [_fact_to_payload(record) for record in world.fact_records],
    }


def _world_from_payload(data: dict) -> world_mod.World:
    world = world_mod.World()
    world.hidden_events = [str(item) for item in _json_list(data.get("hidden_events"))]
    world.rumors = [_rumor_from_payload(row) for row in _json_list(data.get("rumors"))]
    world._rumor_seq = int(data.get("rumor_seq") or 0)

    npcs = _json_dict(data.get("npcs"))
    if npcs:
        # Restore the FULL saved card — debug-panel edits to persona/goals/secret/etc.
        # must persist. Card-derived visuals (color/default_whereabouts) are backfilled
        # from the card definition inside _npc_from_payload only when the save lacks them.
        world.npcs = {
            str(npc_id): _npc_from_payload(npc)
            for npc_id, npc in npcs.items()
            if isinstance(npc, dict)
        }
    world.public = str(data.get("public") or world.public)
    world.canon = str(data.get("canon") or world.canon)
    world.extra_proper_nouns = [
        str(name) for name in _json_list(data.get("extra_proper_nouns"))
    ]
    if isinstance(data.get("scene"), dict):
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
    if facts:
        world.fact_records = [_fact_from_payload(row) for row in facts]
    world._ensure_npc_whereabouts()

    if data.get("dice_seed") is not None:
        world.dice_seed = int(data.get("dice_seed") or 0)
    _fn = data.get("forced_die_next")
    world.forced_die_next = int(_fn) if _fn is not None else None
    _fa = data.get("forced_die_all")
    world.forced_die_all = int(_fa) if _fa is not None else None
    rng_state = _rng_state_from_payload(data.get("rng_state"))
    if rng_state is not None:
        world._rng = random.Random()
        world._rng.setstate(rng_state)
    return world


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
        "default_whereabouts": getattr(npc, "default_whereabouts", None),
        "card_revision": int(getattr(npc, "card_revision", 0) or 0),
    }


def _npc_from_payload(data: dict) -> world_mod.NPC:
    try:
        card_revision = int(data.get("card_revision") or 0)
    except (TypeError, ValueError):
        card_revision = 0
    npc_id = str(data.get("npc_id") or "")
    # Saved card values win (debug edits persist). Card-derived visuals are backfilled
    # from the card DEFINITION only when the save predates them, so old sessions show
    # color/default whereabouts without a migration and without overriding any edit.
    color = str(data.get("color") or "")
    dw = data.get("default_whereabouts")
    dw = dw if isinstance(dw, dict) else None
    if not color or dw is None:
        definition = world_mod._npcs().get(npc_id)
        if definition:
            if not color:
                color = definition.color
            if dw is None and definition.default_whereabouts:
                dw = definition.default_whereabouts
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


def _json_list(value: object) -> list:
    return value if isinstance(value, list) else []


def _json_dict(value: object) -> dict:
    return value if isinstance(value, dict) else {}
