# Prompt cache architecture for GM-Lab

Дата: 2026-06-19

Документ фиксирует, как надо перестроить контекст GM-Lab, чтобы правки мира и
карточек NPC не уничтожали кеш сильнее, чем необходимо.

## Короткий вердикт

Волшебного способа "изменить карточку NPC и сохранить тот же кеш полностью" нет.
Кеш у современных LLM работает по общему правилу:

1. Совпадает начало промпта - кеш живет.
2. Изменилось что-то в начале - все после этого пересчитывается.
3. Чем ближе изменяемые данные к концу запроса, тем меньше хвост, который надо
   пересчитать.

Для GM-Lab это значит:

- статичные правила надо держать в начале;
- историю надо вести append-only, без переписывания старых сообщений;
- текущую карточку NPC, текущий ростер, публичные факты и сцену надо класть как
  поздний динамический блок;
- сброс памяти NPC должен быть отдельной ручной кнопкой, а не автоматикой.

## Источники

- OpenAI prompt caching:
  https://developers.openai.com/api/docs/guides/prompt-caching
  - cache hits возможны только при exact prefix match;
  - static content лучше держать в начале;
  - variable content лучше держать в конце;
  - tools/images тоже должны совпадать между запросами.

- Anthropic prompt caching:
  https://platform.claude.com/docs/en/build-with-claude/prompt-caching
  - порядок кешируемого префикса: tools -> system -> messages;
  - изменение tools бьет system/messages;
  - изменение system бьет messages;
  - static content кладется в начало, cache breakpoint ставится на стабильный
    блок.

- Gemini context caching:
  https://ai.google.dev/gemini-api/docs/caching
  - cached content является prefix к prompt;
  - для implicit caching повышает шанс совпадения общий большой контент в
    начале запроса;
  - dynamic/current content должен оставаться ближе к хвосту.

- TensorRT-LLM KV cache reuse:
  https://nvidia.github.io/TensorRT-LLM/advanced/kv-cache-reuse.html
  - KV cache reuse работает для запросов, которые начинаются с одинакового
    prompt;
  - цель - уменьшить first token latency;
  - reuse не является способом бесплатно менять ранние токены.

- Don't Break the Cache:
  https://arxiv.org/abs/2601.06007
  - свежая работа по prompt caching в long-horizon agentic tasks;
  - общий вывод: стратегическое управление cache boundary и вынос динамики из
    кешируемого префикса стабильнее, чем наивное кеширование всего подряд.

## Что сейчас в GM-Lab

### NPC

Сейчас `agents.py::npc_system_message` подставляет карточку NPC прямо в первый
`system`-блок через `prompts.py::NPC_SYSTEM_TEMPLATE`.

Туда попадают:

- `name`
- `persona`
- `voice`
- `goals`
- `knowledge`
- `secret`
- `pronouns`/род

Проблема: если в debug-панели поменять `persona`, `voice`, `goals`, `knowledge`
или `secret`, меняется начало NPC-промпта. Значит большой накопленный NPC
context перестает нормально переиспользоваться.

Еще хуже: `server.py` на `/debug/npc` сейчас меняет саму карточку, но не чистит:

- `session.npc_messages[npc_id]`
- `session.npc_summaries[npc_id]`
- `session.npc_client_state[npc_id]`

То есть новая карточка начинает конфликтовать со старой личной историей NPC.
Для легкой правки это нормально, если правильно расставить приоритеты. Для
полной смены личности нужен ручной сброс памяти NPC.

### GM

У GM уже есть правильная часть: текущая сцена и последний player action идут в
позднем user-turn через `_gm_turn_context`.

Но есть два слабых места:

1. `_gm_world_setup` все еще лежит рано и содержит изменяемые между ходами
   данные:
   - named NPC roster;
   - public facts.

   `public intro` здесь не проблема: сейчас оно задается при создании мира
   (`World.__init__`, `/new`, загрузка снапшота) и не редактируется in-place
   между ходами. Его выгоднее оставить в раннем стабильном префиксе вместе с
   правилами GM.

2. `build_gm_tools` зашивает текущий мир в tool definitions:
   - prose `Available NPCs: ...`;
   - `enum` из `npc_id`;
   - `enum` из `npc_id` внутри `set_scene.present_npcs`.

Значит переименование NPC, смена роли, смена рода, добавление/удаление NPC или
правка public facts может сломать кеш GM не только через system setup, но и
через tools. Если NPC и локации являются живой частью истории, schema не должна
пытаться перечислять их как статичный контракт.

### Что еще может меняться по ходу истории

Проверка кода показала, что живой мир шире, чем только NPC card:

| Сущность | Где сейчас живет | Сейчас в prompt/cache | Правильное место |
| --- | --- | --- | --- |
| `world.public` / public intro | `World.public`, seed, `/new`, snapshot | ранний `_gm_world_setup` | можно оставить рано только как premise кампании; смена = новый мир/cache boundary |
| public facts | `world.fact_records(kind="public")`, `/debug/fact` | сейчас ранний `_gm_world_setup`, также NPC slice/RAG | поздний current context или RAG/tool result; edit = cache/version bump |
| NPC roster | `world.npcs` | сейчас ранний setup и tool schema | поздний roster/context; backend validation |
| NPC card | `NPC.persona/voice/goals/knowledge/secret` | сейчас NPC `system` | поздний `CURRENT NPC CARD`; optional reset памяти |
| current location/scene | `SceneState.title/location_id/description` | уже в основном поздний `_gm_turn_context` | оставить поздно |
| presence/whereabouts | `scene.present_npcs`, `presence`, `npc_whereabouts` | поздний scene/entity context, но ids есть в tool enum | оставить поздно; убрать enum; валидировать runtime |
| items/exits/constraints/tension | `SceneState.items/exits/constraints/tension` | поздний scene/NPC slice | оставить поздно |
| entity refs | `entity_reference_context()` | поздний current context | оставить поздно |
| GM summary/history | `session.gm_messages`, `gm_summary` | append-only, summary после compact | compact = осознанный cache boundary |
| loaded tool set | `session.loaded_gm_tools` | меняет tool payload | лучше стабильный набор tools или cache key/schema version |
| model/settings/thread ids | runtime settings/client state | меняет request shape/cache key | считать cache boundary |

Отдельная находка: `hidden_events` сейчас не попадают в обычный prompt, но
`get_world_fact` fallback может вернуть recent hidden events как tool result.
Это не ломает ранний cache prefix, но является риском утечки скрытой правды в
публичный GM lookup. Его надо сделать actor-safe или убрать из public fallback.

## Целевая схема NPC

NPC system prompt должен стать полностью статичным.

В нем остаются:

- правила отыгрыша;
- запрет быть GM;
- правила знания и секретов;
- правила реакции на давление;
- правила громкости речи;
- формат JSON;
- правила Markdown/entity refs;
- описание поля gender marker как `M/F/N/PL/OTHER`.

В нем не должно быть конкретной карточки Борина, Лизы, Марет или любого другого
персонажа.

### Новый порядок сообщений NPC

```text
system:
  Static NPC roleplay rules.
  Static JSON contract.
  Static instruction:
  "Your current character is defined by the latest CURRENT NPC CARD block.
   If older memory conflicts with CURRENT NPC CARD, follow CURRENT NPC CARD."

system or user:
  YOUR PRIVATE MEMORY SO FAR
  (compact summary, if exists)

assistant/user history:
  append-only personal NPC history

user:
  CURRENT NPC CARD, revision N
  name: ...
  role: ...
  gender: ...
  persona: ...
  voice: ...
  goals: ...
  knowledge: ...
  secret: ...
  This card overrides older memory if there is a conflict.

  CURRENT SITUATION
  SCENE SLICE
  VISIBLE LIMITS
  YOUR MEMORY / WHAT YOU SAW
```

Смысл: большая старая часть остается стабильной, а при правке карточки
пересчитывается только поздний хвост: новая карточка плюс текущая ситуация.

### Почему карточка после истории

Если положить карточку до истории, то правка карточки ломает кеш всей истории.
Если положить карточку после истории, то история остается cacheable prefix, а
новая карточка становится актуальным override.

Это не значит, что модель "забудет" старую историю. История все еще в контексте.
Но если в старой истории был старый характер, а карточку изменили, статичное
правило должно явно сказать: новая карточка главнее.

## Редактирование NPC

### Легкая правка карточки

Примеры:

- поправить стиль речи;
- уточнить цель;
- добавить знание;
- переименовать;
- поменять род.

Поведение:

- память NPC не сбрасывается автоматически;
- `npc.card_revision` увеличивается;
- следующий вызов NPC получает новую карточку поздним блоком;
- старая история остается, но новая карточка объявлена более приоритетной.

### Опасная правка секрета

Смена `secret` не равна обычной правке стиля речи.

Секрет влияет на:

- что NPC уже мог раскрыть;
- какие `claims` он оставил в истории;
- почему он лгал или боялся;
- что попало в `npc_summary`;
- какие внутренние причины модель уже видела в прошлых ходах.

Если просто заменить `secret`, старая история может начать спорить с новой
карточкой. Модель способна смешать старый и новый секрет или продолжить
защищать уже неактуальную версию правды.

Правильное поведение UI:

- при изменении `secret` явно подсветить, что это опасная правка;
- предложить "Сбросить память NPC";
- не сбрасывать автоматически, чтобы пользователь сам выбрал цену;
- если reset не выбран, новая карточка все равно главнее старой памяти, но в
  интерфейсе надо понимать риск stale-следов.

### Жесткая смена персонажа

Примеры:

- Борин больше не Борин, а фактически другой персонаж;
- переписана вся личность;
- старые ответы больше нельзя считать валидной памятью;
- надо начать NPC-сессию заново.

Поведение:

- пользователь явно нажимает "Сбросить память NPC";
- очищаются `npc_messages[npc_id]` и `npc_summaries[npc_id]`;
- сбрасывается/ротируется `npc_client_state[npc_id]`, чтобы Codex/OAuth thread
  и `prompt_cache_key` не продолжали тащить старую сессию;
- это сознательно убивает кеш и историю только этого NPC.

Автоматически сбрасывать память нельзя: иначе обычная мелкая правка будет
уничтожать полезную историю и кеш без разрешения пользователя.

## `card_revision`

`card_revision` нужен не для магического кеша.

Он нужен для трех вещей:

1. Человеку видно, что карточка менялась.
2. Модели проще понять, что текущая карточка свежее старой памяти.
3. В debug UI можно показывать, на какой версии карточки NPC отвечал.

Для кеша сам номер ревизии не спасает: любой измененный байт в позднем блоке
все равно пересчитывается. Но это маленький хвост, а не вся история.

Важный момент: если не хотим миграцию, поле должно иметь дефолт при загрузке
старых снапшотов. Старые NPC без `card_revision` считаются `0`.

Если локальную SQLite БД разрешено полностью стереть и пересоздать, это еще
проще: можно не держать сложную backward-compatible миграцию для старых
снапшотов. Но архитектурно дефолт `0` все равно полезен как простой KISS-защитный
слой для импортов/старых export JSON.

## Целевая схема GM

GM system prompt должен содержать правила ведения игры и стабильный public
intro мира, но не текущие изменяемые между ходами данные.

В раннем stable prefix можно оставить:

- роль GM;
- public intro/current world premise, если он меняется только при полном
  пересоздании мира;
- правила D&D/RP;
- правила tool routing;
- правила dice/fact-checking;
- правила narration/Markdown/entity refs;
- запрет раскрывать скрытую механику.

В поздний current turn context надо перенести:

- current named NPC roster;
- current public facts;
- current scene state;
- constraints;
- entity reference markup;
- player action.

### Новый порядок сообщений GM

```text
system:
  Static GM rules.
  Stable public intro / world premise.

system:
  STORY SO FAR
  (compact, only after compact; known intentional cache break)

messages:
  append-only GM history

user:
  CURRENT TURN CONTEXT
  named NPC roster
  public facts
  current scene
  constraints
  entity refs
  latest player action
```

## Tools

Tools тоже участвуют в кешируемом префиксе у провайдеров. Поэтому динамический
текст и динамические schema constraints в tool definitions вредны.

Что стоит убрать:

- prose `Available NPCs: ...` из descriptions;
- роли/род/человекочитаемый ростер из tool descriptions;
- `enum` из `npc_id`;
- `enum` из `set_scene.present_npcs`;
- любые будущие `enum` по живым location/item ids.

Почему enum здесь плохой: NPC, локации, присутствие, выходы и предметы могут
меняться по истории. Tools стоят в начале request payload, часто даже раньше
system/messages. Значит любое добавление/удаление NPC или попытка перечислить
локации в schema будет ломать кеш в самом дорогом месте.

Правильный контракт tools:

- `npc_id`: `string`;
- `location_id`: `string`;
- `present_npcs`: `string[]`;
- `items/exits`: обычные object/string fields без перечисления живого мира;
- статичные enums оставлять только для закрытых engine-типов, например
  `whereabouts.status = known|likely|rumored|unknown`.

Актуальные NPC, локации, выходы и видимые предметы должны приходить поздно в
`CURRENT TURN CONTEXT`. Backend обязан валидировать ids при исполнении tool call.
Если модель дала несуществующий id, tool должен вернуть управляемую ошибку или
repair-подсказку, а не падать и не закреплять мусорное состояние.

## Как это влияет на кеш

| Действие | Сейчас | После перестройки |
| --- | --- | --- |
| Обычный ход игрока | GM history cache в целом ок, scene в хвосте | Ок |
| NPC отвечает без правки карточки | Ок, если история растет append-only | Ок |
| Мелкая правка NPC card | Ломает ранний NPC system prefix | Ломает только поздний card/current tail |
| Смена `secret` NPC | Может конфликтовать со старой history/summary | Подсветить как опасную правку; предложить ручной reset |
| Жесткая смена NPC | Старые history/summary могут конфликтовать | Ручной reset чистит только этого NPC |
| Public intro | Уже стабилен между ходами | Оставить в early stable setup |
| Добавить public fact | Может менять ранний `_gm_world_setup` | Факт идет в late current context |
| Переименовать NPC | Бьет GM setup/tools и NPC system | Бьет позднюю NPC card/current context; tool schema не меняется |
| Добавить/удалить NPC | Бьет GM setup/tools enum | Меняет поздний roster/current context; backend validation ловит старые ids |
| Смена локации/выходов/предметов | Сейчас mostly late context, но нельзя добавлять enum | Остается late context; schema остается статичной |
| Compact GM | Намеренно ломает часть префикса | Намеренно, это нормальная цена compact |

## План внедрения

### P1. NPC prompt split

1. Разбить `NPC_SYSTEM_TEMPLATE`:
   - `NPC_SYSTEM_STATIC` без конкретных полей персонажа;
   - отдельный builder для `CURRENT NPC CARD`.
2. `npc_system_message(npc)` заменить на статичное сообщение без `npc`.
3. `npc_user_message(...)` или `npc_request_messages(...)` должен добавлять
   `CURRENT NPC CARD` поздно, после summary/history.
4. Добавить правило override:
   - current card главнее старой памяти при конфликте;
   - но старая память сохраняется как история событий, если она не конфликтует.

### P2. Debug reset NPC memory

1. Добавить в `/debug/npc` параметр `reset_memory`.
2. Если он true:
   - удалить `session.npc_messages[npc_id]`;
   - удалить `session.npc_summaries[npc_id]`;
   - удалить `session.npc_client_state[npc_id]`;
   - удалить live `session.npc_clients[npc_id]`, если он создан.
3. В UI добавить явный checkbox/button "Сбросить память NPC".
4. Не включать reset по умолчанию.

### P3. GM mutable context tail

1. Убрать roster/public facts из раннего `_gm_world_setup`.
2. Добавить их в `_gm_turn_context`.
3. `public intro` оставить в раннем стабильном setup, потому что он меняется
   только при полном пересоздании мира.
4. Оставить ранний GM system максимально статичным.
5. Проверить, что hidden truth и NPC secrets не попадают в GM public context.

### P4. Tool definitions cleanup

1. Убрать `Available NPCs: ...` из descriptions.
2. Убрать `enum` из `npc_id` во всех tools.
3. Убрать `enum` из `set_scene.present_npcs`.
4. Не добавлять enum по `location_id`, item ids, exit ids или другим живым
   сущностям мира.
5. Оставить только статичные engine-enums, которые не зависят от истории,
   например `whereabouts.status`.
6. В tool descriptions объяснять поведение инструмента, а не текущий мир.
7. Валидировать все живые ids на backend и возвращать управляемый repair/error.

### P5. Optional card revision

1. Добавить `card_revision: int = 0` в NPC.
2. Увеличивать при изменении содержательных card fields.
3. Не делать отдельную SQLite migration.
4. При загрузке старого снапшота считать отсутствующее поле нулем.

## Тесты и проверки

Минимальные контрактные проверки:

1. NPC static system не содержит конкретные `persona`, `knowledge`, `secret`.
2. NPC request содержит `CURRENT NPC CARD`.
3. `CURRENT NPC CARD` находится после summary/history.
4. GM early setup содержит stable public intro, но не содержит текущий
   roster/public facts.
5. GM current turn context содержит roster/public facts.
6. Tool descriptions не содержат человекочитаемый `Available NPCs`.
7. Tool schemas не содержат dynamic enum по `npc_id` или `present_npcs`.
8. Tool schemas не содержат dynamic enum по локациям/предметам/выходам.
9. Invalid `npc_id`/location ids обрабатываются backend validation без падения.
10. NPC secrets не попадают в GM context, `/state` и RAG public corpus.
11. `get_world_fact` не раскрывает `hidden_events` через public fallback.
12. `/debug/npc` без reset сохраняет историю NPC.
13. `/debug/npc` с reset чистит только выбранного NPC.

Команды проверки:

```powershell
python -m py_compile world.py server.py agents.py prompts.py orchestrator.py
python test_contracts.py
npm --prefix web run build
```

## Что не делать

- Не пытаться сохранять полный кеш после изменения раннего system/tools: так
  cache/KV reuse не работает.
- Не делать автоматический reset NPC при каждой правке карточки.
- Не переносить NPC secrets в GM context ради удобства.
- Не добавлять `enum` по NPC, локациям, предметам, выходам или другим живым
  сущностям мира в tool schema.
- Не делать миграцию SQLite только ради `card_revision`; дефолта достаточно.

## Итоговая схема простыми словами

Статичные правила лежат в начале и кешируются.

История растет только добавлением новых сообщений и тоже нормально кешируется.

Все, что может часто меняться, кладется ближе к концу: текущая карточка NPC,
сцена, ростер, факты, действие игрока.

Tool schema остается статичной: без enum по NPC, локациям, предметам и выходам.
Актуальный мир передается поздним context, а живые ids проверяются backend-ом.

Если поменяли карточку NPC, модель видит новую карточку как главную, но старая
память остается. Если нужно полностью заменить персонажа, пользователь вручную
сбрасывает память именно этого NPC.

Так кеш не становится идеальным, но перестает ломаться "по пизде" от каждой
обычной правки карточки или текущего состояния.
