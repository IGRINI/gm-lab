# TZ: миры и истории как переносимые пакеты («моды»)

## Статус

- [x] Зафиксировать модель пакетов и раскладку на диске. *(ТЗ ниже.)*
- [x] Перевести миры с таблицы `worlds` на файловые пакеты (источник истины — папки). *(Фаза 1: WorldStore в gml-persistence, `library/worlds/<id>/world.json`, миграция из SQLite, контракт `/worlds` byte-identical, workspace green.)*
- [x] Копировать картинки внутрь пакета мира и отдавать их статик-роутом независимо от `image_enabled`. *(Фаза 2: ingest при сохранении в `assets/`, роут `/world-assets/{id}/{file}` без гейта, read-time rewrite относительный→servable, no-fallback 502, тесты зелёные.)*
- [x] Перевести истории с `include_str!(catalog.json)` на рантайм-скан пакетов; 3 встроенные истории едут дефолтными пакетами. *(Фаза 3: StoryStore в gml-stories, `library/stories/<id>/story.json`, дефолты материализуются из embedded catalog.json (больше не живой путь чтения), порядок `/stories` сохранён через builtin-order, drop-in работает, workspace green. Для ревью: при незаданном `GM_PACKAGES_DIR` глобальный `DEFAULT_STORE` материализует дефолты в реальный `library` — тесты негерметичны без env; захардить в фазе клинапа.)*
- [x] Связать историю с миром (`world_ref`) + поддержать «запечённый» self-contained вариант. *(Фаза 4 backend: StoryStore хранит/отдаёт `kind`+`world_ref`; `POST /stories` создаёт историю, привязанную к миру (валидация существования world_id, no-fallback); self-contained встроенные истории остаются `from_seed`.)*
- [x] Дописать запуск сохранённого мира/истории в игровую сессию. *(Фаза 4 backend: `/chats` принимает `world_id` (играть сохранённый мир процедурно, world_id > inline world_lore); запуск истории — procedural=worldgen(lore)+оверлей, authored=`World::compose_authored` (worldgen(lore) + наложение плота: PC/hidden_truth/scene/npcs/facts, авторская сцена апсертится в канон через set_scene); провенанс `world_ref`/`story_ref` пишется в payload сейва (трейлинг-ключи, byte-identical для старых сейвов). Фронт (web/) — следующий шаг.)*
- [x] Миграция существующих миров из SQLite в папки без потери данных. *(Фаза 1: WorldStore::migrate_from_sqlite — единственный читатель старой таблицы, идемпотентно при старте.)*
- [x] Обновить тесты/контракты под новый формат. *(Фазы 1–4: WorldStore/StoryStore unit-тесты, обновлённый contract.rs (37), golden `/stories` через builtin-order, golden payload roundtrip зелёный.)*
- [x] Фаза 5 — UX шаринга: открыть папку библиотеки, экспорт пакета в zip (+«запечь мир»), импорт zip. *(zip-крейт deflate-only; `POST /library/reveal`, `GET /worlds|stories/{id}/export` (story `?bake=1` запекает мир под `world/`), `POST /library/import?overwrite=1` со staging+atomic swap (zip-slip guard, no-fallback); фронт: кнопки экспорт/импорт/открыть-папку; тесты зелёные.)*

Продолжает `docs/WORLD_CREATION_TAB_TZ.md` (раздел «Что не делаем сейчас»: создание истории из мира, выбор мира при создании истории). Здесь это вводится в скоуп вместе с переносом хранения на файлы.

## Главная идея

Мир и история должны быть **переносимыми артефактами**, которые можно отдать другому человеку, как мод для игры (Minecraft / Project Zomboid). Папку кинул — оно подхватилось.

Три типа артефактов:

| Тип | Аналог | Что внутри | Где живёт | Шарится |
|---|---|---|---|---|
| **World** (мир) | мод / датапак | world bible (`WorldLore`) + картинки | файловый пакет | да |
| **Story** (история) | сценарий-мод, зависит от мира | плот-оверлей (роль игрока, hidden_truth, стартовая сцена) + `world_ref` + свои картинки | файловый пакет | да, вместе с миром |
| **Save** (прохождение) | сейв | живой `WorldCanon` + транскрипт + состояние | SQLite (`dialog_chats`) | опционально (экспорт позже) |

Принцип разделения «мод против сейва» (как в играх): **миры и истории — это контент (моды)**, который шарят; **прохождение — это личный сейв**, который остаётся в БД ради надёжных частых записей хода.

## Зафиксированные решения

1. **Расположение**: библиотека внутри текущей папки данных приложения — `<data_dir>/library/`, где `data_dir` = `directories::ProjectDirs("gm-lab").data_dir()` (на Windows `%APPDATA%\Roaming\gm-lab\data`). Портативный режим «рядом с .exe» — НЕ сейчас (отдельная задача; инвариант README «ничего рядом с бинарём» пока не трогаем). Переопределение пути — env `GM_PACKAGES_DIR`, по аналогии с `GM_DIALOG_DB`/`GM_RAG_CACHE_PATH`.
2. **Источник истины**: для миров и историй — файловые пакеты (папки), сканируются при старте. Для прохождений — SQLite остаётся как есть.
3. **Связь история↔мир**: по умолчанию ссылка `world_ref` (нужны обе папки, как зависимость мода). При экспорте — опция «запечь мир внутрь» для самодостаточного бандла. Это же позволяет 3 текущие самодостаточные истории ехать как пакеты с запечённым миром.

## Раскладка на диске

```
%APPDATA%\Roaming\gm-lab\data\          ← существующий data_dir (тут уже лежит gm_lab_dialogs.sqlite3, .tls)
├─ gm_lab_dialogs.sqlite3               ← БЕЗ изменений: чаты/сейвы + guest_dialog_state
├─ gm_lab_embeddings.sqlite3            ← БЕЗ изменений (глобальный RAG-кэш; пер-мировый — будущее)
└─ library/                             ← НОВОЕ; override: GM_PACKAGES_DIR
   ├─ worlds/
   │  └─ <world_id>/                    ← имя папки = world_id (urlsafe, стабилен)
   │     ├─ world.json                  ← манифест + world bible
   │     ├─ architect.json              ← история чата архитектора + cache id (для доредактирования)
   │     └─ assets/
   │        ├─ cover.png                ← бывш. world_image_url
   │        └─ map.png                  ← бывш. world_map_url
   └─ stories/
      └─ <story_id>/
         ├─ story.json                  ← манифест: world_ref + плот-оверлей
         ├─ assets/ …
         └─ world/                      ← ОПЦИОНАЛЬНО: запечённая копия мира (self-contained вариант)
```

## Форматы файлов

### `world.json`
```json
{
  "format": "gmlab.world/1",
  "id": "porog-vtorogo-neba",
  "version": 3,
  "status": "ready",
  "title": "Порог Второго Неба",
  "genre": "тёмный иссекай",
  "tone": "…",
  "world_size": "…",
  "population": "…",
  "lore": { "…весь WorldLore из crates/gml-world/src/canon/lore.rs…" },
  "assets": { "cover": "assets/cover.png", "map": "assets/map.png" },
  "created_at": "…", "updated_at": "…"
}
```
- `lore` — ровно структура `WorldLore`. Поля `world_image_url`/`world_map_url` теперь хранят **относительный путь внутри пакета** (`assets/cover.png`), а не volatile `/image-files/<run_id>/…`.
- `version` (int) растёт на каждое сохранение — для `world_ref` историй и для «мир обновился».
- `architect.json` вынесен отдельно (большой, нужен только в студии): `architect_messages`, `architect_model_history`, `architect_cache_session_id/thread_id`.

### `story.json`
```json
{
  "format": "gmlab.story/1",
  "id": "derevnya-u-zhivoy-dorogi",
  "version": 1,
  "kind": "authored",
  "world_ref": { "id": "porog-vtorogo-neba", "version": ">=3" },
  "world_embedded": false,
  "title": "Деревня у живой дороги",
  "plot": {
    "player_character": { "…" },
    "hidden_truth": "…",
    "scene": { "…старт…" },
    "story_brief": "…",
    "public_intro": "…",
    "proper_nouns": [], "public_facts": [], "npcs": [], "state_records": [], "time": 480
  }
}
```
- `kind`: `"authored"` (рукописный плот) | `"procedural"` (генерится из мира на лету; тогда `plot` минимален).
- `world_embedded: true` + папка `world/` → self-contained бандл (опция «запечь мир»).
- `plot` — это поля авторского сида из текущего `catalog.json` **минус** world-bible часть (она приходит из мира по `world_ref`). Для самодостаточных легаси-историй world-bible едет в `world/` (запечён).

## Соответствие текущему коду (ложится 1:1)

- `world.json.lore` ← `WorldLore` (`crates/gml-world/src/canon/lore.rs`), без изменения структуры.
- `story.json.plot` ← поля сида из `crates/gml-stories/src/catalog.json` (`player_character`, `hidden_truth`, `scene`, …).
- Процедурный запуск уже умеет «мир + оверлей»: `World::from_worldgen_with_lore` + наложение `story_*` из тела запроса (`crates/gml-server/src/lib.rs:1583`). Запуск истории = `load(world.json.lore)` + наложение `story.json.plot`.
- Контракт `/worlds` (payload-форма) сохраняется → фронт не меняется на фазе 1.

## План по фазам

### Фаза 1 — миры как пакеты (без смены API и фронта)
- Ввести абстракцию `WorldStore` (trait) с **файловой** реализацией: scan `library/worlds/`, load/save `world.json` (+ `architect.json`), атомарная запись (temp + rename).
- Переключить хендлеры `/worlds` (`post_create_world`/`post_update_world`/`list_worlds`/`delete_world`, `crates/gml-server/src/lib.rs`) с таблицы `worlds` на `WorldStore`. Форма payload — прежняя.
- **Миграция**: одноразовый импортер читает строки таблицы `worlds` из `gm_lab_dialogs.sqlite3` и пишет их пакетами в `library/worlds/`. Таблицу `worlds` после этого можно пометить deprecated (не удалять сразу).
- Снять guest-скоупинг с миров (это шарящийся контент, не per-guest); сейвы guest-скоуп сохраняют.

### Фаза 2 — картинки внутри пакета (чинит эфемерность)
- На сохранении мира / приёме сгенерированной картинки: сервер тянет байты из сайдкара (`/image-files/<run_id>/<file>`) и пишет в `library/worlds/<id>/assets/`, переписывает url в лоре на относительный путь.
- Новый статик-роут `GET /world-assets/<world_id>/<file>` (и `/story-assets/<id>/<file>`), отдаёт из папки пакета, **не зависит от** `image_enabled` и от живости сайдкара.
- Фронт: `<img src>` переключается на новый роут (`ImagePreview.jsx`, `WorldArchitectPanel.jsx`).

### Фаза 3 — истории как пакеты
- Заменить `include_str!(catalog.json)` на рантайм-скан `library/stories/` в `crates/gml-stories`.
- 3 встроенные истории шипятся как дефолтные пакеты: при первом старте, если `library/stories/` пуст — распаковать их (с запечённым `world/`, т.к. они самодостаточны).
- Обновить байт-точные тесты каталога (`gml-stories/src/lib.rs` count==3 / id-set / byte-length) под новый источник.

### Фаза 4 — запуск контента в игру (слой из «будущего» в WORLD_CREATION_TAB_TZ)
- «Играть мир» (процедурно): из пакета мира → `world_lore` → существующий процедурный путь `/chats`.
- «Играть историю»: `story.json.plot` + резолв `world_ref` (или запечённый `world/`) → собрать `World` (`from_seed`/`from_worldgen` + оверлей).
- Выбор мира при создании авторской истории (UI следующего шага, отложенный в прошлом ТЗ).
- В payload сейва писать `world_ref`/`story_ref` — чтобы прохождение было воспроизводимо и связано с пакетом.

### Фаза 5 — UX шаринга
- Кнопка «Открыть папку библиотеки».
- Экспорт пакета в zip; для истории — чекбокс «запечь мир внутрь» (`world_embedded`).
- Импорт: бросить папку/zip в `library/` → подхват при следующем скане (или watcher).

## Acceptance criteria

- Миры хранятся как папки в `library/worlds/`, читаются/пишутся как файлы; таблица `worlds` больше не источник истины.
- Существующие миры из SQLite мигрированы в папки без потери полей, истории архитектора и cache id.
- Картинки мира лежат внутри пакета и открываются даже при выключенной генерации картинок и при выключенном сайдкаре.
- Истории читаются из `library/stories/` в рантайме; новую историю можно добавить, бросив папку, без перекомпиляции.
- История ссылается на мир по `world_ref`; при экспорте можно получить самодостаточный бандл с запечённым миром.
- Можно запустить сохранённый мир (процедурно) и сохранённую историю в игровую сессию.
- Прохождения остаются в `gm_lab_dialogs.sqlite3`; частые записи хода не деградируют.
- Контракт `/worlds` и фронт не сломаны на фазе 1.
- Rust tests/clippy и сборка фронта проходят для затронутых пакетов.

## Что НЕ делаем сейчас

- Портативный режим «рядом с .exe» и выбор произвольной папки-библиотеки (отдельная задача; сейчас фиксируем AppData).
- Пер-мировый RAG-индекс (сейчас `gm_lab_embeddings.sqlite3` глобальный).
- Экспорт прохождений (сейвов) в папки как первичный формат.
- Общий таймлайн мира между историями (как и в прошлом ТЗ).
- Файловый watcher горячей перезагрузки (достаточно скана при старте + ручного refresh).
