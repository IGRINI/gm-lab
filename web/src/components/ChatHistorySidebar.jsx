import Icon from "./Icon.jsx";
import { useEffect, useMemo, useRef, useState } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import SearchField from "./SearchField.jsx";
import SearchSkeleton from "./SearchSkeleton.jsx";
import ChatSearchFilters from "./ChatSearchFilters.jsx";
import useAsyncSearch from "../useAsyncSearch.js";
import { useTranslation } from "react-i18next";

// Games-only sidebar for the redesigned shell (§Игра in the TZ). The old omnibus
// (Чаты/Миры/Персонажи tabs + story/character pickers + import/export) is gone:
// worlds/stories/characters now live in the Библиотека screen, and a game is
// created only through the New-Game wizard («+ Новая игра»). This panel just
// lists the saved games and lets the player open or delete one.

function chatTitle(chat, t) {
  return chat?.title?.trim() || t("history.untitled");
}

function chatPreview(chat, t) {
  return chat?.snippet?.trim() || chat?.preview?.trim() || chat?.subtitle?.trim()
    || t("history.emptyPreview");
}

function chatSecondary(chat, searchActive, t) {
  if (searchActive) {
    const snippet = chat?.snippet?.trim();
    if (snippet) return snippet;
    const context = chatContext(chat);
    if (context) return context;
  }
  return chat?.preview?.trim() || chat?.subtitle?.trim() || chatPreview(chat, t);
}

function chatContext(chat) {
  const values = [chat?.world_title, chat?.story_title, chat?.character_title || chat?.character_name]
    .map((value) => (typeof value === "string" ? value.trim() : ""))
    .filter(Boolean);
  return [...new Set(values)].join(" · ");
}

function chatDate(chat, formatter) {
  const raw = chat?.updated_at || chat?.created_at;
  if (!raw) return "";
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) return "";
  return formatter.format(date);
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
  const { i18n, t } = useTranslation("game");
  const dateFormatter = useMemo(
    () => new Intl.DateTimeFormat(i18n.resolvedLanguage || i18n.language, {
      day: "numeric",
      month: "short",
    }),
    [i18n.language, i18n.resolvedLanguage]
  );
  const closeRef = useRef(null);
  const newGameRef = useRef(null);
  const filterButtonRef = useRef(null);
  const [confirmTarget, setConfirmTarget] = useState(null);
  const [deleting, setDeleting] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [filtersOpen, setFiltersOpen] = useState(false);
  const [searchFilters, setSearchFilters] = useState({
    field: "all",
    world_id: "",
    story_id: "",
    character_id: "",
    period: "",
    has_messages: false,
    sort: "relevance",
  });

  const sortedChats = useMemo(() => (Array.isArray(chats) ? chats : []), [chats]);
  const locked = busy;
  const filterCount = Object.entries(searchFilters).filter(([key, value]) => {
    if (key === "field") return value !== "all";
    if (key === "sort") return value !== "relevance";
    return Boolean(value);
  }).length;
  const searchActive = searchQuery.trim().length > 0 || filterCount > 0;
  const searchRefresh = useMemo(
    () => sortedChats.map((chat) => `${chat?.id || ""}:${chat?.updated_at || ""}`).join("|"),
    [sortedChats]
  );
  const chatSearch = useAsyncSearch(
    {
      q: searchQuery,
      scope: "chats",
      ...searchFilters,
      limit: 50,
      _refresh: searchRefresh,
    },
    { enabled: searchActive, delay: 240 }
  );
  const visibleChats = searchActive ? chatSearch.items : sortedChats;
  const searchOptions = useMemo(() => {
    const unique = (idKey, titleKey, fallbackTitleKey = "") => {
      const map = new Map();
      for (const chat of sortedChats) {
        const id = chat?.[idKey] == null ? "" : String(chat[idKey]).trim();
        const rawLabel = chat?.[titleKey] || (fallbackTitleKey ? chat?.[fallbackTitleKey] : "");
        const label = typeof rawLabel === "string" ? rawLabel.trim() : "";
        if (id && !map.has(id)) map.set(id, label || id);
      }
      return [...map].map(([value, label]) => ({ value, label }));
    };
    return {
      worlds: unique("world_id", "world_title"),
      stories: unique("story_id", "story_title"),
      characters: unique("character_id", "character_title", "character_name"),
    };
  }, [sortedChats]);
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
        aria-label={t("history.closeSidebar")}
        aria-hidden={!open}
      />
      <aside
        id="chat-history-sidebar"
        className={
          "chat-sidebar" +
          (open ? " is-open" : "") +
          (isGameView ? "" : " sidebar-off-game")
        }
        aria-label={t("history.gamesAria")}
      >
        <nav className="sidebar-nav" aria-label={t("history.sectionsAria")}>
          <button
            type="button"
            className={"sidebar-nav-item" + (isGameView ? " is-current" : "")}
            onClick={pickNav(onNavGame)}
          >
            <Icon name="d20" size={15} /> {t("history.nav.game")}
          </button>
          <button
            type="button"
            className={"sidebar-nav-item" + (isLibraryView ? " is-current" : "")}
            onClick={pickNav(onNavLibrary)}
          >
            <Icon name="book" size={15} /> {t("history.nav.library")}
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
          <h2>{t("history.title")}</h2>
          <div className="chat-sidebar-head-actions">
            {loading && (
              <span className="chat-sidebar-status" aria-live="polite">{t("history.refreshing")}</span>
            )}
            <button
              ref={newGameRef}
              type="button"
              className="chat-new"
              onClick={onNewGame}
              disabled={busy}
              aria-label={t("history.newGameAria")}
            >
              <Icon name="plus" size={14} />
              <span>{t("history.newGameShort")}</span>
            </button>
            <button
              ref={closeRef}
              type="button"
              className="icon-btn chat-sidebar-close"
              onClick={onClose}
              aria-label={t("history.closeSidebar")}
            >
              <Icon name="x" size={15} />
            </button>
          </div>
        </div>

        <div className="chat-search-block">
          <div className="chat-search-controls">
            <SearchField
              value={searchQuery}
              onChange={setSearchQuery}
              placeholder={t("history.searchPlaceholder")}
              ariaLabel={t("history.searchAria")}
              compact
              loading={chatSearch.revalidating}
            />
            <button
              ref={filterButtonRef}
              type="button"
              className={"chat-filter-trigger" + (filterCount > 0 ? " active" : "")}
              onClick={() => setFiltersOpen((value) => !value)}
              aria-label={t("history.filtersAria")}
              aria-expanded={filtersOpen}
            >
              <Icon name="sliders" size={15} />
              {filterCount > 0 && <span>{filterCount}</span>}
            </button>
          </div>
          <ChatSearchFilters
            open={filtersOpen}
            filters={searchFilters}
            options={searchOptions}
            anchorRef={filterButtonRef}
            onChange={setSearchFilters}
            onClose={() => setFiltersOpen(false)}
            onReset={() => setSearchFilters({
              field: "all",
              world_id: "",
              story_id: "",
              character_id: "",
              period: "",
              has_messages: false,
              sort: "relevance",
            })}
          />
          {searchActive && !chatSearch.initialLoading && !chatSearch.error && (
            <span className="chat-search-summary">{t("history.found", { count: chatSearch.total })}</span>
          )}
        </div>

        {error && <div className="chat-sidebar-error">{error}</div>}

        {searchActive && chatSearch.error && chatSearch.items.length === 0 && (
          <div className="chat-sidebar-error">{chatSearch.error}</div>
        )}

        <nav className="chat-list" aria-label={t("history.savedGamesAria")}>
          {searchActive && chatSearch.initialLoading ? (
            <SearchSkeleton variant="rows" count={5} />
          ) : null}
          {!chatSearch.initialLoading && visibleChats.length === 0 && !loading ? (
            <div className="chat-sidebar-empty">
              {searchActive
                ? t("history.noSearchResults")
                : t("history.noGames")}
            </div>
          ) : null}
          {!chatSearch.initialLoading && visibleChats.map((chat) => {
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
                    <span className="chat-row-title-wrap">
                      {active && <span className="chat-row-active-dot" aria-hidden="true" />}
                      <span className="chat-row-title">{chatTitle(chat, t)}</span>
                    </span>
                    <span className="chat-row-date">{chatDate(chat, dateFormatter)}</span>
                  </span>
                  <span className="chat-row-preview">{chatSecondary(chat, searchActive, t)}</span>
                </button>
                <Tooltip
                  className="tooltip-wrap"
                  tipClassName="ui-tip-wrap"
                  focusable={false}
                  content={
                    <TipContent
                      title={t("history.delete.title")}
                      subtitle={chatTitle(chat, t)}
                      note={t("history.delete.tooltip")}
                    />
                  }
                >
                  <button
                    type="button"
                    className="chat-row-delete"
                    aria-label={t("history.delete.aria", { title: chatTitle(chat, t) })}
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
            <h3 id="confirm-delete-title">{t("history.delete.confirmTitle")}</h3>
            <p className="confirm-name">{t("history.delete.quotedName", { title: chatTitle(confirmItem, t) })}</p>
            <p id="confirm-delete-note" className="confirm-note">
              {t("history.delete.confirmNote")}
            </p>
            <div className="confirm-actions">
              <button type="button" className="btn" onClick={cancelDelete} disabled={deleting} autoFocus>
                {t("actions.cancel")}
              </button>
              <button
                type="button"
                className="btn confirm-danger"
                onClick={confirmDelete}
                disabled={deleting}
              >
                {deleting ? t("history.delete.deleting") : t("history.delete.action")}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
