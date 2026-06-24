import { useEffect, useMemo, useRef, useState } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";

const DATE_FORMATTER = new Intl.DateTimeFormat("ru-RU", {
  day: "2-digit",
  month: "short",
  hour: "2-digit",
  minute: "2-digit",
});

function chatTitle(chat) {
  return chat?.title?.trim() || "Новый чат";
}

function worldTitle(world) {
  return world?.title?.trim() || "Новый мир";
}

function chatPreview(chat) {
  return chat?.preview?.trim() || "Пустой чат";
}

function worldPreview(world) {
  return world?.preview?.trim() || world?.public_premise?.trim?.() || "Пустой мир";
}

function chatDate(chat) {
  const raw = chat?.updated_at || chat?.created_at;
  if (!raw) return "";
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) return "";
  return DATE_FORMATTER.format(date).replace(".", "");
}

function turnCount(chat) {
  const count = Number(chat?.turn_count || 0);
  return `${count} ходов`;
}

function worldMeta(world) {
  const parts = [world?.genre, world?.tone, world?.world_size]
    .map((value) => (typeof value === "string" ? value.trim() : ""))
    .filter(Boolean);
  return parts.length > 0 ? parts.join(" · ") : "мир";
}

function sameChatId(a, b) {
  return a != null && b != null && String(a) === String(b);
}

function storyDescription(story) {
  return story?.story_brief?.trim?.() || story?.description?.trim?.() || "";
}

export default function ChatHistorySidebar({
  chats,
  worlds,
  activeChatId,
  selectedWorldId,
  open,
  busy,
  loading,
  error,
  worldsLoading,
  worldsError,
  stories,
  selectedStoryId,
  storiesLoading,
  storiesError,
  onSelectStory,
  onClose,
  onCreate,
  onCreateWorld,
  onShowWorldCreator,
  onShowChats,
  onSelectWorld,
  onActivate,
  onDelete,
  onDeleteWorld,
}) {
  const closeRef = useRef(null);
  const createChatRef = useRef(null);
  const createWorldRef = useRef(null);
  const [confirmTarget, setConfirmTarget] = useState(null);
  const [deleting, setDeleting] = useState(false);
  const [tab, setTab] = useState("chats");
  const sortedChats = useMemo(() => (Array.isArray(chats) ? chats : []), [chats]);
  const sortedWorlds = useMemo(() => (Array.isArray(worlds) ? worlds : []), [worlds]);
  const storyOptions = useMemo(() => (Array.isArray(stories) ? stories : []), [stories]);
  const selectedStory = storyOptions.find((story) => sameChatId(story.id, selectedStoryId)) || null;
  const hasStories = storyOptions.length > 0;
  const locked = busy || loading;
  const storyLocked = locked || storiesLoading || Boolean(storiesError) || !hasStories;
  const createLocked = locked || storyLocked || !selectedStoryId;
  const isWorldTab = tab === "world";
  const worldLocked = busy || worldsLoading;
  const visibleChats = sortedChats;
  const visibleWorlds = sortedWorlds;
  const confirmKind = confirmTarget?.kind || "";
  const confirmItem =
    confirmKind === "world"
      ? sortedWorlds.find((world) => sameChatId(world.id, confirmTarget?.id)) || null
      : sortedChats.find((chat) => sameChatId(chat.id, confirmTarget?.id)) || null;
  const confirmTitle = confirmKind === "world" ? worldTitle(confirmItem) : chatTitle(confirmItem);

  const cancelDelete = () => {
    if (!deleting) setConfirmTarget(null);
  };
  const confirmDelete = async () => {
    if (!confirmItem || deleting) return;
    setDeleting(true);
    try {
      if (confirmKind === "world") await onDeleteWorld?.(confirmItem.id);
      else await onDelete?.(confirmItem.id);
      setConfirmTarget(null);
    } finally {
      setDeleting(false);
    }
  };

  // Confirm dialog owns Escape while open (capture phase, so it beats the sidebar handler).
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

  useEffect(() => {
    if (!open || typeof document === "undefined") return undefined;
    // Only the mobile drawer needs focus-trapping + Esc-to-close. On desktop the
    // sidebar is a docked, collapsible column: stealing focus on load or collapsing
    // it on Escape would be surprising.
    if (typeof window !== "undefined" && !window.matchMedia("(max-width: 700px)").matches) {
      return undefined;
    }

    const previousFocus = document.activeElement;
    const onKeyDown = (event) => {
      if (event.key !== "Escape") return;
      if (confirmTarget) return; // the confirm dialog handles Escape first
      event.preventDefault();
      onClose();
    };

    document.addEventListener("keydown", onKeyDown);
    const raf = window.requestAnimationFrame(() => {
      const target = closeRef.current || (isWorldTab ? createWorldRef.current : createChatRef.current);
      target?.focus({ preventScroll: true });
    });

    return () => {
      document.removeEventListener("keydown", onKeyDown);
      window.cancelAnimationFrame(raf);
      if (previousFocus && typeof previousFocus.focus === "function") {
        previousFocus.focus({ preventScroll: true });
      }
    };
  }, [open, onClose, isWorldTab, confirmTarget]);

  return (
    <>
      <button
        type="button"
        className={"chat-sidebar-backdrop" + (open ? " is-open" : "")}
        onMouseDown={onClose}
        onClick={onClose}
        tabIndex={open ? 0 : -1}
        aria-label="Закрыть боковую панель"
        aria-hidden={!open}
      />
      <aside
        id="chat-history-sidebar"
        className={"chat-sidebar" + (open ? " is-open" : "")}
        aria-label="Чаты и миры"
      >
        <div className="chat-sidebar-head">
          <div>
            <span>{isWorldTab ? "Миры" : "История"}</span>
            <h2>{isWorldTab ? "Миры" : "Чаты"}</h2>
          </div>
          <button
            ref={closeRef}
            type="button"
            className="icon-btn chat-sidebar-close"
            onClick={onClose}
            aria-label="Закрыть боковую панель"
          >
            x
          </button>
        </div>

        <div className="chat-sidebar-tabs" role="tablist" aria-label="Режим боковой панели">
          <button
            type="button"
            className={"chat-sidebar-tab" + (!isWorldTab ? " active" : "")}
            onClick={() => {
              setTab("chats");
              onShowChats?.();
            }}
            role="tab"
            aria-selected={!isWorldTab}
          >
            Чаты
          </button>
          <button
            type="button"
            className={"chat-sidebar-tab" + (isWorldTab ? " active" : "")}
            onClick={() => {
              // Only flip the tab when the main view will actually switch — otherwise the
              // sidebar would say "Миры" while the chat pane stays open (openWorldCreator
              // is a no-op while a turn/chat action is in flight).
              if (locked) return;
              setTab("world");
              onShowWorldCreator?.();
            }}
            role="tab"
            aria-selected={isWorldTab}
          >
            Миры
          </button>
        </div>

        {!isWorldTab ? (
          <div className="chat-sidebar-actions">
            <div className="story-picker">
              <label htmlFor="new-chat-story">История</label>
              {hasStories && (
                <select
                  id="new-chat-story"
                  value={selectedStoryId || ""}
                  onChange={(event) => onSelectStory(event.target.value)}
                  disabled={storyLocked}
                >
                  {storyOptions.map((story) => (
                    <option key={story.id} value={story.id}>
                      {story.title}
                    </option>
                  ))}
                </select>
              )}
              {selectedStory && storyDescription(selectedStory) && (
                <Tooltip
                  as="p"
                  tipClassName="ui-tip-wrap"
                  content={
                    <TipContent
                      title={selectedStory.title || "История"}
                      subtitle="Короткое описание стартовой истории."
                      note={storyDescription(selectedStory)}
                    />
                  }
                >
                  {storyDescription(selectedStory)}
                </Tooltip>
              )}
              {storiesLoading && <span className="chat-sidebar-status">Загружаю истории...</span>}
              {storiesError && <span className="chat-sidebar-error inline">{storiesError}</span>}
              {!storiesLoading && !storiesError && !hasStories && (
                <span className="chat-sidebar-empty inline">Нет доступных историй.</span>
              )}
            </div>
            <button
              ref={createChatRef}
              type="button"
              className="btn primary chat-new"
              onClick={onCreate}
              disabled={createLocked}
            >
              + Новый чат
            </button>
            {loading && <span className="chat-sidebar-status">Обновляю...</span>}
          </div>
        ) : (
          <div className="chat-sidebar-actions world-sidebar-actions">
            <button
              ref={createWorldRef}
              type="button"
              className="btn primary chat-new"
              onClick={onCreateWorld}
              disabled={worldLocked}
            >
              + Создать мир
            </button>
            {worldsLoading && <span className="chat-sidebar-status">Обновляю...</span>}
          </div>
        )}

        {(isWorldTab ? worldsError : error) && (
          <div className="chat-sidebar-error">{isWorldTab ? worldsError : error}</div>
        )}

        <nav className="chat-list" aria-label={isWorldTab ? "Сохранённые миры" : "Предыдущие чаты"}>
          {isWorldTab && visibleWorlds.length === 0 && !worldsLoading ? (
            <div className="chat-sidebar-empty">
              Сохранённых миров пока нет.
            </div>
          ) : null}
          {!isWorldTab && visibleChats.length === 0 && !loading ? (
            <div className="chat-sidebar-empty">
              Сохранённых чатов пока нет.
            </div>
          ) : null}
          {isWorldTab
            ? visibleWorlds.map((world) => {
              const active = sameChatId(world.id, selectedWorldId);
              return (
                <div
                  key={world.id}
                  className={"chat-history-item world-history-item" + (active ? " active" : "")}
                >
                  <button
                    type="button"
                    className={"chat-history-row world-history-row" + (active ? " active" : "")}
                    onClick={() => onSelectWorld?.(world.id)}
                    disabled={worldLocked}
                    aria-current={active ? "page" : undefined}
                  >
                    <span className="chat-row-head">
                      <span className="chat-row-title">{worldTitle(world)}</span>
                      <span className="chat-row-date">{chatDate(world)}</span>
                    </span>
                    <span className="chat-row-preview">{worldPreview(world)}</span>
                    <span className="chat-row-meta">
                      <span>{worldMeta(world)}</span>
                      {active && <span>открыт</span>}
                    </span>
                  </button>
                  <Tooltip
                    className="tooltip-wrap"
                    tipClassName="ui-tip-wrap"
                    focusable={false}
                    content={
                      <TipContent
                        title="Удалить мир"
                        subtitle={worldTitle(world)}
                        note="Перед удалением появится подтверждение. Игровые чаты не изменятся."
                      />
                    }
                  >
                    <button
                      type="button"
                      className="chat-row-delete"
                      aria-label={`Удалить мир: ${worldTitle(world)}`}
                      onClick={() => setConfirmTarget({ kind: "world", id: world.id })}
                      disabled={worldLocked}
                    >
                      <span aria-hidden="true">🗑</span>
                    </button>
                  </Tooltip>
                </div>
              );
            })
            : (
            visibleChats.map((chat) => {
              const active = chat.active || sameChatId(chat.id, activeChatId);
              return (
                <div
                  key={chat.id}
                  className={"chat-history-item" + (active ? " active" : "")}
                >
                  <button
                    type="button"
                    className={"chat-history-row" + (active ? " active" : "")}
                    onClick={() => {
                      onActivate(chat.id);
                    }}
                    disabled={locked}
                    aria-current={active ? "page" : undefined}
                  >
                    <span className="chat-row-head">
                      <span className="chat-row-title">{chatTitle(chat)}</span>
                      <span className="chat-row-date">{chatDate(chat)}</span>
                    </span>
                    <span className="chat-row-preview">{chatPreview(chat)}</span>
                    <span className="chat-row-meta">
                      <span>{turnCount(chat)}</span>
                      {active && <span>активный</span>}
                    </span>
                  </button>
                  <Tooltip
                    className="tooltip-wrap"
                    tipClassName="ui-tip-wrap"
                    focusable={false}
                    content={
                      <TipContent
                        title="Удалить чат"
                        subtitle={chatTitle(chat)}
                        note="Перед удалением появится подтверждение."
                      />
                    }
                  >
                    <button
                      type="button"
                      className="chat-row-delete"
                      aria-label={`Удалить чат: ${chatTitle(chat)}`}
                      onClick={() => setConfirmTarget({ kind: "chat", id: chat.id })}
                      disabled={locked}
                    >
                      <span aria-hidden="true">🗑</span>
                    </button>
                  </Tooltip>
                </div>
              );
            })
          )}
        </nav>
      </aside>

      {confirmItem && (
        <div
          className="confirm-backdrop"
          role="presentation"
          onMouseDown={cancelDelete}
        >
          <div
            className="confirm-card"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="confirm-delete-title"
            aria-describedby="confirm-delete-note"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <div className="confirm-icon" aria-hidden="true">🗑</div>
            <h3 id="confirm-delete-title">{confirmKind === "world" ? "Удалить мир?" : "Удалить чат?"}</h3>
            <p className="confirm-name">«{confirmTitle}»</p>
            <p id="confirm-delete-note" className="confirm-note">
              {confirmKind === "world"
                ? "Мир удалится из списка сохранённых миров. Игровые чаты и текущая сессия не изменятся. Это действие нельзя отменить."
                : "Чат и все его данные удалятся из базы безвозвратно — история, персонажи, мир и связанные эмбеддинги. Это действие нельзя отменить."}
            </p>
            <div className="confirm-actions">
              <button type="button" className="btn" onClick={cancelDelete} disabled={deleting}>
                Отмена
              </button>
              <button
                type="button"
                className="btn confirm-danger"
                onClick={confirmDelete}
                disabled={deleting}
                autoFocus
              >
                {deleting ? "Удаляю…" : "Удалить"}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
