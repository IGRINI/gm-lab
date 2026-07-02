# ТЗ: Персонажи-пакеты и сюжетный архитектор

Модель: Мир (библия) → Истории/сюжеты (много на мир) → Сейвы (много на историю).
Персонажи (ГГ) — ортогональная сущность-пакет: выбираются при создании сейва,
живут снапшотом внутри сейва, явно экспортируются обратно в библиотеку.
Порядок фаз: К1 (персонажи) → К2 (качество карточки) → С1 (сюжетный архитектор).

## 0. Зафиксированные инварианты (проверено по коду и панелью)

- `PlayerCharacter` уже существует (`gml-world/src/model.rs:114`, 26 полей) и уже
  персистится в payload сейва (`player_character_to_payload`). Карточка целиком уходит
  в промпт GM; NPC её не видят.
- Дайс-движок карточку НЕ читает: модификаторы вбивает модель в нотацию — это
  закреплённый промпт-контракт (`tools.rs:196-264`), менять его в этом ТЗ НЕЛЬЗЯ
  (двойной счёт + кэш-префиксные фикстуры).
- Процедурный путь запуска сегодня не имеет хука ГГ вообще (всегда дефолтный «Искатель»).
- `StoryEnvelope` ПЕРЕСОБИРАЕТ story.json из фиксированного списка ключей
  (`story_store.rs:566`) — незнакомые топ-ключи теряются при записи. Любое новое
  состояние历 должно жить в явном round-trip объекте envelope.
- Байт-идентичность старых сейвов: новые payload-ключи — только трейлинг + только
  когда Some (паттерн world_ref, гейт `package_ref_tests`).

## Фаза К1 — персонажи как пакеты

### К1.1 CharacterStore (gml-persistence, рядом с WorldStore)
- Форма — как StoryStore: in-memory кэш + `reload()` после импорта, write-Mutex,
  общий рут `GM_PACKAGES_DIR`/`default_library_dir()`, каталог
  `library/characters/<id>/character.json`. БЕЗ `ensure_defaults` и воскрешения:
  удалённый персонаж остаётся удалённым.
- Манифест: `{format:"gmlab.character/1", id, version:u64, title, preview,
  created_at, updated_at, payload}`; `payload` — непрозрачный round-trip объект
  (как у WorldEnvelope), внутри `payload.player_character`.
- Канонический сериализатор ГГ ОДИН: форма `player_character_to_payload`
  (session payload) используется и для пакета. `player_character_export` —
  UI/tool-проекция, для пакета НЕ применяется.
- Методы: `list/get/create/delete` + ДВА апдейта:
  - `update_metadata(patch)` — shallow-merge топ-уровня (title/preview), null-drop;
  - `snapshot_character(pc)` — ПОЛНАЯ замена `payload.player_character` (не merge).
  Оба бампают version (`saturating_add(1)`), атомарная запись temp+rename.
- Атомарная запись/скан — локальная копия хелперов (третья); выделение общего
  `pkgfs`-модуля — отдельный клинап-трек, не здесь.

### К1.2 Пакетная механика
- share.rs: `CHARACTER_FORMAT="gmlab.character/1"`, `PackageKind::Character`,
  ветка `detect_kind` по `character.json`, arm в `manifest_id`.
- `import_character_into` (образец import_world_into: staging + swap_in + 409 без
  overwrite) + СТРУКТУРНАЯ валидация ДО swap_in: format верный, `payload` — объект,
  `payload.player_character` — объект, `title` непустой; иначе 400, в библиотеку не
  попадает. Глубокая валидация статов — НЕ на импорте (ленивая коэрция на запуске,
  как у миров). После импорта — `character_store.reload()`.
- Эндпоинты: `GET/POST /characters`, `POST /characters/{id}` (metadata),
  `POST /characters/{id}/delete`, `GET /characters/{id}/export` → `{id}.gmchar.zip`.
- `AppState.character_store`.

### К1.3 Запуск сейва с персонажем
- `POST /chats`: опц. `character_id`. Несуществующий id → 400 (no-fallback).
- Рефактор `post_create_chat`: все три ветки (brief / procedural / named story)
  возвращают `(World, warnings)`; ЕДИНЫЙ хвост: оверлей персонажа → сборка session
  один раз. Прецеденс: выбранный пакет > player_character из plot/seed > дефолт.
- Оверлей = `seed_player_character(payload.player_character)` — полная замена, БЕЗ
  события и БЕЗ инкремента (события — только тул-путь). `card_revision` пакета
  принимается как есть (edu-счётчик героя едет с ним; version пакета — отдельный
  счётчик; UI показывает version).
- Провенанс: `World.char_ref: Option<PackageRef>` (4-е ref-поле). Payload-ключ
  `char_ref` в `world_to_payload` сразу после `world_ref_authored_version`,
  только когда Some; парс в `world_from_payload`; тесты рядом с `package_ref_tests`
  (roundtrip + absent-emits-no-key). НЕ в `player_character_to_payload`.
- Варн `story_pc_override`: если задан `character_id` И plot/seed истории несёт
  собственного `player_character` — предупреждение через `launch_warnings`
  («история написана под своего героя; сюжет/улики/NPC могут ссылаться на него»).
  Warn-but-allow, как world_version_drift.
- Смена персонажа посреди сейва — НЕТ. Прогресс живёт в сейве.

### К1.4 Экспорт прогресса в библиотеку
- `POST /chats/{chat_id}/save-character`, body `{character_id?}`:
  - без id → создать нового персонажа (title = имя ГГ) из текущего снапшота;
  - с id → `snapshot_character` существующего (+version bump); несуществующий → 400
    (фронт предлагает «создать нового»).
- Чтение ГГ: ЕДИНООБРАЗНО через кэш — `ensure_cached` + `with_runtime` под
  per-chat lock (голый `load_chat` для активного чата отдаёт устаревшую строку БД).
- `card_revision` снапшота едет в пакет как есть.
- Удаление персонажа НИКОГДА не трогает сейвы: `char_ref` может «висеть» — это
  провенанс, снапшот самодостаточен. Никакой интеграции с purge/embedding-скоупами.

### К1.5 UI (v1 — минимальный)
- 4-я вкладка «Персонажи»: список (имя, версия, превью), переименовать, удалить,
  ⬇ экспорт; импорт — общий (нотис-лейбл 3-way: Мир/История/Персонаж;
  `onImportPackage` += refreshCharacters()).
- Пикер персонажа в блоке нового чата (под стори-пикером), опциональный
  (пусто = ГГ истории/дефолт), `createLocked` от него не зависит. Также в
  `onPlayWorld`.
- «Сохранить ГГ в библиотеку» — на ПОЛЬЗОВАТЕЛЬСКОЙ поверхности (WorldHud-блок
  игрока), НЕ в DebugPanel (он за developerMode). Выбор «новый / обновить
  исходного»; «исходный» активен только когда `char_ref` есть и резолвится —
  для этого `char_ref {id, version} | null` добавляется в state-payload.
- Создание персонажа v1 = save-back из чата или импорт. Полноформатный редактор
  26 полей — СЛЕДУЮЩАЯ фаза (не строить сейчас); dev-редактор в DebugPanel
  остаётся как есть.
- Портреты (assets/) — потом; механика WorldStore.assets переносится аддитивно,
  формат не меняется. Ключ `assets` в payload сейчас НЕ вводить.

## Фаза К2 — качество карточки (порезано панелью до честного фикса)

- НЕ ДЕЛАТЬ движковый лукап скилов в roll_dice (двойной счёт с нотацией,
  противоречит промпт-контракту, ломает кэш-префикс). Отдельный будущий трек
  «engine-authoritative checks» с полным переписыванием контракта.
- К2.1 Нормализация: в `apply_player_character_fields` числовая коэрция значений
  `abilities/skills/saving_throws/hp` (строки-числа → числа, NaN-мусор отброшен) —
  делает существующий notation-путь надёжным. То же при сидинге.
- К2.2 Инвентарь: дельта-опы `inventory_add/inventory_remove`,
  `equipment_add/equipment_remove` в `update_player_character` (строки; remove =
  trim-exact, удаляет ВСЕ вхождения). Порядок применения: full-rewrite → remove →
  add; результат сравнивается с исходным и питает `changed` (revision/event
  срабатывают штатно). Полная перезапись остаётся (совместимость).
- Синхронизация с канон-предметами сцены (take/drop, player-as-actor) — будущий
  трек, не здесь.

## Фаза С1 — сюжетный архитектор

### С1.1 StoryStore: редактирование
- `StoryEnvelope` получает: round-trip объект `meta` (эмитится только непустым —
  байты builtin-пакетов не меняются) и `created_at/updated_at` (парс-с-дефолтом,
  эмит только когда заданы). Архитекторские поля (`architect_messages`,
  `architect_model_history`, `architect_cache_*`) живут В `meta`, НИКОГДА в
  `seed` (утечёт в worldgen/байт-гейты) и не на топ-уровне (теряется).
- `update_story(id, patch)`: title/description/seed(plot)/meta shallow-merge с
  null-drop (образец merge_world_payload), version bump, обновление in-memory
  кэша, новый вариант ошибки `StoryNotFound`.
- `POST /stories/{id}` + `persist_story_payload` (draft-first: персист ДО вызова
  модели, как у мира).
- Архитектор работает ТОЛЬКО с authored-историями, привязанными к миру
  (`world_ref`). Builtin-и (self-contained) не редактируются архитектором.

### С1.2 Агент (gml-agents) — генерализация, не форк
- Из `world_architect.rs` извлекается generic-луп
  `architect_turn(system, tools, apply_tool, ...)` (HopSink/ArchitectStream/
  нормализация вызовов/стats — уже generic); мир и сюжет — два тонких конфига.
  Регресс-гейт: голдены/тесты мирового архитектора не меняются.
- Новое: `STORY_ARCHITECT_SYSTEM` + тулы `draft_story_plot`/`edit_story_plot`.
  Схема целится ТОЛЬКО в существующий рантайм-контракт authored-плота:
  `title, description, story_brief, public_intro, hidden_truth,
  player_character{...}, scene{title,description,location_id,present_npcs,exits,
  items,constraints,tension}, npcs[], public_facts[], state_records[],
  proper_nouns[], time`. НИКАКИХ acts/objectives/endings — рантайм их не читает
  (будущий трек «plot progression engine»).
- Контекст: полная ВНУТРЕННЯЯ WorldLore связанного мира (включая hidden_premise/
  hidden_secrets — агент GM-доверенный) как стабильный system-блок под кэш
  (cache_session_id/thread_id); image-поля лора не инжектятся.
- ГГ в сюжете: архитектор может предложить `player_character` (авторский
  протагонист); пикер при запуске его перекрывает (с варном К1.3).

### С1.3 SSE + фронт
- `POST /story-architect/chat` — зеркало мирового (draft-first, события
  architect_delta/tool/done/error; фронтовый streamArchitect переиспользуется).
- Панель: НЕ форк — `WorldArchitectPanel` параметризуется config-пропом
  (endpoint, тулы, дескрипторы полей формы, опц. read-only блок мира).
- Точки входа: «+ История» продолжает открывать CreateStoryModal, но модал
  становится PROCEDURAL-ONLY (authored-ветка из него удаляется); в модале ссылка
  «✨ Открыть в архитекторе» — единственный путь к authored. В списке историй
  «✎» открывает архитектора для существующей authored-истории.

## Будущие треки (зафиксировано, не делать сейчас)
- Plot progression engine (acts/objectives/reveals/endings + трекер на World +
  инжект в GM-промпт).
- Engine-authoritative checks (лукап статов движком + переписывание контракта
  roll_dice).
- Синхронизация инвентаря ГГ с канон-предметами (player as canon actor).
- Полноформатный редактор персонажа во вкладке; портреты (assets/).
- Дрифт версии ИСТОРИИ для существующих сейвов (story_ref.version vs live) —
  по аналогии с world_version_drift, когда редактирование историй станет частым.
- Дедуп пакетных сторов (pkgfs-модуль: атомарная запись/скан/abspath).
