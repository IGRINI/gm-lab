import { useEffect, useMemo } from "react";
import {
  authMethods,
  bindingReady,
  connectorAuthenticated,
  connectorAuthUrl,
  connectorById,
  connectorIdOf,
  connectorName,
  modelIdOf,
  modelLabel,
  modelsForConnector,
  normalizeModelBinding,
  resolveModelBinding,
} from "../connectorCatalog.js";

export function ConnectorAuthPrompt({ prompt, connectorId }) {
  if (!prompt || prompt.connector_id !== connectorId) return null;
  const isDeviceCode = prompt.kind === "device_code";
  const url = connectorAuthUrl(prompt);
  return (
    <div className="connector-auth-prompt" role="status">
      <span>{isDeviceCode ? "Откройте страницу и подтвердите код:" : "Завершите вход в браузере."}</span>
      {isDeviceCode && prompt.user_code && <code>{prompt.user_code}</code>}
      {url && (
        <a href={url} target="_blank" rel="noreferrer">
          Открыть страницу входа
        </a>
      )}
    </div>
  );
}

export default function ConnectorModelPicker({
  connectors = [],
  models = [],
  connectorModelsLoadingIds = [],
  onEnsureConnectorModels,
  value,
  onChange,
  connectorLocked = false,
  disabled = false,
  compact = false,
  authBusyConnectorIds = [],
  authCancellingConnectorIds = [],
  authPrompts = {},
  onAuthStart,
  onAuthCancel,
  ariaLabel = "Коннектор и модель",
}) {
  const resolved = useMemo(
    () => resolveModelBinding(value, connectors, models),
    [value, connectors, models]
  );
  const connector = connectorById(connectors, resolved.connector_id);
  const availableModels = modelsForConnector(models, resolved.connector_id);
  const modelAvailable = availableModels.some((model) => modelIdOf(model) === resolved.model_id);
  const methods = authMethods(connector);
  const authenticated = connectorAuthenticated(connector);
  const authBusy = authBusyConnectorIds.includes(resolved.connector_id);
  const authCancelling = authCancellingConnectorIds.includes(resolved.connector_id);
  const authPrompt = authPrompts[resolved.connector_id] || null;
  const modelsLoading = connectorModelsLoadingIds.includes(resolved.connector_id);

  useEffect(() => {
    if (resolved.connector_id) onEnsureConnectorModels?.(resolved.connector_id);
  }, [onEnsureConnectorModels, resolved.connector_id]);

  useEffect(() => {
    const current = normalizeModelBinding(value);
    if (
      resolved.connector_id &&
      resolved.model_id &&
      (current.connector_id !== resolved.connector_id || current.model_id !== resolved.model_id)
    ) {
      onChange?.(resolved);
    }
  }, [value, resolved, onChange]);

  const changeConnector = (connectorId) => {
    onEnsureConnectorModels?.(connectorId);
    const nextConnector = connectorById(connectors, connectorId);
    const nextModels = modelsForConnector(models, connectorId);
    const defaultModelId = String(nextConnector?.default_model_id || nextConnector?.default_model || "").trim();
    const defaultModel = nextModels.find((model) => modelIdOf(model) === defaultModelId);
    onChange?.({
      connector_id: connectorId,
      model_id: modelIdOf(defaultModel || nextModels[0]),
    });
  };

  const ready = bindingReady(resolved, connectors, models);

  return (
    <div
      className={`connector-model-picker${compact ? " connector-model-picker--compact" : ""}`}
      aria-label={ariaLabel}
    >
      <label className="connector-model-field">
        <span>Коннектор</span>
        <select
          value={resolved.connector_id}
          onChange={(event) => changeConnector(event.target.value)}
          disabled={disabled || connectorLocked || connectors.length === 0}
        >
          {(Array.isArray(connectors) ? connectors : []).map((item) => {
            const id = connectorIdOf(item);
            return <option key={id} value={id}>{connectorName(item)}</option>;
          })}
        </select>
      </label>

      <label className="connector-model-field connector-model-field--model">
        <span>Модель</span>
        <select
          value={resolved.model_id}
          onChange={(event) => onChange?.({ ...resolved, model_id: event.target.value })}
          disabled={disabled || modelsLoading || availableModels.length === 0}
        >
          {!modelAvailable && resolved.model_id && (
            <option value={resolved.model_id}>{resolved.model_id} · недоступна</option>
          )}
          {availableModels.map((model) => {
            const id = modelIdOf(model);
            return <option key={id} value={id}>{modelLabel(model)}</option>;
          })}
        </select>
      </label>

      {connectorLocked && (
        <span className="connector-model-lock" title="Коннектор закреплён за этой историей">
          Коннектор закреплён
        </span>
      )}

      {!authenticated && methods.length > 0 && (
        <div className="connector-model-auth">
          <span>{connectorName(connector)} не подключён</span>
          {authBusy ? (
            <button
              type="button"
              className="btn small"
              disabled={authCancelling || typeof onAuthCancel !== "function"}
              onClick={() => onAuthCancel?.(resolved.connector_id)}
            >
              {authCancelling ? "Отменяю…" : "Отменить"}
            </button>
          ) : methods.map((method) => (
            <button
              key={method.id}
              type="button"
              className="btn small"
              disabled={disabled || typeof onAuthStart !== "function"}
              onClick={() => onAuthStart?.(resolved.connector_id, method.id)}
            >
              {methods.length === 1 ? "Подключить" : method.name}
            </button>
          ))}
        </div>
      )}

      <ConnectorAuthPrompt prompt={authPrompt} connectorId={resolved.connector_id} />

      {!connector && resolved.connector_id && (
        <span className="connector-model-error">Коннектор недоступен</span>
      )}

      {modelsLoading && (
        <span className="connector-model-lock">Загружаю модели…</span>
      )}

      {connector && !ready && authenticated && !modelsLoading && (
        <div className="connector-model-retry">
          <span className="connector-model-error">У коннектора нет доступной модели</span>
          <button
            type="button"
            className="btn small"
            disabled={disabled || typeof onEnsureConnectorModels !== "function"}
            onClick={() => onEnsureConnectorModels?.(resolved.connector_id, { force: true })}
          >
            Повторить
          </button>
        </div>
      )}
    </div>
  );
}
