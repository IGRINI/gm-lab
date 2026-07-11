import { useEffect, useMemo, useRef, useState } from "react";

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
  { key: "worlds", label: "Миры" },
  { key: "stories", label: "Истории" },
  { key: "characters", label: "Персонажи" },
];

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
          {playLabel}
        </button>
        <button type="button" className="btn lib-card-studio" disabled={locked} onClick={onOpenStudio}>
          {studioLabel}
        </button>
        <DropMenu label="⋯" ariaLabel={`Ещё · ${title}`} align="right" items={menuItems} disabled={locked} />
      </div>
    </article>
  );
}

function EmptyState({ text, ctaLabel, onCta, locked }) {
  return (
    <div className="lib-empty">
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
  const [filter, setFilter] = useState("worlds");
  const [confirmTarget, setConfirmTarget] = useState(null); // { entity, kind }
  const [deleting, setDeleting] = useState(false);
  const [importing, setImporting] = useState(false);
  const [importError, setImportError] = useState("");
  const [importNotice, setImportNotice] = useState("");
  const importInputRef = useRef(null);

  const worldList = useMemo(() => (Array.isArray(worlds) ? worlds : []), [worlds]);
  const storyList = useMemo(() => (Array.isArray(stories) ? stories : []), [stories]);
  const characterList = useMemo(() => (Array.isArray(characters) ? characters : []), [characters]);
  const worldsById = useMemo(() => {
    const map = new Map();
    for (const world of worldList) if (world?.id != null) map.set(String(world.id), world);
    return map;
  }, [worldList]);

  const locked = Boolean(busy);
  const counts = { worlds: worldList.length, stories: storyList.length, characters: characterList.length };

  const activeLoading =
    filter === "worlds" ? worldsLoading : filter === "stories" ? storiesLoading : charactersLoading;
  const activeError =
    filter === "worlds" ? worldsError : filter === "stories" ? storiesError : charactersError;

  const storyWorldLabel = (story) => {
    const refId = story?.world_ref?.id;
    if (refId == null) return "";
    const world = worldsById.get(String(refId));
    const label = world ? worldTitle(world) : textValue(story?.world_ref?.title);
    return label ? `→ ${label}` : "";
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

  const renderGrid = () => {
    if (activeLoading) return <div className="lib-status">Загрузка…</div>;
    if (activeError) return <div className="lib-status lib-status--error">{activeError}</div>;

    if (filter === "worlds") {
      if (worldList.length === 0) {
        return (
          <EmptyState
            text="Миров пока нет — создайте в студии или импортируйте пакет."
            ctaLabel="+ Создать мир"
            onCta={() => onCreate?.("world")}
            locked={locked}
          />
        );
      }
      return (
        <div className="lib-grid">
          {worldList.map((world) => (
            <LibraryCard
              key={world.id}
              kind="world"
              title={worldTitle(world)}
              meta={worldMeta(world)}
              preview={worldPreview(world)}
              playLabel="Играть"
              studioLabel="Студия"
              locked={locked}
              onPlay={() => onPlay?.(world, "world")}
              onOpenStudio={() => onOpenStudio?.(world, "world")}
              menuItems={[
                { key: "export", label: "Экспорт", onClick: () => onExport?.(world, "world") },
                { key: "delete", label: "Удалить", danger: true, onClick: () => askDelete(world, "world") },
              ]}
            />
          ))}
        </div>
      );
    }

    if (filter === "stories") {
      if (storyList.length === 0) {
        return (
          <EmptyState
            text="Историй пока нет — создайте в студии сюжета над одним из миров."
            ctaLabel="+ Создать историю"
            onCta={() => onCreate?.("story")}
            locked={locked}
          />
        );
      }
      return (
        <div className="lib-grid">
          {storyList.map((story) => (
            <LibraryCard
              key={story.id}
              kind="story"
              title={storyTitle(story)}
              meta={storyWorldLabel(story)}
              preview={storyPreview(story)}
              playLabel="Играть"
              studioLabel="Студия"
              locked={locked}
              onPlay={() => onPlay?.(story, "story")}
              onOpenStudio={() => onOpenStudio?.(story, "story")}
              menuItems={[
                { key: "export", label: "Экспорт", onClick: () => onExport?.(story, "story") },
                { key: "export-world", label: "Экспорт с миром", onClick: () => onExport?.(story, "story", { bake: true }) },
                { key: "delete", label: "Удалить", danger: true, onClick: () => askDelete(story, "story") },
              ]}
            />
          ))}
        </div>
      );
    }

    if (characterList.length === 0) {
      return (
        <EmptyState
          text="Персонажей пока нет — создайте в студии или импортируйте .gmchar."
          ctaLabel="+ Создать персонажа"
          onCta={() => onCreate?.("character")}
          locked={locked}
        />
      );
    }
    return (
      <div className="lib-grid">
        {characterList.map((character) => {
          const items = [
            { key: "export", label: "Экспорт", onClick: () => onExport?.(character, "character") },
          ];
          if (onRename) {
            items.push({ key: "rename", label: "Переименовать", onClick: () => onRename?.(character, "character") });
          }
          items.push({ key: "delete", label: "Удалить", danger: true, onClick: () => askDelete(character, "character") });
          return (
            <LibraryCard
              key={character.id}
              kind="character"
              title={characterTitle(character)}
              meta={characterMeta(character)}
              preview={characterPreview(character)}
              playLabel="Играть"
              studioLabel="Студия"
              locked={locked}
              onPlay={() => onPlay?.(character, "character")}
              onOpenStudio={() => onOpenStudio?.(character, "character")}
              menuItems={items}
            />
          );
        })}
      </div>
    );
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
            label="+ Создать ▾"
            ariaLabel="Создать пакет"
            align="right"
            buttonClassName="primary"
            items={createItems}
            disabled={locked}
          />
          <button type="button" className="btn" onClick={triggerImport} disabled={locked || importing}>
            {importing ? "Импорт…" : "Импорт"}
          </button>
          <button type="button" className="btn" onClick={() => onReveal?.()}>
            Открыть папку
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
            <div className="confirm-icon" aria-hidden="true">🗑</div>
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
                ? "Мир удалится из библиотеки. Игровые чаты и текущая сессия не изменятся. Это действие нельзя отменить."
                : confirmKind === "story"
                  ? "История удалится из библиотеки. Сохранённые игры не изменятся. Это действие нельзя отменить."
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
