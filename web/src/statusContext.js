import { createContext } from "react";

// Legacy backend labels remain available for custom statuses, while known status
// keys are translated by the active UI catalog.
export const StatusLabelsContext = createContext({});

export function localizeStatusLabel(
  t,
  status,
  serverLabels = {},
  keyPrefix = "status.labels",
  unknownKey = "status.unknown"
) {
  const key = String(status || "").trim();
  if (!key) return t(unknownKey);
  return t(`${keyPrefix}.${key}`, { defaultValue: serverLabels[key] || key });
}
