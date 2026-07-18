import Icon from "./Icon.jsx";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useDevSettings, setDeveloperMode, setFlag, FLAG_META } from "../devSettings.js";
import { useInterfaceSettings, setSceneBackground } from "../interfaceSettings.js";
import {
  availableLanguages,
  DEFAULT_LANGUAGE,
  resolveUiLanguage,
  setUiLanguage,
} from "../i18n/index.js";
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
import { localizeServerMessage } from "../serverMessages.js";

function fmtExpiry(raw, t, language) {
  const ms = raw > 0 && raw < 1_000_000_000_000 ? raw * 1000 : raw;
  const left = ms - Date.now();
  const when = new Intl.DateTimeFormat(language, {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(ms));
  let rel;
  if (left <= 0) rel = t("connectors:auth.expiredRelative");
  else if (left < 60 * 60 * 1000) {
    rel = t("connectors:auth.expiresIn", {
      duration: t("common:duration.minutes", { count: Math.max(1, Math.round(left / 60000)) }),
    });
  } else if (left < 24 * 60 * 60 * 1000) {
    rel = t("connectors:auth.expiresIn", {
      duration: t("common:duration.hours", { count: Math.round(left / 3600000) }),
    });
  } else {
    rel = t("connectors:auth.expiresIn", {
      duration: t("common:duration.days", { count: Math.round(left / 86400000) }),
    });
  }
  return `${when} (${rel})`;
}

function connectorStatus(connector, t, language) {
  if (!connector) return null;
  const name = connectorName(connector);
  const methods = authMethods(connector);
  const auth = connector.auth || {};
  if (methods.length === 0) {
    return {
      level: "ok",
      title: t("connectors:auth.available", { name }),
      rows: [],
      note: t("connectors:auth.notRequired"),
    };
  }
  const authNote = (fallback) => localizeServerMessage(auth, t, { fallbackText: fallback });
  const hasAuthDetail = Boolean(auth.message || auth.error || auth.code);
  const exp = typeof auth.expires_at === "number" ? auth.expires_at : null;
  const rows = [];
  if (auth.account_id) rows.push({ k: t("connectors:auth.account"), v: String(auth.account_id) });
  if (auth.account_label) rows.push({ k: t("connectors:auth.account"), v: String(auth.account_label) });
  if (auth.email) rows.push({ k: t("connectors:auth.email"), v: String(auth.email) });

  if (connectorAuthenticated(connector)) {
    if (exp != null) {
      rows.push({ k: t("connectors:auth.token"), v: fmtExpiry(exp, t, language) });
    }
    return {
      level: "ok",
      title: t("connectors:auth.connected", { name }),
      rows,
      note: hasAuthDetail ? authNote(t("connectors:auth.connectedNote")) : "",
    };
  }

  const status = String(auth.state || auth.status || "").toLowerCase();
  if (status === "pending" || status === "authorizing") {
    return {
      level: "warn",
      title: t("connectors:auth.connecting", { name }),
      rows,
      note: authNote(t("connectors:auth.browserPrompt")),
    };
  }
  if (status === "expired") {
    return {
      level: "error",
      title: t("connectors:auth.notCompleted", { name }),
      rows,
      note: authNote(t("connectors:auth.expired")),
    };
  }
  if (["denied", "error", "failed", "invalid"].includes(status)) {
    return {
      level: "error",
      title: t("connectors:auth.error", { name }),
      rows,
      note: authNote(t("connectors:auth.failedNote")),
    };
  }
  return {
    level: "off",
    title: t("connectors:auth.disconnected", { name }),
    rows,
    note: authNote(t("connectors:auth.required")),
  };
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

function fmtElapsed(ms, t) {
  if (typeof ms !== "number" || !Number.isFinite(ms) || ms < 0) return "";
  const sec = Math.round(ms / 1000);
  if (sec < 60) return t("common:duration.seconds", { count: sec });
  const min = Math.floor(sec / 60);
  const rest = sec % 60;
  const minutes = t("common:duration.minutes", { count: min });
  return rest ? `${minutes} ${t("common:duration.seconds", { count: rest })}` : minutes;
}

function componentLine(component, fallback, t) {
  if (!component?.enabled) return `${fallback}: ${t("settings:sidecar.off")}`;
  const label = component.up ? t("settings:sidecar.ready") : t("settings:sidecar.loading");
  const model = component.model ? ` · ${component.model}` : "";
  const models = Array.isArray(component.models) && component.models.length
    ? ` · ${component.models.join(", ")}`
    : "";
  const quant = component.quant ? ` · ${component.quant}` : "";
  return `${fallback}: ${label}${model}${models}${quant}`;
}

function imageComponentLine(component, t) {
  const name = t("settings:sidecar.image");
  if (!component?.enabled) return `${name}: ${t("settings:sidecar.off")}`;
  const label = component.up
    ? t("settings:sidecar.ready")
    : component.runtime_ready
      ? t("settings:sidecar.warming")
      : t("settings:sidecar.loading");
  const models = Array.isArray(component.models) && component.models.length
    ? ` · ${component.models.join(", ")}`
    : "";
  const comfy = component.comfy_up ? " · ComfyUI" : "";
  return `${name}: ${label}${models}${comfy}`;
}

function sidecarUiStatus(status, t) {
  if (!status || status.enabled === false) return null;
  const c = status.components || {};
  const hasRag = !!(c.embedder?.enabled || c.reranker?.enabled);
  const hasTts = !!c.tts?.enabled;
  const hasImage = !!c.image?.enabled;
  const parts = [];
  if (hasRag) parts.push("RAG");
  if (hasTts) parts.push("TTS");
  if (hasImage) parts.push(t("settings:sidecar.image"));
  const name = parts.join("/") || t("settings:sidecar.inference");
  if (status.ready) {
    return { level: "ok", label: name, title: t("settings:sidecar.readyTitle") };
  }
  if (status.state === "failed") {
    return { level: "error", label: name, title: t("settings:sidecar.failedTitle") };
  }
  if (status.state === "unavailable") {
    return { level: "warn", label: name, title: t("settings:sidecar.unavailableTitle") };
  }
  return { level: "warn", label: name, title: t("settings:sidecar.loadingTitle") };
}

function SidecarTooltip({ status, ui }) {
  const { t } = useTranslation(["common", "settings"]);
  const c = status?.components || {};
  const elapsed = fmtElapsed(status?.elapsed_ms, t);
  const timeout = fmtElapsed(status?.ready_timeout_ms, t);
  const rows = [
    status?.base_url ? { k: t("settings:sidecar.url"), v: status.base_url } : null,
    status?.pid ? { k: t("settings:sidecar.pid"), v: String(status.pid) } : null,
    elapsed
      ? {
          k: t("settings:sidecar.elapsed"),
          v: timeout ? t("settings:sidecar.elapsedOf", { elapsed, timeout }) : elapsed,
        }
      : null,
    status?.manager_state
      ? {
          k: t("settings:sidecar.state"),
          v: t(`settings:sidecar.states.${status.manager_state}`, {
            defaultValue: t("settings:sidecar.states.unknown"),
          }),
        }
      : null,
  ].filter(Boolean);
  const note = status?.error
    ? localizeServerMessage(status, t, { fallbackText: ui.title })
    : [
      componentLine(c.embedder, t("settings:sidecar.embedder"), t),
      componentLine(c.reranker, t("settings:sidecar.reranker"), t),
      componentLine(c.tts, "TTS", t),
      imageComponentLine(c.image, t),
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
  { key: "gm" },
  { key: "npc" },
  { key: "compact" },
];

function settingLabel(value, t) {
  return t(`settings:values.${value}`, { defaultValue: value });
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
  const { t } = useTranslation("settings");
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
        <span>{t("reasoning.effort")}</span>
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
            <option key={value} value={value}>{settingLabel(value, t)}</option>
          ))}
        </select>
      </label>

      <label className="field">
        <span>{t("reasoning.summary")}</span>
        <select
          value={summaryValue}
          disabled={effortValue === "none"}
          onChange={(e) => setRole(role, { summary: e.target.value })}
        >
          {summaryOptions.map((value) => (
            <option key={value} value={value}>{settingLabel(value, t)}</option>
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
  const { i18n, t } = useTranslation(["common", "connectors"]);
  const methods = authMethods(connector);
  const authenticated = connectorAuthenticated(connector);
  const status = connectorStatus(connector, t, resolveUiLanguage(i18n.resolvedLanguage || i18n.language));
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
            {status?.title || t("connectors:auth.unknown")}
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
            {cancelling ? t("connectors:auth.cancelling") : t("connectors:auth.cancelConnection")}
          </button>
        ) : !authenticated && methods.map((method) => (
          <button
            key={method.id}
            type="button"
            className="btn primary"
            onClick={() => onAuthStart?.(connectorIdOf(connector), method.id)}
          >
            {methods.length === 1
              ? t("common:actions.connect")
              : t("connectors:auth.connectMethod", { method: method.name })}
          </button>
        ))}
        {authenticated && methods.length > 0 && (
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={() => onLogout?.(connectorIdOf(connector))}
          >
            {busy ? t("connectors:auth.disconnecting") : t("common:actions.logout")}
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
  const { i18n, t } = useTranslation(["common", "connectors", "settings"]);
  const [draft, setDraft] = useState(settings);
  const dev = useDevSettings();
  const interfaceSettings = useInterfaceSettings();
  const [tab, setTab] = useState("model");
  const interfaceLanguage = resolveUiLanguage(i18n.resolvedLanguage || i18n.language);
  const rawResponseLanguage = String(draft.response_language || DEFAULT_LANGUAGE);
  const installedResponseLanguage = availableLanguages.find(
    (language) => language.code.toLowerCase() === rawResponseLanguage.toLowerCase()
  );
  const responseLanguage = installedResponseLanguage?.code || rawResponseLanguage;
  const responseLanguages = installedResponseLanguage
    ? availableLanguages
    : [...availableLanguages, { code: rawResponseLanguage, name: rawResponseLanguage }];

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
    { id: "model", label: t("settings:tabs.model") },
    { id: "view", label: t("settings:tabs.interface") },
    ...((connectors || []).length > 0
      ? [{ id: "connection", label: t("connectors:settings.tab") }]
      : []),
    ...(dev.developerMode ? [{ id: "debug", label: t("settings:tabs.debug") }] : []),
  ];

  const activeTab = tabs.find((t) => t.id === tab) || tabs[0];

  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={onClose}>
      <form className="settings-modal" onSubmit={submit} onMouseDown={(e) => e.stopPropagation()}>
        <div className="settings-side">
          <div className="settings-side-head">{t("settings:title")}</div>
          <nav className="settings-tabs" role="tablist" aria-label={t("settings:sectionsAria")}>
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
          <button
            type="button"
            className="icon-btn"
            onClick={onClose}
            aria-label={t("settings:closeAria")}
          >
            <Icon name="x" size={15} />
          </button>
        </div>

        <div className="settings-main-body">
        {tab === "view" && (
          <>
          <section className="settings-section">
            <h3>{t("settings:language.section")}</h3>
            <label className="field">
              <span className="toggle-label">
                {t("settings:language.interface")}
                <small>{t("settings:language.interfaceHint")}</small>
              </span>
              <select
                value={interfaceLanguage}
                onChange={(event) => void setUiLanguage(event.target.value)}
              >
                {availableLanguages.map((language) => (
                  <option key={language.code} value={language.code}>{language.name}</option>
                ))}
              </select>
            </label>
          </section>
          <section className="settings-section">
            <h3>{t("settings:appearance.section")}</h3>
            <p>{t("settings:appearance.description")}</p>
            <ToggleField
              label={t("settings:appearance.sceneBackground")}
              hint={t("settings:appearance.sceneBackgroundHint")}
              checked={interfaceSettings.sceneBackground}
              onChange={setSceneBackground}
            />
          </section>
          <section className="settings-section">
            <h3>{t("settings:developer.section")}</h3>
            <p>{t("settings:developer.description")}</p>
            <ToggleField
              label={t("settings:developer.label")}
              hint={t("settings:developer.hint")}
              checked={dev.developerMode}
              onChange={setDeveloperMode}
            />
            {dev.developerMode && (
              <p className="settings-note">
                {t("settings:developer.debugNote")}
              </p>
            )}
          </section>
          </>
        )}

        {tab === "debug" && dev.developerMode && (
          <>
            <section className="settings-section">
              <h3>{t("settings:debug.visibilitySection")}</h3>
              <p>{t("settings:debug.visibilityDescription")}</p>
              {FLAG_META.map((flag) => (
                <ToggleField
                  key={flag.key}
                  label={t(`settings:debug.flags.${flag.key}.label`)}
                  hint={t(`settings:debug.flags.${flag.key}.hint`)}
                  checked={dev.flags[flag.key] !== false}
                  onChange={(on) => setFlag(flag.key, on)}
                />
              ))}
            </section>
            <section className="settings-section">
              <h3>{t("settings:debug.toolsSection")}</h3>
              <p>{t("settings:debug.toolsDescription")}</p>
              <button type="button" className="btn" onClick={() => onOpenTokenCounter?.()}>
                {t("settings:debug.tokenCounter")}
              </button>
            </section>
          </>
        )}

        {tab === "connection" && (
          <section className="settings-section">
            <p>{t("connectors:settings.description")}</p>
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
            <h3>{t(`settings:roles.${role.key}.title`)}</h3>
            <p>{t(`settings:roles.${role.key}.note`)}</p>
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
          <h3>{t("settings:general.section")}</h3>
          <p>{t("settings:general.description")}</p>

        <label className="field">
          <span className="toggle-label">
            {t("settings:language.response")}
            <small>{t("settings:language.responseHint")}</small>
          </span>
          <select
            value={responseLanguage}
            onChange={(event) => set({ response_language: event.target.value })}
          >
            {responseLanguages.map((language) => (
              <option key={language.code} value={language.code}>{language.name}</option>
            ))}
          </select>
        </label>

        <label className="field">
          <span>{t("settings:general.verbosity")}</span>
          <select
            value={draft.text_verbosity || "default"}
            onChange={(e) => set({ text_verbosity: e.target.value })}
          >
            {(settingsOptions.text_verbosities || []).map((value) => (
              <option key={value} value={value}>{settingLabel(value, t)}</option>
            ))}
          </select>
        </label>

        <label className="field">
          <span>{t("settings:general.tools")}</span>
          <select
            value={draft.tool_choice || "auto"}
            onChange={(e) => set({ tool_choice: e.target.value })}
          >
            {(settingsOptions.tool_choices || []).map((value) => (
              <option key={value} value={value}>{settingLabel(value, t)}</option>
            ))}
          </select>
        </label>

        <label className="field check-field">
          <span>{t("settings:general.streamGm")}</span>
          <input
            type="checkbox"
            checked={draft.stream_gm_content !== false}
            onChange={(e) => set({ stream_gm_content: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>{t("settings:general.parallelTools")}</span>
          <input
            type="checkbox"
            checked={!!draft.parallel_tool_calls}
            onChange={(e) => set({ parallel_tool_calls: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>{t("settings:general.suggestOptions")}</span>
          <input
            type="checkbox"
            checked={!!draft.gm_suggest_options}
            onChange={(e) => set({ gm_suggest_options: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>{t("settings:general.tts")}</span>
          <input
            type="checkbox"
            checked={!!draft.tts_enabled}
            onChange={(e) => set({ tts_enabled: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>{t("settings:general.ttsAutoplay")}</span>
          <input
            type="checkbox"
            checked={!!draft.tts_autoplay}
            onChange={(e) => set({ tts_autoplay: e.target.checked })}
          />
        </label>

        <label className="field check-field">
          <span>{t("settings:general.images")}</span>
          <input
            type="checkbox"
            checked={draft.image_enabled !== false}
            onChange={(e) => set({ image_enabled: e.target.checked })}
          />
        </label>

        <label className="field" title={t("settings:general.imageProviderHint")}>
          <span>{t("settings:general.imageProvider")}</span>
          <select
            value={draft.image_provider || "local"}
            onChange={(e) => set({ image_provider: e.target.value })}
            disabled={draft.image_enabled === false}
          >
            {(settingsOptions.image_providers || ["local", "grok", "grok_quality"]).map((value) => (
              <option key={value} value={value}>
                {t(`settings:general.imageProviders.${value}`)}
              </option>
            ))}
          </select>
        </label>

        <label className="field">
          <span>{t("settings:general.toolHopLimit")}</span>
          <Tooltip
            className="tooltip-block"
            tipClassName="ui-tip-wrap"
            focusable={false}
            content={
              <TipContent
                title={t("settings:general.toolHopLimit")}
                subtitle={t("settings:general.toolHopSubtitle")}
                rows={[["0", t("settings:general.unlimited")]]}
                note={t("settings:general.toolHopNote")}
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
          <span>{t("settings:general.outputLimit")}</span>
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
            {tab === "model" ? t("common:actions.cancel") : t("common:actions.close")}
          </button>
          {tab === "model" && (
            <button type="submit" className="btn primary">{t("common:actions.save")}</button>
          )}
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
  const { i18n, t } = useTranslation(["common", "connectors", "settings"]);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [tokenCounterOpen, setTokenCounterOpen] = useState(false);
  const interfaceLanguage = resolveUiLanguage(i18n.resolvedLanguage || i18n.language);

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
  const conn = connectorStatus(activeConnector, t, interfaceLanguage);
  const sidecar = sidecarUiStatus(sidecarStatus, t);

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
              title={t(chatsOpen ? "common:sidebar.hide" : "common:sidebar.show")}
              note={t(chatsOpen ? "common:sidebar.hideNote" : "common:sidebar.showNote")}
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
            aria-label={t(chatsOpen ? "common:sidebar.collapse" : "common:sidebar.expand")}
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
        <span>Tale<b>Shift</b></span>
      </h1>
      </div>
      <nav className="header-nav" aria-label={t("common:nav.sections")}>
        <button
          type="button"
          className={"header-nav-btn" + (isGame ? " active" : "")}
          onClick={onNavGame}
          aria-current={isGame ? "page" : undefined}
        >
          <Icon name="d20" size={14} />
          <span>{t("common:nav.game")}</span>
        </button>
        <button
          type="button"
          className={"header-nav-btn" + (isLibrary ? " active" : "")}
          onClick={onNavLibrary}
          aria-current={isLibrary ? "page" : undefined}
        >
          <Icon name="book" size={14} />
          <span>{t("common:nav.library")}</span>
        </button>
        {imageLabEnabled && (
          <button
            type="button"
            className={"header-nav-btn" + (isImage ? " active" : "")}
            onClick={onNavImage}
            aria-current={isImage ? "page" : undefined}
          >
            <Icon name="image" size={14} />
            <span>{t("common:nav.imageLab")}</span>
          </button>
        )}
      </nav>
      <div className="header-right">
      {onOpenSearch && (
        <button
          type="button"
          className="header-search-trigger"
          onClick={onOpenSearch}
          aria-label={t("common:search.open")}
          aria-keyshortcuts="Control+K Meta+K"
        >
          <Icon name="search" size={15} />
          <span>{t("common:search.label")}</span>
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
            title={t("common:model.label")}
            subtitle={t("common:model.tooltip")}
            note={t("common:model.changeNote")}
          />
        }
      >
        <select
          className="model-select"
          value={binding.model_id}
          onChange={(e) => onModelChange(e.target.value)}
          aria-label={t("common:model.label")}
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
          content={
            <TipContent
              title={t("common:model.refresh")}
              note={t("common:model.refreshNote")}
            />
          }
        >
          <button
            type="button"
            className="icon-btn"
            disabled={connectorModelsLoading || typeof onEnsureConnectorModels !== "function"}
            onClick={() => onEnsureConnectorModels?.(binding.connector_id, { force: true })}
            aria-label={t("common:model.refresh")}
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
          content={
            <TipContent
              title={t("settings:tooltip.title")}
              note={t("settings:tooltip.note")}
            />
          }
        >
          <button
            className="icon-btn"
            onClick={() => setSettingsOpen(true)}
            aria-label={t("settings:title")}
          >
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
