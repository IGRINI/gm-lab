function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

export function connectorIdOf(connector) {
  return textValue(connector?.id || connector?.connector_id);
}

export function connectorName(connector) {
  return textValue(connector?.display_name || connector?.name || connector?.label)
    || connectorIdOf(connector)
    || "Коннектор";
}

export function modelIdOf(model) {
  return textValue(model?.id || model?.model_id || model?.slug);
}

export function modelConnectorId(model) {
  return textValue(model?.connector_id || model?.connector || model?.provider_id || model?.provider);
}

export function modelLabel(model) {
  const id = modelIdOf(model);
  const name = textValue(model?.display_name || model?.name || model?.label);
  const base = name && name !== id ? `${name} · ${id}` : id;
  return model?.supported === false ? `${base} · экспериментальная` : base;
}

export function normalizeModelBinding(binding) {
  return {
    connector_id: textValue(binding?.connector_id || binding?.connector),
    model_id: textValue(binding?.model_id || binding?.model),
  };
}

export function authMethods(connector) {
  const methods = Array.isArray(connector?.auth_methods) ? connector.auth_methods : [];
  return methods
    .map((method) => {
      if (typeof method === "string") return { id: method, name: method };
      const id = textValue(method?.id || method?.method_id || method?.name);
      if (!id) return null;
      return {
        ...method,
        id,
        name: textValue(method?.label || method?.display_name || method?.name) || id,
      };
    })
    .filter(Boolean);
}

export function connectorAuthenticated(connector) {
  if (authMethods(connector).length === 0) return true;
  const auth = connector?.auth || {};
  if (auth.authenticated === true) return true;
  const status = textValue(auth.state || auth.status).toLowerCase();
  return status === "signed_in"
    || status === "not_required"
    || status === "authenticated"
    || status === "connected"
    || status === "ready";
}

export function connectorAuthState(auth) {
  return textValue(auth?.state || auth?.status).toLowerCase();
}

export function connectorAuthUrl(start) {
  const raw = textValue(
    start?.kind === "device_code" ? start?.verification_url : start?.authorization_url
  );
  if (!raw) return "";
  try {
    const url = new URL(raw);
    return url.protocol === "https:" || url.protocol === "http:" ? url.toString() : "";
  } catch {
    return "";
  }
}

export function connectorById(connectors, connectorId) {
  return (Array.isArray(connectors) ? connectors : []).find(
    (connector) => connectorIdOf(connector) === connectorId
  ) || null;
}

function collectModel(out, seen, model, fallbackConnectorId) {
  if (!model || typeof model !== "object") return;
  const id = modelIdOf(model);
  if (!id) return;
  const connectorId = modelConnectorId(model) || fallbackConnectorId;
  const key = `${connectorId}:${id}`;
  if (seen.has(key)) return;
  seen.add(key);
  out.push({ ...model, id, connector_id: connectorId });
}

// The connector endpoint may expose one flat model array, a connector-keyed
// object, or models nested in connector descriptors. Normalize all three once
// so the rest of the UI only deals with a flat connector-tagged catalog.
export function normalizeModels(connectors, rawModels, fallbackConnectorId = "") {
  const out = [];
  const seen = new Set();
  const list = Array.isArray(connectors) ? connectors : [];

  for (const connector of list) {
    const connectorId = connectorIdOf(connector);
    for (const model of Array.isArray(connector?.models) ? connector.models : []) {
      collectModel(out, seen, model, connectorId);
    }
  }

  if (Array.isArray(rawModels)) {
    const inferred = fallbackConnectorId || (list.length === 1 ? connectorIdOf(list[0]) : "");
    for (const model of rawModels) collectModel(out, seen, model, inferred);
  } else if (rawModels && typeof rawModels === "object") {
    for (const [connectorId, models] of Object.entries(rawModels)) {
      for (const model of Array.isArray(models) ? models : []) {
        collectModel(out, seen, model, connectorId);
      }
    }
  }
  return out;
}

export function modelsForConnector(models, connectorId) {
  const list = Array.isArray(models) ? models : [];
  const selectable = list.filter((model) => model?.selectable !== false);
  const tagged = selectable.filter((model) => modelConnectorId(model) === connectorId);
  if (tagged.length > 0) return tagged;
  // Compatibility for a single-connector catalog whose model rows predate the
  // connector_id field. Never leak untagged rows into a different connector.
  const connectorIds = new Set(selectable.map(modelConnectorId).filter(Boolean));
  return connectorIds.size === 0 ? selectable : [];
}

export function resolveModelBinding(binding, connectors, models) {
  const current = normalizeModelBinding(binding);
  const connectorList = Array.isArray(connectors) ? connectors : [];
  const requestedConnector = current.connector_id
    ? connectorById(connectorList, current.connector_id)
    : null;
  if (current.connector_id && !requestedConnector) return current;
  const connector = requestedConnector || connectorList[0] || null;
  const connectorId = connectorIdOf(connector);
  const available = modelsForConnector(models, connectorId);
  const defaultModelId = textValue(connector?.default_model_id || connector?.default_model);
  const defaultModel = available.find((model) => modelIdOf(model) === defaultModelId);
  return {
    connector_id: connectorId,
    model_id: current.model_id || modelIdOf(defaultModel || available[0]),
  };
}

export function bindingReady(binding, connectors, models) {
  const normalized = normalizeModelBinding(binding);
  const connector = connectorById(connectors, normalized.connector_id);
  if (!connector || !connectorAuthenticated(connector)) return false;
  return modelsForConnector(models, normalized.connector_id)
    .some((model) => modelIdOf(model) === normalized.model_id);
}
