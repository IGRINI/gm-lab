import { useEffect, useMemo, useState } from "react";

function modelLabel(m) {
  const base = m.name && m.name !== m.id ? `${m.name} · ${m.id}` : m.id;
  return m.supported === false ? `${base} · экспериментальная` : base;
}

const ROLE_CONFIG = [
  {
    key: "gm",
    title: "Гейм-мастер",
    note: "Решает, что происходит в сцене, какие тулы вызвать и что сказать игроку.",
  },
  {
    key: "npc",
    title: "Персонажи",
    note: "Отыгрывает конкретного персонажа: речь, действия, память и скрытые мотивы.",
  },
  {
    key: "compact",
    title: "Компакт",
    note: "Сжимает старую историю. Обычно сюда не нужен высокий reasoning.",
  },
];

const LABELS = {
  none: "выкл",
  minimal: "минимум",
  low: "низкая",
  medium: "средняя",
  high: "высокая",
  xhigh: "очень высокая",
  auto: "авто",
  concise: "кратко",
  detailed: "подробно",
  default: "по умолчанию",
  required: "обязательно",
};

function settingLabel(value) {
  return LABELS[value] || value;
}

function effortFromItem(item) {
  if (!item) return "";
  if (typeof item === "string") return item;
  if (typeof item === "object") return item.effort || item.id || item.value || "";
  return String(item);
}

function modelReasoningEfforts(model, fallback, current) {
  if (model?.supports_reasoning_summaries === false) return ["none"];
  const raw = model?.supported_reasoning_levels || model?.supported_reasoning_efforts || [];
  const fromModel = Array.isArray(raw)
    ? raw.map(effortFromItem).filter(Boolean)
    : [];
  const base = fromModel.length ? fromModel : fallback;
  const out = ["none"];
  for (const effort of base) {
    if (effort && !out.includes(effort)) out.push(effort);
  }
  for (const item of Array.isArray(current) ? current : [current]) {
    if (item && !out.includes(item)) out.push(item);
  }
  return out;
}

function summaryOptionsFor(model, fallback, effort, current) {
  if (effort === "none" || model?.supports_reasoning_summaries === false) return ["none"];
  const out = [...fallback];
  if (current && !out.includes(current)) out.push(current);
  return out;
}

function roleField(role, field) {
  return `${role}_reasoning_${field}`;
}

function RoleReasoningFields({ role, draft, settingsOptions, currentModel, setRole }) {
  const effortKey = roleField(role, "effort");
  const summaryKey = roleField(role, "summary");
  const effort = draft[effortKey] || "none";
  const summary = draft[summaryKey] || "none";
  const effortOptions = modelReasoningEfforts(
    currentModel,
    settingsOptions.reasoning_efforts || [],
    effort
  );
  const effortValue = effortOptions.includes(effort) ? effort : effortOptions[0];
  const summaryOptions = summaryOptionsFor(
    currentModel,
    settingsOptions.reasoning_summaries || [],
    effortValue,
    summary
  );
  const summaryValue = summaryOptions.includes(summary) ? summary : summaryOptions[0];

  return (
    <>
      <label className="field">
        <span>Думалка</span>
        <select
          value={effortValue}
          onChange={(e) => {
            const next = e.target.value;
            setRole(role, {
              effort: next,
              summary: next === "none" ? "none" : summary,
            });
          }}
        >
          {effortOptions.map((value) => (
            <option key={value} value={value}>{settingLabel(value)}</option>
          ))}
        </select>
      </label>

      <label className="field">
        <span>Заметки думалки</span>
        <select
          value={summaryValue}
          disabled={effortValue === "none"}
          onChange={(e) => setRole(role, { summary: e.target.value })}
        >
          {summaryOptions.map((value) => (
            <option key={value} value={value}>{settingLabel(value)}</option>
          ))}
        </select>
      </label>
    </>
  );
}

function SettingsModal({ settings, settingsOptions, currentModel, onApply, onClose }) {
  const [draft, setDraft] = useState(settings);

  useEffect(() => {
    setDraft(settings);
  }, [settings]);

  const set = (patch) => setDraft((prev) => ({ ...prev, ...patch }));
  const setRole = (role, patch) => {
    const next = {};
    if ("effort" in patch) next[roleField(role, "effort")] = patch.effort;
    if ("summary" in patch) next[roleField(role, "summary")] = patch.summary;
    set(next);
  };
  const submit = (e) => {
    e.preventDefault();
    onApply(draft);
    onClose();
  };

  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={onClose}>
      <form className="settings-modal" onSubmit={submit} onMouseDown={(e) => e.stopPropagation()}>
        <div className="modal-head">
          <h2>Настройки модели</h2>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Закрыть">
            x
          </button>
        </div>

        {ROLE_CONFIG.map((role) => (
          <section className="settings-section" key={role.key}>
            <h3>{role.title}</h3>
            <p>{role.note}</p>
            <RoleReasoningFields
              role={role.key}
              draft={draft}
              settingsOptions={settingsOptions}
              currentModel={currentModel}
              setRole={setRole}
            />
          </section>
        ))}

        <section className="settings-section">
          <h3>Общее</h3>
          <p>Эти параметры применяются ко всем запросам модели.</p>

        <label className="field">
          <span>Многословность текста</span>
          <select
            value={draft.text_verbosity || "default"}
            onChange={(e) => set({ text_verbosity: e.target.value })}
          >
            {(settingsOptions.text_verbosities || []).map((value) => (
              <option key={value} value={value}>{settingLabel(value)}</option>
            ))}
          </select>
        </label>

        <label className="field">
          <span>Тулы</span>
          <select
            value={draft.tool_choice || "auto"}
            onChange={(e) => set({ tool_choice: e.target.value })}
          >
            {(settingsOptions.tool_choices || []).map((value) => (
              <option key={value} value={value}>{settingLabel(value)}</option>
            ))}
          </select>
        </label>

        <label className="field check-field">
          <span>Параллельные тулы</span>
          <input
            type="checkbox"
            checked={!!draft.parallel_tool_calls}
            onChange={(e) => set({ parallel_tool_calls: e.target.checked })}
          />
        </label>

        <label className="field">
          <span>Лимит ответа</span>
          <input
            type="number"
            min="0"
            max={settingsOptions.max_output_tokens_max || undefined}
            step="256"
            value={draft.max_output_tokens || 0}
            onChange={(e) => set({ max_output_tokens: Number(e.target.value || 0) })}
          />
        </label>
        </section>

        <div className="modal-actions">
          <button type="button" className="btn" onClick={onClose}>Отмена</button>
          <button type="submit" className="btn primary">Сохранить</button>
        </div>
      </form>
    </div>
  );
}

export default function Header({
  onToggleChats,
  srv,
  models,
  settings,
  settingsOptions,
  onModelChange,
  onSettingsChange,
  onCodex,
  onLogout,
  onExport,
  onReset,
}) {
  const isCodex = srv.backend === "codex";
  const codexOk = !!(srv.codex_auth && srv.codex_auth.authenticated);
  const [settingsOpen, setSettingsOpen] = useState(false);

  // Ensure the current model is always selectable, even if not in the catalog.
  const options = useMemo(() => {
    const list = (models || []).map((m) => ({ id: m.id, label: modelLabel(m) }));
    if (srv.model && !list.some((o) => o.id === srv.model)) {
      list.unshift({ id: srv.model, label: srv.model });
    }
    return list;
  }, [models, srv.model]);

  const currentModel = useMemo(
    () => (models || []).find((m) => m.id === srv.model || m.slug === srv.model) || null,
    [models, srv.model]
  );

  const chip = srv.backend ? srv.backend + (srv.stream_gm_content ? " · GM stream" : "") : "…";

  return (
    <header>
      {onToggleChats && (
        <button
          type="button"
          className="btn btn-icon chat-toggle"
          onClick={onToggleChats}
          title="Список чатов"
          aria-label="Открыть список чатов"
        >
          <span className="bi" aria-hidden="true">☰</span>
          <span className="btn-label">Чаты</span>
        </button>
      )}
      <h1>
        GM-<b>Lab</b>
      </h1>
      <span className="chip">{chip}</span>
      <select
        className="model-select"
        title="Модель"
        value={srv.model || ""}
        onChange={(e) => onModelChange(e.target.value)}
      >
        {options.map((o) => (
          <option key={o.id} value={o.id}>
            {o.label}
          </option>
        ))}
      </select>
      <div className="spacer" />
      <div className="header-actions">
        <button className="btn" onClick={() => setSettingsOpen(true)}>
          Настройки
        </button>
        {isCodex && (
          <button
            className={"btn" + (codexOk ? " auth-ok" : "")}
            onClick={onCodex}
            title={codexOk ? "Codex подключён" : "Подключить Codex"}
          >
            <span className="btn-label">{codexOk ? "Codex подключён" : "Подключить Codex"}</span>
            <span className="btn-short">{codexOk ? "Codex ✓" : "Codex"}</span>
          </button>
        )}
        {isCodex && codexOk && (
          <button className="btn" onClick={onLogout}>
            Выйти
          </button>
        )}
        <button className="btn btn-icon" onClick={onExport} title="Скачать JSON" aria-label="Скачать JSON">
          <span className="bi" aria-hidden="true">⬇</span>
          <span className="btn-label">JSON</span>
        </button>
        <button className="btn btn-icon" onClick={onReset} title="Сброс партии" aria-label="Сброс партии">
          <span className="bi" aria-hidden="true">⟲</span>
          <span className="btn-label">Сброс</span>
        </button>
      </div>
      {settingsOpen && (
        <SettingsModal
          settings={settings}
          settingsOptions={settingsOptions}
          currentModel={currentModel}
          onApply={onSettingsChange}
          onClose={() => setSettingsOpen(false)}
        />
      )}
    </header>
  );
}
