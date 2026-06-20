"""Mock backend for smoke-testing the React frontend without real models.

Serves the built index.html plus canned /state, /models, /transcript and a
scripted SSE /turn that exercises every event kind (streaming deltas, spoilers,
npc subagent, tool call, meta/meta_total). Not part of the app — test only.

Run:  python mock_server.py    (port 8000)
"""
from __future__ import annotations

import json
import os
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

import runtime_settings
import world as world_mod

HERE = os.path.dirname(os.path.abspath(__file__))
PORT = int(os.environ.get("PORT") or os.environ.get("GM_PORT") or "8000")

SCENE = "Ледяной порт на краю мира. Туман, скрип снастей, пропавший корабль «Морянка»."
NPCS = [
    {"id": "n_borin", "name": "Борин", "role": "капитан стражи", "pronouns": "он", "color": "#e6c08a"},
    {"id": "n_liza", "name": "Лиза", "role": "торговка", "pronouns": "она", "color": "#c4a7e7"},
    {"id": "n_maret", "name": "Капитан Марет", "role": "моряк", "pronouns": "он", "color": "#9ccfd8"},
]

# Single source of truth for settings shape/options/cap lives in runtime_settings;
# the mock just mirrors it so the frontend smoke test never drifts from the real app.
SETTINGS = runtime_settings.defaults()
SETTINGS_OPTIONS = runtime_settings.options()

STATE = {
    "model": "qwen2.5:14b",
    "backend": "ollama",
    "stream_gm_content": True,
    "public": SCENE,
    "scene": {"title": "Ледяной порт", "present_npcs": ["n_borin", "n_liza"],
              "npc_whereabouts": {"n_maret": {"status": "likely", "location_name": "у причала"}}},
    "npcs": NPCS,
    "status_labels": dict(world_mod.WHEREABOUTS_STATUS_LABELS),
    "settings": SETTINGS,
    "settings_options": SETTINGS_OPTIONS,
    "context_usage": {
        "current": 12300,
        "world": 1800,
        "next_compact": {"label": "ГМ", "used": 12800, "limit": 100000, "remaining": 87200},
        "gm": {"active": 12300, "history": 12800, "summary": 900, "limit": 100000, "remaining": 87200},
        "npc": {"name": "Борин", "active": 5300, "history": 4100, "summary": 600, "limit": 64000, "remaining": 59900},
        "npcs": [
            {"id": "n_borin", "name": "Борин", "color": "#e6c08a", "has_session": True, "active": 5300, "history": 4100, "summary": 600, "limit": 64000, "remaining": 59900},
            {"id": "n_liza", "name": "Лиза", "color": "#c4a7e7", "has_session": False, "active": 1500, "history": 0, "summary": 0, "limit": 64000, "remaining": 64000},
            {"id": "n_maret", "name": "Капитан Марет", "color": "#9ccfd8", "has_session": False, "active": 1500, "history": 0, "summary": 0, "limit": 64000, "remaining": 64000},
        ],
    },
}

MODELS = {
    "ok": True,
    "model": "qwen2.5:14b",
    "models": [
        {"id": "qwen2.5:14b", "name": "Qwen 2.5 14B", "supported": True},
        {"id": "llama3.1:8b", "name": "Llama 3.1 8B", "supported": True},
        {"id": "gpt-oss:20b", "name": "gpt-oss 20B", "supported": False},
    ],
    "settings": SETTINGS,
    "settings_options": SETTINGS_OPTIONS,
}


PLAYER_CHARACTER = {
    "name": "Дарра", "pronouns": "F", "class_role": "странствующая сыщица", "level": 2,
    "background": "вольная сыщица, ищет правду за плату или из упрямства",
    "age": "Фактически 34 года", "physical_type": "невысокая жилистая женщина",
    "distinctive_features": "цепкий взгляд, кольцо-печатка, записная книжка",
    "life_status": "alive", "life_status_note": "", "condition": "в дороге, собрана",
    "personality": "остра на язык, наблюдательна, недоверчива к властям",
    "values": "правда, независимость, расплата за тех, кого заставили молчать",
    "gm_notes": "Попала в порт проездом; власти стражи нет, только репутация дознавателя.",
    "abilities": {"STR": 9, "DEX": 13, "CON": 11, "INT": 13, "WIS": 14, "CHA": 15},
    "skills": {"Insight": 4, "Perception": 4, "Persuasion": 4, "Deception": 4},
    "saving_throws": {"WIS": 4, "CHA": 4}, "passive_perception": 14, "ac": 13,
    "hp": {"current": 16, "max": 16}, "speed": "30 ft", "senses": "обычное зрение",
    "languages": "Общий; воровское арго",
    "inventory": ["дорожный плащ", "кинжал", "потайной фонарь", "лупа", "набор отмычек"],
    "equipment": ["проклёпанная кожаная куртка"],
    "features": ["Глаз дознавателя", "Язык без костей", "Тихие пальцы"],
    "card_revision": 0,
}

DEBUG = {
    "ok": True,
    "meta": {"model": "qwen2.5:14b", "backend": "ollama", "turns": 3,
             "run_usage": {"input": 2100, "output": 580, "cached_tokens": 800},
             "context_usage": {"current": 12300, "limit": 100000, "remaining": 87700}},
    "runtime": {
        "settings": SETTINGS,
        "cache": {"prompt_cache_key": "gm-lab:mock-thread-abc123",
                  "thread_id": "mock-thread-abc123", "store": False},
    },
    "time": {"current_date_label": "День 1"},
    "player_character": PLAYER_CHARACTER,
    "story": {
        "title": "Ледяной порт",
        "objective": "Привести игрока к разгадке пропажи «Морянки».",
        "public_intro": SCENE,
        "hidden_truth": "Корабль увели контрабандисты к Чёрным скалам.",
        "constraints": ["Туман ограничивает видимость"],
        "hidden_events": ["Ночью кто-то жёг сигнальный огонь у скал"],
    },
    "roll_override": {"next": None, "all": None},
    "status_labels": dict(world_mod.WHEREABOUTS_STATUS_LABELS),
    "facts": [
        {"id": "public_1", "kind": "public", "text": "«Морянка» не вернулась из последнего рейса.", "keywords": ["морянка", "рейс"]},
        {"id": "truth_1", "kind": "truth", "text": "Марет знает маршрут к Чёрным скалам.", "keywords": ["марет", "маршрут"]},
        {"id": "rumor_1", "kind": "rumor", "text": "У скал видели чужие огни.", "keywords": ["скалы", "огни"]},
    ],
    "state_records": [
        {"record_id": "sr_known_borin", "kind": "fact", "scope": "public",
         "text": "Игрок знает капитана стражи по имени Борин.", "entity_id": "n_borin", "active": True},
        {"record_id": "sr_liza_note", "kind": "npc_memory", "scope": "owner",
         "text": "Лиза помнит, что Борин не спал в ночь пропажи.", "entity_id": "n_liza", "active": True},
    ],
    "rumors": [{"seq": 1, "speaker": "Лиза", "text": "Борин в ту ночь не спал.", "confirmed": False}],
    "scene": {"title": "Ледяной порт", "location_id": "ice_port", "present_npcs": ["n_borin"],
              "tension": "ровное", "description": SCENE,
              "constraints": ["Туман ограничивает видимость"],
              "items": [{"item_id": "boat", "name": "пустая шлюпка с «Морянки»", "location": "у пирса",
                         "visible": True, "portable": False, "owner": "", "details": "метка на борту"}],
              "exits": [{"exit_id": "tavern", "name": "переулок к таверне",
                         "destination": "таверна «Треснувший якорь»", "visible": True, "blocked_by": ""}]},
    "npcs": [
        {**n, "present": (n["id"] == "n_borin"), "public_label": n["role"],
         "whereabouts": {"location_name": "у причала", "status": "likely", "details": ""},
         "persona": f"{n['name']} — житель порта.", "voice": "Сдержанно, по делу.",
         "goals": "Защищать свои интересы.", "knowledge": "То, что очевидно в сцене.",
         "secret": "Личная тайна не задана.",
         "mechanics": {"abilities": {"STR": 12, "DEX": 11, "CON": 13, "INT": 10, "WIS": 12, "CHA": 11},
                       "skills": {"Perception": 3}, "saving_throws": {}, "passive_perception": 13,
                       "ac": 14, "hp": {"current": 20, "max": 20}, "speed": "30 ft",
                       "senses": "обычное зрение", "languages": "Общий"},
         "summary": "", "commitments": [], "messages": 0, "history": ""}
        for n in NPCS
    ],
    "memory": {"gm_summary": "", "loaded_gm_tools": ["ask_npc", "roll_dice"], "events": []},
}


# --- in-memory chat list (mock only) so the sidebar + delete flow can be previewed ---
MOCK_CHATS = [
    {"id": "chat_ice", "title": "Ледяной порт", "preview": "Туман у причала, пропала «Морянка».", "turn_count": 3},
    {"id": "chat_garden", "title": "Стеклянный сад Элирии", "preview": "Чёрные прожилки на лунных орхидеях.", "turn_count": 1},
    {"id": "chat_turn", "title": "Убийство в Тёрнвейле", "preview": "Купец мёртв, гильдия молчит.", "turn_count": 5},
]
ACTIVE_CHAT_ID = {"value": "chat_ice"}


def _chats_payload():
    return [
        {**c, "created_at": "2026-06-19 10:00", "updated_at": "2026-06-20 12:30",
         "active": c["id"] == ACTIVE_CHAT_ID["value"]}
        for c in MOCK_CHATS
    ]


def _chat_one(chat_id):
    for c in MOCK_CHATS:
        if c["id"] == chat_id:
            return {**c, "created_at": "2026-06-19 10:00", "updated_at": "2026-06-20 12:30", "active": True}
    return {"id": chat_id, "title": "Новый чат", "preview": "", "turn_count": 0,
            "created_at": "", "updated_at": "", "active": True}


def meta(label, secs=2.0, tps=44, tin=1200, tout=320, cached=0):
    return {
        "label": label, "secs": secs, "tps": tps, "in": tin, "out": tout,
        "cached": cached, "prompt_secs": 0.6, "eval_secs": round(secs - 0.6, 2),
        "load_secs": 0,
    }


def meta_total():
    calls = [
        {"label": "GM", "in": 1200, "out": 320, "tps": 44, "secs": 2.0},
        {"label": "NPC Борин", "in": 900, "out": 260, "tps": 51, "secs": 1.7},
    ]
    return {
        "secs": 5.1, "tokens": 2680, "in": 2100, "out": 580, "cached": 800,
        "calls": calls, "sys_estimate": 1500,
        "context": STATE["context_usage"],
    }


def _seed_block(i):
    g = f"g{i}"
    n = f"n{i}"
    return [
        {"kind": "player", "data": f"[{i}] Осматриваю причал и прислушиваюсь к туману."},
        {"kind": "gm_thinking", "sid": g,
         "data": "Игрок осматривает причал. Описываю атмосферу, ввожу зацепку про «Морянку», даю Борину повод вмешаться."},
        {"kind": "gm_tool_call", "data": {"name": "get_world_fact",
         "arguments": {"query": "пропавший корабль «Морянка» — последние свидетельства"}}},
        {"kind": "world_fact", "data": "«Морянка» ушла три дня назад и не вернулась; последним её видел Марет."},
        {"kind": "gm_tool_call", "data": {"name": "roll_dice",
         "arguments": {"notation": "1d20+3",
                       "reason": "Проверка Внимательности (Восприятие), DC 13 — разглядеть детали сквозь туман."}}},
        {"kind": "dice", "data": {"ok": True, "notation": "1d20+3", "sides": 20, "count": 1, "keep": "",
                                   "rolls": [14], "kept": [14], "modifier": 3, "total": 17,
                                   "grade": "success", "natural": 14, "target_number": 13,
                                   "target_kind": "DC", "roll_kind": "check",
                                   "detail": "1d20+3 -> [14] +3 = 17 vs DC 13: grade=success, margin=+4, natural=14"}},
        {"kind": "gm_narration", "sid": g,
         "data": "Туман липнет к лицу. Доски причала стонут под сапогами. У дальнего пирса покачивается пустая шлюпка — с «Морянки», ты узнаёшь метку на борту."},
        {"kind": "gm_tool_call", "data": {"name": "ask_npc",
         "arguments": {"npc_id": "n_borin",
                       "situation": "Игрок стоит у пустой шлюпки с «Морянки» и осматривает её. Борин — капитан стражи — замечает чужака у пирса."}}},
        {"kind": "npc_start", "agent": "Борин", "sid": n},
        {"kind": "npc_thinking", "sid": n,
         "data": "Чужак суёт нос в дела порта. Проверю, что знает, но виду не подам."},
        {"kind": "npc_speech", "sid": n, "data": {
            "speech": "Эй, путник. У пирса нечего ловить. Шлюпку видел? Значит, видел больше, чем стоило.",
            "action": "кладёт ладонь на рукоять топора",
            "claims": ["шлюпка с «Морянки»", "Борин — капитан стражи", "туман мешает обзору"],
        }},
        {"kind": "scene_update", "data": {"name": "Борин", "present": True,
         "present_npcs": ["Борин"]}},
        {"kind": "meta", "data": meta("GM ход", cached=800)},
        {"kind": "meta_total", "data": meta_total()},
    ]


def _showcase_block():
    """One turn that exercises the remaining tool cards: set_npc_whereabouts,
    ask_npc (first + correction redo), move_npc and set_scene."""
    g = "gs"
    n = "ns"
    return [
        {"kind": "player", "data": "Спрашиваю Лизу, где найти капитана Марета, и иду за ним."},
        {"kind": "gm_thinking", "sid": g,
         "data": "Игрок ищет Марета. Уточняю его местонахождение, ввожу Лизу, при необходимости меняю сцену."},
        {"kind": "gm_tool_call", "data": {"name": "set_npc_whereabouts", "arguments": {
            "npc_id": "n_maret", "status": "likely",
            "location_name": "таверна «Треснувший якорь»", "source": "со слов Лизы",
            "details": "Марет пережидает туман там после смены."}}},
        {"kind": "npc_whereabouts", "data": {"name": "Капитан Марет", "present": False,
            "whereabouts": {"status": "likely", "location_name": "таверна «Треснувший якорь»",
                            "details": "Со слов Лизы — пережидает туман там."}}},
        {"kind": "gm_tool_call", "data": {"name": "ask_npc", "arguments": {
            "npc_id": "n_liza",
            "situation": "Игрок подходит к торговке Лизе и спрашивает, где найти капитана Марета."}}},
        {"kind": "npc_start", "agent": "Лиза", "sid": n},
        {"kind": "npc_thinking", "sid": n,
         "data": "Чужак ищет Марета. Подскажу, но не задаром — пусть запомнит, кто помог."},
        {"kind": "npc_speech", "sid": n, "data": {
            "speech": "Марет? В «Треснувшем якоре», где ж ещё. Туман — он там и сидит.",
            "action": "кивает в сторону переулка",
            "claims": ["Марет в таверне", "Марет вспыльчив про «Морянку»"]}},
        {"kind": "gm_tool_call", "data": {"name": "ask_npc", "arguments": {
            "npc_id": "n_liza",
            "situation": "Игрок подходит к торговке Лизе и спрашивает, где найти капитана Марета.",
            "correction": "Лиза осторожна с чужаками и не выдала бы Марета так прямо. Перепиши реплику уклончивее, намёком, без прямого адреса."}}},
        {"kind": "gm_reject", "agent": "Лиза",
         "data": "слишком прямо выдала Марета — переписать уклончивее"},
        {"kind": "gm_tool_call", "data": {"name": "move_npc", "arguments": {
            "npc_id": "n_liza", "present": True, "visible": True, "can_hear": True,
            "location": "у своего лотка на причале", "activity": "раскладывает товар",
            "attitude": "настороженно-дружелюбна",
            "reason": "Игрок подошёл к лотку Лизы и заговорил с ней."}}},
        {"kind": "gm_tool_call", "data": {"name": "set_scene", "arguments": {
            "title": "Таверна «Треснувший якорь»",
            "description": "Низкий зал, чад от ламп и мокрой шерсти. За дальним столом, спиной к двери, сидит широкоплечий моряк.",
            "location_id": "cracked_anchor_tavern",
            "present_npcs": ["n_maret"],
            "items": [{"id": "map_scrap", "name": "обрывок карты"},
                      {"id": "mug", "name": "кружка с элем"}],
            "exits": [{"id": "door", "name": "дверь на причал", "destination": "Ледяной порт", "visible": True},
                      {"id": "back", "name": "задняя дверь", "destination": "переулок",
                       "visible": True, "blocked_by": "заперта изнутри"}],
            "constraints": ["Марет вооружён ножом", "в зале ещё трое моряков"],
            "tension": "Марет не хочет говорить о «Морянке» при свидетелях.",
            "reason": "Игрок дошёл до таверны, куда указала Лиза."}}},
        {"kind": "scene_update", "data": {"title": "Таверна «Треснувший якорь»",
            "scene_id": "cracked_anchor_tavern", "present_npcs": ["Капитан Марет"]}},
        {"kind": "meta", "data": meta("GM ход", secs=3.1, cached=900)},
        {"kind": "meta_total", "data": meta_total()},
    ]


def seed_transcript():
    events = []
    for i in range(1, 3):  # a couple of full turns -> enough to scroll & virtualize
        events.extend(_seed_block(i))
    events.extend(_showcase_block())
    return events


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def _json(self, obj, code=200):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _body(self):
        n = int(self.headers.get("Content-Length", 0) or 0)
        if not n:
            return {}
        try:
            data = json.loads(self.rfile.read(n) or b"{}")
        except Exception:
            return {}
        return data if isinstance(data, dict) else {}

    def _handle_chats(self, path):
        parts = path.strip("/").split("/")
        if path == "/chats":  # create
            new_id = "chat_new_%d" % (len(MOCK_CHATS) + 1)
            MOCK_CHATS.insert(0, {"id": new_id, "title": "Новый чат", "preview": "", "turn_count": 0})
            ACTIVE_CHAT_ID["value"] = new_id
            self._json({"ok": True, "active_chat_id": new_id, "chat": _chat_one(new_id),
                        "state": STATE, "transcript": {"events": seed_transcript()}})
            return
        chat_id = parts[1] if len(parts) == 3 else ""
        if path.endswith("/activate"):
            ACTIVE_CHAT_ID["value"] = chat_id
            self._json({"ok": True, "chat": _chat_one(chat_id),
                        "state": STATE, "transcript": {"events": seed_transcript()}})
            return
        if path.endswith("/delete"):
            before = len(MOCK_CHATS)
            MOCK_CHATS[:] = [c for c in MOCK_CHATS if c["id"] != chat_id]
            deleted = len(MOCK_CHATS) < before
            if not deleted:
                self._json({"ok": False, "error": "chat not found"}, 404)
                return
            if ACTIVE_CHAT_ID["value"] == chat_id:
                ACTIVE_CHAT_ID["value"] = MOCK_CHATS[0]["id"] if MOCK_CHATS else ""
            if not MOCK_CHATS:  # mimic server creating a fresh chat when none remain
                MOCK_CHATS.append({"id": "chat_fresh", "title": "Новый чат", "preview": "", "turn_count": 0})
                ACTIVE_CHAT_ID["value"] = "chat_fresh"
            self._json({"ok": True, "deleted": True, "active_chat_id": ACTIVE_CHAT_ID["value"],
                        "chats": _chats_payload(), "chat": _chat_one(ACTIVE_CHAT_ID["value"]),
                        "state": STATE, "transcript": {"events": seed_transcript()},
                        "embeddings_purged": 7})
            return
        self._json({"error": "not found"}, 404)

    def do_GET(self):
        path = urlparse(self.path).path
        if path == "/" or path.startswith("/index"):
            with open(os.path.join(HERE, "index.html"), "rb") as f:
                body = f.read()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if path == "/state":
            self._json(STATE)
            return
        if path == "/models":
            self._json(MODELS)
            return
        if path == "/settings":
            self._json({"ok": True, "settings": SETTINGS, "settings_options": SETTINGS_OPTIONS})
            return
        if path == "/transcript":
            self._json({"events": seed_transcript()})
            return
        if path == "/export":
            self._json({"mock": True})
            return
        if path == "/debug":
            self._json(DEBUG)
            return
        if path == "/chats":
            self._json({"ok": True, "active_chat_id": ACTIVE_CHAT_ID["value"], "chats": _chats_payload()})
            return
        self._json({"error": "not found"}, 404)

    def do_POST(self):
        path = urlparse(self.path).path
        if path == "/chats" or path.startswith("/chats/"):
            self._handle_chats(path)
            return
        if path == "/settings":
            SETTINGS.update(self._body().get("settings") or {})
            self._json({
                "ok": True,
                "settings": SETTINGS,
                "settings_options": SETTINGS_OPTIONS,
                "state": STATE,
            })
            return
        if path in ("/cmd", "/model", "/codex/login", "/codex/logout"):
            self._json({"ok": True, "state": STATE})
            return
        if path == "/turn":
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream; charset=utf-8")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            self._stream_turn()
            return
        if path == "/debug/roll":
            body = self._body()
            for key in ("next", "all"):
                if key in body:
                    v = body.get(key)
                    DEBUG["roll_override"][key] = int(v) if v not in (None, "") else None
            self._json(DEBUG)
            return
        if path == "/debug/fact":
            body = self._body()
            text = (body.get("text") or "").strip()
            if text:
                DEBUG["facts"].append({
                    "id": f"dbg_{len(DEBUG['facts']) + 1}",
                    "kind": body.get("kind") or "public", "text": text, "keywords": [],
                })
            self._json(DEBUG)
            return
        if path == "/debug/fact_delete":
            fid = self._body().get("id")
            DEBUG["facts"] = [f for f in DEBUG["facts"] if f["id"] != fid]
            self._json(DEBUG)
            return
        if path == "/debug/npc":
            body = self._body()
            nid = body.get("id")
            fields = body.get("fields") if isinstance(body.get("fields"), dict) else {}
            for n in DEBUG["npcs"]:
                if n["id"] == nid:
                    n.update(fields)
                    if "present" in body:
                        n["present"] = bool(body.get("present"))
                    if isinstance(body.get("whereabouts"), dict):
                        n["whereabouts"] = body["whereabouts"]
            self._json(DEBUG)
            return
        if path == "/debug/player":
            DEBUG["player_character"].update(self._body().get("fields") or {})
            self._json(DEBUG)
            return
        if path == "/debug/story":
            body = self._body()
            for key in ("title", "public_intro", "hidden_truth"):
                if key in body:
                    DEBUG["story"][key] = body[key]
            if "hidden_events" in body:
                DEBUG["story"]["hidden_events"] = list(body.get("hidden_events") or [])
            self._json(DEBUG)
            return
        if path == "/debug/scene":
            patch = self._body().get("patch") or {}
            DEBUG["scene"].update(patch)
            self._json(DEBUG)
            return
        if path == "/debug/state_record":
            body = self._body()
            for rec in (body.get("add") or []):
                DEBUG["state_records"].append({
                    "record_id": f"sr_dbg_{len(DEBUG['state_records']) + 1}",
                    "kind": rec.get("kind", "fact"), "scope": rec.get("scope", "public"),
                    "text": rec.get("text", ""), "entity_id": rec.get("entity_id", ""), "active": True})
            for rid in (body.get("delete") or []):
                DEBUG["state_records"] = [r for r in DEBUG["state_records"] if r["record_id"] != rid]
            self._json(DEBUG)
            return
        if path == "/debug/rumor":
            body = self._body()
            action = body.get("action")
            if action == "add":
                DEBUG["rumors"].append({"seq": len(DEBUG["rumors"]) + 1,
                                        "speaker": body.get("speaker", ""),
                                        "text": body.get("text", ""), "confirmed": False})
            elif action == "delete":
                DEBUG["rumors"] = [r for r in DEBUG["rumors"] if r.get("seq") != body.get("seq")]
            elif action == "confirm":
                for r in DEBUG["rumors"]:
                    if r.get("seq") == body.get("seq"):
                        r["confirmed"] = bool(body.get("confirmed"))
            self._json(DEBUG)
            return
        self._json({"error": "not found"}, 404)

    def _push(self, ev):
        self.wfile.write(("data: " + json.dumps(ev, ensure_ascii=False) + "\n\n").encode("utf-8"))
        self.wfile.flush()

    def _stream(self, channel, sid, text):
        for word in text.split(" "):
            self._push({"kind": "delta", "sid": sid, "data": {"channel": channel, "text": word + " "}})
            time.sleep(0.02)

    def _stream_turn(self):
        try:
            self._push({"kind": "player", "data": "Иду к пустой шлюпке и осматриваю её изнутри."})
            time.sleep(0.1)
            self._stream("gm_thinking", "live",
                         "Игрок лезет в шлюпку. Внутри улика: обрывок карты. Дам Марету повод появиться.")
            self._push({"kind": "gm_tool_call", "data": {"name": "roll_dice",
                        "arguments": {"notation": "1d20+4",
                                      "reason": "Проверка Расследования, DC 12 — найти улики в шлюпке."}}})
            time.sleep(0.05)
            self._push({"kind": "dice", "data": {"ok": True, "notation": "1d20+4", "sides": 20, "count": 1,
                                                  "keep": "", "rolls": [11], "kept": [11], "modifier": 4,
                                                  "total": 15, "grade": "success", "natural": 11,
                                                  "target_number": 12, "target_kind": "DC", "roll_kind": "check",
                                                  "detail": "1d20+4 -> [11] +4 = 15 vs DC 12: grade=success, margin=+3, natural=11"}})
            self._push({"kind": "world_fact", "data": "На дне шлюпки — мокрый обрывок карты с пометкой у Чёрных скал."})
            time.sleep(0.05)
            self._stream("gm_narration", "live",
                         "В шлюпке стоит вода и пахнет смолой. Под банкой ты нащупываешь свёрток: обрывок карты, чернила расплылись, но пометка у Чёрных скал ещё читается.")
            self._push({"kind": "gm_tool_call", "data": {"name": "ask_npc",
                        "arguments": {"npc_id": "n_maret",
                                      "situation": "Игрок нашёл в шлюпке обрывок карты с пометкой у Чёрных скал. Марет видит находку."}}})
            time.sleep(0.05)
            self._push({"kind": "npc_start", "agent": "Капитан Марет", "sid": "npc_live"})
            time.sleep(0.05)
            self._push({"kind": "npc_thinking", "sid": "npc_live",
                        "data": "Он нашёл карту. Если узнает про скалы — пойдёт туда. Надо опередить или отговорить."})
            self._stream("npc_speech", "npc_live",
                         "Положи это на место. Чёрные скалы — не для таких, как ты. «Морянка» туда и ушла. Назад не вернулась.")
            self._push({"kind": "npc_speech", "sid": "npc_live", "data": {
                "speech": "Положи это на место. Чёрные скалы — не для таких, как ты. «Морянка» туда и ушла. Назад не вернулась.",
                "action": "перехватывает твою руку у запястья",
                "claims": ["карта ведёт к Чёрным скалам", "«Морянка» шла к скалам", "Марет знает маршрут"],
            }})
            self._push({"kind": "scene_update", "data": {"name": "Капитан Марет", "present": True,
                        "present_npcs": ["Борин", "Капитан Марет"]}})
            self._push({"kind": "meta", "data": meta("GM ход", secs=2.4, cached=900)})
            self._push({"kind": "meta_total", "data": meta_total()})
            self._push({"kind": "done"})
        except (BrokenPipeError, ConnectionAbortedError, ConnectionResetError):
            pass


def main():
    srv = ThreadingHTTPServer(("127.0.0.1", PORT), Handler)
    print(f"mock GM-Lab on http://127.0.0.1:{PORT}")
    srv.serve_forever()


if __name__ == "__main__":
    main()
