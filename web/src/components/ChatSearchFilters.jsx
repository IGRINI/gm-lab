import { useEffect, useRef } from "react";
import Icon from "./Icon.jsx";

const FIELD_OPTIONS = [
  ["all", "Везде"],
  ["title", "В названиях"],
  ["world", "В мирах"],
  ["story", "В историях"],
  ["character", "В персонажах"],
  ["messages", "В сообщениях"],
];

function EntitySelect({ label, value, options, onChange }) {
  return (
    <label className="chat-filter-field">
      <span>{label}</span>
      <select value={value || ""} onChange={(event) => onChange(event.target.value)}>
        <option value="">Любой</option>
        {(options || []).map((option) => (
          <option key={option.value} value={option.value}>{option.label}</option>
        ))}
      </select>
    </label>
  );
}

export default function ChatSearchFilters({
  open,
  filters,
  options,
  anchorRef,
  onChange,
  onClose,
  onReset,
}) {
  const ref = useRef(null);
  const update = (key, value) => onChange?.({ ...filters, [key]: value });

  useEffect(() => {
    if (!open) return undefined;
    const onPointer = (event) => {
      if (ref.current?.contains(event.target) || anchorRef?.current?.contains(event.target)) return;
      onClose?.();
    };
    const onKey = (event) => {
      if (event.key === "Escape") onClose?.();
    };
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, onClose, anchorRef]);

  if (!open) return null;
  return (
    <div ref={ref} className="chat-filter-popover" role="dialog" aria-label="Фильтры поиска игр">
      <div className="chat-filter-head">
        <strong>Фильтры</strong>
        <button type="button" className="icon-btn" onClick={onClose} aria-label="Закрыть фильтры">
          <Icon name="x" size={14} />
        </button>
      </div>
      <div className="chat-filter-grid">
        <label className="chat-filter-field chat-filter-field--wide">
          <span>Где искать</span>
          <select value={filters?.field || "all"} onChange={(event) => update("field", event.target.value)}>
            {FIELD_OPTIONS.map(([value, label]) => <option key={value} value={value}>{label}</option>)}
          </select>
        </label>
        <EntitySelect label="Мир" value={filters?.world_id} options={options?.worlds} onChange={(value) => update("world_id", value)} />
        <EntitySelect label="История" value={filters?.story_id} options={options?.stories} onChange={(value) => update("story_id", value)} />
        <EntitySelect label="Персонаж" value={filters?.character_id} options={options?.characters} onChange={(value) => update("character_id", value)} />
        <label className="chat-filter-field">
          <span>Период</span>
          <select value={filters?.period || ""} onChange={(event) => update("period", event.target.value)}>
            <option value="">За всё время</option>
            <option value="7d">7 дней</option>
            <option value="30d">30 дней</option>
            <option value="90d">90 дней</option>
          </select>
        </label>
        <label className="chat-filter-field">
          <span>Сортировка</span>
          <select value={filters?.sort || "relevance"} onChange={(event) => update("sort", event.target.value)}>
            <option value="relevance">По релевантности</option>
            <option value="updated">Сначала новые</option>
          </select>
        </label>
        <label className="chat-filter-check chat-filter-field--wide">
          <input
            type="checkbox"
            checked={Boolean(filters?.has_messages)}
            onChange={(event) => update("has_messages", event.target.checked)}
          />
          <span>Только игры с сообщениями</span>
        </label>
      </div>
      <div className="chat-filter-actions">
        <button type="button" className="btn" onClick={onReset}>Сбросить</button>
        <button type="button" className="btn primary" onClick={onClose}>Готово</button>
      </div>
    </div>
  );
}
