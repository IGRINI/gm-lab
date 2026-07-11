//! `MockClient` — the in-process deterministic backend for `GM_BACKEND=mock`.
//!
//! Faithful port of `llm_client.MockClient`. It drives the canonical scenario
//! the orchestrator contract tests depend on:
//!   1. The player loudly accuses Borin of murder.
//!   2. The GM calls `ask_npc` (Borin).
//!   3. Borin tries to slip out a (nonexistent) back door — an impossible claim.
//!   4. The GM reviews the draft and returns it for a redo with a `correction`.
//!   5. Borin replays, stalling, and the GM ends the scene.
//!
//! Step selection is driven by the number of `role == "tool"` messages already
//! in the GM history (`n_tool`), exactly as Python. The canned world-seed,
//! scene-delta, and NPC JSON are reproduced verbatim — the contract tests assert
//! on this content.

use async_trait::async_trait;
use serde_json::{Map, Value};

use gml_types::ParsedCall;

use crate::backend::{
    channel, Backend, BackendError, ChatOutput, ChatStreamOutput, DeltaSink, JsonStreamOutput,
};
use crate::parsing::mock_stats;

/// In-process deterministic backend.
pub struct MockClient {
    model: std::sync::Mutex<String>,
    call_log: std::sync::Mutex<Vec<Map<String, Value>>>,
}

impl Default for MockClient {
    fn default() -> Self {
        Self::new()
    }
}

impl MockClient {
    /// Construct a fresh mock client (`self._model = "mock"`).
    pub fn new() -> Self {
        MockClient {
            model: std::sync::Mutex::new("mock".to_string()),
            call_log: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// A snapshot of the call log.
    pub fn call_log(&self) -> Vec<Map<String, Value>> {
        self.call_log.lock().expect("call_log lock").clone()
    }

    /// `_remember(label)` — append a canned-stats row, return the stats.
    fn remember(&self, label: &str) -> Map<String, Value> {
        let s = mock_stats();
        let mut row = Map::new();
        row.insert("label".to_string(), Value::String(label.to_string()));
        for (k, v) in &s {
            row.insert(k.clone(), v.clone());
        }
        let prompt = s
            .get("prompt_eval_count")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let eval = s.get("eval_count").and_then(|v| v.as_i64()).unwrap_or(0);
        row.insert("tokens".to_string(), Value::from(prompt + eval));
        self.call_log.lock().expect("call_log lock").push(row);
        s
    }

    /// The synchronous body of `chat` — used by both `chat` and `chat_stream`.
    fn chat_impl(&self, messages: &Value) -> ChatOutput {
        self.remember("chat");
        let system_text = join_role_contents(messages, "system");
        if system_text.contains("GM-Lab world architect") {
            // Agent loop: first hop drafts the bible (tool call); once the draft
            // result is fed back, finish with a chat reply (no tool) so the loop
            // ends — mirrors a real model's think → draft → reply flow.
            if count_tool_messages(messages) == 0 {
                return world_architect_chat_output();
            }
            return world_architect_reply_output();
        }
        if system_text.contains("GM-Lab story architect") {
            // Same agent loop for the STORY architect: first hop drafts the plot
            // (tool call), then finishes with a chat reply once the tool result
            // is fed back.
            if count_tool_messages(messages) == 0 {
                return story_architect_chat_output();
            }
            return story_architect_reply_output();
        }
        if system_text.contains("GM-Lab character architect") {
            // Same agent loop for the CHARACTER architect: first hop drafts the
            // hero sheet (tool call), then finishes with a chat reply once the
            // tool result is fed back.
            if count_tool_messages(messages) == 0 {
                return character_architect_chat_output();
            }
            return character_architect_reply_output();
        }
        let n_tool = count_tool_messages(messages);

        if n_tool == 0 {
            // First move: call the NPC.
            let calls = vec![ParsedCall::new(
                "ask_npc",
                obj([
                    ("npc_id", Value::String("borin".to_string())),
                    (
                        "situation",
                        Value::String("Игрок громко обвиняет Борина в убийстве.".to_string()),
                    ),
                ]),
                "mock0",
            )];
            return ChatOutput {
                thinking: "Нужен Борин — зову ask_npc.".to_string(),
                content: String::new(),
                assistant_msg: toolmsg(&calls),
                calls,
            };
        }
        if n_tool == 1 {
            // GM reviews the draft -> return with a correction.
            let calls = vec![ParsedCall::new(
                "ask_npc",
                obj([
                    ("npc_id", Value::String("borin".to_string())),
                    (
                        "situation",
                        Value::String("Игрок громко обвиняет Борина в убийстве.".to_string()),
                    ),
                    (
                        "correction",
                        Value::String(
                            "Задней двери у «Грифона» нет — выход только через зал, на виду. \
                             Так не улизнёшь, отыграй иначе."
                                .to_string(),
                        ),
                    ),
                ]),
                "mock1",
            )];
            return ChatOutput {
                thinking: "Борин рвётся в несуществующую заднюю дверь — возвращаю на переделку."
                    .to_string(),
                content: String::new(),
                assistant_msg: toolmsg(&calls),
                calls,
            };
        }
        let content =
            "Борин мнётся за стойкой, так и не сумев улизнуть. Зал притих и смотрит на вас."
                .to_string();
        ChatOutput {
            thinking: "NPC отыграл, завершаю сцену.".to_string(),
            content: content.clone(),
            calls: Vec::new(),
            assistant_msg: assistant_plain(&content),
        }
    }

    /// The synchronous body of `chat_json` — returns the canned dict by inspecting
    /// the system/user message text. Faithful to Python `MockClient.chat_json`.
    fn chat_json_impl(&self, messages: &Value) -> Map<String, Value> {
        self.remember("chat_json");
        let system_text = join_role_contents(messages, "system");

        if system_text.contains("current-scene NPC roster changes") {
            // {"moves": []}
            return obj([("moves", Value::Array(Vec::new()))]);
        }
        if system_text.contains("GM-Lab location generator") {
            return obj([
                ("name", Value::String("Дорожная остановка".to_string())),
                ("kind", Value::String("travel_situation".to_string())),
                (
                    "visible_summary",
                    Value::String("У дороги видны свежие следы остановившейся повозки.".to_string()),
                ),
                (
                    "description",
                    Value::String(
                        "На обочине темнеют колеи, рядом валяется оборванная верёвка и пахнет мокрой кожей."
                            .to_string(),
                    ),
                ),
                (
                    "hidden_summary",
                    Value::String("Караван ушёл не сам: его аккуратно увели с дороги.".to_string()),
                ),
                (
                    "features",
                    Value::Array(vec![
                        Value::String("свежие колеи".to_string()),
                        Value::String("оборванная верёвка".to_string()),
                    ]),
                ),
                (
                    "sensory_details",
                    Value::Array(vec![Value::String("запах мокрой кожи".to_string())]),
                ),
                (
                    "choices",
                    Value::Array(vec![
                        Value::String("осмотреть следы".to_string()),
                        Value::String("продолжить путь".to_string()),
                    ]),
                ),
                (
                    "consequences",
                    Value::Array(vec![Value::String(
                        "задержка может раскрыть, кто свернул с тракта".to_string(),
                    )]),
                ),
                (
                    "hidden_clues",
                    Value::Array(vec![Value::String("следы подков без гвоздей".to_string())]),
                ),
                (
                    "knows_more",
                    Value::Array(vec![Value::String("местные возчики".to_string())]),
                ),
                ("transitions", Value::Array(Vec::new())),
                (
                    "anti_repeat_key",
                    Value::String("road-stop-abandoned-cart-tracks".to_string()),
                ),
                (
                    "memory_note",
                    Value::String("На дороге найдены следы уведённой повозки.".to_string()),
                ),
            ]);
        }
        if system_text.contains("GM-Lab NPC generator") {
            return serde_json::json!({
                "name": "Тихон Ржавый",
                "pronouns": "М",
                "role": "бармен",
                "public_label": "бармен за стойкой",
                "age": "за пятьдесят",
                "physical_type": "грузный, с проседью и мозолистыми руками",
                "distinctive_features": "рыжий шрам на предплечье, полотенце через плечо",
                "persona": "Немногословный хозяин таверны, что подмечает каждого гостя и держит язык за зубами.",
                "personality": "спокойный, наблюдательный, недоверчивый к чужакам",
                "values": "порядок в зале и безопасность своих завсегдатаев",
                "habits": "протирает одну и ту же кружку, пока слушает разговоры",
                "pressure_response": "уходит в глухую оборону и отмалчивается, но не лжёт прямо",
                "boundaries": "не выдаёт постояльцев и не лезет в чужую поножовщину",
                "voice": "низкий хриплый говор, короткие фразы, редкая сухая усмешка",
                "goals": ["сохранить таверну на плаву", "понять, что случилось со смотрителем"],
                "agenda": "протирает кружки и слушает зал",
                "attitude_to_player": 0,
                "knowledge": "Знает завсегдатаев таверны и слухи о ночных гостях смотрителя.",
                "secret": "Прячет письмо пропавшего смотрителя под стойкой.",
                "mechanics": {
                    "abilities": {"STR": 12, "DEX": 9, "CON": 13, "INT": 10, "WIS": 12, "CHA": 11},
                    "skills": {"Проницательность": 3, "Обман": 2},
                    "ac": 11,
                    "hp": {"current": 16, "max": 16},
                    "speed": "30 футов",
                    "senses": "обычное зрение",
                    "languages": "Общий"
                },
                "anti_repeat_key": "barkeep-rusty-tikhon",
                "memory_note": "Тихон Ржавый видел, кто приходил к смотрителю ночью."
            })
            .as_object()
            .cloned()
            .unwrap_or_default();
        }
        if system_text.contains("starting scene") || system_text.contains("WorldSeed") {
            return world_seed_json();
        }
        let user = join_role_contents(messages, "user");
        if user.contains("REDO") {
            return obj([
                (
                    "reasoning",
                    Value::String("Чёрт, незаметно не выйти. Придётся тянуть время.".to_string()),
                ),
                (
                    "speech",
                    Value::String("Сейчас, дружище, эль принесу, обожди-ка.".to_string()),
                ),
                (
                    "action",
                    Value::String("медленно бредёт к бочкам, не сводя глаз с гостя".to_string()),
                ),
                ("claims", Value::Array(Vec::new())),
            ]);
        }
        obj([
            (
                "reasoning",
                Value::String("Надо предупредить своих, пока не поздно.".to_string()),
            ),
            (
                "speech",
                Value::String("Я... э-э, мне на кухню надо, отойду на минутку.".to_string()),
            ),
            (
                "action",
                Value::String(
                    "пытается незаметно выскользнуть через заднюю дверь трактира".to_string(),
                ),
            ),
            (
                "claims",
                Value::Array(vec![Value::String(
                    "В трактире есть задняя дверь".to_string(),
                )]),
            ),
        ])
    }
}

fn world_architect_chat_output() -> ChatOutput {
    // FLAT draft args — matches the flat draft_world_bible schema. The backend
    // (nest_draft_args) folds these into the canonical nested `world_lore`.
    let arr =
        |items: &[&str]| Value::Array(items.iter().map(|s| Value::String(s.to_string())).collect());
    let calls = vec![ParsedCall::new(
        "draft_world_bible",
        obj([
            ("title", Value::String("Порог Второго Неба".to_string())),
            ("genre", Value::String("fantasy isekai".to_string())),
            ("tone", Value::String("tense hopeful".to_string())),
            (
                "world_size",
                Value::String(
                    "Континент с несколькими королевствами, духами дорог и дальними землями за картой."
                        .to_string(),
                ),
            ),
            (
                "population",
                Value::String(
                    "Десятки миллионов жителей: люди, духи мест, малые народы и редкие призванные чужаки."
                        .to_string(),
                ),
            ),
            (
                "public_premise",
                Value::String(
                    "Имя, клятва и долг имеют силу закона и магии; старые договоры с духами снова дают трещину."
                        .to_string(),
                ),
            ),
            (
                "hidden_premise",
                Value::String(
                    "Призванные появляются потому, что старый договор мира треснул и ищет внешнюю переменную."
                        .to_string(),
                ),
            ),
            (
                "dogmas",
                arr(&[
                    "имя и клятва имеют юридическую и мистическую силу",
                    "духи мест помнят долги лучше людей",
                ]),
            ),
            (
                "world_laws",
                arr(&[
                    "магия требует имени, цены или признанного права",
                    "дальняя дорога меняет слухи и баланс сил",
                ]),
            ),
            ("regions", arr(&["Семь земель под Осколочной Луной"])),
            ("power_centers", arr(&["Корона Второго Неба и храмовые суды"])),
            ("religions", arr(&["культ дорожных духов и официальная вера клятв"])),
            ("gods", arr(&["Старшие Духи Порогов"])),
            ("cultures", arr(&["родовые дома, гильдии рунников и призванные чужаки"])),
            (
                "history",
                arr(&["После войны семи клятв границы стали держаться на договорах с духами."]),
            ),
            (
                "economy",
                arr(&["долги, дорожные пошлины, рунические замки и сезонные караваны"]),
            ),
            (
                "daily_life",
                arr(&["люди боятся нарушить клятву публично и ценят свидетелей сделки"]),
            ),
            ("hidden_secrets", arr(&["часть пророчеств написана прошлыми призванными"])),
            (
                "location_rules",
                arr(&["каждая новая локация должна иметь связь с долгом, властью, дорогой или духом места"]),
            ),
            (
                "prohibited_elements",
                arr(&["технологический постапокалипсис без объяснения как чужеродный артефакт"]),
            ),
        ]),
        "mock_world_architect0",
    )];
    ChatOutput {
        thinking: "Собираю структурированную библию мира.".to_string(),
        content: String::new(),
        assistant_msg: toolmsg(&calls),
        calls,
    }
}

/// Second hop of the mock architect: after the `draft_world_bible` tool result is
/// fed back, the agent finishes with a short chat reply (no more tool calls), so
/// the loop terminates — the same shape a real model produces.
fn world_architect_reply_output() -> ChatOutput {
    let reply = "Собрал черновик мира «Порог Второго Неба»: клятвы как закон, духи мест, \
                 призванные чужаки и трещина в старом договоре. Что усилить дальше — веру, \
                 политику дворов или историю войны клятв?"
        .to_string();
    ChatOutput {
        thinking: "Черновик собран — проверяю, чего ещё не хватает, и отвечаю пользователю."
            .to_string(),
        content: reply.clone(),
        assistant_msg: assistant_plain(&reply),
        calls: Vec::new(),
    }
}

/// First hop of the mock STORY architect: draft a playable plot via
/// `draft_story_plot`. Nested plot args (scene / player_character / npcs) — the
/// story schema is nested, unlike the flat world bible.
fn story_architect_chat_output() -> ChatOutput {
    let arr =
        |items: &[&str]| Value::Array(items.iter().map(|s| Value::String(s.to_string())).collect());
    let calls = vec![ParsedCall::new(
        "draft_story_plot",
        obj([
            (
                "title",
                Value::String("Деревня у живой дороги".to_string()),
            ),
            (
                "description",
                Value::String("Короткий пролог у пробуждающейся дороги.".to_string()),
            ),
            (
                "story_brief",
                Value::String(
                    "Ты пришёл в деревню, где дорога просыпается по ночам и требует свою плату."
                        .to_string(),
                ),
            ),
            (
                "public_intro",
                Value::String("Деревня живёт по правилам дороги.".to_string()),
            ),
            (
                "hidden_truth",
                Value::String(
                    "Староста скормил дороге собственного сына ради урожая.".to_string(),
                ),
            ),
            (
                "player_character",
                Value::Object(obj([
                    ("name", Value::String("Мира".to_string())),
                    (
                        "class_role",
                        Value::String("странствующий писец".to_string()),
                    ),
                ])),
            ),
            (
                "scene",
                Value::Object(obj([
                    ("title", Value::String("Ворота деревни".to_string())),
                    ("location_id", Value::String("village_gate".to_string())),
                    (
                        "description",
                        Value::String("Покосившиеся ворота у кромки живой дороги.".to_string()),
                    ),
                    ("present_npcs", arr(&["starosta"])),
                    ("tension", Value::String("Дорога вот-вот проснётся.".to_string())),
                ])),
            ),
            (
                "npcs",
                Value::Array(vec![Value::Object(obj([
                    ("id", Value::String("starosta".to_string())),
                    ("name", Value::String("Старый Гедд".to_string())),
                    ("role", Value::String("староста".to_string())),
                    (
                        "persona",
                        Value::String("Усталый человек, скрывающий вину.".to_string()),
                    ),
                ]))]),
            ),
            (
                "public_facts",
                Value::Array(vec![Value::Object(obj([
                    ("id", Value::String("road_wakes".to_string())),
                    (
                        "text",
                        Value::String("Дорога шевелится в полнолуние.".to_string()),
                    ),
                    ("kind", Value::String("public".to_string())),
                ]))]),
            ),
            ("proper_nouns", arr(&["Живая Дорога"])),
            ("time", Value::from(1080)),
        ]),
        "mock_story_architect0",
    )];
    ChatOutput {
        thinking: "Собираю играбельный старт сюжета в рамках мира.".to_string(),
        content: String::new(),
        assistant_msg: toolmsg(&calls),
        calls,
    }
}

/// Second hop of the mock STORY architect: finish with a short chat reply so the
/// loop terminates.
fn story_architect_reply_output() -> ChatOutput {
    let reply = "Собрал черновик сюжета «Деревня у живой дороги»: пробуждающаяся дорога, \
                 виноватый староста и стартовая сцена у ворот. Что усилить — улики, \
                 второстепенных персонажей или ставку героя?"
        .to_string();
    ChatOutput {
        thinking: "Черновик сюжета собран — отвечаю пользователю.".to_string(),
        content: reply.clone(),
        assistant_msg: assistant_plain(&reply),
        calls: Vec::new(),
    }
}

/// First hop of the mock CHARACTER architect: draft a launchable hero via
/// `draft_player_character`. FLAT sheet args (abilities/hp/inventory/spells) —
/// the character schema is flat, like the world bible.
fn character_architect_chat_output() -> ChatOutput {
    let arr =
        |items: &[&str]| Value::Array(items.iter().map(|s| Value::String(s.to_string())).collect());
    let calls = vec![ParsedCall::new(
        "draft_player_character",
        obj([
            ("name", Value::String("Кара Вент".to_string())),
            ("pronouns", Value::String("Ж".to_string())),
            (
                "class_role",
                Value::String("странствующая следопытка".to_string()),
            ),
            ("level", Value::from(2)),
            (
                "background",
                Value::String(
                    "Выросла на пограничных трактах, читает следы лучше любых карт.".to_string(),
                ),
            ),
            (
                "physical_type",
                Value::String("жилистая, с обветренным лицом".to_string()),
            ),
            (
                "abilities",
                Value::Object(obj([
                    ("STR", Value::from(12)),
                    ("DEX", Value::from(16)),
                    ("CON", Value::from(13)),
                    ("INT", Value::from(10)),
                    ("WIS", Value::from(14)),
                    ("CHA", Value::from(9)),
                ])),
            ),
            (
                "skills",
                Value::Object(obj([
                    ("Выживание", Value::from(4)),
                    ("Скрытность", Value::from(5)),
                ])),
            ),
            ("ac", Value::from(14)),
            (
                "hp",
                Value::Object(obj([
                    ("current", Value::from(18)),
                    ("max", Value::from(18)),
                ])),
            ),
            ("speed", Value::String("30 ft".to_string())),
            ("languages", Value::String("Общий, Лесной".to_string())),
            (
                "inventory",
                arr(&["короткий лук", "колчан стрел", "охотничий нож", "плащ следопыта"]),
            ),
            (
                "spells",
                Value::Array(vec![Value::Object(obj([
                    ("name", Value::String("Отметка охотника".to_string())),
                    ("level", Value::from(1)),
                    ("concentration", Value::Bool(true)),
                    ("ritual", Value::Bool(false)),
                    (
                        "effect",
                        Value::String("Помечает цель, добавляя урон по ней.".to_string()),
                    ),
                ]))]),
            ),
            (
                "spell_slots",
                Value::Object(obj([("1", Value::from(2))])),
            ),
            (
                "spell_slots_max",
                Value::Object(obj([("1", Value::from(2))])),
            ),
        ]),
        "mock_character_architect0",
    )];
    ChatOutput {
        thinking: "Собираю играбельного героя-следопыта.".to_string(),
        content: String::new(),
        assistant_msg: toolmsg(&calls),
        calls,
    }
}

/// Second hop of the mock CHARACTER architect: finish with a short chat reply so
/// the loop terminates.
fn character_architect_reply_output() -> ChatOutput {
    let reply = "Собрал героиню «Кара Вент»: следопытка 2 уровня, ловкая, с луком и \
                 отметкой охотника. Что доработать — предысторию, снаряжение или заклинания?"
        .to_string();
    ChatOutput {
        thinking: "Черновик персонажа собран — отвечаю пользователю.".to_string(),
        content: reply.clone(),
        assistant_msg: assistant_plain(&reply),
        calls: Vec::new(),
    }
}

/// Build `toolmsg(calls)` — assistant message with mock tool_calls.
///
/// Python:
/// ```python
/// def toolmsg(calls):
///     return {"role": "assistant", "content": "",
///             "tool_calls": [{"id": f"mock{i}", "type": "function",
///                             "function": {"name": c["name"], "arguments": c["arguments"]}}
///                            for i, c in enumerate(calls)]}
/// ```
/// Note: the tool-call `id` is regenerated as `mock{i}` from the enumerate index
/// (it does NOT reuse `c["id"]`). `arguments` is the raw dict (not a JSON string),
/// matching Python exactly.
fn toolmsg(calls: &[ParsedCall]) -> Value {
    let mut tool_calls = Vec::with_capacity(calls.len());
    for (i, c) in calls.iter().enumerate() {
        tool_calls.push(serde_json::json!({
            "id": format!("mock{i}"),
            "type": "function",
            "function": {"name": c.name, "arguments": Value::Object(c.arguments.clone())},
        }));
    }
    let mut m = Map::new();
    m.insert("role".to_string(), Value::String("assistant".to_string()));
    m.insert("content".to_string(), Value::String(String::new()));
    m.insert("tool_calls".to_string(), Value::Array(tool_calls));
    Value::Object(m)
}

/// Final assistant message `{"role":"assistant","content":content}`.
fn assistant_plain(content: &str) -> Value {
    let mut m = Map::new();
    m.insert("role".to_string(), Value::String("assistant".to_string()));
    m.insert("content".to_string(), Value::String(content.to_string()));
    Value::Object(m)
}

/// Helper to build an object preserving insertion order.
fn obj<const N: usize>(pairs: [(&str, Value); N]) -> Map<String, Value> {
    let mut m = Map::new();
    for (k, v) in pairs {
        m.insert(k.to_string(), v);
    }
    m
}

/// `n_tool = sum(1 for m in messages if _attr(m, "role") == "tool")`.
fn count_tool_messages(messages: &Value) -> usize {
    match messages {
        Value::Array(items) => items
            .iter()
            .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some("tool"))
            .count(),
        _ => 0,
    }
}

/// `" ".join(str(_attr(m, "content", "")) for m in messages if _attr(m, "role") == role)`.
fn join_role_contents(messages: &Value, role: &str) -> String {
    let Value::Array(items) = messages else {
        return String::new();
    };
    let parts: Vec<String> = items
        .iter()
        .filter(|m| m.get("role").and_then(|r| r.as_str()) == Some(role))
        .map(|m| py_str_content(m.get("content")))
        .collect();
    parts.join(" ")
}

/// `str(_attr(m, "content", ""))` — Python str() of the content (default "").
fn py_str_content(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

/// The canned WorldSeed JSON (verbatim from Python `MockClient.chat_json`).
fn world_seed_json() -> Map<String, Value> {
    serde_json::from_value(serde_json::json!({
        "public_intro": "Ледяной порт Нордхольм. В таверне пахнет мокрыми канатами; \
                         порт шепчется о пропавшем корабле «Северная свеча».",
        "hidden_truth": "The ship was hidden in a frozen cove by smugglers.",
        "proper_nouns": ["Нордхольм", "«Северная свеча»"],
        "public_facts": [
            "Корабль «Северная свеча» не вернулся в порт Нордхольм.",
            "Ива держит портовую таверну.",
            "Рун служил на пристани и знает моряков."
        ],
        "npcs": [
            {"id": "iva", "name": "Ива", "role": "tavern keeper",
             "persona": "Practical keeper of the port tavern, tired but observant.",
             "voice": "Dry, direct, with sea-port slang.",
             "goals": "Keep order and learn what happened to the missing ship.",
             "knowledge": "Knows dock gossip and who drank in the tavern last night.",
             "secret": "Owes money to people tied to the frozen cove."},
            {"id": "run", "name": "Рун", "role": "sailor",
             "persona": "Young sailor, nervous and superstitious.",
             "voice": "Fast, hushed, often glances at the door.",
             "goals": "Avoid blame for the missing ship.",
             "knowledge": "Saw a strange lantern signal before dawn.",
             "secret": "Skipped his watch for a few minutes."}
        ],
        "scene": {
            "id": "northolm_tavern",
            "location_id": "northolm_port",
            "title": "Портовая таверна Нордхольма",
            "description": "Игрок стоит в портовой таверне. За окнами ледяные причалы; \
                            Ива у стойки, Рун у печи.",
            "present_npcs": ["iva", "run"],
            "items": [
                {"id": "counter", "name": "стойка", "location": "у стены",
                 "visible": true, "portable": false},
                {"id": "harbor_map", "name": "карта гавани", "location": "на стене",
                 "visible": true, "portable": false}
            ],
            "exits": [
                {"id": "dock_door", "name": "дверь к причалам",
                 "destination": "ледяные причалы", "visible": true}
            ],
            "constraints": [
                "Only Ива and Рун are present as named NPCs in the tavern.",
                "The missing ship is not visible from here."
            ],
            "tension": "People are cold, worried, and watching strangers."
        }
    }))
    .expect("world seed json is a valid object")
}

#[async_trait]
impl Backend for MockClient {
    fn model(&self) -> String {
        self.model.lock().expect("model lock").clone()
    }

    fn set_model(&self, model: &str) {
        // self._model = (model or "").strip() or self._model
        let m = model.trim();
        let mut guard = self.model.lock().expect("model lock");
        if !m.is_empty() {
            *guard = m.to_string();
        }
    }

    async fn list_models(&self) -> Vec<Value> {
        let m = self.model.lock().expect("model lock").clone();
        vec![serde_json::json!({"id": m, "name": m, "supported": true})]
    }

    async fn chat(
        &self,
        messages: &Value,
        _tools: Option<&Value>,
        _think: Option<bool>,
        _reasoning_role: &str,
    ) -> Result<ChatOutput, BackendError> {
        Ok(self.chat_impl(messages))
    }

    async fn chat_json(
        &self,
        messages: &Value,
        _schema: &Value,
        _think: Option<bool>,
        _reasoning_role: &str,
    ) -> Result<Map<String, Value>, BackendError> {
        Ok(self.chat_json_impl(messages))
    }

    async fn summarize(
        &self,
        _text: &str,
        _proper_nouns: &[String],
    ) -> Result<String, BackendError> {
        Ok("(compressed summary of previous turns)".to_string())
    }

    async fn chat_stream(
        &self,
        messages: &Value,
        tools: Option<&Value>,
        _think: Option<bool>,
        reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<ChatStreamOutput, BackendError> {
        if reasoning_role == "npc" {
            let n_tool = count_tool_messages(messages);
            let user = join_role_contents(messages, "user");
            if tools.is_some() && n_tool == 0 && user.contains("NPC_NOTE_MEMORY_TOOL_SENTINEL") {
                let calls = vec![ParsedCall::new(
                    "npc_note_memory",
                    obj([
                        (
                            "text",
                            Value::String(
                                "NPC_NOTE_MEMORY_TOOL_SENTINEL Борин запомнил угрозу игрока."
                                    .to_string(),
                            ),
                        ),
                        ("kind", Value::String("interaction".to_string())),
                        ("about", Value::String("player".to_string())),
                        ("privacy", Value::String("public".to_string())),
                    ]),
                    "npc_mock_note0",
                )];
                let stats = self.remember("chat_stream");
                return Ok(ChatStreamOutput {
                    thinking: "Нужно записать это в собственную память.".to_string(),
                    content: String::new(),
                    calls: calls.clone(),
                    assistant_msg: toolmsg(&calls),
                    stats,
                });
            }
            if tools.is_some() && n_tool == 0 && user.contains("NPC_RELATIONSHIP_TOOL_SENTINEL") {
                let calls = vec![ParsedCall::new(
                    "npc_recall_relationship",
                    obj([("target", Value::String("player".to_string()))]),
                    "npc_mock_rel0",
                )];
                let stats = self.remember("chat_stream");
                return Ok(ChatStreamOutput {
                    thinking: "Нужно вспомнить отношение к игроку.".to_string(),
                    content: String::new(),
                    calls: calls.clone(),
                    assistant_msg: toolmsg(&calls),
                    stats,
                });
            }
            if tools.is_some() && n_tool == 0 && user.contains("REMEMBER_TOOL_SENTINEL") {
                let calls = vec![ParsedCall::new(
                    "remember",
                    obj([("query", Value::String("REMEMBER_TOOL_SENTINEL".to_string()))]),
                    "npc_mock0",
                )];
                let stats = self.remember("chat_stream");
                return Ok(ChatStreamOutput {
                    thinking: "Нужно вспомнить личную память.".to_string(),
                    content: String::new(),
                    calls: calls.clone(),
                    assistant_msg: toolmsg(&calls),
                    stats,
                });
            }

            let tool_text = join_role_contents(messages, "tool");
            let data = if tool_text.contains("NPC_RELATIONSHIP_MEMORY_SENTINEL") {
                obj([
                    (
                        "response",
                        Value::String(
                            "Борин вспоминает прежнюю услугу игрока и говорит мягче.".to_string(),
                        ),
                    ),
                    (
                        "beats",
                        Value::Array(vec![Value::String(
                            "понижает голос и перестает пятиться".to_string(),
                        )]),
                    ),
                    (
                        "claims",
                        Value::Array(vec![Value::String(
                            "NPC_RELATIONSHIP_MEMORY_SENTINEL".to_string(),
                        )]),
                    ),
                ])
            } else if tool_text.contains("\"status\":\"stored\"")
                && tool_text.contains("\"scope\":\"npc\"")
            {
                obj([
                    (
                        "response",
                        Value::String("Борин коротко кивает, запоминая сказанное.".to_string()),
                    ),
                    (
                        "beats",
                        Value::Array(vec![Value::String(
                            "запоминает угрозу и держится настороженно".to_string(),
                        )]),
                    ),
                    ("claims", Value::Array(Vec::new())),
                ])
            } else if tool_text.contains("BORIN_REMEMBER_TOOL_SENTINEL") {
                obj([
                    (
                        "reasoning",
                        Value::String("Я опираюсь на собственное воспоминание.".to_string()),
                    ),
                    (
                        "speech",
                        Value::String("Помню: BORIN_REMEMBER_TOOL_SENTINEL.".to_string()),
                    ),
                    (
                        "action",
                        Value::String("приглушает голос и смотрит на стойку".to_string()),
                    ),
                    (
                        "claims",
                        Value::Array(vec![Value::String(
                            "BORIN_REMEMBER_TOOL_SENTINEL".to_string(),
                        )]),
                    ),
                ])
            } else {
                self.chat_json_impl(messages)
            };
            let content = py_json_dumps_default(&Value::Object(data));
            for chunk in chunk_by_chars(&content, 6) {
                sink.emit(channel::CONTENT, &chunk);
            }
            let stats = self.remember("chat_stream");
            return Ok(ChatStreamOutput {
                thinking: String::new(),
                content: content.clone(),
                calls: Vec::new(),
                assistant_msg: assistant_plain(&content),
                stats,
            });
        }

        // thinking, content, calls, msg = self.chat(...)
        let out = self.chat_impl(messages);
        // for w in (thinking or "").split(): yield ("thinking", w + " ")
        for w in split_whitespace(&out.thinking) {
            sink.emit(channel::THINKING, &format!("{w} "));
        }
        for w in split_whitespace(&out.content) {
            sink.emit(channel::CONTENT, &format!("{w} "));
        }
        let stats = self.remember("chat_stream");
        Ok(ChatStreamOutput {
            thinking: out.thinking,
            content: out.content,
            calls: out.calls,
            assistant_msg: out.assistant_msg,
            stats,
        })
    }

    async fn chat_json_stream(
        &self,
        messages: &Value,
        _schema: &Value,
        _think: Option<bool>,
        _reasoning_role: &str,
        sink: &mut (dyn DeltaSink + Send),
    ) -> Result<JsonStreamOutput, BackendError> {
        let data = self.chat_json_impl(messages);
        // s = json.dumps(data, ensure_ascii=False)
        // NOTE: Python json.dumps default separators include a space after ':'
        // and ',' (", " / ": "); the mock stream chunks this string. The content
        // is re-parsed by the orchestrator, so the inter-chunk spacing is not
        // wire-load-bearing, but we reproduce json.dumps' default spacing for
        // fidelity of the streamed deltas.
        let s = py_json_dumps_default(&Value::Object(data.clone()));
        // for i in range(0, len(s), 6): yield ("content", s[i:i+6])
        for chunk in chunk_by_chars(&s, 6) {
            sink.emit(channel::CONTENT, &chunk);
        }
        let stats = self.remember("chat_json_stream");
        Ok(JsonStreamOutput { data, stats })
    }
}

/// Python `str.split()` (no args) — split on runs of whitespace, dropping empties.
fn split_whitespace(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}

/// Chunk a string into pieces of at most `n` CHARS (Python slices by code point).
fn chunk_by_chars(s: &str, n: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let end = (i + n).min(chars.len());
        out.push(chars[i..end].iter().collect());
        i = end;
    }
    out
}

/// Serialize like Python `json.dumps(data, ensure_ascii=False)` (default
/// separators `", "` and `": "`, raw UTF-8). serde_json's pretty printer is not
/// it; we emit the default-spaced compact form manually.
fn py_json_dumps_default(v: &Value) -> String {
    let compact = serde_json::to_string(v).expect("json serialize");
    // Convert compact separators (',' and ':') into Python defaults (", " / ": ")
    // WITHOUT touching separators inside strings.
    insert_default_spaces(&compact)
}

fn insert_default_spaces(compact: &str) -> String {
    let mut out = String::with_capacity(compact.len() + compact.len() / 8);
    let mut in_string = false;
    let mut escaped = false;
    for c in compact.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            ',' => {
                out.push(',');
                out.push(' ');
            }
            ':' => {
                out.push(':');
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_chat_step0_calls_ask_npc() {
        let mc = MockClient::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "GM"},
            {"role": "user", "content": "Я обвиняю Борина!"}
        ]);
        let out = mc.chat(&messages, None, Some(false), "gm").await.unwrap();
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0].name, "ask_npc");
        assert_eq!(out.calls[0].id, "mock0");
        assert_eq!(
            out.calls[0].arguments.get("npc_id"),
            Some(&Value::from("borin"))
        );
        assert!(out.calls[0].arguments.get("correction").is_none());
        assert_eq!(out.content, "");
        // assistant_msg is a toolmsg
        assert_eq!(out.assistant_msg.get("content"), Some(&Value::from("")));
        assert!(out.assistant_msg.get("tool_calls").is_some());
    }

    #[tokio::test]
    async fn mock_chat_step1_returns_correction() {
        let mc = MockClient::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "GM"},
            {"role": "user", "content": "Я обвиняю Борина!"},
            {"role": "assistant", "content": "", "tool_calls": []},
            {"role": "tool", "content": "npc draft"}
        ]);
        let out = mc.chat(&messages, None, Some(false), "gm").await.unwrap();
        assert_eq!(out.calls.len(), 1);
        assert_eq!(out.calls[0].id, "mock1");
        let corr = out.calls[0]
            .arguments
            .get("correction")
            .and_then(|v| v.as_str())
            .unwrap();
        assert!(corr.contains("Задней двери у «Грифона» нет"));
    }

    #[tokio::test]
    async fn mock_chat_step2_ends_scene() {
        let mc = MockClient::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "GM"},
            {"role": "user", "content": "Я обвиняю Борина!"},
            {"role": "tool", "content": "draft 1"},
            {"role": "tool", "content": "draft 2"}
        ]);
        let out = mc.chat(&messages, None, Some(false), "gm").await.unwrap();
        assert!(out.calls.is_empty());
        assert!(out.content.contains("Борин мнётся за стойкой"));
        // final assistant_msg has no tool_calls
        assert!(out.assistant_msg.get("tool_calls").is_none());
        assert_eq!(
            out.assistant_msg.get("role"),
            Some(&Value::from("assistant"))
        );
    }

    #[tokio::test]
    async fn mock_chat_json_world_seed() {
        let mc = MockClient::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "Generate the starting scene for this WorldSeed."}
        ]);
        let out = mc
            .chat_json(&messages, &Value::Null, Some(true), "gm")
            .await
            .unwrap();
        assert!(out.contains_key("public_intro"));
        assert_eq!(
            out.get("hidden_truth"),
            Some(&Value::from(
                "The ship was hidden in a frozen cove by smugglers."
            ))
        );
        let npcs = out.get("npcs").and_then(|v| v.as_array()).unwrap();
        assert_eq!(npcs.len(), 2);
        assert_eq!(npcs[0].get("id"), Some(&Value::from("iva")));
    }

    #[tokio::test]
    async fn mock_chat_json_scene_delta_empty_moves() {
        let mc = MockClient::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "Report current-scene NPC roster changes as moves."}
        ]);
        let out = mc
            .chat_json(&messages, &Value::Null, Some(true), "gm")
            .await
            .unwrap();
        assert_eq!(out.get("moves"), Some(&Value::Array(Vec::new())));
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn mock_chat_json_npc_first_draft_impossible_claim() {
        let mc = MockClient::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "You are an NPC."},
            {"role": "user", "content": "Игрок обвиняет тебя."}
        ]);
        let out = mc
            .chat_json(&messages, &Value::Null, Some(false), "npc")
            .await
            .unwrap();
        assert_eq!(
            out.get("action").and_then(|v| v.as_str()),
            Some("пытается незаметно выскользнуть через заднюю дверь трактира")
        );
        let claims = out.get("claims").and_then(|v| v.as_array()).unwrap();
        assert_eq!(claims, &vec![Value::from("В трактире есть задняя дверь")]);
    }

    #[tokio::test]
    async fn mock_chat_json_npc_redo_after_correction() {
        let mc = MockClient::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "You are an NPC."},
            {"role": "user", "content": "Замечание ГМ: REDO this, no back door."}
        ]);
        let out = mc
            .chat_json(&messages, &Value::Null, Some(false), "npc")
            .await
            .unwrap();
        assert_eq!(
            out.get("speech").and_then(|v| v.as_str()),
            Some("Сейчас, дружище, эль принесу, обожди-ка.")
        );
        assert_eq!(out.get("claims"), Some(&Value::Array(Vec::new())));
    }

    #[tokio::test]
    async fn mock_determinism_same_input_same_output() {
        let mc1 = MockClient::new();
        let mc2 = MockClient::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "GM"},
            {"role": "user", "content": "test"}
        ]);
        let a = mc1.chat(&messages, None, Some(false), "gm").await.unwrap();
        let b = mc2.chat(&messages, None, Some(false), "gm").await.unwrap();
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn mock_set_model_and_list() {
        let mc = MockClient::new();
        assert_eq!(mc.model(), "mock");
        mc.set_model("  "); // empty after trim -> unchanged
        assert_eq!(mc.model(), "mock");
        mc.set_model(" custom ");
        assert_eq!(mc.model(), "custom");
        let models = mc.list_models().await;
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].get("id"), Some(&Value::from("custom")));
    }

    #[tokio::test]
    async fn mock_summarize_canned() {
        let mc = MockClient::new();
        let s = mc.summarize("anything", &[]).await.unwrap();
        assert_eq!(s, "(compressed summary of previous turns)");
    }

    #[tokio::test]
    async fn mock_chat_stream_emits_word_deltas_and_returns_final() {
        let mc = MockClient::new();
        let messages = serde_json::json!([
            {"role": "tool", "content": "a"},
            {"role": "tool", "content": "b"}
        ]);
        let deltas = std::sync::Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let d2 = deltas.clone();
        let mut sink = move |ch: &str, delta: &str| {
            d2.lock().unwrap().push((ch.to_string(), delta.to_string()));
        };
        let out = mc
            .chat_stream(&messages, None, Some(false), "gm", &mut sink)
            .await
            .unwrap();
        // final scene narration
        assert!(out.content.contains("Борин мнётся за стойкой"));
        let recorded = deltas.lock().unwrap().clone();
        // thinking words then content words, each suffixed with a space
        assert!(recorded.iter().any(|(c, _)| c == "thinking"));
        assert!(recorded.iter().any(|(c, _)| c == "content"));
        assert!(recorded.iter().all(|(_, d)| d.ends_with(' ')));
        // stats present
        assert_eq!(out.stats.get("eval_count"), Some(&Value::from(120)));
    }

    #[tokio::test]
    async fn mock_chat_json_stream_chunks_and_parses() {
        let mc = MockClient::new();
        let messages = serde_json::json!([
            {"role": "system", "content": "WorldSeed: build starting scene"}
        ]);
        let chunks = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let c2 = chunks.clone();
        let mut sink = move |_ch: &str, delta: &str| {
            c2.lock().unwrap().push(delta.to_string());
        };
        let out = mc
            .chat_json_stream(&messages, &Value::Null, Some(true), "gm", &mut sink)
            .await
            .unwrap();
        assert!(out.data.contains_key("public_intro"));
        let recorded = chunks.lock().unwrap().clone();
        // chunks of at most 6 chars each
        assert!(recorded.iter().all(|s| s.chars().count() <= 6));
        // reassembled equals the json.dumps-default string
        let joined: String = recorded.concat();
        let reparsed: Value = serde_json::from_str(&joined).unwrap();
        assert_eq!(reparsed.get("hidden_truth"), out.data.get("hidden_truth"));
    }

    #[test]
    fn insert_default_spaces_respects_strings() {
        // Commas/colons inside strings are NOT spaced.
        let compact = r#"{"a":"x,y:z","b":1}"#;
        assert_eq!(insert_default_spaces(compact), r#"{"a": "x,y:z", "b": 1}"#);
    }
}
