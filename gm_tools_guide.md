# GM tools guide

Эта заметка описывает, как устроены инструменты ГМ, как работает `tool_search`,
и что нужно менять при добавлении, изменении или удалении tools.

Главное правило: tool schema является ранним model-facing контрактом и должна
оставаться стабильной для prompt cache. Живое состояние мира передается поздно,
через current context и runtime-валидацию, а не через descriptions/enums.

## Где что лежит

| Зона | Файл | Ответственность |
|---|---|---|
| Tool schemas/catalog/search | `agents.py` | Описания tools, начальный набор, поиск tools, сборка списка visible tools |
| Tool routing policy | `prompts.py` | Когда ГМ обязан вызвать tool, когда сначала вызвать `tool_search` |
| Tool execution | `orchestrator.py` | Реальное выполнение tool-call, изменение `World`/`Session`, debug events |
| World/session mutators | `world.py`, `orchestrator.py` | Доменные операции, валидация id, repair hints |
| Persistence | `dialog_store.py` | Сохранение состояния, если новый tool меняет persisted fields |
| Contracts | `test_contracts.py` | Model-boundary контракты: schema, search, execution, cache-safety |

## Runtime flow

1. `Session.loaded_gm_tools` стартует с `agents.initial_gm_tool_names()`.
2. `agents.build_gm_tools_for_model(world, loaded_tool_names)` отдает модели
   только initial tools плюс уже найденные tools.
3. Если нужного tool нет в текущем списке, `prompts.GM_SYSTEM` требует сначала
   вызвать `tool_search`.
4. `orchestrator._run_tool(..., name="tool_search", ...)` вызывает
   `agents.search_gm_tools(...)` и добавляет найденные имена в
   `session.loaded_gm_tools`.
5. На следующем GM step найденный tool уже виден модели.
6. Когда модель вызывает обычный tool, `orchestrator._run_tool` выполняет
   соответствующую ветку и возвращает tool result в историю ГМ.

`loaded_gm_tools` сейчас хранится только в runtime `Session`. Он показывается в
`/debug`, но не сериализуется в SQLite; после восстановления сессии tools можно
переоткрыть через `tool_search`.

## Prompt-cache правила

Нельзя добавлять в tool schema то, что меняется по истории:

- текущий roster NPC;
- имена/роли/секреты NPC;
- `enum` по `npc_id`;
- `enum` по `location_id`;
- `enum` по item/exit ids;
- публичные факты, scene state, whereabouts.

Можно оставлять только статичные engine enums, которые не зависят от истории.
Текущий пример: `whereabouts.status = known | likely | rumored | unknown`.

Для живых сущностей используйте `type: "string"` и backend validation. Если id
не существует, tool должен вернуть управляемую ошибку или repair payload, а не
падать.

Живое состояние мира должно приходить поздно:

- GM: `_gm_turn_context(...)`;
- NPC: late `CURRENT NPC CARD`;
- scene/NPC perception: `world.scene_context()`, `world.npc_scene_slice(...)`,
  entity refs, whereabouts.

## Как добавить новый tool

1. Добавить schema в `agents.py`.
   - Формат: OpenAI-style wrapper `{"type": "function", "function": {...}}`.
   - `name` стабильный, snake_case.
   - `description` на английском, без живого roster/scene/facts.
   - Argument values, которые попадут в debug/player-facing text, просить писать
     по-русски.
   - `parameters.additionalProperties` ставить `False`.
   - Mutable ids делать строками, без dynamic enum.

2. Добавить tool в `build_gm_tools(world)`.
   - Catalog строится из этого списка.
   - Не завязывать schema на конкретный `world`, кроме статичных engine enums.

3. Решить видимость.
   - Если tool нужен почти каждый ход, добавить имя в `_INITIAL_GM_TOOL_NAMES`.
   - Если tool редкий, оставить discoverable через `tool_search`.
   - Начальный набор держать маленьким: он попадает в ранний request payload.

4. Добавить search hints в `_TOOL_SEARCH_HINTS`.
   - Включить русские и английские ключевые слова.
   - Добавить синонимы действия, а не имена конкретных NPC.
   - Exact load уже работает через `select:tool_name`.

5. Добавить routing rule в `prompts.py`, если tool должен менять поведение ГМ.
   - Правило должно говорить, когда tool обязателен.
   - Если tool может быть не виден, правило должно оставлять обязанность
     сначала вызвать `tool_search`, а не заменить tool narration-ом.

6. Добавить execution branch в `orchestrator._run_tool`.
   - Нормализовать `args`, валидировать ids через доменный слой.
   - Не раскрывать hidden truth/NPC secrets в GM tool results.
   - Для структурированного результата возвращать JSON через
     `json.dumps(..., ensure_ascii=False)`.
   - Для debug UI отдавать отдельный event, если действие должно быть видно.

7. Если tool делает видимое действие до финального narration, добавить имя в
   `_VISIBLE_PRELUDE_TOOLS`.
   - Туда подходят: dice, scene transition, NPC movement/reaction.
   - Обычно туда не нужно добавлять `tool_search` и чистые lookup tools.

8. Если tool меняет persisted state, обновить `dialog_store.py`.
   - Добавить round-trip тест.
   - Для старых snapshot-ов задать safe default без миграции, если возможно.

9. Добавить contract tests.
   - Tool присутствует в catalog.
   - Tool виден initial или находится через `search_gm_tools`.
   - `select:tool_name` работает.
   - Schema strict: `additionalProperties is False`.
   - Нет dynamic enum для живых ids.
   - Descriptions не содержат текущий roster/имена NPC.
   - Unknown ids обрабатываются управляемо.
   - Execution branch меняет ровно нужное состояние.

10. Обновить документацию.
    - Этот файл, если меняется общий workflow.
    - `README.md`, если появляется новая user-facing возможность.
    - `prompt_cache_architecture.md`, если меняется cache policy.

## Как удалить tool

1. Убрать schema из `build_gm_tools` или удалить константу schema.
2. Убрать execution branch из `orchestrator._run_tool`.
3. Убрать имя из `_INITIAL_GM_TOOL_NAMES`.
4. Убрать `_TOOL_SEARCH_HINTS` для этого имени.
5. Убрать routing rule из `prompts.py`.
6. Убрать имя из `_VISIBLE_PRELUDE_TOOLS`, если оно там было.
7. Обновить tests, которые ждут наличие tool или search result.
8. Проверить old sessions/debug:
   - `loaded_gm_tools` может содержать старое имя в live process, но
     `build_gm_tools_for_model` отфильтрует его, если tool больше нет в catalog.
   - Старые tool-call сообщения в `gm_messages` остаются историей; удалять их не
     нужно без отдельной миграции.

## Как изменить existing tool

- Если меняется только описание без поведения, все равно прогнать
  `test_contracts.py`: descriptions участвуют в model boundary и search text.
- Если меняется schema, обновить execution branch и tests одновременно.
- Если добавляется новый required argument, проверить старые prompts и retry/error
  поведение: модель может какое-то время вызывать старую форму из истории.
- Если меняется meaning аргумента, лучше добавить новый аргумент и поддержать
  старый fallback, чем молча переиспользовать имя с другой семантикой.

## Search behavior

`tool_search` поддерживает два режима:

- keyword search: `tool_search({"query": "перейти новая сцена"})`;
- exact load: `tool_search({"query": "select:move_npc,set_npc_whereabouts"})`.

Search text строится из:

- имени tool;
- имени с пробелами вместо `_`;
- description;
- parameter descriptions/enums;
- `_TOOL_SEARCH_HINTS`.

Если новый tool плохо находится, сначала поправить `_TOOL_SEARCH_HINTS`, затем
добавить тест на `search_gm_tools(...)`.

## Verification checklist

Минимум после изменения tools:

```powershell
python -m py_compile world.py server.py agents.py prompts.py orchestrator.py dialog_store.py
python test_contracts.py
python test_dialog_store.py
```

Если менялся frontend/debug UI:

```powershell
npm --prefix web run build
```

Если менялся HTTP handler или debug mutation path, дополнительно сделать smoke
через `server.py` на отдельном порту и проверить реальный endpoint.
