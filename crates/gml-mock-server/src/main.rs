//! gml-mock-server — deterministic HTTP+SSE fake for frontend smoke-testing.
//!
//! Faithful port of `gm-lab/mock_server.py` (566 lines). The Python file is a
//! **standalone hand-written fake** (a `BaseHTTPRequestHandler` over
//! `ThreadingHTTPServer`), NOT a wrapper that runs the real server with
//! `GM_BACKEND=mock`. It serves canned `/state`, `/models`, `/settings`,
//! `/transcript`, `/debug`, `/chats`, a scripted SSE `/turn` that exercises
//! every event kind (streaming deltas, gm thinking/narration, npc subagent,
//! tool calls, dice, scene updates, meta / meta_total, player_options), plus a
//! suite of `/debug/*` mutation endpoints so the dev UI can be previewed end to
//! end without any model. This Rust port reproduces those canned responses and
//! SSE behavior so the EXISTING React frontend (`web/src`) can run against it.
//!
//! Test/dev infra only — never shipped. Run: `cargo run -p gml-mock-server`
//! (listens on 127.0.0.1:8000; honors `PORT` / `GM_PORT` like the Python file).
//! The Vite dev proxy (`web/vite.config.js`) targets `http://127.0.0.1:8000`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_stream::stream;
use axum::body::{Body, Bytes};
use axum::extract::{Path as AxPath, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::Duration;

use gml_config::RuntimeSettings;
use gml_world::WHEREABOUTS_STATUS_LABELS;

const SCENE: &str = "Ледяной порт на краю мира. Туман, скрип снастей, пропавший корабль «Морянка».";

/// The three canned NPCs (`NPCS` in the Python module). `(id, name, role,
/// pronouns, color)` in declaration order.
const NPCS: [(&str, &str, &str, &str, &str); 3] = [
    ("n_borin", "Борин", "капитан стражи", "он", "#e6c08a"),
    ("n_liza", "Лиза", "торговка", "она", "#c4a7e7"),
    ("n_maret", "Капитан Марет", "моряк", "он", "#9ccfd8"),
];

/// `dict(world_mod.WHEREABOUTS_STATUS_LABELS)` — mirrored from gml-world so the
/// mock never drifts from the real app's status labels.
fn status_labels() -> Value {
    let mut m = Map::new();
    for (k, v) in WHEREABOUTS_STATUS_LABELS {
        m.insert(k.to_string(), Value::from(v));
    }
    Value::Object(m)
}

/// `runtime_settings.defaults()` — the real app's default settings shape.
fn settings_defaults() -> Value {
    let rs = RuntimeSettings::from_env();
    let mut m = Map::new();
    for (k, v) in rs.defaults() {
        m.insert(k, v);
    }
    Value::Object(m)
}

/// `runtime_settings.options()` — allowed-value lists + caps for the UI.
fn settings_options() -> Value {
    Value::Object(RuntimeSettings::from_env().options())
}

// --- mutable mock state -----------------------------------------------------

/// In-memory mock state replacing the Python module globals (`SETTINGS`,
/// `DEBUG`, `MOCK_CHATS`, `ACTIVE_CHAT_ID`). Guarded by a single mutex.
struct MockState {
    settings: Value,
    debug: Value,
    chats: Vec<Value>,
    active_chat_id: String,
    completed_turn_requests: HashMap<(String, String), String>,
}

type Shared = Arc<Mutex<MockState>>;

impl MockState {
    fn new() -> Self {
        MockState {
            settings: settings_defaults(),
            debug: debug_payload(),
            chats: initial_chats(),
            active_chat_id: "chat_ice".to_string(),
            completed_turn_requests: HashMap::new(),
        }
    }
}

/// `STATE` — the canned `/state` payload. Always rebuilt fresh so it tracks the
/// current `settings` (the Python `STATE["settings"]` aliases the SETTINGS dict,
/// which POST /settings mutates).
fn state_payload(settings: &Value) -> Value {
    json!({
        "model": "qwen2.5:14b",
        "backend": "ollama",
        "stream_gm_content": true,
        "public": SCENE,
        "scene": {
            "title": "Ледяной порт",
            "present_npcs": ["n_borin", "n_liza"],
            "npc_whereabouts": {"n_maret": {"status": "likely", "location_name": "у причала"}}
        },
        "npcs": npcs_array(),
        "status_labels": status_labels(),
        "settings": settings,
        "settings_options": settings_options(),
        "context_usage": context_usage(),
    })
}

/// The bare `NPCS` list as JSON objects.
fn npcs_array() -> Value {
    let arr: Vec<Value> = NPCS
        .iter()
        .map(|(id, name, role, pronouns, color)| {
            json!({"id": id, "name": name, "role": role, "pronouns": pronouns, "color": color})
        })
        .collect();
    Value::from(arr)
}

fn context_usage() -> Value {
    json!({
        "current": 12300,
        "world": 1800,
        "next_compact": {"label": "ГМ", "used": 12800, "limit": 100000, "remaining": 87200},
        "gm": {"active": 12300, "history": 12800, "summary": 900, "limit": 100000, "remaining": 87200},
        "npc": {"name": "Борин", "active": 5300, "history": 4100, "summary": 600, "limit": 64000, "remaining": 59900},
        "npcs": [
            {"id": "n_borin", "name": "Борин", "color": "#e6c08a", "has_session": true, "active": 5300, "history": 4100, "summary": 600, "limit": 64000, "remaining": 59900},
            {"id": "n_liza", "name": "Лиза", "color": "#c4a7e7", "has_session": false, "active": 1500, "history": 0, "summary": 0, "limit": 64000, "remaining": 64000},
            {"id": "n_maret", "name": "Капитан Марет", "color": "#9ccfd8", "has_session": false, "active": 1500, "history": 0, "summary": 0, "limit": 64000, "remaining": 64000}
        ]
    })
}

/// `MODELS` — canned `/models` payload.
fn models_payload(settings: &Value) -> Value {
    json!({
        "ok": true,
        "model": "qwen2.5:14b",
        "models": [
            {"id": "qwen2.5:14b", "name": "Qwen 2.5 14B", "supported": true},
            {"id": "llama3.1:8b", "name": "Llama 3.1 8B", "supported": true},
            {"id": "gpt-oss:20b", "name": "gpt-oss 20B", "supported": false}
        ],
        "settings": settings,
        "settings_options": settings_options(),
    })
}

fn player_character() -> Value {
    json!({
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
    })
}

/// `DEBUG` — the full canned `/debug` state dump.
fn debug_payload() -> Value {
    let npcs: Vec<Value> = NPCS
        .iter()
        .map(|(id, name, role, pronouns, color)| {
            json!({
                "id": id, "name": name, "role": role, "pronouns": pronouns, "color": color,
                "present": *id == "n_borin", "public_label": role,
                "whereabouts": {"location_name": "у причала", "status": "likely", "details": ""},
                "persona": format!("{name} — житель порта."), "voice": "Сдержанно, по делу.",
                "goals": "Защищать свои интересы.", "knowledge": "То, что очевидно в сцене.",
                "secret": "Личная тайна не задана.",
                "mechanics": {"abilities": {"STR": 12, "DEX": 11, "CON": 13, "INT": 10, "WIS": 12, "CHA": 11},
                              "skills": {"Perception": 3}, "saving_throws": {}, "passive_perception": 13,
                              "ac": 14, "hp": {"current": 20, "max": 20}, "speed": "30 ft",
                              "senses": "обычное зрение", "languages": "Общий"},
                "summary": "", "commitments": [], "messages": 0, "history": ""
            })
        })
        .collect();
    json!({
        "ok": true,
        "meta": {"model": "qwen2.5:14b", "backend": "ollama", "turns": 3,
                 "run_usage": {"input": 2100, "output": 580, "cached_tokens": 800},
                 "context_usage": {"current": 12300, "limit": 100000, "remaining": 87700}},
        "runtime": {
            "settings": settings_defaults(),
            "cache": {"prompt_cache_key": "gm-lab:mock-thread-abc123",
                      "thread_id": "mock-thread-abc123", "store": false},
        },
        "time": {"current_date_label": "День 1"},
        "player_character": player_character(),
        "story": {
            "title": "Ледяной порт",
            "objective": "Привести игрока к разгадке пропажи «Морянки».",
            "public_intro": SCENE,
            "hidden_truth": "Корабль увели контрабандисты к Чёрным скалам.",
            "constraints": ["Туман ограничивает видимость"],
            "hidden_events": ["Ночью кто-то жёг сигнальный огонь у скал"],
        },
        "roll_override": {"next": Value::Null, "all": Value::Null},
        "status_labels": status_labels(),
        "facts": [
            {"id": "public_1", "kind": "public", "text": "«Морянка» не вернулась из последнего рейса.", "keywords": ["морянка", "рейс"]},
            {"id": "truth_1", "kind": "truth", "text": "Марет знает маршрут к Чёрным скалам.", "keywords": ["марет", "маршрут"]},
            {"id": "rumor_1", "kind": "rumor", "text": "У скал видели чужие огни.", "keywords": ["скалы", "огни"]},
        ],
        "state_records": [
            {"record_id": "sr_known_borin", "kind": "fact", "scope": "public",
             "text": "Игрок знает капитана стражи по имени Борин.", "entity_id": "n_borin", "active": true},
            {"record_id": "sr_liza_note", "kind": "npc_memory", "scope": "owner",
             "text": "Лиза помнит, что Борин не спал в ночь пропажи.", "entity_id": "n_liza", "active": true},
        ],
        "rumors": [{"seq": 1, "speaker": "Лиза", "text": "Борин в ту ночь не спал.", "confirmed": false}],
        "scene": {"title": "Ледяной порт", "location_id": "ice_port", "present_npcs": ["n_borin"],
                  "tension": "ровное", "description": SCENE,
                  "constraints": ["Туман ограничивает видимость"],
                  "items": [{"item_id": "boat", "name": "пустая шлюпка с «Морянки»", "location": "у пирса",
                             "visible": true, "portable": false, "owner": "", "details": "метка на борту"}],
                  "exits": [{"exit_id": "tavern", "name": "переулок к таверне",
                             "destination": "таверна «Треснувший якорь»", "visible": true, "blocked_by": ""}]},
        "npcs": npcs,
        "memory": {"gm_summary": "", "loaded_gm_tools": ["ask_npc", "roll_dice"], "events": []},
    })
}

// --- in-memory chat list ----------------------------------------------------

/// `MOCK_CHATS` — the initial sidebar chat list (id, title, preview, turn_count).
fn initial_chats() -> Vec<Value> {
    vec![
        json!({"id": "chat_ice", "title": "Ледяной порт", "preview": "Туман у причала, пропала «Морянка».", "turn_count": 3}),
        json!({"id": "chat_garden", "title": "Стеклянный сад Элирии", "preview": "Чёрные прожилки на лунных орхидеях.", "turn_count": 1}),
        json!({"id": "chat_turn", "title": "Убийство в Тёрнвейле", "preview": "Купец мёртв, гильдия молчит.", "turn_count": 5}),
    ]
}

/// `_chats_payload()` — every chat enriched with created/updated/active.
fn chats_payload(st: &MockState) -> Value {
    let arr: Vec<Value> = st
        .chats
        .iter()
        .map(|c| {
            let mut o = c.as_object().cloned().unwrap_or_default();
            o.insert("created_at".into(), Value::from("2026-06-19 10:00"));
            o.insert("updated_at".into(), Value::from("2026-06-20 12:30"));
            let id = o.get("id").and_then(|v| v.as_str()).unwrap_or("");
            o.insert("active".into(), Value::from(id == st.active_chat_id));
            Value::Object(o)
        })
        .collect();
    Value::from(arr)
}

/// `_chat_one(chat_id)` — one enriched chat, or a fresh "Новый чат" fallback.
fn chat_one(st: &MockState, chat_id: &str) -> Value {
    for c in &st.chats {
        if c.get("id").and_then(|v| v.as_str()) == Some(chat_id) {
            let mut o = c.as_object().cloned().unwrap_or_default();
            o.insert("created_at".into(), Value::from("2026-06-19 10:00"));
            o.insert("updated_at".into(), Value::from("2026-06-20 12:30"));
            o.insert("active".into(), Value::from(true));
            return Value::Object(o);
        }
    }
    json!({"id": chat_id, "title": "Новый чат", "preview": "", "turn_count": 0,
           "created_at": "", "updated_at": "", "active": true})
}

// --- meta helpers -----------------------------------------------------------

/// `meta(label, ...)` — per-call timing/usage payload.
fn meta(label: &str, secs: f64, tps: i64, tin: i64, tout: i64, cached: i64) -> Value {
    json!({
        "label": label, "secs": secs, "tps": tps, "in": tin, "out": tout,
        "cached": cached, "prompt_secs": 0.6,
        "eval_secs": ((secs - 0.6) * 100.0).round() / 100.0,
        "load_secs": 0,
    })
}

/// `meta_total()` — aggregate run usage payload.
fn meta_total() -> Value {
    json!({
        "secs": 5.1, "tokens": 2680, "in": 2100, "out": 580, "cached": 800,
        "calls": [
            {"label": "GM", "in": 1200, "out": 320, "tps": 44, "secs": 2.0},
            {"label": "NPC Борин", "in": 900, "out": 260, "tps": 51, "secs": 1.7}
        ],
        "sys_estimate": 1500,
        "context": context_usage(),
    })
}

// --- seed transcript --------------------------------------------------------

/// `_seed_block(i)` — one full canned turn exercising the common event kinds.
fn seed_block(i: i64) -> Vec<Value> {
    let g = format!("g{i}");
    let n = format!("n{i}");
    vec![
        json!({"kind": "player", "data": format!("[{i}] Осматриваю причал и прислушиваюсь к туману.")}),
        json!({"kind": "gm_thinking", "sid": g,
               "data": "Игрок осматривает причал. Описываю атмосферу, ввожу зацепку про «Морянку», даю Борину повод вмешаться."}),
        json!({"kind": "gm_tool_call", "data": {"name": "get_world_fact",
               "arguments": {"query": "пропавший корабль «Морянка» — последние свидетельства"}}}),
        json!({"kind": "world_fact", "data": "«Морянка» ушла три дня назад и не вернулась; последним её видел Марет."}),
        json!({"kind": "gm_tool_call", "data": {"name": "roll_dice",
               "arguments": {"notation": "1d20+3",
                             "reason": "Проверка Внимательности (Восприятие), DC 13 — разглядеть детали сквозь туман.",
                             "modifier_note": "навык Внимательности"}}}),
        json!({"kind": "dice", "data": {"ok": true, "notation": "1d20+3", "sides": 20, "count": 1, "keep": "",
                                         "rolls": [14], "kept": [14], "modifier": 3, "total": 17,
                                         "modifier_note": "навык Внимательности",
                                         "grade": "success", "natural": 14, "target_number": 13,
                                         "target_kind": "DC", "roll_kind": "check",
                                         "detail": "1d20+3 -> [14] +3 = 17 vs DC 13: grade=success, margin=+4, natural=14"}}),
        json!({"kind": "gm_narration", "sid": g,
               "data": "Туман липнет к лицу. Доски причала стонут под сапогами. У дальнего пирса покачивается пустая шлюпка — с «Морянки», ты узнаёшь метку на борту."}),
        json!({"kind": "gm_tool_call", "data": {"name": "ask_npc",
               "arguments": {"npc_id": "n_borin",
                             "situation": "Игрок стоит у пустой шлюпки с «Морянки» и осматривает её. Борин — капитан стражи — замечает чужака у пирса."}}}),
        json!({"kind": "npc_start", "agent": "Борин", "sid": n}),
        json!({"kind": "npc_thinking", "sid": n,
               "data": "Чужак суёт нос в дела порта. Проверю, что знает, но виду не подам."}),
        json!({"kind": "npc_speech", "sid": n, "data": {
            "speech": "Эй, путник. У пирса нечего ловить. Шлюпку видел? Значит, видел больше, чем стоило.",
            "action": "кладёт ладонь на рукоять топора",
            "claims": ["шлюпка с «Морянки»", "Борин — капитан стражи", "туман мешает обзору"]}}),
        json!({"kind": "scene_update", "data": {"name": "Борин", "present": true, "present_npcs": ["Борин"]}}),
        json!({"kind": "meta", "data": meta("GM ход", 2.0, 44, 1200, 320, 800)}),
        json!({"kind": "meta_total", "data": meta_total()}),
    ]
}

/// `_showcase_block()` — one turn exercising the remaining tool cards plus
/// player_options.
fn showcase_block() -> Vec<Value> {
    let g = "gs";
    let n = "ns";
    vec![
        json!({"kind": "player", "data": "Спрашиваю Лизу, где найти капитана Марета, и иду за ним."}),
        json!({"kind": "gm_thinking", "sid": g,
               "data": "Игрок ищет Марета. Уточняю его местонахождение, ввожу Лизу, при необходимости меняю сцену."}),
        json!({"kind": "gm_tool_call", "data": {"name": "set_npc_whereabouts", "arguments": {
            "npc_id": "n_maret", "status": "likely",
            "location_name": "таверна «Треснувший якорь»", "source": "со слов Лизы",
            "details": "Марет пережидает туман там после смены."}}}),
        json!({"kind": "npc_whereabouts", "data": {"name": "Капитан Марет", "present": false,
            "whereabouts": {"status": "likely", "location_name": "таверна «Треснувший якорь»",
                            "details": "Со слов Лизы — пережидает туман там."}}}),
        json!({"kind": "gm_tool_call", "data": {"name": "ask_npc", "arguments": {
            "npc_id": "n_liza",
            "situation": "Игрок подходит к торговке Лизе и спрашивает, где найти капитана Марета."}}}),
        json!({"kind": "npc_start", "agent": "Лиза", "sid": n}),
        json!({"kind": "npc_thinking", "sid": n,
               "data": "Чужак ищет Марета. Подскажу, но не задаром — пусть запомнит, кто помог."}),
        json!({"kind": "npc_speech", "sid": n, "data": {
            "speech": "Марет? В «Треснувшем якоре», где ж ещё. Туман — он там и сидит.",
            "action": "кивает в сторону переулка",
            "claims": ["Марет в таверне", "Марет вспыльчив про «Морянку»"]}}),
        json!({"kind": "gm_tool_call", "data": {"name": "ask_npc", "arguments": {
            "npc_id": "n_liza",
            "situation": "Игрок подходит к торговке Лизе и спрашивает, где найти капитана Марета.",
            "correction": "Лиза осторожна с чужаками и не выдала бы Марета так прямо. Перепиши реплику уклончивее, намёком, без прямого адреса."}}}),
        json!({"kind": "gm_reject", "agent": "Лиза",
               "data": "слишком прямо выдала Марета — переписать уклончивее"}),
        json!({"kind": "gm_tool_call", "data": {"name": "move_npc", "arguments": {
            "npc_id": "n_liza", "present": true, "visible": true, "can_hear": true,
            "location": "у своего лотка на причале", "activity": "раскладывает товар",
            "attitude": "настороженно-дружелюбна",
            "reason": "Игрок подошёл к лотку Лизы и заговорил с ней."}}}),
        json!({"kind": "gm_tool_call", "data": {"name": "set_scene", "arguments": {
            "title": "Таверна «Треснувший якорь»",
            "description": "Низкий зал, чад от ламп и мокрой шерсти. За дальним столом, спиной к двери, сидит широкоплечий моряк.",
            "location_id": "cracked_anchor_tavern",
            "present_npcs": ["n_maret"],
            "items": [{"id": "map_scrap", "name": "обрывок карты"},
                      {"id": "mug", "name": "кружка с элем"}],
            "exits": [{"id": "door", "name": "дверь на причал", "destination": "Ледяной порт", "visible": true},
                      {"id": "back", "name": "задняя дверь", "destination": "переулок",
                       "visible": true, "blocked_by": "заперта изнутри"}],
            "constraints": ["Марет вооружён ножом", "в зале ещё трое моряков"],
            "tension": "Марет не хочет говорить о «Морянке» при свидетелях.",
            "reason": "Игрок дошёл до таверны, куда указала Лиза."}}}),
        json!({"kind": "scene_update", "data": {"title": "Таверна «Треснувший якорь»",
            "scene_id": "cracked_anchor_tavern", "present_npcs": ["Капитан Марет"]}}),
        json!({"kind": "meta", "data": meta("GM ход", 3.1, 44, 1200, 320, 900)}),
        json!({"kind": "meta_total", "data": meta_total()}),
        json!({"kind": "player_options", "data": {
            "question": "Что Дарра делает дальше?",
            "options": [
                {"label": "К Марет", "message": "Хватит топтаться: иду к столу капитана Марета и сажусь напротив, чтобы он не смог уйти незамеченным."},
                {"label": "Осмотреть кухню", "message": "Остаюсь в зале и внимательно осматриваю рабочий стол, пол и заднюю дверь на случай, если мелкий след прилип уже внутри таверны."},
                {"label": "К Лизе", "message": "Возвращаюсь к Лизе и тихо спрашиваю, не заметила ли она, кто выходил через заднюю дверь перед моим приходом."},
                {"label": "Заказать эль", "message": "Подхожу к стойке, заказываю кружку эля и завожу разговор с трактирщиком, чтобы он расслабился и проговорился о Марете."},
                {"label": "Осмотреть заднюю дверь", "message": "Иду к задней двери, которую заперли изнутри, и осматриваю засов и пол перед ней — нет ли свежих следов или царапин."},
                {"label": "Подозвать стражу", "message": "Выхожу к причалу и зову Борина обратно: пусть подстрахует у входа, пока я говорю с капитаном."},
                {"label": "Ждать и слушать", "message": "Сажусь в тёмном углу, заказываю воду и просто слушаю разговоры моряков, пока кто-нибудь не упомянет «Морянку»."}]}}),
    ]
}

/// `seed_transcript()` — two full turns + the showcase turn.
fn seed_transcript() -> Value {
    let mut events = Vec::new();
    for i in 1..3 {
        events.extend(seed_block(i));
    }
    events.extend(showcase_block());
    json!({"events": events})
}

// --- HTTP handlers ----------------------------------------------------------

fn json_response(obj: Value, code: StatusCode) -> Response {
    (code, Json(obj)).into_response()
}

async fn get_index() -> Response {
    // The Python mock serves an inlined index.html next to it. In the Rust dev
    // workflow the frontend is served by Vite (port 5173) which proxies API
    // calls here, so `/` is rarely hit. Serve dist/index.html if present.
    let candidates = [
        "web/dist/index.html",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/dist/index.html"),
    ];
    for path in candidates {
        if let Ok(body) = std::fs::read(path) {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            return (headers, body).into_response();
        }
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    let body = "<!doctype html><meta charset=utf-8><title>gml-mock-server</title>\
        <p>Mock GM-Lab backend is running. Point the Vite dev server (port 5173) here, \
        or build the frontend into web/dist/."
        .to_string();
    (headers, body).into_response()
}

async fn get_state(State(s): State<Shared>) -> Response {
    let st = s.lock().unwrap();
    json_response(state_payload(&st.settings), StatusCode::OK)
}

async fn get_models(State(s): State<Shared>) -> Response {
    let st = s.lock().unwrap();
    json_response(models_payload(&st.settings), StatusCode::OK)
}

async fn get_settings(State(s): State<Shared>) -> Response {
    let st = s.lock().unwrap();
    json_response(
        json!({"ok": true, "settings": st.settings, "settings_options": settings_options()}),
        StatusCode::OK,
    )
}

async fn get_transcript() -> Response {
    json_response(seed_transcript(), StatusCode::OK)
}

async fn get_export() -> Response {
    json_response(json!({"mock": true}), StatusCode::OK)
}

async fn get_debug(State(s): State<Shared>) -> Response {
    let st = s.lock().unwrap();
    json_response(st.debug.clone(), StatusCode::OK)
}

async fn get_chats(State(s): State<Shared>) -> Response {
    let st = s.lock().unwrap();
    json_response(
        json!({"ok": true, "active_chat_id": st.active_chat_id, "chats": chats_payload(&st)}),
        StatusCode::OK,
    )
}

fn not_found() -> Response {
    json_response(json!({"error": "not found"}), StatusCode::NOT_FOUND)
}

// --- POST /chats and /chats/{id}/(activate|delete) --------------------------

async fn post_chats_create(State(s): State<Shared>) -> Response {
    let mut st = s.lock().unwrap();
    let new_id = format!("chat_new_{}", st.chats.len() + 1);
    st.chats.insert(
        0,
        json!({"id": new_id, "title": "Новый чат", "preview": "", "turn_count": 0}),
    );
    st.active_chat_id = new_id.clone();
    let chat = chat_one(&st, &new_id);
    json_response(
        json!({"ok": true, "active_chat_id": new_id, "chat": chat,
               "state": state_payload(&st.settings), "transcript": seed_transcript()}),
        StatusCode::OK,
    )
}

async fn post_chat_activate(State(s): State<Shared>, AxPath(chat_id): AxPath<String>) -> Response {
    let mut st = s.lock().unwrap();
    st.active_chat_id = chat_id.clone();
    let chat = chat_one(&st, &chat_id);
    json_response(
        json!({"ok": true, "chat": chat,
               "state": state_payload(&st.settings), "transcript": seed_transcript()}),
        StatusCode::OK,
    )
}

async fn post_chat_delete(State(s): State<Shared>, AxPath(chat_id): AxPath<String>) -> Response {
    let mut st = s.lock().unwrap();
    let before = st.chats.len();
    st.chats
        .retain(|c| c.get("id").and_then(|v| v.as_str()) != Some(chat_id.as_str()));
    if st.chats.len() >= before {
        return json_response(
            json!({"ok": false, "error": "chat not found"}),
            StatusCode::NOT_FOUND,
        );
    }
    if st.active_chat_id == chat_id {
        st.active_chat_id = st
            .chats
            .first()
            .and_then(|c| c.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
    }
    if st.chats.is_empty() {
        // mimic the server creating a fresh chat when none remain
        st.chats.push(
            json!({"id": "chat_fresh", "title": "Новый чат", "preview": "", "turn_count": 0}),
        );
        st.active_chat_id = "chat_fresh".to_string();
    }
    let active = st.active_chat_id.clone();
    let chats = chats_payload(&st);
    let chat = chat_one(&st, &active);
    json_response(
        json!({"ok": true, "deleted": true, "active_chat_id": active,
               "chats": chats, "chat": chat,
               "state": state_payload(&st.settings), "transcript": seed_transcript(),
               "embeddings_purged": 7}),
        StatusCode::OK,
    )
}

// --- POST /settings, /cmd, /model, /codex/* ---------------------------------

async fn post_settings(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let mut st = s.lock().unwrap();
    let incoming = body.map(|Json(v)| v).unwrap_or(Value::Null);
    if let Some(patch) = incoming.get("settings").and_then(|v| v.as_object()) {
        if let Some(cur) = st.settings.as_object_mut() {
            for (k, v) in patch {
                cur.insert(k.clone(), v.clone());
            }
        }
    }
    json_response(
        json!({"ok": true, "settings": st.settings, "settings_options": settings_options(),
               "state": state_payload(&st.settings)}),
        StatusCode::OK,
    )
}

async fn post_ok_state(State(s): State<Shared>) -> Response {
    let st = s.lock().unwrap();
    json_response(
        json!({"ok": true, "state": state_payload(&st.settings)}),
        StatusCode::OK,
    )
}

// --- POST /debug/* mutations (each returns the fresh DEBUG payload) ----------

fn body_obj(body: Option<Json<Value>>) -> Map<String, Value> {
    body.map(|Json(v)| v)
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

async fn post_debug_roll(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let b = body_obj(body);
    let mut st = s.lock().unwrap();
    if let Some(over) = st.debug["roll_override"].as_object_mut() {
        for key in ["next", "all"] {
            if let Some(v) = b.get(key) {
                let parsed = match v {
                    Value::Null => Value::Null,
                    Value::String(s) if s.is_empty() => Value::Null,
                    Value::String(s) => s.parse::<i64>().map(Value::from).unwrap_or(Value::Null),
                    Value::Number(_) => Value::from(v.as_i64().unwrap_or(0)),
                    _ => Value::Null,
                };
                over.insert(key.to_string(), parsed);
            }
        }
    }
    let dbg = st.debug.clone();
    json_response(dbg, StatusCode::OK)
}

async fn post_debug_fact(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let b = body_obj(body);
    let mut st = s.lock().unwrap();
    let text = b
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if !text.is_empty() {
        let kind = b
            .get("kind")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("public")
            .to_string();
        let len = st.debug["facts"].as_array().map(|a| a.len()).unwrap_or(0);
        if let Some(facts) = st.debug["facts"].as_array_mut() {
            facts.push(json!({"id": format!("dbg_{}", len + 1), "kind": kind, "text": text, "keywords": []}));
        }
    }
    let dbg = st.debug.clone();
    json_response(dbg, StatusCode::OK)
}

async fn post_debug_fact_delete(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let b = body_obj(body);
    let mut st = s.lock().unwrap();
    let fid = b.get("id").cloned().unwrap_or(Value::Null);
    if let Some(facts) = st.debug["facts"].as_array_mut() {
        facts.retain(|f| f.get("id") != Some(&fid));
    }
    let dbg = st.debug.clone();
    json_response(dbg, StatusCode::OK)
}

async fn post_debug_player(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let b = body_obj(body);
    let mut st = s.lock().unwrap();
    if let Some(fields) = b.get("fields").and_then(|v| v.as_object()) {
        if let Some(pc) = st.debug["player_character"].as_object_mut() {
            for (k, v) in fields {
                pc.insert(k.clone(), v.clone());
            }
        }
    }
    let dbg = st.debug.clone();
    json_response(dbg, StatusCode::OK)
}

async fn post_debug_npc(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let b = body_obj(body);
    let mut st = s.lock().unwrap();
    let nid = b.get("id").cloned().unwrap_or(Value::Null);
    let fields = b.get("fields").and_then(|v| v.as_object()).cloned();
    let present = b.get("present").cloned();
    let whereabouts = b.get("whereabouts").cloned();
    if let Some(npcs) = st.debug["npcs"].as_array_mut() {
        for n in npcs.iter_mut() {
            if n.get("id") == Some(&nid) {
                if let (Some(obj), Some(f)) = (n.as_object_mut(), fields.as_ref()) {
                    for (k, v) in f {
                        obj.insert(k.clone(), v.clone());
                    }
                }
                if let Some(p) = &present {
                    if let Some(obj) = n.as_object_mut() {
                        obj.insert("present".into(), Value::from(truthy(p)));
                    }
                }
                if let Some(w) = &whereabouts {
                    if w.is_object() {
                        if let Some(obj) = n.as_object_mut() {
                            obj.insert("whereabouts".into(), w.clone());
                        }
                    }
                }
            }
        }
    }
    let dbg = st.debug.clone();
    json_response(dbg, StatusCode::OK)
}

async fn post_debug_story(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let b = body_obj(body);
    let mut st = s.lock().unwrap();
    if let Some(story) = st.debug["story"].as_object_mut() {
        for key in ["title", "public_intro", "hidden_truth"] {
            if let Some(v) = b.get(key) {
                story.insert(key.to_string(), v.clone());
            }
        }
        if b.contains_key("hidden_events") {
            let evs = b
                .get("hidden_events")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            story.insert("hidden_events".into(), Value::from(evs));
        }
    }
    let dbg = st.debug.clone();
    json_response(dbg, StatusCode::OK)
}

async fn post_debug_scene(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let b = body_obj(body);
    let mut st = s.lock().unwrap();
    if let Some(patch) = b.get("patch").and_then(|v| v.as_object()) {
        if let Some(scene) = st.debug["scene"].as_object_mut() {
            for (k, v) in patch {
                scene.insert(k.clone(), v.clone());
            }
        }
    }
    let dbg = st.debug.clone();
    json_response(dbg, StatusCode::OK)
}

async fn post_debug_state_record(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let b = body_obj(body);
    let mut st = s.lock().unwrap();
    if let Some(add) = b.get("add").and_then(|v| v.as_array()) {
        for rec in add {
            let len = st.debug["state_records"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            let entry = json!({
                "record_id": format!("sr_dbg_{}", len + 1),
                "kind": rec.get("kind").and_then(|v| v.as_str()).unwrap_or("fact"),
                "scope": rec.get("scope").and_then(|v| v.as_str()).unwrap_or("public"),
                "text": rec.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                "entity_id": rec.get("entity_id").and_then(|v| v.as_str()).unwrap_or(""),
                "active": true,
            });
            if let Some(records) = st.debug["state_records"].as_array_mut() {
                records.push(entry);
            }
        }
    }
    if let Some(del) = b.get("delete").and_then(|v| v.as_array()) {
        let ids: Vec<&Value> = del.iter().collect();
        if let Some(records) = st.debug["state_records"].as_array_mut() {
            records.retain(|r| !ids.iter().any(|d| r.get("record_id") == Some(*d)));
        }
    }
    let dbg = st.debug.clone();
    json_response(dbg, StatusCode::OK)
}

async fn post_debug_rumor(State(s): State<Shared>, body: Option<Json<Value>>) -> Response {
    let b = body_obj(body);
    let mut st = s.lock().unwrap();
    let action = b.get("action").and_then(|v| v.as_str()).unwrap_or("");
    match action {
        "add" => {
            let len = st.debug["rumors"].as_array().map(|a| a.len()).unwrap_or(0);
            let entry = json!({
                "seq": len + 1,
                "speaker": b.get("speaker").and_then(|v| v.as_str()).unwrap_or(""),
                "text": b.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                "confirmed": false,
            });
            if let Some(rumors) = st.debug["rumors"].as_array_mut() {
                rumors.push(entry);
            }
        }
        "delete" => {
            let seq = b.get("seq").cloned().unwrap_or(Value::Null);
            if let Some(rumors) = st.debug["rumors"].as_array_mut() {
                rumors.retain(|r| r.get("seq") != Some(&seq));
            }
        }
        "confirm" => {
            let seq = b.get("seq").cloned().unwrap_or(Value::Null);
            let confirmed = b.get("confirmed").map(truthy).unwrap_or(false);
            if let Some(rumors) = st.debug["rumors"].as_array_mut() {
                for r in rumors.iter_mut() {
                    if r.get("seq") == Some(&seq) {
                        if let Some(obj) = r.as_object_mut() {
                            obj.insert("confirmed".into(), Value::from(confirmed));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    let dbg = st.debug.clone();
    json_response(dbg, StatusCode::OK)
}

/// Python `bool(x)` truthiness for the JSON values the frontend sends.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Null => false,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

// --- SSE /turn --------------------------------------------------------------

fn turn_sse_response(body: Body) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    (headers, body).into_response()
}

fn turn_done(request_id: &str, ok: bool, retryable: bool, replayed: bool) -> Value {
    json!({
        "kind": "done",
        "ok": ok,
        "retryable": retryable,
        "replayed": replayed,
        "request_id": request_id,
    })
}

/// Discovery stub: the mock's scripted turns finish within one request, so
/// there is never a live turn to re-attach to.
async fn get_active_turn(State(state): State<Shared>) -> Response {
    let chat_id = {
        let state = state.lock().expect("mock state lock");
        state.active_chat_id.clone()
    };
    json_response(
        json!({"ok": true, "chat_id": chat_id, "request_id": Value::Null}),
        StatusCode::OK,
    )
}

/// Re-attach stub: a committed request id answers with one replayed `done`
/// frame, anything else mirrors the real server's `turn_not_running`.
async fn get_turn_stream(
    State(state): State<Shared>,
    AxPath(request_id): AxPath<String>,
) -> Response {
    let committed = {
        let state = state.lock().expect("mock state lock");
        let key = (state.active_chat_id.clone(), request_id.clone());
        state.completed_turn_requests.contains_key(&key)
    };
    if committed {
        let frame = format!(
            "data: {}\n\n",
            serde_json::to_string(&turn_done(&request_id, true, false, true))
                .unwrap_or_default()
        );
        return turn_sse_response(Body::from(frame));
    }
    json_response(
        json!({
            "ok": false,
            "code": "turn_not_running",
            "error": "this turn request is not running and was not committed",
        }),
        StatusCode::NOT_FOUND,
    )
}

/// `_stream_turn()` — the scripted live turn. Each event is wrapped as a
/// `data: {json}\n\n` frame and ends with the real server's structured `done`.
async fn post_turn(State(state): State<Shared>, body: Bytes) -> Response {
    let data: Value = serde_json::from_slice(&body).unwrap_or_else(|_| json!({}));
    let text = data
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("Иду к пустой шлюпке и осматриваю её изнутри.")
        .to_string();
    let request_id = match data.get("request_id") {
        None | Some(Value::Null) => uuid::Uuid::new_v4().to_string(),
        Some(Value::String(value)) if value.trim().is_empty() => uuid::Uuid::new_v4().to_string(),
        Some(Value::String(value))
            if value.trim().len() <= 128 && !value.trim().chars().any(char::is_control) =>
        {
            value.trim().to_string()
        }
        _ => {
            return json_response(
                json!({"ok": false, "error": "request_id is invalid"}),
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let saved_text = {
        let mut state = state.lock().expect("mock state lock");
        let key = (state.active_chat_id.clone(), request_id.clone());
        match state.completed_turn_requests.get(&key) {
            Some(saved) => Some(saved.clone()),
            None => {
                state.completed_turn_requests.insert(key, text.clone());
                None
            }
        }
    };
    if let Some(saved_text) = saved_text {
        let (ok, retryable, replayed, error) = if saved_text == text {
            (true, false, true, None)
        } else {
            (
                false,
                false,
                false,
                Some("request_id has already been used for another turn"),
            )
        };
        let mut frames = String::new();
        if let Some(error) = error {
            frames.push_str(&format!(
                "data: {}\n\n",
                serde_json::to_string(
                    &json!({"kind": "error", "agent": "ГМ", "data": error, "sid": null})
                )
                .unwrap_or_default()
            ));
        }
        frames.push_str(&format!(
            "data: {}\n\n",
            serde_json::to_string(&turn_done(&request_id, ok, retryable, replayed))
                .unwrap_or_default()
        ));
        return turn_sse_response(Body::from(frames));
    }

    let body = Body::from_stream(stream! {
        // helper closures can't yield, so inline frame builders.
        macro_rules! frame {
            ($v:expr) => {
                Ok::<_, std::io::Error>(axum::body::Bytes::from(format!(
                    "data: {}\n\n",
                    serde_json::to_string(&$v).unwrap_or_default()
                )))
            };
        }

        yield frame!(json!({"kind": "player", "data": text}));
        tokio::time::sleep(Duration::from_millis(100)).await;

        // _stream("gm_thinking", "live", ...): one delta per word.
        for word in "Игрок лезет в шлюпку. Внутри улика: обрывок карты. Дам Марету повод появиться.".split(' ') {
            yield frame!(json!({"kind": "delta", "sid": "live", "data": {"channel": "gm_thinking", "text": format!("{word} ")}}));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        yield frame!(json!({"kind": "gm_tool_call", "data": {"name": "roll_dice",
            "arguments": {"notation": "1d20+4",
                          "reason": "Проверка Расследования, DC 12 — найти улики в шлюпке."}}}));
        tokio::time::sleep(Duration::from_millis(50)).await;

        yield frame!(json!({"kind": "dice", "data": {"ok": true, "notation": "1d20+4", "sides": 20, "count": 1,
            "keep": "", "rolls": [11], "kept": [11], "modifier": 4,
            "total": 15, "grade": "success", "natural": 11,
            "target_number": 12, "target_kind": "DC", "roll_kind": "check",
            "detail": "1d20+4 -> [11] +4 = 15 vs DC 12: grade=success, margin=+3, natural=11"}}));

        yield frame!(json!({"kind": "world_fact", "data": "На дне шлюпки — мокрый обрывок карты с пометкой у Чёрных скал."}));
        tokio::time::sleep(Duration::from_millis(50)).await;

        for word in "В шлюпке стоит вода и пахнет смолой. Под банкой ты нащупываешь свёрток: обрывок карты, чернила расплылись, но пометка у Чёрных скал ещё читается.".split(' ') {
            yield frame!(json!({"kind": "delta", "sid": "live", "data": {"channel": "gm_narration", "text": format!("{word} ")}}));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        yield frame!(json!({"kind": "gm_tool_call", "data": {"name": "ask_npc",
            "arguments": {"npc_id": "n_maret",
                          "situation": "Игрок нашёл в шлюпке обрывок карты с пометкой у Чёрных скал. Марет видит находку."}}}));
        tokio::time::sleep(Duration::from_millis(50)).await;

        yield frame!(json!({"kind": "npc_start", "agent": "Капитан Марет", "sid": "npc_live"}));
        tokio::time::sleep(Duration::from_millis(50)).await;

        yield frame!(json!({"kind": "npc_thinking", "sid": "npc_live",
            "data": "Он нашёл карту. Если узнает про скалы — пойдёт туда. Надо опередить или отговорить."}));

        for word in "Положи это на место. Чёрные скалы — не для таких, как ты. «Морянка» туда и ушла. Назад не вернулась.".split(' ') {
            yield frame!(json!({"kind": "delta", "sid": "npc_live", "data": {"channel": "npc_speech", "text": format!("{word} ")}}));
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        yield frame!(json!({"kind": "npc_speech", "sid": "npc_live", "data": {
            "speech": "Положи это на место. Чёрные скалы — не для таких, как ты. «Морянка» туда и ушла. Назад не вернулась.",
            "action": "перехватывает твою руку у запястья",
            "claims": ["карта ведёт к Чёрным скалам", "«Морянка» шла к скалам", "Марет знает маршрут"]}}));

        yield frame!(json!({"kind": "scene_update", "data": {"name": "Капитан Марет", "present": true,
            "present_npcs": ["Борин", "Капитан Марет"]}}));
        yield frame!(json!({"kind": "meta", "data": meta("GM ход", 2.4, 44, 1200, 320, 900)}));
        yield frame!(json!({"kind": "meta_total", "data": meta_total()}));
        yield frame!(turn_done(&request_id, true, false, false));
    });
    turn_sse_response(body)
}

// --- router + main ----------------------------------------------------------

/// Build the full mock router. Used by `main` and the in-crate tests.
fn build_router(state: Shared) -> Router {
    Router::new()
        .route("/", get(get_index))
        .route("/index.html", get(get_index))
        .route("/state", get(get_state))
        .route("/models", get(get_models))
        .route("/settings", get(get_settings).post(post_settings))
        .route("/transcript", get(get_transcript))
        .route("/export", get(get_export))
        .route("/debug", get(get_debug).post(not_found_post))
        .route("/chats", get(get_chats).post(post_chats_create))
        .route("/chats/{chat_id}/activate", post(post_chat_activate))
        .route("/chats/{chat_id}/delete", post(post_chat_delete))
        .route("/cmd", post(post_ok_state))
        .route("/model", post(post_ok_state))
        .route("/codex/login", post(post_ok_state))
        .route("/codex/logout", post(post_ok_state))
        .route("/turn", post(post_turn))
        .route("/turn/active", get(get_active_turn))
        .route("/turn/{request_id}/stream", get(get_turn_stream))
        .route("/debug/roll", post(post_debug_roll))
        .route("/debug/fact", post(post_debug_fact))
        .route("/debug/fact_delete", post(post_debug_fact_delete))
        .route("/debug/player", post(post_debug_player))
        .route("/debug/npc", post(post_debug_npc))
        .route("/debug/story", post(post_debug_story))
        .route("/debug/scene", post(post_debug_scene))
        .route("/debug/state_record", post(post_debug_state_record))
        .route("/debug/rumor", post(post_debug_rumor))
        .with_state(state)
}

async fn not_found_post() -> Response {
    not_found()
}

fn port() -> u16 {
    for key in ["PORT", "GM_PORT"] {
        if let Ok(v) = std::env::var(key) {
            if let Ok(p) = v.trim().parse::<u16>() {
                return p;
            }
        }
    }
    8000
}

#[tokio::main]
async fn main() {
    let state: Shared = Arc::new(Mutex::new(MockState::new()));
    let app = build_router(state);
    let port = port();
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .unwrap_or_else(|e| panic!("mock GM-Lab failed to bind {addr}: {e}"));
    println!("mock GM-Lab on http://{addr}");
    axum::serve(listener, app)
        .await
        .expect("mock server crashed");
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn app() -> Router {
        build_router(Arc::new(Mutex::new(MockState::new())))
    }

    async fn get_json(app: Router, path: &str) -> Value {
        let resp = app
            .into_service()
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "GET {path}");
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn state_has_contract_keys() {
        let v = get_json(app(), "/state").await;
        for key in [
            "model",
            "backend",
            "stream_gm_content",
            "public",
            "scene",
            "npcs",
            "status_labels",
            "settings",
            "settings_options",
            "context_usage",
        ] {
            assert!(v.get(key).is_some(), "missing /state key {key}");
        }
    }

    #[tokio::test]
    async fn models_and_settings_shape() {
        let m = get_json(app(), "/models").await;
        assert_eq!(m["ok"], json!(true));
        assert!(m["models"].as_array().unwrap().len() == 3);
        let s = get_json(app(), "/settings").await;
        assert_eq!(s["ok"], json!(true));
        assert!(s.get("settings").is_some());
        assert!(s.get("settings_options").is_some());
    }

    #[tokio::test]
    async fn transcript_and_debug() {
        let t = get_json(app(), "/transcript").await;
        let events = t["events"].as_array().unwrap();
        assert!(events.iter().any(|e| e["kind"] == json!("player_options")));
        assert!(events.iter().any(|e| e["kind"] == json!("npc_speech")));
        let d = get_json(app(), "/debug").await;
        assert_eq!(d["ok"], json!(true));
        assert!(d.get("player_character").is_some());
        assert!(d.get("scene").is_some());
    }

    #[tokio::test]
    async fn chats_lifecycle() {
        let a = app();
        let chats = get_json(a.clone(), "/chats").await;
        assert_eq!(chats["active_chat_id"], json!("chat_ice"));
        assert_eq!(chats["chats"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn turn_sse_frames() {
        let app = app();
        let request = || {
            Request::builder()
                .method("POST")
                .uri("/turn")
                .header("content-type", "application/json")
                .body(Body::from(
                    "{\"text\":\"hi\",\"request_id\":\"mock-idempotency\"}",
                ))
                .unwrap()
        };
        let resp = app.clone().into_service().oneshot(request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(ct.starts_with("text/event-stream"));
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        // every frame is `data: {...}\n\n`; stream ends with the done frame.
        assert!(text.contains("data: "));
        assert!(text.contains("\"kind\":\"done\"") || text.contains("\"kind\": \"done\""));
        assert!(
            text.contains("\"channel\":\"gm_narration\"")
                || text.contains("\"channel\": \"gm_narration\"")
        );
        for chunk in text.split("\n\n").filter(|c| !c.trim().is_empty()) {
            assert!(chunk.starts_with("data: "), "bad SSE frame: {chunk}");
        }

        let replay = app.into_service().oneshot(request()).await.unwrap();
        let replay_bytes = to_bytes(replay.into_body(), usize::MAX).await.unwrap();
        let replay_text = String::from_utf8(replay_bytes.to_vec()).unwrap();
        let replay_frames: Vec<Value> = replay_text
            .split("\n\n")
            .filter_map(|frame| frame.trim().strip_prefix("data: "))
            .map(|payload| serde_json::from_str(payload).unwrap())
            .collect();
        assert_eq!(replay_frames.len(), 1);
        assert_eq!(replay_frames[0]["kind"], "done");
        assert_eq!(replay_frames[0]["ok"], true);
        assert_eq!(replay_frames[0]["replayed"], true);
        assert_eq!(replay_frames[0]["request_id"], "mock-idempotency");
    }

    #[tokio::test]
    async fn debug_fact_add_and_delete() {
        let a = app();
        // add
        let resp = a
            .clone()
            .into_service()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/debug/fact")
                    .header("content-type", "application/json")
                    .body(Body::from("{\"text\":\"новый факт\",\"kind\":\"rumor\"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let facts = v["facts"].as_array().unwrap();
        assert_eq!(facts.len(), 4);
        assert_eq!(facts.last().unwrap()["text"], json!("новый факт"));
        assert_eq!(facts.last().unwrap()["kind"], json!("rumor"));
    }
}
