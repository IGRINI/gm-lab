import Icon from "./Icon.jsx";
import { useEffect, useMemo, useRef, useState } from "react";
import { useDevSettings, setDeveloperMode, setFlag, FLAG_META } from "../devSettings.js";
import TokenCounter from "./TokenCounter.jsx";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import { ConnectorAuthPrompt } from "./ConnectorModelPicker.jsx";
import {
  authMethods,
  connectorAuthenticated,
  connectorById,
  connectorIdOf,
  connectorName,
  modelIdOf,
  modelLabel,
  modelsForConnector,
  normalizeModelBinding,
} from "../connectorCatalog.js";

function fmtExpiry(raw) {
  const ms = raw > 0 && raw < 1_000_000_000_000 ? raw * 1000 : raw;
  const left = ms - Date.now();
  const when = new Date(ms).toLocaleString("ru-RU", {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
  let rel;
  if (left <= 0) rel = "истёк";
  else if (left < 60 * 60 * 1000) rel = `через ${Math.max(1, Math.round(left / 60000))} мин`;
  else if (left < 24 * 60 * 60 * 1000) rel = `через ${Math.round(left / 3600000)} ч`;
  else rel = `через ${Math.round(left / 86400000)} дн`;
  return `${when} (${rel})`;
}

function connectorStatus(connector) {
  if (!connector) return null;
  const name = connectorName(connector);
  const methods = authMethods(connector);
  const auth = connector.auth || {};
  if (methods.length === 0) {
    return { level: "ok", title: `${name} доступен`, rows: [], note: "Авторизация не требуется." };
  }
  const msg = auth.message || "";
  const exp = typeof auth.expires_at === "number" ? auth.expires_at : null;
  const rows = [];
  if (auth.account_id) rows.push({ k: "Аккаунт", v: String(auth.account_id) });
  if (auth.account_label) rows.push({ k: "Аккаунт", v: String(auth.account_label) });
  if (auth.email) rows.push({ k: "Почта", v: String(auth.email) });

  if (connectorAuthenticated(connector)) {
    if (exp != null) rows.push({ k: "Токен", v: fmtExpiry(exp) });
    return { level: "ok", title: `${name} подключён`, rows, note: msg };
  }

  const status = String(auth.state || auth.status || "").toLowerCase();
  if (status === "pending" || status === "authorizing") {
    return { level: "warn", title: `${name}: подключение…`, rows, note: msg };
  }
  if (status === "expired") {
    return { level: "error", title: `${name}: вход не завершён`, rows, note: msg || "Срок авторизации истёк." };
  }
  if (/invalid|ошиб|error/i.test(msg)) {
    return { level: "error", title: `${name}: ошибка авторизации`, rows, note: msg };
  }
  return { level: "off", title: `${name} не подключён`, rows, note: msg || "Требуется подключение." };
}

function ConnTooltip({ status }) {
  return (
    <div className="conn-tip">
      <div className="conn-tip-head">
        <span className={"conn-dot " + status.level} />
        <b>{status.title}</b>
      </div>
      {status.rows.length > 0 && (
        <div className="conn-tip-rows">
          {status.rows.map((r, i) => (
            <div className="conn-tip-row" key={i}>
              <span className="conn-tip-row-k">{r.k}</span>
              <span className="conn-tip-row-v">{r.v}</span>
            </div>
          ))}
        </div>
      )}
      {status.note && <small className="conn-tip-note">{status.note}</small>}
    </div>
  );
}

function fmtElapsed(ms) {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0) return "";
  const sec = Math.round(ms / 1000);
  if (sec < 60) return `${sec} с`;
  const min = Math.floor(sec / 60);
  const rest = sec % 60;
  return rest ? `${min} мин ${rest} с` : `${min} мин`;
}

function componentLine(component, fallback) {
  if (!component?.enabled) return `${fallback}: выкл`;
  const label = component.up ? "готов" : "загрузка";
  const model = component.model ? ` · ${component.model}` : "";
  const models = Array.isArray(component.models) && component.models.length
    ? ` · ${component.models.join(", ")}`
    : "";
  const quant = component.quant ? ` · ${component.quant}` : "";
  return `${fallback}: ${label}${model}${models}${quant}`;
}

function imageComponentLine(component) {
  if (!component?.enabled) return "Image: выкл";
  const label = component.up ? "готов" : component.runtime_ready ? "прогрев" : "загрузка";
  const models = Array.isArray(component.models) && component.models.length
    ? ` · ${component.models.join(", ")}`
    : "";
  const comfy = component.comfy_up ? " · ComfyUI" : "";
  return `Image: ${label}${models}${comfy}`;
}

function sidecarUiStatus(status) {
  if (!status || status.enabled === false) return null;
  const c = status.components || {};
  const hasRag = !!(c.embedder?.enabled || c.reranker?.enabled);
  const hasTts = !!c.tts?.enabled;
  const hasImage = !!c.image?.enabled;
  const parts = [];
  if (hasRag) parts.push("RAG");
  if (hasTts) parts.push("TTS");
  if (hasImage) parts.push("Image");
  const name = parts.join("/") || "Инференс";
  if (status.ready) {
    return { level: "ok", label: name, title: "Инференс готов" };
  }
  if (status.state === "failed") {
    return { level: "error", label: name, title: "Инференс не загрузился" };
  }
  if (status.state === "unavailable") {
    return { level: "warn", label: name, title: "Статус инференса недоступен" };
  }
  return { level: "warn", label: name, title: "Инференс загружается" };
}

function SidecarTooltip({ status, ui }) {
  const c = status?.components || {};
  const elapsed = fmtElapsed(status?.elapsed_ms);
  const timeout = fmtElapsed(status?.ready_timeout_ms);
  const rows = [
    status?.base_url ? { k: "URL", v: status.base_url } : null,
    status?.pid ? { k: "PID", v: String(status.pid) } : null,
    elapsed ? { k: "Прошло", v: timeout ? `${elapsed} из ${timeout}` : elapsed } : null,
    status?.manager_state ? { k: "Состояние", v: status.manager_state } : null,
  ].filter(Boolean);
  const note =
    status?.error ||
    [
      componentLine(c.embedder, "Эмбеддер"),
      componentLine(c.reranker, "Реранкер"),
      componentLine(c.tts, "TTS"),
      imageComponentLine(c.image),
    ].join("\n");

  return (
    <div className="conn-tip sidecar-tip">
      <div className="conn-tip-head">
        <span className={"conn-dot " + ui.level} />
        <b>{ui.title}</b>
      </div>
      {rows.length > 0 && (
        <div className="conn-tip-rows">
          {rows.map((r, i) => (
            <div className="conn-tip-row" key={i}>
              <span className="conn-tip-row-k">{r.k}</span>
              <span className="conn-tip-row-v">{r.v}</span>
            </div>
          ))}
        </div>
      )}
      <small className="conn-tip-note">{note}</small>
    </div>
  );
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

function ToggleField({ label, hint, checked, onChange }) {
  return (
    <label className="field check-field toggle-field">
      <span className="toggle-label">
        {label}
        {hint ? <small>{hint}</small> : null}
      </span>
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
    </label>
  );
}

function ConnectorSettingsCard({
  connector,
  busy,
  cancelling,
  authPrompt,
  onAuthStart,
  onAuthCancel,
  onLogout,
}) {
  const methods = authMethods(connector);
  const authenticated = connectorAuthenticated(connector);
  const status = connectorStatus(connector);
  return (
    <article className="connector-settings-card">
      <div className="connector-settings-head">
        <div>
          <h3>{connectorName(connector)}</h3>
          {connector.description && <p>{connector.description}</p>}
        </div>
        <span className="conn-status-line">
          <span className={`conn-dot ${status?.level || "off"}`} />
          <span className={`conn-status ${status?.level || "off"}`}>
            {status?.title || "Состояние неизвестно"}
          </span>
        </span>
      </div>
      {status?.note && <p className="settings-note conn-tab-note">{status.note}</p>}
      <ConnectorAuthPrompt prompt={authPrompt} connectorId={connectorIdOf(connector)} />
      <div className="connector-settings-actions">
        {!authenticated && busy ? (
          <button
            type="button"
            className="btn"
            disabled={cancelling || typeof onAuthCancel !== "function"}
            onClick={() => onAuthCancel?.(connectorIdOf(connector))}
          >
            {cancelling ? "Отменяю…" : "Отменить подключение"}
          </button>
        ) : !authenticated && methods.map((method) => (
          <button
            key={method.id}
            type="button"
            className="btn primary"
            onClick={() => onAuthStart?.(connectorIdOf(connector), method.id)}
          >
            {methods.length === 1 ? "Подключить" : `Подключить · ${method.name}`}
          </button>
        ))}
        {authenticated && methods.length > 0 && (
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={() => onLogout?.(connectorIdOf(connector))}
          >
            {busy ? "Отключение…" : "Выйти"}
          </button>
        )}
      </div>
    </article>
  );
}

function SettingsModal({
  settings,
  settingsOptions,
  currentModel,
  connectors,
  connectorAuthBusyIds,
  connectorAuthCancellingIds,
  connectorAuthPrompts,
  onApply,
  onClose,
  onOpenTokenCounter,
  onConnectorAuthStart,
  onConnectorAuthCancel,
  onConnectorLogout,
}) {
  const [draft, setDraft] = useState(settings);
  const dev = useDevSettings();
  const [tab, setTab] = useState("model");

  useEffect(() => {
    setDraft(settings);
  }, [settings]);

  // Leaving developer mode hides the debug-view tab; bounce off it if it's open.
  useEffect(() => {
    if (!dev.developerMode && tab === "debug") setTab("view");
  }, [dev.developerMode, tab]);

  // The connection tab follows the generic connector catalog.
  useEffect(() => {
    if ((!connectors || connectors.length === 0) && tab === "connection") setTab("model");
  }, [connectors, tab]);

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

  const tabs = [
    { id: "model", label: "Модель" },
    { id: "view", label: "Интерфейс" },
    ...((connectors || []).length > 0 ? [{ id: "connection", label: "Подключения" }] : []),
    ...(dev.developerMode ? [{ id: "debug", label: "Дебаг-вид" }] : []),
  ];

  const activeTab = tabs.find((t) => t.id === tab) || tabs[0];

  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={onClose}>
      <form className="settings-modal" onSubmit={submit} onMouseDown={(e) => e.stopPropagation()}>
        <div className="settings-side">
          <div className="settings-side-head">Настройки</div>
          <nav className="settings-tabs" role="tablist" aria-label="Разделы настроек">
            {tabs.map((t) => (
              <button
                key={t.id}
                type="button"
                role="tab"
                aria-selected={tab === t.id}
                className={"settings-tab-btn" + (tab === t.id ? " active" : "")}
                onClick={() => setTab(t.id)}
              >
                {t.label}
              </button>
            ))}
          </nav>
        </div>

        <div className="settings-main">
        <div className="settings-main-head">
          <h2>{activeTab.label}</h2>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Закрыть">
            <Icon name="x" size={15} />
          </button>
        </div>

        <div className="settings-main-body">
        {tab === "view" && (
          <section className="settings-section">
            <h3>Режим разработчика</h3>
            <p>
              Выключен — чистый вид для игрока: без счётчиков токенов, вызовов
              инструментов, мыслей ГМ и персонажей, операций с памятью мира и
              панели дебага истории. Включи, чтобы видеть всю «кухню».
            </p>
            <ToggleField
              label="Режим разработчика"
              hint="По умолчанию выключен."
              checked={dev.developerMode}
              onChange={setDeveloperMode}
            />
            {dev.developerMode && (
              <p className="settings-note">
                Что именно показывать — на вкладке «Дебаг-вид». Переключатели применяются сразу.
              </p>
            )}
          </section>
        )}

        {tab === "debug" && dev.developerMode && (
          <>
            <section className="settings-section">
              <h3>Что показывать</h3>
              <p>Тонкая настройка видимости интерфейса. Изменения применяются в реальном времени.</p>
              {FLAG_META.map((flag) => (
                <ToggleField
                  key={flag.key}
                  label={flag.label}
                  hint={flag.hint}
                  checked={dev.flags[flag.key] !== false}
                  onChange={(on) => setFlag(flag.key, on)}
                />
              ))}
            </section>
            <section className="settings-section">
              <h3>Инструменты</h3>
              <p>Подсчёт токенов текста через OpenAI API (нужен сохранённый API-ключ).</p>
              <button type="button" className="btn" onClick={() => onOpenTokenCounter?.()}>
                🔢 Подсчёт токенов
              </button>
            </section>
          </>
        )}

        {tab === "connection" && (
          <section className="settings-section">
            <p>Каждый коннектор сам управляет способом входа и своими моделями.</p>
            <div className="connector-settings-list">
              {(connectors || []).map((connector) => (
                <ConnectorSettingsCard
                  key={connectorIdOf(connector)}
                  connector={connector}
                  busy={connectorAuthBusyIds.includes(connectorIdOf(connector))}
                  cancelling={connectorAuthCancellingIds.includes(connectorIdOf(connector))}
                  authPrompt={connectorAuthPrompts[connectorIdOf(connector)] || null}
                  onAuthStart={onConnectorAuthStart}
                  onAuthCancel={onConnectorAuthCancel}
                  onLogout={onConnectorLogout}
                />
              ))}
            </div>
          </section>
        )}

        {tab === "model" && (
        <>
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
          <span>Стримить текст ГМ</span>
          <input
            type="checkbox"
            checked={draft.stream_gm_content !== false}
            onChange={(e) => set({ stream_gm_content: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>Параллельные тулы</span>
          <input
            type="checkbox"
            checked={!!draft.parallel_tool_calls}
            onChange={(e) => set({ parallel_tool_calls: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>ГМ будет предлагать варианты</span>
          <input
            type="checkbox"
            checked={!!draft.gm_suggest_options}
            onChange={(e) => set({ gm_suggest_options: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>Озвучка реплик (TTS)</span>
          <input
            type="checkbox"
            checked={!!draft.tts_enabled}
            onChange={(e) => set({ tts_enabled: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>Автовоспроизведение озвучки (по очереди)</span>
          <input
            type="checkbox"
            checked={!!draft.tts_autoplay}
            onChange={(e) => set({ tts_autoplay: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>Генерация картинок</span>
          <input
            type="checkbox"
            checked={draft.image_enabled !== false}
            onChange={(e) => set({ image_enabled: e.target.checked })}
          />
        </label>

        <label className="field">
          <span>Лимит tool-hop</span>
          <Tooltip
            className="tooltip-block"
            tipClassName="ui-tip-wrap"
            focusable={false}
            content={
              <TipContent
                title="Лимит tool-hop"
                subtitle="Сколько внутренних инструментов ГМ может вызвать за один ход."
                rows={[["0", "без ограничения"]]}
                note="Поставь число, если нужно жёстко остановить слишком длинную цепочку действий."
              />
            }
          >
            <input
              type="number"
              min="0"
              max={settingsOptions.max_tool_hops_max || undefined}
              step="1"
              value={draft.max_tool_hops || 0}
              onChange={(e) => set({ max_tool_hops: Number(e.target.value || 0) })}
            />
          </Tooltip>
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
        </>
        )}

        </div>

        <div className="modal-actions">
          <button type="button" className="btn" onClick={onClose}>
            {tab === "model" ? "Отмена" : "Закрыть"}
          </button>
          {tab === "model" && <button type="submit" className="btn primary">Сохранить</button>}
        </div>
        </div>
      </form>
    </div>
  );
}

export default function Header({
  onToggleChats,
  chatsOpen = false,
  showChatToggle = true,
  mainView = "chat",
  onNavGame,
  onNavLibrary,
  onNavImage,
  onOpenSearch,
  imageLabEnabled = false,
  srv,
  sidecarStatus,
  connectors,
  models,
  connectorModelsLoadingIds = [],
  onEnsureConnectorModels,
  modelBinding,
  settings,
  settingsOptions,
  onModelChange,
  onSettingsChange,
  connectorAuthBusyIds = [],
  connectorAuthCancellingIds = [],
  connectorAuthPrompts = {},
  onConnectorAuthStart,
  onConnectorAuthCancel,
  onConnectorLogout,
}) {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [tokenCounterOpen, setTokenCounterOpen] = useState(false);

  const binding = normalizeModelBinding(modelBinding || {
    connector_id: srv.backend,
    model_id: srv.model,
  });
  const activeConnector = connectorById(connectors, binding.connector_id);
  const connectorModels = modelsForConnector(models, binding.connector_id);
  const connectorModelsLoading = connectorModelsLoadingIds.includes(binding.connector_id);

  // Ensure the current model is always selectable, even if not in the catalog.
  const options = useMemo(() => {
    const list = connectorModels.map((model) => ({ id: modelIdOf(model), label: modelLabel(model) }));
    if (binding.model_id && !list.some((option) => option.id === binding.model_id)) {
      list.unshift({ id: binding.model_id, label: binding.model_id });
    }
    return list;
  }, [connectorModels, binding.model_id]);

  const currentModel = useMemo(
    () => connectorModels.find((model) => modelIdOf(model) === binding.model_id) || null,
    [connectorModels, binding.model_id]
  );

  const chip = activeConnector ? connectorName(activeConnector) : binding.connector_id || "…";
  const conn = connectorStatus(activeConnector);
  const sidecar = sidecarUiStatus(sidecarStatus);

  // Nav highlight: the studios (world/story/character) live inside Библиотека.
  const isGame = mainView === "chat";
  const isImage = mainView === "image";
  const isLibrary = !isGame && !isImage;

  return (
    <header>
      <div className="header-left">
      {showChatToggle && onToggleChats && (
        <Tooltip
          className="tooltip-wrap"
          tipClassName="ui-tip-wrap"
          focusable={false}
          disabled={chatsOpen}
          content={
            <TipContent
              title={chatsOpen ? "Скрыть чаты и миры" : "Показать чаты и миры"}
              note={chatsOpen ? "Освободит место для текущей сцены и диалога." : "Откроет боковую панель с чатами и мирами."}
            />
          }
        >
          <button
            type="button"
            className={
              "btn btn-icon chat-toggle" +
              (chatsOpen ? " is-active" : "") +
              (isGame ? "" : " chat-toggle--offgame")
            }
            onClick={onToggleChats}
            aria-label={chatsOpen ? "Свернуть чаты и миры" : "Развернуть чаты и миры"}
            aria-expanded={chatsOpen}
            aria-controls="chat-history-sidebar"
          >
            <Icon name="panel-left" size={16} />
          </button>
        </Tooltip>
      )}
      <h1>
        <span className="logo-tile" aria-hidden="true">
          <Icon name="d20" size={16} className="logo-mark" />
        </span>
        <span>GM-<b>Lab</b></span>
      </h1>
      </div>
      <nav className="header-nav" aria-label="Разделы">
        <button
          type="button"
          className={"header-nav-btn" + (isGame ? " active" : "")}
          onClick={onNavGame}
          aria-current={isGame ? "page" : undefined}
        >
          <Icon name="d20" size={14} />
          <span>Игра</span>
        </button>
        <button
          type="button"
          className={"header-nav-btn" + (isLibrary ? " active" : "")}
          onClick={onNavLibrary}
          aria-current={isLibrary ? "page" : undefined}
        >
          <Icon name="book" size={14} />
          <span>Библиотека</span>
        </button>
        {imageLabEnabled && (
          <button
            type="button"
            className={"header-nav-btn" + (isImage ? " active" : "")}
            onClick={onNavImage}
            aria-current={isImage ? "page" : undefined}
          >
            <Icon name="image" size={14} />
            <span>Image Lab</span>
          </button>
        )}
      </nav>
      <div className="header-right">
      {onOpenSearch && (
        <button
          type="button"
          className="header-search-trigger"
          onClick={onOpenSearch}
          aria-label="Открыть общий поиск"
          aria-keyshortcuts="Control+K Meta+K"
        >
          <Icon name="search" size={15} />
          <span>Поиск</span>
          <kbd>Ctrl K</kbd>
        </button>
      )}
      {conn ? (
        <Tooltip
          content={<ConnTooltip status={conn} />}
          tipClassName="conn-tip-wrap"
          className="chip chip-conn"
        >
          <span className={"conn-dot " + conn.level} aria-hidden="true" />
          <span className="chip-conn-label">{chip}</span>
        </Tooltip>
      ) : (
        <span className="chip">{chip}</span>
      )}
      {sidecar && (
        <Tooltip
          content={<SidecarTooltip status={sidecarStatus} ui={sidecar} />}
          tipClassName="conn-tip-wrap"
          className={"chip chip-sidecar " + sidecar.level}
        >
          <span className={"conn-dot " + sidecar.level} aria-hidden="true" />
          <span className="chip-sidecar-label">{sidecar.label}</span>
        </Tooltip>
      )}
      <Tooltip
        className="tooltip-wrap"
        tipClassName="ui-tip-wrap"
        focusable={false}
        content={
          <TipContent
            title="Модель"
            subtitle="Какая модель отвечает за следующий ход."
            note="Смена применяется к новым запросам, уже идущий ответ не переключается."
          />
        }
      >
        <select
          className="model-select"
          value={binding.model_id}
          onChange={(e) => onModelChange(e.target.value)}
          aria-label="Модель"
          disabled={!binding.connector_id || options.length === 0}
        >
          {options.map((o) => (
            <option key={o.id} value={o.id}>
              {o.label}
            </option>
          ))}
        </select>
      </Tooltip>
      {binding.connector_id && connectorModels.length === 0 && (
        <Tooltip
          className="tooltip-wrap"
          tipClassName="ui-tip-wrap"
          focusable={false}
          content={<TipContent title="Обновить модели" note="Повторно запросит список у текущего коннектора." />}
        >
          <button
            type="button"
            className="icon-btn"
            disabled={connectorModelsLoading || typeof onEnsureConnectorModels !== "function"}
            onClick={() => onEnsureConnectorModels?.(binding.connector_id, { force: true })}
            aria-label="Обновить модели"
          >
            <Icon name="refresh" size={15} />
          </button>
        </Tooltip>
      )}
      <div className="header-actions">
        <Tooltip
          className="tooltip-wrap"
          tipClassName="ui-tip-wrap"
          focusable={false}
          disabled={settingsOpen}
          content={<TipContent title="Настройки" note="Модель, интерфейс, озвучка и подключения." />}
        >
          <button className="icon-btn" onClick={() => setSettingsOpen(true)} aria-label="Настройки">
            <Icon name="sliders" size={16} />
          </button>
        </Tooltip>
      </div>
      </div>
      {settingsOpen && (
        <SettingsModal
          settings={settings}
          settingsOptions={settingsOptions}
          currentModel={currentModel}
          connectors={connectors}
          connectorAuthBusyIds={connectorAuthBusyIds}
          connectorAuthCancellingIds={connectorAuthCancellingIds}
          connectorAuthPrompts={connectorAuthPrompts}
          onApply={onSettingsChange}
          onClose={() => setSettingsOpen(false)}
          onOpenTokenCounter={() => setTokenCounterOpen(true)}
          onConnectorAuthStart={onConnectorAuthStart}
          onConnectorAuthCancel={onConnectorAuthCancel}
          onConnectorLogout={onConnectorLogout}
        />
      )}
      {tokenCounterOpen && (
        <TokenCounter
          models={connectorModels}
          currentModel={binding.model_id}
          onClose={() => setTokenCounterOpen(false)}
        />
      )}
    </header>
  );
}
