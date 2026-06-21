// Character colors are NOT defined here. The single source of truth is the NPC
// card (world.py NPC.color), delivered via /state (roster + entity_refs). When a
// card sets no color, the UI uses the theme token var(--entity-unknown) — there
// is intentionally no name→color table in code.

export const fmtK = (n) =>
  n >= 1000 ? (n / 1000).toFixed(1).replace(/\.0$/, "") + "k" : "" + n;
