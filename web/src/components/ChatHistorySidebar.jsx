import { useEffect, useMemo, useRef } from "react";

const DATE_FORMATTER = new Intl.DateTimeFormat("ru-RU", {
  day: "2-digit",
  month: "short",
  hour: "2-digit",
  minute: "2-digit",
});

function chatTitle(chat) {
  return chat?.title?.trim() || "Новый чат";
}

function chatPreview(chat) {
  return chat?.preview?.trim() || "Пустой чат";
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

function sameChatId(a, b) {
  return a != null && b != null && String(a) === String(b);
}

function storyDescription(story) {
  return story?.description?.trim?.() || "";
}

export default function ChatHistorySidebar({
  chats,
  activeChatId,
  open,
  busy,
  loading,
  error,
  stories,
  selectedStoryId,
  storiesLoading,
  storiesError,
  onSelectStory,
  onClose,
  onCreate,
  onActivate,
}) {
  const closeRef = useRef(null);
  const createRef = useRef(null);
  const sortedChats = useMemo(() => (Array.isArray(chats) ? chats : []), [chats]);
  const storyOptions = useMemo(() => (Array.isArray(stories) ? stories : []), [stories]);
  const selectedStory = storyOptions.find((story) => sameChatId(story.id, selectedStoryId)) || null;
  const hasStories = storyOptions.length > 0;
  const locked = busy || loading;
  const storyLocked = locked || storiesLoading || Boolean(storiesError) || !hasStories;
  const createLocked = locked || storyLocked || !selectedStoryId;

  useEffect(() => {
    if (!open || typeof document === "undefined") return undefined;

    const previousFocus = document.activeElement;
    const onKeyDown = (event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      onClose();
    };

    document.addEventListener("keydown", onKeyDown);
    const raf = window.requestAnimationFrame(() => {
      const target = closeRef.current || createRef.current;
      target?.focus({ preventScroll: true });
    });

    return () => {
      document.removeEventListener("keydown", onKeyDown);
      window.cancelAnimationFrame(raf);
      if (previousFocus && typeof previousFocus.focus === "function") {
        previousFocus.focus({ preventScroll: true });
      }
    };
  }, [open, onClose]);

  return (
    <>
      <button
        type="button"
        className={"chat-sidebar-backdrop" + (open ? " is-open" : "")}
        onMouseDown={onClose}
        onClick={onClose}
        tabIndex={open ? 0 : -1}
        aria-label="Закрыть список чатов"
        aria-hidden={!open}
      />
      <aside
        id="chat-history-sidebar"
        className={"chat-sidebar" + (open ? " is-open" : "")}
        aria-label="Список чатов"
      >
        <div className="chat-sidebar-head">
          <div>
            <span>История</span>
            <h2>Чаты</h2>
          </div>
          <button
            ref={closeRef}
            type="button"
            className="icon-btn chat-sidebar-close"
            onClick={onClose}
            aria-label="Закрыть список чатов"
          >
            x
          </button>
        </div>

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
              <p title={storyDescription(selectedStory)}>{storyDescription(selectedStory)}</p>
            )}
            {storiesLoading && <span className="chat-sidebar-status">Загружаю истории...</span>}
            {storiesError && <span className="chat-sidebar-error inline">{storiesError}</span>}
            {!storiesLoading && !storiesError && !hasStories && (
              <span className="chat-sidebar-empty inline">Нет доступных историй.</span>
            )}
          </div>
          <button
            ref={createRef}
            type="button"
            className="btn primary chat-new"
            onClick={onCreate}
            disabled={createLocked}
          >
            + Новый чат
          </button>
          {loading && <span className="chat-sidebar-status">Обновляю...</span>}
        </div>

        {error && <div className="chat-sidebar-error">{error}</div>}

        <nav className="chat-list" aria-label="Предыдущие чаты">
          {sortedChats.length === 0 && !loading ? (
            <div className="chat-sidebar-empty">Сохраненных чатов пока нет.</div>
          ) : (
            sortedChats.map((chat) => {
              const active = chat.active || sameChatId(chat.id, activeChatId);
              return (
                <button
                  key={chat.id}
                  type="button"
                  className={"chat-history-row" + (active ? " active" : "")}
                  onClick={() => {
                    if (!active) onActivate(chat.id);
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
              );
            })
          )}
        </nav>
      </aside>
    </>
  );
}
