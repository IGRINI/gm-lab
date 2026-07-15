import Icon from "./Icon.jsx";
import { useEffect, useMemo, useRef, useState } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";

// Games-only sidebar for the redesigned shell (§Игра in the TZ). The old omnibus
// (Чаты/Миры/Персонажи tabs + story/character pickers + import/export) is gone:
// worlds/stories/characters now live in the Библиотека screen, and a game is
// created only through the New-Game wizard («+ Новая игра»). This panel just
// lists the saved games and lets the player open or delete one.

const DATE_FORMATTER = new Intl.DateTimeFormat("ru-RU", {
  day: "2-digit",
  month: "short",
  hour: "2-digit",
  minute: "2-digit",
});

function chatTitle(chat) {
  return chat?.title?.trim() || "Новая игра";
}

function chatPreview(chat) {
  return chat?.preview?.trim() || "Пустая игра";
}

function chatDate(chat) {
  const raw = chat?.updated_at || chat?.created_at;
  if (!raw) return "";
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) return "";
  return DATE_FORMATTER.format(date).replace(".", "");
}

// Russian plural: 1 ход, 2–4 хода, 0/5–20 ходов (11–14 always "many").
function pluralRu(n, one, few, many) {
  const mod100 = n % 100;
  const mod10 = n % 10;
  if (mod100 >= 11 && mod100 <= 14) return many;
  if (mod10 === 1) return one;
  if (mod10 >= 2 && mod10 <= 4) return few;
  return many;
}

function turnCount(chat) {
  const count = Number(chat?.turn_count || 0);
  return `${count} ${pluralRu(count, "ход", "хода", "ходов")}`;
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
  onNewGame,
  onActivate,
  onDelete,
  // Разделы приложения: на мобилке дровер — единственная навигация
  // (нав-пилюля хедера спрятана), поэтому секция разделов живёт здесь.
  mainView = "chat",
  onNavGame,
  onNavLibrary,
  onNavImage,
  imageLabEnabled = false,
}) {
  const closeRef = useRef(null);
  const newGameRef = useRef(null);
  const [confirmTarget, setConfirmTarget] = useState(null);
  const [deleting, setDeleting] = useState(false);

  const sortedChats = useMemo(() => (Array.isArray(chats) ? chats : []), [chats]);
  const locked = busy || loading;
  const isGameView = mainView === "chat";
  const isImageView = mainView === "image";
  const isLibraryView = !isGameView && !isImageView;
  // Выбор раздела закрывает дровер (актуально только на мобилке — на десктопе
  // секция разделов скрыта CSS'ом и сюда не попасть).
  const pickNav = (fn) => () => {
    fn?.();
    onClose?.();
  };
  const confirmItem = sortedChats.find((chat) => sameChatId(chat.id, confirmTarget?.id)) || null;

  const cancelDelete = () => {
    if (!deleting) setConfirmTarget(null);
  };
  const confirmDelete = async () => {
    if (!confirmItem || deleting) return;
    setDeleting(true);
    try {
      await onDelete?.(confirmItem.id);
      setConfirmTarget(null);
    } finally {
      setDeleting(false);
    }
  };

  // Confirm dialog owns Escape while open (capture phase, so it beats the sidebar).
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

  // Only the mobile drawer needs focus-trapping + Esc-to-close. On desktop the
  // sidebar is a docked, collapsible column.
  useEffect(() => {
    if (!open || typeof document === "undefined") return undefined;
    if (typeof window !== "undefined" && !window.matchMedia("(max-width: 700px)").matches) {
      return undefined;
    }
    const previousFocus = document.activeElement;
    const onKeyDown = (event) => {
      if (event.key !== "Escape") return;
      if (confirmTarget) return;
      event.preventDefault();
      onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    const raf = window.requestAnimationFrame(() => {
      (closeRef.current || newGameRef.current)?.focus({ preventScroll: true });
    });
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      window.cancelAnimationFrame(raf);
      if (previousFocus && typeof previousFocus.focus === "function") {
        previousFocus.focus({ preventScroll: true });
      }
    };
  }, [open, onClose, confirmTarget]);

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
        className={
          "chat-sidebar" +
          (open ? " is-open" : "") +
          (isGameView ? "" : " sidebar-off-game")
        }
        aria-label="Игры"
      >
        <nav className="sidebar-nav" aria-label="Разделы">
          <button
            type="button"
            className={"sidebar-nav-item" + (isGameView ? " is-current" : "")}
            onClick={pickNav(onNavGame)}
          >
            <Icon name="d20" size={15} /> Игра
          </button>
          <button
            type="button"
            className={"sidebar-nav-item" + (isLibraryView ? " is-current" : "")}
            onClick={pickNav(onNavLibrary)}
          >
            <Icon name="book" size={15} /> Библиотека
          </button>
          {imageLabEnabled && (
            <button
              type="button"
              className={"sidebar-nav-item" + (isImageView ? " is-current" : "")}
              onClick={pickNav(onNavImage)}
            >
              <Icon name="image" size={15} /> Image Lab
            </button>
          )}
        </nav>
        <div className="chat-sidebar-head">
          <div>
            <h2>Мои игры</h2>
          </div>
          <button
            ref={closeRef}
            type="button"
            className="icon-btn chat-sidebar-close"
            onClick={onClose}
            aria-label="Закрыть боковую панель"
          >
            <Icon name="x" size={15} />
          </button>
        </div>

        <div className="chat-sidebar-actions">
          <button
            ref={newGameRef}
            type="button"
            className="btn primary chat-new"
            onClick={onNewGame}
            disabled={busy}
          >
            <Icon name="plus" size={15} /> Новая игра
          </button>
          {loading && <span className="chat-sidebar-status">Обновляю…</span>}
        </div>

        {error && <div className="chat-sidebar-error">{error}</div>}

        <nav className="chat-list" aria-label="Сохранённые игры">
          {sortedChats.length === 0 && !loading ? (
            <div className="chat-sidebar-empty">
              Сохранённых игр пока нет. Нажмите «+ Новая игра», чтобы начать.
            </div>
          ) : null}
          {sortedChats.map((chat) => {
            const active = chat.active || sameChatId(chat.id, activeChatId);
            return (
              <div key={chat.id} className={"chat-history-item" + (active ? " active" : "")}>
                <button
                  type="button"
                  className={"chat-history-row" + (active ? " active" : "")}
                  onClick={() => onActivate(chat.id)}
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
                    {active && <span>активная</span>}
                  </span>
                </button>
                <Tooltip
                  className="tooltip-wrap"
                  tipClassName="ui-tip-wrap"
                  focusable={false}
                  content={
                    <TipContent
                      title="Удалить игру"
                      subtitle={chatTitle(chat)}
                      note="Перед удалением появится подтверждение."
                    />
                  }
                >
                  <button
                    type="button"
                    className="chat-row-delete"
                    aria-label={`Удалить игру: ${chatTitle(chat)}`}
                    onClick={() => setConfirmTarget({ kind: "chat", id: chat.id })}
                    disabled={locked}
                  >
                    <Icon name="trash" size={14} />
                  </button>
                </Tooltip>
              </div>
            );
          })}
        </nav>
      </aside>

      {confirmItem && (
        <div className="confirm-backdrop" role="presentation" onMouseDown={cancelDelete}>
          <div
            className="confirm-card"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="confirm-delete-title"
            aria-describedby="confirm-delete-note"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <div className="confirm-icon" aria-hidden="true"><Icon name="trash" size={19} /></div>
            <h3 id="confirm-delete-title">Удалить игру?</h3>
            <p className="confirm-name">«{chatTitle(confirmItem)}»</p>
            <p id="confirm-delete-note" className="confirm-note">
              Игра и все её данные удалятся из базы безвозвратно — история, персонажи, мир и
              связанные эмбеддинги. Это действие нельзя отменить.
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
    </>
  );
}
