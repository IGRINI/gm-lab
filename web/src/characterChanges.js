function listOf(value) {
  if (value == null) return [];
  return Array.isArray(value) ? value : [value];
}

function valueKey(value) {
  if (value && typeof value === "object") {
    try {
      return `json:${JSON.stringify(value)}`;
    } catch {
      return `text:${String(value)}`;
    }
  }
  return `${typeof value}:${String(value)}`;
}

function subtractValues(source, consumedBy) {
  const counts = new Map();
  for (const value of consumedBy) {
    const key = valueKey(value);
    counts.set(key, (counts.get(key) || 0) + 1);
  }
  const remainder = [];
  for (const value of source) {
    const key = valueKey(value);
    const count = counts.get(key) || 0;
    if (count > 0) counts.set(key, count - 1);
    else remainder.push(value);
  }
  return remainder;
}

export function normalizeCharacterChanges(payload) {
  const raw = Array.isArray(payload?.changes) ? payload.changes : [];
  return raw
    .filter((change) => {
      const field = String(change?.field || "").trim();
      return change && typeof change === "object" && field && field !== "card_revision";
    })
    .map((change) => {
      const before = change.before;
      const after = change.after;
      const explicitAdded = Object.hasOwn(change, "added");
      const explicitRemoved = Object.hasOwn(change, "removed");
      const arrays = Array.isArray(before) && Array.isArray(after);
      return {
        field: String(change.field).trim(),
        before,
        after,
        added: explicitAdded
          ? listOf(change.added)
          : arrays
            ? subtractValues(after, before)
            : [],
        removed: explicitRemoved
          ? listOf(change.removed)
          : arrays
            ? subtractValues(before, after)
            : [],
      };
    });
}

export function formatCharacterChangeValue(value, t) {
  if (value == null || value === "") return "—";
  if (typeof value === "boolean") {
    return value
      ? t("playerCards.changeValues.yes")
      : t("playerCards.changeValues.no");
  }
  if (Array.isArray(value)) {
    return value.length ? value.map((item) => formatCharacterChangeValue(item, t)).join(", ") : "—";
  }
  if (typeof value === "object") {
    if (Object.hasOwn(value, "current") || Object.hasOwn(value, "max")) {
      return `${value.current ?? "?"} / ${value.max ?? "?"}`;
    }
    const entries = Object.entries(value);
    if (!entries.length) return "—";
    return entries
      .map(([key, item]) => `${key}: ${formatCharacterChangeValue(item, t)}`)
      .join(" · ");
  }
  return String(value);
}

export function characterChangeFieldLabel(field, t) {
  const key = String(field || "").replaceAll(".", "_");
  return t(`playerCards.changeFields.${key}`, { defaultValue: field });
}
