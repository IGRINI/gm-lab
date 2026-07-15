import Icon from "./Icon.jsx";
import { useEffect, useMemo, useRef, useState } from "react";
import SearchField from "./SearchField.jsx";
import SearchSkeleton from "./SearchSkeleton.jsx";
import useAsyncSearch from "../useAsyncSearch.js";

// Full-screen Библиотека view (§Библиотека in the TZ). Toolbar with filter pills
// + a «+ Создать ▾» / Импорт / Открыть папку cluster, then a card grid for the
// active kind. Every card offers Играть (→ wizard, pre-selected), Студия (→ the
// entity's architect studio) and a «⋯» menu (Экспорт / kind-specific extras /
// Удалить с подтверждением).
//
// Purely props-driven — no global state. Handlers receive the raw entity plus its
// kind string ("world" | "story" | "character"):
//   onPlay(entity, kind)          open the New-Game wizard pre-selected on it
//   onOpenStudio(entity, kind)    open the world/story/character studio
//   onExport(entity, kind, opts)  opts.bake === true for a story's «С миром»
//   onDelete(entity, kind)        actual delete (LibraryScreen owns the confirm UI)
//   onRename(entity, kind)        optional; only wired into the character menu
//   onCreate(kind)                "world" | "story" | "character" → studio
//   onImport(file, overwrite)     resolves to { kind, id }; LibraryScreen owns the
//                                 file input + collision-retry UX
//   onReveal()                    open the library folder on disk

const FILTERS = [
  { key: "all", label: "Все" },
  { key: "worlds", label: "Миры" },
  { key: "stories", label: "Истории" },
  { key: "characters", label: "Персонажи" },
];

const FILTER_TYPES = {
  all: ["world", "story", "character"],
  worlds: ["world"],
  stories: ["story"],
  characters: ["character"],
};

const KIND_LABELS = { world: "Мир", story: "История", character: "Персонаж" };

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function worldTitle(world) {
  return textValue(world?.title) || "Новый мир";
}

function storyTitle(story) {
  return textValue(story?.title) || textValue(story?.name) || "Новая история";
}

function characterTitle(character) {
  return textValue(character?.title) || "Персонаж";
}

function worldMeta(world) {
  const parts = [world?.genre, world?.tone].map(textValue).filter(Boolean);
  return parts.join(" · ");
}

function worldPreview(world) {
  return textValue(world?.preview) || textValue(world?.public_premise) || textValue(world?.world_size);
}

function storyPreview(story) {
  return textValue(story?.story_brief) || textValue(story?.description) || textValue(story?.public_intro);
}

function characterPc(character) {
  const pc = character?.payload?.player_character;
  return pc && typeof pc === "object" ? pc : {};
}

function characterMeta(character) {
  const pc = characterPc(character);
  const role = textValue(pc.class_role);
  const level = pc.level;
  const parts = [];
  if (role) parts.push(role);
  if (Number.isFinite(Number(level)) && Number(level) > 0) parts.push(`уровень ${Number(level)}`);
  if (parts.length > 0) return parts.join(" · ");
  const preview = textValue(character?.preview);
  return preview && preview !== characterTitle(character) ? preview : "";
}

function characterPreview(character) {
  const pc = characterPc(character);
  return textValue(pc.background) || textValue(pc.personality) || "";
}

// A small popover menu (used for «+ Создать ▾» and each card's «⋯»). Closes on
// outside-click and Escape. `items` are `{ key, label, danger?, onClick }`.
function DropMenu({ label, ariaLabel, items, disabled, align = "right", buttonClassName = "" }) {
  const [open, setOpen] = useState(false);
  const ref = useRef(null);

  useEffect(() => {
    if (!open) return undefined;
    const onDoc = (event) => {
      if (ref.current && !ref.current.contains(event.target)) setOpen(false);
    };
    const onKey = (event) => {
      if (event.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div className="lib-menu" ref={ref}>
      <button
        type="button"
        className={"btn lib-menu-btn" + (buttonClassName ? " " + buttonClassName : "")}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={ariaLabel}
        disabled={disabled}
        onClick={() => setOpen((value) => !value)}
      >
        {label}
      </button>
      {open && (
        <div className={"lib-menu-pop lib-menu-pop--" + align} role="menu">
          {items.map((item) => (
            <button
              key={item.key || item.label}
              type="button"
              role="menuitem"
              className={"lib-menu-item" + (item.danger ? " danger" : "")}
              onClick={() => {
                setOpen(false);
                item.onClick?.();
              }}
            >
              {item.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function LibraryCard({ kind, title, badge, meta, preview, playLabel, studioLabel, onPlay, onOpenStudio, menuItems, locked }) {
  return (
    <article className="lib-card">
      <div className="lib-card-main">
        <div className="lib-card-head">
          <h3 className="lib-card-title" title={title}>{title}</h3>
          {badge && <span className="lib-card-badge">{badge}</span>}
        </div>
        {meta && <div className="lib-card-meta">{meta}</div>}
        {preview
          ? <p className="lib-card-preview">{preview}</p>
          : <p className="lib-card-preview lib-card-preview--empty">Без описания</p>}
      </div>
      <div className="lib-card-actions">
        <button type="button" className="btn primary lib-card-play" disabled={locked} onClick={onPlay}>
          <Icon name="play" size={13} /> {playLabel}
        </button>
        <button type="button" className="btn lib-card-studio" disabled={locked} onClick={onOpenStudio}>
          <Icon name="pen" size={13} /> {studioLabel}
        </button>
        <DropMenu label={<Icon name="dots" size={15} />} ariaLabel={`Ещё · ${title}`} align="right" items={menuItems} disabled={locked} />
      </div>
    </article>
  );
}

function EmptyState({ icon, title, text, ctaLabel, onCta, locked }) {
  return (
    <div className="lib-empty">
      <span className="lib-empty-icon" aria-hidden="true">
        <Icon name={icon} size={22} />
      </span>
      <h2 className="lib-empty-title">{title}</h2>
      <p className="lib-empty-text">{text}</p>
      {ctaLabel && (
        <button type="button" className="btn primary" onClick={onCta} disabled={locked}>
          {ctaLabel}
        </button>
      )}
    </div>
  );
}

export default function LibraryScreen({
  worlds = [],
  stories = [],
  characters = [],
  busy = false,
  worldsLoading = false,
  worldsError = "",
  storiesLoading = false,
  storiesError = "",
  charactersLoading = false,
  charactersError = "",
  onPlay,
  onOpenStudio,
  onExport,
  onDelete,
  onRename,
  onCreate,
  onImport,
  onReveal,
}) {
  const [filter, setFilter] = useState("all");
  const [searchQuery, setSearchQuery] = useState("");
  const [confirmTarget, setConfirmTarget] = useState(null); // { entity, kind }
  const [deleting, setDeleting] = useState(false);
  const [importing, setImporting] = useState(false);
  const [importError, setImportError] = useState("");
  const [importNotice, setImportNotice] = useState("");
  const importInputRef = useRef(null);

  const worldList = useMemo(() => (Array.isArray(worlds) ? worlds : []), [worlds]);
  const storyList = useMemo(() => (Array.isArray(stories) ? stories : []), [stories]);
  const characterList = useMemo(() => (Array.isArray(characters) ? characters : []), [characters]);
  const searchActive = searchQuery.trim().length > 0;
  const searchRefresh = useMemo(
    () => [worldList, storyList, characterList]
      .flat()
      .map((entity) => `${entity?.id || ""}:${entity?.updated_at || entity?.version || ""}`)
      .join("|"),
    [worldList, storyList, characterList]
  );
  const librarySearch = useAsyncSearch(
    {
      q: searchQuery,
      scope: "library",
      types: FILTER_TYPES[filter] || FILTER_TYPES.all,
      limit: 50,
      _refresh: searchRefresh,
    },
    { enabled: searchActive, delay: 200 }
  );
  const worldsById = useMemo(() => {
    const map = new Map();
    for (const world of worldList) if (world?.id != null) map.set(String(world.id), world);
    return map;
  }, [worldList]);

  const locked = Boolean(busy);
  const counts = {
    all: worldList.length + storyList.length + characterList.length,
    worlds: worldList.length,
    stories: storyList.length,
    characters: characterList.length,
  };

  const activeLoading =
    filter === "all"
      ? worldsLoading || storiesLoading || charactersLoading
      : filter === "worlds"
        ? worldsLoading
        : filter === "stories"
          ? storiesLoading
          : charactersLoading;
  const activeError =
    filter === "all"
      ? [worldsError, storiesError, charactersError].filter(Boolean).join(" · ")
      : filter === "worlds"
        ? worldsError
        : filter === "stories"
          ? storiesError
          : charactersError;

  const storyWorldLabel = (story) => {
    const refId = story?.world_ref?.id;
    if (refId == null) return "";
    const world = worldsById.get(String(refId));
    const label = world ? worldTitle(world) : textValue(story?.world_ref?.title);
    return label ? `→ ${label}` : "";
  };

  const storiesById = useMemo(() => {
    const map = new Map();
    for (const story of storyList) if (story?.id != null) map.set(String(story.id), story);
    return map;
  }, [storyList]);

  // «→ мир · история» for a character card: the base packages the hero was
  // authored for (world_ref/story_ref provenance). A deleted base is labelled
  // honestly — the ref may dangle by design and the label must not vanish, but
  // a raw machine id never leaks into player-facing prose.
  const characterBaseLabel = (character) => {
    const parts = [];
    const worldRefId = character?.world_ref?.id;
    if (worldRefId != null) {
      const world = worldsById.get(String(worldRefId));
      parts.push(world ? worldTitle(world) : "мир недоступен");
    }
    const storyRefId = character?.story_ref?.id;
    if (storyRefId != null) {
      const story = storiesById.get(String(storyRefId));
      parts.push(story ? storyTitle(story) : "история недоступна");
    }
    return parts.length > 0 ? `→ ${parts.join(" · ")}` : "";
  };

  const createItems = [
    { key: "world", label: "Мир", onClick: () => onCreate?.("world") },
    { key: "story", label: "Историю", onClick: () => onCreate?.("story") },
    { key: "character", label: "Персонажа", onClick: () => onCreate?.("character") },
  ];

  const askDelete = (entity, kind) => setConfirmTarget({ entity, kind });
  const cancelDelete = () => {
    if (!deleting) setConfirmTarget(null);
  };
  const confirmDelete = async () => {
    if (!confirmTarget || deleting) return;
    setDeleting(true);
    try {
      await onDelete?.(confirmTarget.entity, confirmTarget.kind);
      setConfirmTarget(null);
    } finally {
      setDeleting(false);
    }
  };

  // Confirm dialog owns Escape while open (capture phase), mirroring the games
  // sidebar so the destructive confirms across the app dismiss the same way.
  useEffect(() => {
    if (!confirmTarget || typeof document === "undefined") return undefined;
    const onKey = (event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      if (!deleting) setConfirmTarget(null);
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [confirmTarget, deleting]);

  // File import (mirrors ChatHistorySidebar's proven collision-retry UX). On a
  // 409 id collision the picked file is re-sent with overwrite after a confirm.
  const triggerImport = () => {
    if (importing || locked) return;
    setImportError("");
    setImportNotice("");
    importInputRef.current?.click();
  };
  const isCollision = (error) =>
    error?.collision === true ||
    error?.status === 409 ||
    /already exists|уже существует/i.test(error?.message || "");
  const runImport = async (file, overwrite) => {
    const data = await onImport?.(file, overwrite);
    // Full noun + participle per kind so the gender agrees («История импортирована»).
    const label =
      data?.kind === "story"
        ? "История импортирована"
        : data?.kind === "character"
          ? "Персонаж импортирован"
          : "Мир импортирован";
    setImportNotice(`${label}${data?.id ? `: ${data.id}` : ""}`);
  };
  const onImportFile = async (event) => {
    const file = event.target.files?.[0];
    event.target.value = ""; // allow re-picking the same file
    if (!file) return;
    setImporting(true);
    setImportError("");
    setImportNotice("");
    try {
      await runImport(file, false);
    } catch (error) {
      if (
        isCollision(error) &&
        typeof window !== "undefined" &&
        window.confirm("Пакет с таким id уже существует. Заменить?")
      ) {
        try {
          await runImport(file, true);
        } catch (retryError) {
          setImportError(retryError?.message || "импорт не выполнен");
        }
      } else {
        setImportError(error?.message || "импорт не выполнен");
      }
    } finally {
      setImporting(false);
    }
  };

  const confirmKind = confirmTarget?.kind || "";
  const confirmName =
    confirmKind === "world"
      ? worldTitle(confirmTarget?.entity)
      : confirmKind === "story"
        ? storyTitle(confirmTarget?.entity)
        : characterTitle(confirmTarget?.entity);

  const renderCard = (entity, kind, match = null) => {
    const badge = filter === "all" ? KIND_LABELS[kind] : "";
    if (kind === "world") {
      return (
        <LibraryCard
          key={`world:${entity.id}`}
          kind="world"
          title={worldTitle(entity)}
          badge={badge}
          meta={worldMeta(entity)}
          preview={textValue(match?.snippet) || worldPreview(entity)}
          playLabel="Играть"
          studioLabel="Студия"
          locked={locked}
          onPlay={() => onPlay?.(entity, "world")}
          onOpenStudio={() => onOpenStudio?.(entity, "world")}
          menuItems={[
            { key: "export", label: "Экспорт", onClick: () => onExport?.(entity, "world") },
            { key: "delete", label: "Удалить", danger: true, onClick: () => askDelete(entity, "world") },
          ]}
        />
      );
    }
    if (kind === "story") {
      return (
        <LibraryCard
          key={`story:${entity.id}`}
          kind="story"
          title={storyTitle(entity)}
          badge={badge}
          meta={storyWorldLabel(entity)}
          preview={textValue(match?.snippet) || storyPreview(entity)}
          playLabel="Играть"
          studioLabel="Студия"
          locked={locked}
          onPlay={() => onPlay?.(entity, "story")}
          onOpenStudio={() => onOpenStudio?.(entity, "story")}
          menuItems={[
            { key: "export", label: "Экспорт", onClick: () => onExport?.(entity, "story") },
            { key: "export-world", label: "Экспорт с миром", onClick: () => onExport?.(entity, "story", { bake: true }) },
            { key: "delete", label: "Удалить", danger: true, onClick: () => askDelete(entity, "story") },
          ]}
        />
      );
    }

    const menuItems = [
      { key: "export", label: "Экспорт", onClick: () => onExport?.(entity, "character") },
    ];
    if (onRename) {
      menuItems.push({ key: "rename", label: "Переименовать", onClick: () => onRename?.(entity, "character") });
    }
    menuItems.push({ key: "delete", label: "Удалить", danger: true, onClick: () => askDelete(entity, "character") });
    const meta = [characterMeta(entity), characterBaseLabel(entity)].filter(Boolean).join(" · ");
    return (
      <LibraryCard
        key={`character:${entity.id}`}
        kind="character"
        title={characterTitle(entity)}
        badge={badge}
        meta={meta}
        preview={textValue(match?.snippet) || characterPreview(entity)}
        playLabel="Играть"
        studioLabel="Студия"
        locked={locked}
        onPlay={() => onPlay?.(entity, "character")}
        onOpenStudio={() => onOpenStudio?.(entity, "character")}
        menuItems={menuItems}
      />
    );
  };

  const entityForSearchResult = (result) => {
    const id = result?.id == null ? "" : String(result.id);
    const source = result?.type === "world" ? worldList : result?.type === "story" ? storyList : characterList;
    return source.find((entity) => String(entity?.id) === id) || null;
  };

  const renderGrid = () => {
    if (activeLoading) return <SearchSkeleton variant="cards" count={6} />;
    if (activeError) return <div className="lib-status lib-status--error">{activeError}</div>;

    if (searchActive) {
      if (librarySearch.initialLoading) return <SearchSkeleton variant="cards" count={6} />;
      if (librarySearch.error && librarySearch.items.length === 0) {
        return <div className="lib-status lib-status--error">{librarySearch.error}</div>;
      }
      const results = librarySearch.items
        .map((match) => ({ match, entity: entityForSearchResult(match) }))
        .filter((entry) => entry.entity && (FILTER_TYPES[filter] || FILTER_TYPES.all).includes(entry.match.type));
      if (results.length === 0) {
        return (
          <EmptyState
            icon="search"
            title="Ничего не найдено"
            text={`В ${filter === "all" ? "библиотеке" : "этой вкладке"} нет совпадений. Попробуйте другое слово.`}
          />
        );
      }
      return (
        <div className="lib-grid">
          {results.map(({ match, entity }) => renderCard(entity, match.type, match))}
        </div>
      );
    }

    if (filter === "all") {
      const entries = [
        ...worldList.map((entity) => ({ kind: "world", entity })),
        ...storyList.map((entity) => ({ kind: "story", entity })),
        ...characterList.map((entity) => ({ kind: "character", entity })),
      ];
      if (entries.length === 0) {
        return (
          <EmptyState
            icon="book"
            title="Библиотека пока пуста"
            text="Создайте первый мир — затем на его основе можно собрать историю и персонажей."
            ctaLabel="+ Создать мир"
            onCta={() => onCreate?.("world")}
            locked={locked}
          />
        );
      }
      return <div className="lib-grid">{entries.map(({ entity, kind }) => renderCard(entity, kind))}</div>;
    }

    if (filter === "worlds") {
      if (worldList.length === 0) {
        return (
          <EmptyState
            icon="globe"
            title="Создайте первый мир"
            text="Миров пока нет — создайте в студии или импортируйте пакет."
            ctaLabel="+ Создать мир"
            onCta={() => onCreate?.("world")}
            locked={locked}
          />
        );
      }
      return <div className="lib-grid">{worldList.map((world) => renderCard(world, "world"))}</div>;
    }

    if (filter === "stories") {
      if (storyList.length === 0) {
        return (
          <EmptyState
            icon="scroll"
            title="Создайте первую историю"
            text="Историй пока нет — создайте в студии сюжета над одним из миров."
            ctaLabel="+ Создать историю"
            onCta={() => onCreate?.("story")}
            locked={locked}
          />
        );
      }
      return <div className="lib-grid">{storyList.map((story) => renderCard(story, "story"))}</div>;
    }

    if (characterList.length === 0) {
      return (
        <EmptyState
          icon="user"
          title="Создайте первого персонажа"
          text="Персонажей пока нет — создайте в студии или импортируйте .gmchar."
          ctaLabel="+ Создать персонажа"
          onCta={() => onCreate?.("character")}
          locked={locked}
        />
      );
    }
    return <div className="lib-grid">{characterList.map((character) => renderCard(character, "character"))}</div>;
  };

  return (
    <div className="library-screen">
      <div className="library-screen-head">
        <div className="lib-filters" role="tablist" aria-label="Библиотека">
          {FILTERS.map((tab) => (
            <button
              key={tab.key}
              type="button"
              role="tab"
              aria-selected={filter === tab.key}
              className={"lib-pill" + (filter === tab.key ? " active" : "")}
              onClick={() => setFilter(tab.key)}
            >
              {tab.label}
              <span className="lib-pill-count">{counts[tab.key]}</span>
            </button>
          ))}
        </div>
        <div className="lib-toolbar-actions">
          <DropMenu
            label={<><Icon name="plus" size={14} /> Создать</>}
            ariaLabel="Создать пакет"
            align="right"
            buttonClassName="primary"
            items={createItems}
            disabled={locked}
          />
          <button type="button" className="btn" onClick={triggerImport} disabled={locked || importing}>
            <Icon name="upload" size={14} /> {importing ? "Импорт…" : "Импорт"}
          </button>
          <button type="button" className="btn" onClick={() => onReveal?.()}>
            <Icon name="folder" size={14} /> Открыть папку
          </button>
          <input
            ref={importInputRef}
            type="file"
            className="visually-hidden-input"
            onChange={onImportFile}
            aria-hidden="true"
            tabIndex={-1}
          />
        </div>
      </div>

      <div className="library-search-row">
        <SearchField
          value={searchQuery}
          onChange={setSearchQuery}
          placeholder={filter === "all" ? "Искать по всей библиотеке" : `Искать: ${FILTERS.find((tab) => tab.key === filter)?.label.toLowerCase() || "вкладка"}`}
          ariaLabel="Поиск по библиотеке"
          loading={librarySearch.revalidating}
        />
        {searchActive && !librarySearch.initialLoading && !librarySearch.error && (
          <span className="library-search-count">
            {librarySearch.total} найдено
          </span>
        )}
      </div>

      {importError && <div className="lib-notice lib-notice--error">{importError}</div>}
      {importNotice && <div className="lib-notice">{importNotice}</div>}

      <div className="lib-grid-scroll">{renderGrid()}</div>

      {confirmTarget && (
        <div className="confirm-backdrop" role="presentation" onMouseDown={cancelDelete}>
          <div
            className="confirm-card"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="lib-confirm-title"
            aria-describedby="lib-confirm-note"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <div className="confirm-icon" aria-hidden="true"><Icon name="trash" size={19} /></div>
            <h3 id="lib-confirm-title">
              {confirmKind === "world"
                ? "Удалить мир?"
                : confirmKind === "story"
                  ? "Удалить историю?"
                  : "Удалить персонажа?"}
            </h3>
            <p className="confirm-name">«{confirmName}»</p>
            <p id="lib-confirm-note" className="confirm-note">
              {confirmKind === "world"
                ? "Мир удалится из библиотеки. Игровые чаты и текущая сессия не изменятся; истории и персонажи, созданные на этом мире, сохранятся, но потеряют его материал как основу. Это действие нельзя отменить."
                : confirmKind === "story"
                  ? "История удалится из библиотеки. Сохранённые игры не изменятся; персонажи, созданные под эту историю, сохранятся, но потеряют её материал как основу. Это действие нельзя отменить."
                  : "Персонаж удалится из библиотеки. Сохранённые игры не изменятся — их снапшот самодостаточен. Это действие нельзя отменить."}
            </p>
            <div className="confirm-actions">
              <button type="button" className="btn" onClick={cancelDelete} disabled={deleting} autoFocus>
                Отмена
              </button>
              <button
                type="button"
                className="btn confirm-danger"
                onClick={confirmDelete}
                disabled={deleting}
              >
                {deleting ? "Удаляю…" : "Удалить"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
