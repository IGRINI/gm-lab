import { createContext } from "react";

export const EntityRegistryContext = createContext({ byKey: {} });

export function entityKey(kind, id) {
  const cleanKind = canonicalKind(kind);
  const cleanId = String(id || "").trim().toLowerCase();
  return cleanKind && cleanId ? `${cleanKind}:${cleanId}` : "";
}

export function canonicalKind(kind) {
  const value = String(kind || "").trim().toLowerCase();
  if (value === "loc" || value === "location") return "loc";
  return value;
}

export function normalizeEntities(raw, npcs = []) {
  const byKey = {};
  const list = Array.isArray(raw?.entities) ? raw.entities : Array.isArray(raw) ? raw : [];
  const roster = Array.isArray(npcs) ? npcs : [];
  for (const entity of list) {
    if (!entity || typeof entity !== "object") continue;
    const kind = canonicalKind(entity.kind || entity.type);
    const id = String(entity.id || "").trim();
    const key = entity.key || entityKey(kind, id);
    if (!key) continue;
    const lookup = id.toLowerCase();
    const npc = kind === "npc"
      ? roster.find((candidate) => [candidate?.id, candidate?.name, candidate?.label]
          .some((value) => String(value || "").trim().toLowerCase() === lookup))
      : null;
    byKey[key.toLowerCase()] = { ...(npc || {}), ...entity, kind, id, key };
  }
  return { byKey };
}

export function resolveEntity(registry, kind, id) {
  const key = entityKey(kind, id);
  return key ? registry?.byKey?.[key.toLowerCase()] || null : null;
}
