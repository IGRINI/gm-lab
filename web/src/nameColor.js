// Resolve a character name to its display color. Source of truth is the NPC card
// (the roster carries .color). Stable GM ids and legacy display labels have no
// card, so they use the GM accent. New behavior must pass stable ids instead of
// adding localized labels here.
// Anything unknown → the neutral entity token. Used everywhere a name is shown so
// names are always tinted with the character's color.
export function nameColor(name, roster) {
  const clean = String(name || "").trim();
  if (["gm", "GM", "ГМ", "Гейм-мастер"].includes(clean)) return "var(--gm)";
  const hit = (roster || []).find((n) => n.name === clean || n.id === clean);
  return (hit && hit.color) || "var(--entity-unknown)";
}
