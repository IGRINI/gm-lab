"""Local web server for GM-Lab.

GET  /           -> index.html
GET  /state      -> current guest state
GET  /transcript -> replayable current guest event log
GET  /export     -> current guest JSON export
POST /turn       -> SSE turn stream
POST /cmd        -> reset / new <brief> / constraint <txt> / event <txt>

Run:  python server.py
LAN:  $env:GM_HOST="0.0.0.0"; python server.py
DB:   $env:GM_DIALOG_DB="E:\\path\\gm_lab_dialogs.sqlite3"; python server.py
"""
from __future__ import annotations

from http.cookies import CookieError, SimpleCookie
import json
import os
import re
import secrets
import webbrowser
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

import config
import agents
import codex_oauth
import runtime_settings
import world as world_mod
from dialog_store import DialogRuntime, DialogStore
from llm_client import make_client
from orchestrator import Session, context_usage, run_turn

HERE = os.path.dirname(os.path.abspath(__file__))
PORT = int(os.environ.get("GM_PORT", "8000"))
HOST = os.environ.get("GM_HOST", "127.0.0.1")
DIALOG_DB = os.environ.get("GM_DIALOG_DB", os.path.join(HERE, "gm_lab_dialogs.sqlite3"))

COOKIE_NAME = "gm_lab_guest"
COOKIE_MAX_AGE = 60 * 60 * 24 * 365
GUEST_ID_RE = re.compile(r"^[A-Za-z0-9_-]{24,80}$")
REPLAY_SKIP_KINDS = {"delta"}

dialog_store = DialogStore(DIALOG_DB, make_client)


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
    model = (
        getattr(session.client, "model", "")
        or (getattr(session, "client_model", "") if _session_matches_backend(session) else "")
        or _default_model()
    )
    data = {
        "model": model,
        "backend": config.BACKEND,
        "stream_gm_content": config.STREAM_GM_CONTENT,
        "settings": runtime_settings.get(),
        "settings_options": runtime_settings.options(),
        "run_usage": session.run_usage,
        "context_usage": context_usage(session),
        "public": w.public,
        "scene": w.scene_export(),
        "entities": w.entity_refs(),
        "status_labels": dict(world_mod.WHEREABOUTS_STATUS_LABELS),
        "npcs": [
            {"id": n.npc_id, "name": n.name, "role": world_mod._public_role(n.role),
             "pronouns": world_mod._public_gender(n.pronouns), "color": n.color}
            for n in w.npcs.values()
        ],
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
        },
        "world": {
            "public": w.public,
            "constraints": w.constraints,
            "scene": w.scene_export(),
            "rumors": [vars(r) | {"witnesses": sorted(r.witnesses)} for r in w.rumors],
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
            "color": npc.color,
            "role": npc.role,
            "pronouns": npc.pronouns,
            "card_revision": int(getattr(npc, "card_revision", 0) or 0),
            "present": npc_id in w.scene.present_npcs,
            "presence": vars(presence) if presence else None,
            "whereabouts": w.npc_whereabouts_export(npc_id),
            "persona": npc.persona,
            "voice": npc.voice,
            "goals": npc.goals,
            "knowledge": npc.knowledge,
            "secret": npc.secret,
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
        "story": {
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
        "roll_override": {
            "next": getattr(w, "forced_die_next", None),
            "all": getattr(w, "forced_die_all", None),
        },
        "status_labels": dict(world_mod.WHEREABOUTS_STATUS_LABELS),
        "facts": facts,
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


def _new_guest_id() -> str:
    return secrets.token_urlsafe(32)


def _guest_id_from_cookie(header: str | None) -> str | None:
    if not header:
        return None
    cookie = SimpleCookie()
    try:
        cookie.load(header)
    except CookieError:
        return None
    morsel = cookie.get(COOKIE_NAME)
    if not morsel:
        return None
    value = morsel.value.strip()
    return value if GUEST_ID_RE.fullmatch(value) else None


def _cookie_header(guest_id: str) -> str:
    return (
        f"{COOKIE_NAME}={guest_id}; "
        f"Max-Age={COOKIE_MAX_AGE}; Path=/; SameSite=Lax; HttpOnly"
    )


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _dialog(self) -> DialogRuntime:
        dialog = getattr(self, "_dialog_runtime", None)
        if dialog is not None:
            return dialog
        guest_id = _guest_id_from_cookie(self.headers.get("Cookie")) or _new_guest_id()
        self._guest_id = guest_id
        self._dialog_runtime = dialog_store.get(guest_id)
        return self._dialog_runtime

    def _set_guest_cookie(self) -> None:
        guest_id = getattr(self, "_guest_id", "")
        if guest_id:
            self.send_header("Set-Cookie", _cookie_header(guest_id))

    def _json(self, obj, code=200):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self._set_guest_cookie()
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

    def do_GET(self):
        path = urlparse(self.path).path
        if path == "/" or path.startswith("/index"):
            self._dialog()
            with open(os.path.join(HERE, "index.html"), "rb") as f:
                body = f.read()
            self.send_response(200)
            self._set_guest_cookie()
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            # index.html is a single inlined bundle with no cache-busting filename,
            # so tell the browser never to serve a stale copy after a rebuild.
            self.send_header("Cache-Control", "no-store, must-revalidate")
            self.end_headers()
            self._write_body(body)
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
            self._set_guest_cookie()
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
                    dialog.session = Session(None)
                    dialog.session.client_backend = config.BACKEND
                    dialog.session.client_model = model
                    dialog.session.client_session_id = session_id
                    dialog.session.client_thread_id = thread_id
                    dialog.transcript.clear()
                    dialog.turn_count = 0
                elif cmd == "new" and arg:
                    client = ensure_client(dialog)
                    seed = agents.build_world_seed(client, arg)
                    dialog.session = Session(client, world_mod.World.from_seed(seed))
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

        if path == "/turn":
            text = (self._body().get("text") or "").strip()
            self.send_response(200)
            self._set_guest_cookie()
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
    srv = ThreadingHTTPServer((HOST, PORT), Handler)
    shown_host = "localhost" if HOST in ("", "0.0.0.0") else HOST
    url = f"http://{shown_host}:{PORT}"
    model = _default_model()
    print(f"GM-Lab веб-интерфейс: {url}  (модель {model}, backend {config.BACKEND})")
    print(f"SQLite dialogs: {DIALOG_DB}")
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
