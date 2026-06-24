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

export default function ChatHistorySidebar({
  chats,
  activeChatId,
  open,
  busy,
  loading,
  error,
  onClose,
  onCreateWorld,
  onActivate,
  onDelete,
}) {
  const closeRef = useRef(null);
  const createWorldRef = useRef(null);
  const [confirmId, setConfirmId] = useState(null);
  const [deleting, setDeleting] = useState(false);
  const sortedChats = useMemo(() => (Array.isArray(chats) ? chats : []), [chats]);
  const locked = busy || loading;
  const confirmChat = confirmId
    ? sortedChats.find((chat) => sameChatId(chat.id, confirmId)) || null
    : null;

  const cancelDelete = () => {
    if (!deleting) setConfirmId(null);
  };
  const confirmDelete = async () => {
    if (!confirmChat || deleting) return;
    setDeleting(true);
    try {
      await onDelete?.(confirmChat.id);
      setConfirmId(null);
    } finally {
      setDeleting(false);
    }
  };

  // Confirm dialog owns Escape while open (capture phase, so it beats the sidebar handler).
  useEffect(() => {
    if (!confirmId || typeof document === "undefined") return undefined;
    const onKey = (event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      if (!deleting) setConfirmId(null);
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [confirmId, deleting]);

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
      if (confirmId) return; // the confirm dialog handles Escape first
      event.preventDefault();
      onClose();
    };

    document.addEventListener("keydown", onKeyDown);
    const raf = window.requestAnimationFrame(() => {
      const target = closeRef.current || createWorldRef.current;
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
        aria-label="Закрыть список миров"
        aria-hidden={!open}
      />
      <aside
        id="chat-history-sidebar"
        className={"chat-sidebar" + (open ? " is-open" : "")}
        aria-label="Список миров"
      >
        <div className="chat-sidebar-head">
          <div>
            <span>Миры</span>
            <h2>Список миров</h2>
          </div>
          <button
            ref={closeRef}
            type="button"
            className="icon-btn chat-sidebar-close"
            onClick={onClose}
            aria-label="Закрыть список миров"
          >
            x
          </button>
        </div>

        <div className="chat-sidebar-actions world-sidebar-actions">
          <button
            ref={createWorldRef}
            type="button"
            className="btn primary chat-new"
            onClick={onCreateWorld}
            disabled={locked}
          >
            + Создать мир
          </button>
          {loading && <span className="chat-sidebar-status">Обновляю...</span>}
        </div>

        {error && <div className="chat-sidebar-error">{error}</div>}

        <nav className="chat-list" aria-label="Предыдущие миры">
          {sortedChats.length === 0 && !loading ? (
            <div className="chat-sidebar-empty">Сохранённых миров пока нет.</div>
          ) : (
            sortedChats.map((chat) => {
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
                      onClick={() => setConfirmId(chat.id)}
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

      {confirmChat && (
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
            <h3 id="confirm-delete-title">Удалить чат?</h3>
            <p className="confirm-name">«{chatTitle(confirmChat)}»</p>
            <p id="confirm-delete-note" className="confirm-note">
              Чат и все его данные удалятся из базы безвозвратно — история, персонажи,
              мир и связанные эмбеддинги. Это действие нельзя отменить.
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
