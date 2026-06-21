// Resolve a character name to its display color. Source of truth is the NPC card
// (the roster carries .color). "ГМ" / "Гейм-мастер" has no card → the GM accent.
// Anything unknown → the neutral entity token. Used everywhere a name is shown so
// names are always tinted with the character's color.
export function nameColor(name, roster) {
  const clean = String(name || "").trim();
  if (clean === "ГМ" || clean === "Гейм-мастер") return "var(--gm)";
  const hit = (roster || []).find((n) => n.name === clean || n.id === clean);
  return (hit && hit.color) || "var(--entity-unknown)";
}
