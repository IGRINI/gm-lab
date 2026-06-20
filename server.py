"""Local web server for GM-Lab.

GET  /           -> index.html
GET  /state      -> shared chat state
GET  /transcript -> replayable shared event log
GET  /export     -> shared JSON export
GET  /chats      -> shared chat list
GET  /stories    -> selectable story catalog
POST /chats      -> create a chat
POST /chats/{id}/activate -> switch active chat
POST /turn       -> SSE turn stream
POST /cmd        -> reset / new <brief> / constraint <txt> / event <txt>

Run:  python server.py
LAN:  $env:GM_HOST="0.0.0.0"; python server.py
DB:   $env:GM_DIALOG_DB="E:\\path\\gm_lab_dialogs.sqlite3"; python server.py
"""
from __future__ import annotations

import json
import os
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import unquote, urlparse

import config
import agents
import codex_oauth
import runtime_settings
import stories
import world as world_mod
from dialog_store import DEFAULT_CHAT_TITLE, DialogRuntime, DialogStore
from llm_client import make_client
from orchestrator import Session, context_usage, run_turn

HERE = os.path.dirname(os.path.abspath(__file__))
PORT = int(os.environ.get("GM_PORT", "8000"))
HOST = os.environ.get("GM_HOST", "127.0.0.1")
DIALOG_DB = os.environ.get("GM_DIALOG_DB", os.path.join(HERE, "gm_lab_dialogs.sqlite3"))

CHAT_SCOPE_ID = (os.environ.get("GM_CHAT_SCOPE_ID") or "shared").strip() or "shared"
REPLAY_SKIP_KINDS = {"delta"}

dialog_store = DialogStore(DIALOG_DB, make_client)
MIGRATED_CHAT_COUNT = 0


def _default_model() -> str:
    if config.BACKEND == "codex":
        return config.CODEX_MODEL or config.MODEL
    return config.MODEL or "default"


def _session_matches_backend(session: Session) -> bool:
    stored = getattr(session, "client_backend", "")
    return not stored or stored == config.BACKEND


def state(dialog: DialogRuntime) -> dict:
    session = dialog.session
    w = session.world
    settings = runtime_settings.get()
    model = (
        getattr(session.client, "model", "")
        or (getattr(session, "client_model", "") if _session_matches_backend(session) else "")
        or _default_model()
    )
    def public_npc(npc) -> dict:
        label = w.npc_player_label(npc.npc_id)
        return {
            "id": npc.npc_id,
            "name": label,
            "label": label,
            "known_name": w.npc_known_name(npc.npc_id),
            "public_label": getattr(npc, "public_label", ""),
            "role": world_mod._public_role(npc.role),
            "pronouns": world_mod._public_gender(npc.pronouns),
            "color": npc.color,
            "physical_type": getattr(npc, "physical_type", ""),
            "distinctive_features": getattr(npc, "distinctive_features", ""),
            "condition": getattr(npc, "condition", ""),
            "life_status": getattr(npc, "life_status", "alive"),
        }

    data = {
        "model": model,
        "backend": config.BACKEND,
        "stream_gm_content": runtime_settings.stream_gm_content_enabled(settings),
        "settings": settings,
        "settings_options": runtime_settings.options(),
        "run_usage": session.run_usage,
        "context_usage": context_usage(session),
        "story_id": getattr(w, "story_id", ""),
        "story_title": getattr(w, "story_title", ""),
        "public": w.public,
        "time": w.time_export(),
        "player_character": w.player_character_export(public=True),
        "scene": w.scene_export(),
        "entities": w.entity_refs(),
        "status_labels": dict(world_mod.WHEREABOUTS_STATUS_LABELS),
        "npcs": [public_npc(n) for n in w.npcs.values()],
    }
    if config.BACKEND == "codex":
        data["codex_auth"] = codex_oauth.auth_status()
    return data


def _ser_messages(msgs):
    out = []
    for m in msgs:
        if isinstance(m, dict):
            out.append(m)
        elif hasattr(m, "model_dump"):
            out.append(m.model_dump())
        else:
            out.append(str(m))
    return out


def export_data(dialog: DialogRuntime) -> dict:
    session = dialog.session
    w = session.world
    model = getattr(session.client, "model", config.MODEL)
    return {
        "meta": {
            "model": model,
            "backend": config.BACKEND,
            "turns": dialog.turn_count,
            "run_usage": session.run_usage,
            "story_id": getattr(w, "story_id", ""),
            "story_title": getattr(w, "story_title", ""),
        },
        "world": {
            "story_id": getattr(w, "story_id", ""),
            "story_title": getattr(w, "story_title", ""),
            "public": w.public,
            "time": w.time_export(),
            "player_character": w.player_character_export(public=False),
            "constraints": w.constraints,
            "scene": w.scene_export(),
            "rumors": [vars(r) | {"witnesses": sorted(r.witnesses)} for r in w.rumors],
            "state_records": _debug_state_records(w),
            "npc_commitments": session.commitments,
            "npc_summaries": session.npc_summaries,
            "npc_messages": session.npc_messages,
            "npc_client_state": session.npc_client_state,
            "events": [
                vars(e) | {"witnesses": sorted(e.witnesses)}
                for e in session.events
            ],
        },
        "transcript": dialog.transcript,
        "gm_messages": _ser_messages(session.gm_messages),
    }


def _debug_event(event) -> dict:
    actor_id = getattr(event, "actor", "")
    return {
        "seq": getattr(event, "seq", 0),
        "turn": getattr(event, "turn", 0),
        "actor": actor_id,
        "kind": getattr(event, "kind", ""),
        "speech": getattr(event, "speech", ""),
        "action": getattr(event, "action", ""),
        "witnesses": sorted(getattr(event, "witnesses", []) or []),
    }


def _debug_rumor(rumor) -> dict:
    return {
        "seq": getattr(rumor, "seq", 0),
        "turn": getattr(rumor, "turn", 0),
        "speaker": getattr(rumor, "speaker", ""),
        "text": getattr(rumor, "text", ""),
        "witnesses": sorted(getattr(rumor, "witnesses", []) or []),
        "confirmed": bool(getattr(rumor, "confirmed", False)),
    }


def _debug_pending(pending: dict) -> dict:
    out = {}
    for npc_id, row in (pending or {}).items():
        if not isinstance(row, dict):
            out[npc_id] = str(row)
            continue
        out[npc_id] = {
            "seq": row.get("seq", 0),
            "speech": row.get("speech", ""),
            "action": row.get("action", ""),
            "claims": list(row.get("claims") or []),
            "witnesses": sorted(row.get("witnesses") or []),
        }
    return out


def _debug_state_records(world: world_mod.World) -> list[dict]:
    if not hasattr(world, "state_records_export"):
        return []
    return world.state_records_export("gm", active=None)


def debug_data(dialog: DialogRuntime) -> dict:
    session = dialog.session
    w = session.world
    model = (
        getattr(session.client, "model", "")
        or (getattr(session, "client_model", "") if _session_matches_backend(session) else "")
        or _default_model()
    )
    facts = [
        {
            "id": record.fact_id,
            "kind": record.kind,
            "text": record.text,
            "keywords": list(record.keywords),
            "source": record.source,
            "confirmed": bool(record.confirmed),
        }
        for record in getattr(w, "fact_records", [])
    ]
    npcs = []
    for npc_id, npc in sorted(w.npcs.items()):
        presence = w.scene.presence.get(npc_id)
        npcs.append({
            "id": npc_id,
            "name": npc.name,
            "player_label": w.npc_player_label(npc_id),
            "known_name": w.npc_known_name(npc_id),
            "color": npc.color,
            "role": npc.role,
            "pronouns": npc.pronouns,
            "public_label": getattr(npc, "public_label", ""),
            "age": getattr(npc, "age", ""),
            "physical_type": getattr(npc, "physical_type", ""),
            "distinctive_features": getattr(npc, "distinctive_features", ""),
            "life_status": getattr(npc, "life_status", "alive"),
            "life_status_note": getattr(npc, "life_status_note", ""),
            "condition": getattr(npc, "condition", ""),
            "card_revision": int(getattr(npc, "card_revision", 0) or 0),
            "present": npc_id in w.scene.present_npcs,
            "presence": vars(presence) if presence else None,
            "whereabouts": w.npc_whereabouts_export(npc_id),
            "persona": npc.persona,
            "personality": getattr(npc, "personality", ""),
            "values": getattr(npc, "values", ""),
            "habits": getattr(npc, "habits", ""),
            "pressure_response": getattr(npc, "pressure_response", ""),
            "boundaries": getattr(npc, "boundaries", ""),
            "voice": npc.voice,
            "goals": npc.goals,
            "knowledge": npc.knowledge,
            "secret": npc.secret,
            "mechanics": {
                "abilities": getattr(npc, "abilities", {}),
                "skills": getattr(npc, "skills", {}),
                "saving_throws": getattr(npc, "saving_throws", {}),
                "passive_perception": getattr(npc, "passive_perception", None),
                "ac": getattr(npc, "ac", None),
                "hp": getattr(npc, "hp", {}),
                "speed": getattr(npc, "speed", ""),
                "senses": getattr(npc, "senses", ""),
                "languages": getattr(npc, "languages", ""),
            },
            "summary": session.npc_summaries.get(npc_id, ""),
            "commitments": session.commitments.get(npc_id, []),
            "messages": len(session.npc_messages.get(npc_id, [])),
            "history": session.npc_history_text(npc_id, max_messages=6),
        })
    return {
        "ok": True,
        "meta": {
            "model": model,
            "backend": config.BACKEND,
            "turns": dialog.turn_count,
            "run_usage": session.run_usage,
            "context_usage": context_usage(session),
        },
        "runtime": {
            "settings": runtime_settings.get(),
            "cache": {
                "prompt_cache_key": config.CODEX_PROMPT_CACHE_KEY
                or getattr(session, "client_thread_id", ""),
                "thread_id": getattr(session, "client_thread_id", ""),
                "store": False,
            },
        },
        "story": {
            "id": getattr(w, "story_id", ""),
            "title": getattr(w, "story_title", ""),
            "objective": (
                "Вести игрока к раскрытию скрытой правды истории через действия, улики, "
                "свидетелей и последствия, не выдавая секреты без игрового основания."
            ),
            "public_intro": w.public,
            "hidden_truth": getattr(w, "canon", ""),
            "constraints": list(getattr(w, "constraints", []) or []),
            "hidden_events": list(getattr(w, "hidden_events", []) or []),
        },
        "scene": w.scene_export(),
        "time": w.time_export(),
        "player_character": w.player_character_export(public=False),
        "roll_override": {
            "next": getattr(w, "forced_die_next", None),
            "all": getattr(w, "forced_die_all", None),
        },
        "status_labels": dict(world_mod.WHEREABOUTS_STATUS_LABELS),
        "facts": facts,
        "state_records": _debug_state_records(w),
        "rumors": [_debug_rumor(rumor) for rumor in getattr(w, "rumors", [])],
        "npcs": npcs,
        "memory": {
            "gm_summary": session.gm_summary,
            "gm_messages": len(session.gm_messages),
            "loaded_gm_tools": sorted(session.loaded_gm_tools),
            "events": [_debug_event(event) for event in session.events[-80:]],
            "pending": _debug_pending(session.pending),
            "delivered": session.delivered,
        },
    }


def replay_events(dialog: DialogRuntime) -> list[dict]:
    events = []
    for row in dialog.transcript:
        event = row.get("event") if isinstance(row, dict) else None
        if not isinstance(event, dict):
            continue
        if event.get("kind") in REPLAY_SKIP_KINDS:
            continue
        events.append(event)
    return events


def ensure_client(dialog: DialogRuntime):
    if dialog.session.client is None:
        if not _session_matches_backend(dialog.session):
            dialog.session.client_model = ""
            dialog.session.client_session_id = ""
            dialog.session.client_thread_id = ""
            dialog.session.npc_client_state = {}
        dialog.session.client_backend = config.BACKEND
        dialog.session.client = make_client()
        if hasattr(dialog.session.client, "set_session_identity"):
            dialog.session.client.set_session_identity(
                getattr(dialog.session, "client_session_id", ""),
                getattr(dialog.session, "client_thread_id", ""),
            )
            dialog.session.client_session_id = getattr(dialog.session.client, "session_id", "")
            dialog.session.client_thread_id = getattr(dialog.session.client, "thread_id", "")
        model = getattr(dialog.session, "client_model", "")
        if model and hasattr(dialog.session.client, "set_model"):
            dialog.session.client.set_model(model)
    return dialog.session.client


def _model_hint_for_new_chat(dialog: DialogRuntime | None) -> str:
    if dialog is None or not _session_matches_backend(dialog.session):
        return ""
    return str(
        getattr(dialog.session, "client_model", "")
        or getattr(getattr(dialog.session, "client", None), "model", "")
        or ""
    )


def _seeded_session(brief: str, model_hint: str = "") -> Session:
    client = make_client()
    if model_hint and hasattr(client, "set_model"):
        client.set_model(model_hint)
    seed = agents.build_world_seed(client, brief)
    session = Session(client, world_mod.World.from_seed(seed))
    session.client_backend = config.BACKEND
    session.client_model = str(getattr(client, "model", "") or model_hint or "")
    session.client_session_id = str(getattr(client, "session_id", "") or "")
    session.client_thread_id = str(getattr(client, "thread_id", "") or "")
    return session


def _story_session(story_id: str, model_hint: str = "") -> Session:
    session = Session(None, world_mod.World.from_story(story_id))
    session.client_backend = config.BACKEND
    session.client_model = model_hint
    return session


def _chat_response(dialog: DialogRuntime, active: bool) -> dict:
    return {
        "id": dialog.chat_id,
        "title": dialog.title or DEFAULT_CHAT_TITLE,
        "preview": dialog.preview or "",
        "turn_count": int(dialog.turn_count or 0),
        "created_at": dialog.created_at or "",
        "updated_at": dialog.updated_at or "",
        "active": bool(active),
    }


def _bool_from_body(value, default: bool = True) -> bool:
    if value is None:
        return default
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() not in {"0", "false", "no", "off"}
    return bool(value)


def _list_models(client) -> list[dict]:
    if hasattr(client, "list_models"):
        return client.list_models()
    model = getattr(client, "model", config.MODEL or "default")
    return [{"id": model, "name": model, "supported": True}]


def _find_model(models: list[dict], model_id: str) -> dict | None:
    for model in models or []:
        if not isinstance(model, dict):
            continue
        if model.get("id") == model_id or model.get("slug") == model_id:
            return model
    return None


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _chat_scope_id(self) -> str:
        return CHAT_SCOPE_ID

    def _dialog(self) -> DialogRuntime:
        dialog = getattr(self, "_dialog_runtime", None)
        if dialog is not None:
            return dialog
        chat_scope_id = self._chat_scope_id()
        self._dialog_runtime = dialog_store.get_active(chat_scope_id)
        return self._dialog_runtime

    def _json(self, obj, code=200):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self._write_body(body)

    def _write_body(self, body: bytes) -> None:
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionAbortedError, ConnectionResetError):
            pass

    def _body(self) -> dict:
        n = int(self.headers.get("Content-Length", 0) or 0)
        if not n:
            return {}
        try:
            return json.loads(self.rfile.read(n) or b"{}")
        except Exception:
            return {}

    def _activate_chat_response(self, chat_scope_id: str, chat_id: str) -> None:
        dialog = dialog_store.activate_chat(chat_scope_id, chat_id)
        if dialog is None:
            self._json({"ok": False, "error": "chat not found"}, 404)
            return
        self._dialog_runtime = dialog
        with dialog.lock:
            self._json({
                "ok": True,
                "chat": _chat_response(dialog, active=True),
                "state": state(dialog),
                "transcript": {"events": replay_events(dialog)},
            })

    def do_GET(self):
        path = urlparse(self.path).path
        if path == "/" or path.startswith("/index"):
            self._dialog()
            with open(os.path.join(HERE, "index.html"), "rb") as f:
                body = f.read()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            # index.html is a single inlined bundle with no cache-busting filename,
            # so tell the browser never to serve a stale copy after a rebuild.
            self.send_header("Cache-Control", "no-store, must-revalidate")
            self.end_headers()
            self._write_body(body)
            return

        if path == "/chats":
            chat_scope_id = self._chat_scope_id()
            dialog_store.get_active(chat_scope_id)
            chats = dialog_store.list_chats(chat_scope_id)
            self._json({
                "ok": True,
                "active_chat_id": dialog_store.active_chat_id(chat_scope_id),
                "chats": chats,
            })
            return

        if path == "/stories":
            self._json({
                "ok": True,
                "default_story_id": stories.DEFAULT_STORY_ID,
                "stories": stories.list_stories(),
            })
            return

        if path == "/state":
            dialog = self._dialog()
            with dialog.lock:
                payload = state(dialog)
                payload["context_usage"] = context_usage(dialog.session)
                self._json(payload)
            return

        if path == "/transcript":
            dialog = self._dialog()
            with dialog.lock:
                self._json({"events": replay_events(dialog)})
            return

        if path == "/export":
            dialog = self._dialog()
            with dialog.lock:
                body = json.dumps(
                    export_data(dialog),
                    ensure_ascii=False,
                    indent=2,
                    default=str,
                ).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Disposition", 'attachment; filename="gm-lab-export.json"')
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self._write_body(body)
            return

        if path == "/debug":
            dialog = self._dialog()
            with dialog.lock:
                self._json(debug_data(dialog))
            return

        if path == "/models":
            dialog = self._dialog()
            try:
                with dialog.lock:
                    client = ensure_client(dialog)
                    models = _list_models(client)
                    current = getattr(client, "model", config.MODEL)
                self._json({
                    "ok": True,
                    "model": current,
                    "models": models,
                    "settings": runtime_settings.get(),
                    "settings_options": runtime_settings.options(),
                })
            except Exception as ex:
                self._json({"ok": False, "error": str(ex), "models": []}, 400)
            return

        if path == "/settings":
            self._dialog()
            self._json({
                "ok": True,
                "settings": runtime_settings.get(),
                "settings_options": runtime_settings.options(),
            })
            return

        if path == "/codex/status":
            self._dialog()
            self._json(codex_oauth.auth_status())
            return

        self._json({"error": "not found"}, 404)

    def do_POST(self):
        path = urlparse(self.path).path

        if path == "/chats":
            chat_scope_id = self._chat_scope_id()
            data = self._body()
            brief = str(data.get("brief") or "").strip()
            story_id = str(data.get("story_id") or "").strip()
            title = str(data.get("title") or "").strip()
            activate = _bool_from_body(data.get("activate"), default=True)
            session = None
            if story_id and story_id not in stories.story_ids():
                self._json({"ok": False, "error": f"unknown story_id: {story_id}"}, 400)
                return
            if not brief and not story_id:
                self._json({"ok": False, "error": "story_id is required"}, 400)
                return
            if brief:
                active_dialog = None
                active_chat_id = dialog_store.active_chat_id(chat_scope_id)
                if active_chat_id:
                    try:
                        active_dialog = dialog_store.get(chat_scope_id, active_chat_id)
                    except KeyError:
                        active_dialog = None
                try:
                    session = _seeded_session(
                        brief,
                        _model_hint_for_new_chat(active_dialog),
                    )
                except Exception as ex:
                    self._json({"ok": False, "error": str(ex)}, 400)
                    return
            else:
                active_dialog = None
                active_chat_id = dialog_store.active_chat_id(chat_scope_id)
                if active_chat_id:
                    try:
                        active_dialog = dialog_store.get(chat_scope_id, active_chat_id)
                    except KeyError:
                        active_dialog = None
                session = _story_session(
                    story_id,
                    _model_hint_for_new_chat(active_dialog),
                )
            dialog = dialog_store.create_chat(
                chat_scope_id,
                session=session,
                title=title or brief or getattr(session.world, "story_title", ""),
                activate=activate,
            )
            active_chat_id = dialog_store.active_chat_id(chat_scope_id)
            if dialog.chat_id == active_chat_id:
                self._dialog_runtime = dialog
            response = {
                "ok": True,
                "active_chat_id": active_chat_id,
                "chat": _chat_response(dialog, active=dialog.chat_id == active_chat_id),
            }
            if dialog.chat_id == active_chat_id:
                with dialog.lock:
                    response["state"] = state(dialog)
                    response["transcript"] = {"events": replay_events(dialog)}
            self._json(response)
            return

        if path.startswith("/chats/") and path.endswith("/activate"):
            chat_scope_id = self._chat_scope_id()
            parts = path.strip("/").split("/")
            chat_id = unquote(parts[1]) if len(parts) == 3 else ""
            self._activate_chat_response(chat_scope_id, chat_id)
            return

        if path.startswith("/chats/") and path.endswith("/delete"):
            chat_scope_id = self._chat_scope_id()
            parts = path.strip("/").split("/")
            chat_id = unquote(parts[1]) if len(parts) == 3 else ""
            result = dialog_store.delete_chat(chat_scope_id, chat_id)
            if not result.get("deleted"):
                self._json({"ok": False, "error": result.get("reason") or "chat not found"}, 404)
                return
            # The cached active dialog on this handler may now be stale/deleted; reload the
            # (possibly newly created) active session so the client can switch to it.
            self._dialog_runtime = None
            active = dialog_store.get_active(chat_scope_id)
            self._dialog_runtime = active
            with active.lock:
                response = {
                    "ok": True,
                    "deleted": True,
                    "active_chat_id": dialog_store.active_chat_id(chat_scope_id),
                    "chats": dialog_store.list_chats(chat_scope_id),
                    "chat": _chat_response(active, active=True),
                    "state": state(active),
                    "transcript": {"events": replay_events(active)},
                    "embeddings_purged": int(result.get("embeddings_purged") or 0),
                }
            self._json(response)
            return

        dialog = self._dialog()

        if path == "/codex/login":
            if config.BACKEND != "codex":
                self._json({"ok": False, "error": "GM_BACKEND is not codex"}, 400)
                return
            try:
                codex_oauth.run_oauth()
                self._json({"ok": True, "auth": codex_oauth.auth_status()})
            except Exception as ex:
                self._json({"ok": False, "error": str(ex), "auth": codex_oauth.auth_status()}, 400)
            return

        if path == "/codex/logout":
            try:
                codex_oauth.revoke_credential()
                self._json({"ok": True, "auth": codex_oauth.auth_status()})
            except Exception as ex:
                self._json({"ok": False, "error": str(ex), "auth": codex_oauth.auth_status()}, 400)
            return

        if path == "/model":
            data = self._body()
            model = (data.get("model") or "").strip()
            if not model:
                self._json({"ok": False, "error": "model is required"}, 400)
                return
            with dialog.lock:
                client = ensure_client(dialog)
                if not hasattr(client, "set_model"):
                    self._json({"ok": False, "error": "current backend cannot change model"}, 400)
                    return
                client.set_model(model)
                dialog.session.set_model_for_all_clients(model)
                try:
                    model_meta = _find_model(_list_models(client), model)
                    runtime_settings.reconcile_for_model(model_meta)
                except Exception:
                    pass
                dialog.session.client_session_id = getattr(
                    client, "session_id", getattr(dialog.session, "client_session_id", "")
                )
                dialog.session.client_thread_id = getattr(
                    client, "thread_id", getattr(dialog.session, "client_thread_id", "")
                )
                for npc_id in list(dialog.session.npc_clients):
                    dialog.session.remember_npc_client(npc_id)
                dialog_store.save(dialog)
                response = {"ok": True, "state": state(dialog)}
            self._json(response)
            return

        if path == "/settings":
            data = self._body()
            settings = runtime_settings.update(data.get("settings") if "settings" in data else data)
            with dialog.lock:
                dialog_store.save(dialog)
                response = {
                    "ok": True,
                    "settings": settings,
                    "settings_options": runtime_settings.options(),
                    "state": state(dialog),
                }
            self._json(response)
            return

        if path == "/cmd":
            data = self._body()
            cmd, arg = data.get("cmd", ""), (data.get("arg") or "").strip()
            if cmd == "new" and arg:
                with dialog.lock:
                    model_hint = _model_hint_for_new_chat(dialog)
                try:
                    session = _seeded_session(arg, model_hint)
                except Exception as ex:
                    self._json({"ok": False, "error": str(ex)}, 400)
                    return
                new_dialog = dialog_store.create_chat(
                    dialog.guest_id,
                    session=session,
                    title=arg,
                    activate=True,
                )
                self._dialog_runtime = new_dialog
                with new_dialog.lock:
                    response = {
                        "ok": True,
                        "chat": _chat_response(new_dialog, active=True),
                        "state": state(new_dialog),
                    }
                self._json(response)
                return

            with dialog.lock:
                if cmd == "reset":
                    same_backend = _session_matches_backend(dialog.session)
                    model = ""
                    session_id = ""
                    thread_id = ""
                    if same_backend:
                        model = (
                            getattr(dialog.session, "client_model", "")
                            or getattr(getattr(dialog.session, "client", None), "model", "")
                        )
                        session_id = (
                            getattr(dialog.session, "client_session_id", "")
                            or getattr(getattr(dialog.session, "client", None), "session_id", "")
                        )
                        thread_id = (
                            getattr(dialog.session, "client_thread_id", "")
                            or getattr(getattr(dialog.session, "client", None), "thread_id", "")
                        )
                    story_id = getattr(dialog.session.world, "story_id", "")
                    if story_id not in stories.story_ids():
                        self._json(
                            {"ok": False, "error": f"cannot reset non-catalog story: {story_id or 'unknown'}"},
                            400,
                        )
                        return
                    dialog.session = Session(None, world_mod.World.from_story(story_id))
                    dialog.session.client_backend = config.BACKEND
                    dialog.session.client_model = model
                    dialog.session.client_session_id = session_id
                    dialog.session.client_thread_id = thread_id
                    dialog.transcript.clear()
                    dialog.turn_count = 0
                elif cmd == "constraint" and arg:
                    dialog.session.world.constraints.append(arg)
                elif cmd == "event" and arg:
                    dialog.session.world.hidden_events.append(arg)
                else:
                    self._json(
                        {"ok": False, "error": f"unknown or incomplete command: {cmd}"},
                        400,
                    )
                    return
                dialog_store.save(dialog)
                response = {"ok": True, "state": state(dialog)}
            self._json(response)
            return

        if path == "/debug/roll":
            data = self._body()

            def _die_or_none(value):
                if value in (None, ""):
                    return None
                try:
                    return max(1, int(value))
                except (TypeError, ValueError):
                    return None

            with dialog.lock:
                w = dialog.session.world
                if "next" in data:
                    w.forced_die_next = _die_or_none(data.get("next"))
                if "all" in data:
                    w.forced_die_all = _die_or_none(data.get("all"))
                dialog_store.save(dialog)
                response = debug_data(dialog)
            self._json(response)
            return

        if path == "/debug/fact":
            data = self._body()
            with dialog.lock:
                dialog.session.world.add_fact(data.get("text"), data.get("kind") or "public")
                dialog_store.save(dialog)
                response = debug_data(dialog)
            self._json(response)
            return

        if path == "/debug/fact_delete":
            data = self._body()
            with dialog.lock:
                dialog.session.world.remove_fact(data.get("id"))
                dialog_store.save(dialog)
                response = debug_data(dialog)
            self._json(response)
            return

        if path == "/debug/player":
            data = self._body()
            with dialog.lock:
                fields = data.get("fields") if isinstance(data.get("fields"), dict) else {}
                dialog.session.world.update_player_character(fields, data.get("reason", "debug edit"))
                dialog_store.save(dialog)
                response = debug_data(dialog)
            self._json(response)
            return

        if path == "/debug/npc":
            data = self._body()
            with dialog.lock:
                npc_id = str(data.get("id") or "")
                # All card/presence/whereabouts/reset logic (incl. the presence-change guard
                # and the explicit-only memory reset) lives in Session.apply_debug_edit so it
                # is testable directly; here we only persist + reply.
                if dialog.session.apply_debug_edit(npc_id, data):
                    dialog_store.save(dialog)
                    response = debug_data(dialog)
                else:
                    response = {"ok": False, "error": f"no such npc: {npc_id}"}
            self._json(response, 200 if response.get("ok") else 400)
            return

        if path == "/debug/story":
            data = self._body()
            with dialog.lock:
                w = dialog.session.world
                if "title" in data:
                    w.set_story_title(data.get("title"))
                if "public_intro" in data:
                    w.set_public_intro(data.get("public_intro"))
                if "hidden_truth" in data:
                    w.set_hidden_truth(data.get("hidden_truth"))
                if "hidden_events" in data:
                    w.set_hidden_events(data.get("hidden_events"))
                dialog_store.save(dialog)
                response = debug_data(dialog)
            self._json(response)
            return

        if path == "/debug/scene":
            data = self._body()
            with dialog.lock:
                patch = data.get("patch") if isinstance(data.get("patch"), dict) else data
                dialog.session.world.patch_scene(patch)
                dialog_store.save(dialog)
                response = debug_data(dialog)
            self._json(response)
            return

        if path == "/debug/state_record":
            data = self._body()
            with dialog.lock:
                dialog.session.world.apply_state_record_batch(
                    add=data.get("add"),
                    update=data.get("update"),
                    delete=data.get("delete"),
                    hard_delete=bool(data.get("hard_delete")),
                )
                dialog_store.save(dialog)
                response = debug_data(dialog)
            self._json(response)
            return

        if path == "/debug/rumor":
            data = self._body()
            with dialog.lock:
                w = dialog.session.world
                action = str(data.get("action") or "").lower()
                if action == "add":
                    w.add_debug_rumor(data.get("speaker"), data.get("text"))
                elif action == "delete":
                    w.remove_rumor(data.get("seq"))
                elif action == "confirm":
                    w.set_rumor_confirmed(data.get("seq"), data.get("confirmed", True))
                dialog_store.save(dialog)
                response = debug_data(dialog)
            self._json(response)
            return

        if path == "/turn":
            text = (self._body().get("text") or "").strip()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("X-Accel-Buffering", "no")
            self.end_headers()

            def push(ev):
                line = "data: " + json.dumps(ev, ensure_ascii=False) + "\n\n"
                self.wfile.write(line.encode("utf-8"))
                self.wfile.flush()

            with dialog.lock:
                ensure_client(dialog)
                dialog.turn_count += 1
                turn_no = dialog.turn_count
                try:
                    for ev in run_turn(dialog.session, text):
                        dialog.transcript.append({"turn": turn_no, "event": ev})
                        push(ev)
                except BrokenPipeError:
                    pass
                except Exception as ex:
                    error_event = {"kind": "error", "agent": "ГМ", "data": str(ex)}
                    dialog.transcript.append({"turn": turn_no, "event": error_event})
                    try:
                        push(error_event)
                    except Exception:
                        pass
                finally:
                    dialog_store.save(dialog)
                try:
                    push({"kind": "done"})
                except Exception:
                    pass
            return

        self._json({"error": "not found"}, 404)


def main():
    global MIGRATED_CHAT_COUNT
    MIGRATED_CHAT_COUNT = dialog_store.merge_all_chats_into_scope(CHAT_SCOPE_ID)
    srv = ThreadingHTTPServer((HOST, PORT), Handler)
    shown_host = "localhost" if HOST in ("", "0.0.0.0") else HOST
    url = f"http://{shown_host}:{PORT}"
    model = _default_model()
    print(f"GM-Lab веб-интерфейс: {url}  (модель {model}, backend {config.BACKEND})")
    print(f"SQLite dialogs: {DIALOG_DB}")
    print(f"Shared chat scope: {CHAT_SCOPE_ID}")
    if MIGRATED_CHAT_COUNT:
        print(f"Migrated dialogs into shared scope: {MIGRATED_CHAT_COUNT}")
    print("Ctrl+C — остановить.")
    if os.environ.get("GM_OPEN_BROWSER", "0") == "1":
        try:
            webbrowser.open(url)
        except Exception:
            pass
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\nстоп")
        srv.shutdown()


if __name__ == "__main__":
    main()
