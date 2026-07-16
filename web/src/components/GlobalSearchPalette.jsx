import { useEffect, useRef, useState } from "react";
import Icon from "./Icon.jsx";
import SearchField from "./SearchField.jsx";
import SearchSkeleton from "./SearchSkeleton.jsx";
import useAsyncSearch from "../useAsyncSearch.js";
import { useTranslation } from "react-i18next";

const SCOPES = ["all", "library", "chats"];

const TYPE_META = {
  world: { icon: "globe" },
  story: { icon: "scroll" },
  character: { icon: "user" },
  chat: { icon: "message" },
};

export default function GlobalSearchPalette({ open, onOpen, onClose, onSelect }) {
  const { t } = useTranslation("game");
  const [query, setQuery] = useState("");
  const [scope, setScope] = useState("all");
  const [activeIndex, setActiveIndex] = useState(0);
  const inputRef = useRef(null);
  const modalRef = useRef(null);
  const previousFocusRef = useRef(null);
  const wasOpenRef = useRef(false);
  const search = useAsyncSearch(
    { q: query, scope, limit: 30 },
    { enabled: open, delay: query.trim() ? 180 : 0 }
  );

  useEffect(() => {
    const onShortcut = (event) => {
      if (!(event.ctrlKey || event.metaKey) || event.key.toLowerCase() !== "k") return;
      event.preventDefault();
      if (!open) onOpen?.();
      else inputRef.current?.focus({ preventScroll: true });
    };
    document.addEventListener("keydown", onShortcut);
    return () => document.removeEventListener("keydown", onShortcut);
  }, [open, onOpen]);

  useEffect(() => {
    if (!open) {
      wasOpenRef.current = false;
      return undefined;
    }
    if (!wasOpenRef.current) {
      previousFocusRef.current = document.activeElement;
      setQuery("");
      setScope("all");
      setActiveIndex(0);
      wasOpenRef.current = true;
    }
    const frame = window.requestAnimationFrame(() => inputRef.current?.focus({ preventScroll: true }));
    const onKey = (event) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose?.();
        return;
      }
      if (event.key !== "Tab" || !modalRef.current) return;
      const focusable = [...modalRef.current.querySelectorAll(
        'button:not(:disabled), input:not(:disabled), select:not(:disabled), [tabindex]:not([tabindex="-1"])'
      )];
      if (focusable.length === 0) return;
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      window.cancelAnimationFrame(frame);
      document.removeEventListener("keydown", onKey, true);
      const previous = previousFocusRef.current;
      if (previous && typeof previous.focus === "function") previous.focus({ preventScroll: true });
    };
  }, [open, onClose]);

  useEffect(() => setActiveIndex(0), [query, scope]);

  useEffect(() => {
    setActiveIndex((index) => Math.min(index, Math.max(0, search.items.length - 1)));
  }, [search.items.length]);

  if (!open) return null;

  const pick = (item) => {
    if (!item) return;
    onSelect?.(item);
  };
  const onInputKeyDown = (event) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (search.items.length > 0) {
        setActiveIndex((index) => Math.min(search.items.length - 1, index + 1));
      }
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActiveIndex((index) => Math.max(0, index - 1));
    } else if (event.key === "Enter" && search.items[activeIndex]) {
      event.preventDefault();
      pick(search.items[activeIndex]);
    }
  };

  return (
    <div className="global-search-backdrop" role="presentation" onMouseDown={onClose}>
      <section
        ref={modalRef}
        className="global-search-palette"
        role="dialog"
        aria-modal="true"
        aria-labelledby="global-search-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <div className="global-search-head">
          <div>
            <h2 id="global-search-title">{t("globalSearch.title")}</h2>
            <span>{t("globalSearch.subtitle")}</span>
          </div>
          <button type="button" className="icon-btn" onClick={onClose} aria-label={t("globalSearch.closeAria")}>
            <Icon name="x" size={16} />
          </button>
        </div>
        <SearchField
          inputRef={inputRef}
          value={query}
          onChange={setQuery}
          onKeyDown={onInputKeyDown}
          placeholder={t("globalSearch.placeholder")}
          ariaLabel={t("globalSearch.aria")}
          loading={search.revalidating}
        />
        <div className="global-search-scopes" role="tablist" aria-label={t("globalSearch.scopeAria")}>
          {SCOPES.map((value) => (
            <button
              key={value}
              type="button"
              role="tab"
              aria-selected={scope === value}
              className={scope === value ? "active" : ""}
              onClick={() => setScope(value)}
            >
              {t(`globalSearch.scopes.${value}`)}
            </button>
          ))}
        </div>
        <div className="global-search-results" aria-live="polite">
          {search.initialLoading ? <SearchSkeleton variant="rows" count={6} /> : null}
          {!search.initialLoading && search.error && search.items.length === 0 ? (
            <div className="global-search-state is-error">{search.error}</div>
          ) : null}
          {!search.initialLoading && !search.error && search.items.length === 0 ? (
            <div className="global-search-state">
              <Icon name="search" size={22} />
              <strong>{query.trim() ? t("globalSearch.notFound") : t("globalSearch.empty")}</strong>
              <span>{query.trim() ? t("globalSearch.notFoundHint") : t("globalSearch.emptyHint")}</span>
            </div>
          ) : null}
          {!search.initialLoading && search.items.map((item, index) => {
            const type = Object.hasOwn(TYPE_META, item.type) ? item.type : "chat";
            const meta = TYPE_META[type];
            return (
              <button
                key={`${item.type}:${item.id}`}
                type="button"
                className={`global-search-result${activeIndex === index ? " active" : ""}`}
                onClick={() => pick(item)}
                onMouseEnter={() => setActiveIndex(index)}
              >
                <span className="global-search-result-icon"><Icon name={meta.icon} size={16} /></span>
                <span className="global-search-result-copy">
                  <span className="global-search-result-title">
                    <strong>{item.title || t("globalSearch.untitled")}</strong>
                    <em>{t(`globalSearch.types.${type}`)}</em>
                  </span>
                  {item.subtitle && <span className="global-search-result-subtitle">{item.subtitle}</span>}
                  {item.snippet && item.snippet !== item.subtitle && (
                    <span className="global-search-result-snippet">{item.snippet}</span>
                  )}
                </span>
                <Icon name="chevron-right" size={14} className="global-search-result-arrow" />
              </button>
            );
          })}
        </div>
        <div className="global-search-foot">
          <span><kbd>↑</kbd><kbd>↓</kbd> {t("globalSearch.shortcuts.select")}</span>
          <span><kbd>Enter</kbd> {t("globalSearch.shortcuts.open")}</span>
          <span><kbd>Esc</kbd> {t("globalSearch.shortcuts.close")}</span>
          {search.total > search.items.length && (
            <span className="global-search-total">
              {t("globalSearch.shown", { shown: search.items.length, total: search.total })}
            </span>
          )}
        </div>
      </section>
    </div>
  );
}
