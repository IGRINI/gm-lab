import { useEffect, useRef } from "react";
import Icon from "./Icon.jsx";
import { useTranslation } from "react-i18next";

const FIELD_OPTIONS = ["all", "title", "world", "story", "character", "messages"];

function EntitySelect({ label, value, options, onChange }) {
  const { t } = useTranslation("game");
  return (
    <label className="chat-filter-field">
      <span>{label}</span>
      <select value={value || ""} onChange={(event) => onChange(event.target.value)}>
        <option value="">{t("filters.any")}</option>
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
  const { t } = useTranslation("game");
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
    <div ref={ref} className="chat-filter-popover" role="dialog" aria-label={t("filters.dialogAria")}>
      <div className="chat-filter-head">
        <strong>{t("filters.title")}</strong>
        <button type="button" className="icon-btn" onClick={onClose} aria-label={t("filters.closeAria")}>
          <Icon name="x" size={14} />
        </button>
      </div>
      <div className="chat-filter-grid">
        <label className="chat-filter-field chat-filter-field--wide">
          <span>{t("filters.field")}</span>
          <select value={filters?.field || "all"} onChange={(event) => update("field", event.target.value)}>
            {FIELD_OPTIONS.map((value) => (
              <option key={value} value={value}>{t(`filters.fields.${value}`)}</option>
            ))}
          </select>
        </label>
        <EntitySelect label={t("filters.world")} value={filters?.world_id} options={options?.worlds} onChange={(value) => update("world_id", value)} />
        <EntitySelect label={t("filters.story")} value={filters?.story_id} options={options?.stories} onChange={(value) => update("story_id", value)} />
        <EntitySelect label={t("filters.character")} value={filters?.character_id} options={options?.characters} onChange={(value) => update("character_id", value)} />
        <label className="chat-filter-field">
          <span>{t("filters.period")}</span>
          <select value={filters?.period || ""} onChange={(event) => update("period", event.target.value)}>
            <option value="">{t("filters.periods.all")}</option>
            <option value="7d">{t("filters.periods.days7")}</option>
            <option value="30d">{t("filters.periods.days30")}</option>
            <option value="90d">{t("filters.periods.days90")}</option>
          </select>
        </label>
        <label className="chat-filter-field">
          <span>{t("filters.sort")}</span>
          <select value={filters?.sort || "relevance"} onChange={(event) => update("sort", event.target.value)}>
            <option value="relevance">{t("filters.sorts.relevance")}</option>
            <option value="updated">{t("filters.sorts.updated")}</option>
          </select>
        </label>
        <label className="chat-filter-check chat-filter-field--wide">
          <input
            type="checkbox"
            checked={Boolean(filters?.has_messages)}
            onChange={(event) => update("has_messages", event.target.checked)}
          />
          <span>{t("filters.hasMessages")}</span>
        </label>
      </div>
      <div className="chat-filter-actions">
        <button type="button" className="btn" onClick={onReset}>{t("filters.reset")}</button>
        <button type="button" className="btn primary" onClick={onClose}>{t("filters.done")}</button>
      </div>
    </div>
  );
}
