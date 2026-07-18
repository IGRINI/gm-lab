**English** | [Русский](ru/UI_DESIGN_SYSTEM.md)

# TaleShift Web UI Design System

Style v4, "quiet dark workspace" (2026-07-15): a **low-chroma dark desktop
workbench**, also known as Minimal Dark Productivity UI. Its foundation is flat
graphite surfaces, subtle borders, compact controls, and one terracotta accent.
Glass is reserved for genuinely floating layers, while large editorial presentation
appears only in briefs, first-run experiences, and empty states.

Tokens and primitives live in `web/src/theme.css`. Screen-level files consume them
without introducing their own palettes.

## Rules (must not be broken)

1. **A workbench, not a collection of islands.** The header and sidebar form a
   persistent frame with subtle borders. Surface and border separate cards. Strong
   shadows are reserved for modals, menus, and toasts.
2. **Active state = surface + marker.** Use `--active-pill` and a thin `--brand`
   line; reserve a solid `--brand` fill for the primary CTA.
3. **Calm geometry:** `--r-sm 8 / --r-md 10 / --r-lg 12 / --r-xl 16 /
   --r-overlay 24 / --r-full`. Ordinary buttons are not pills; use `r-full` only
   for genuine tags, counters, and statuses.
4. **One grotesque typeface:** Inter Variable everywhere (`--font-book` also
   points to Inter; there are no serifs in the UI). Sizes: 11 kicker / 12 caption /
   13 default / 14 card heading / 16 section heading / 19–22 titles. Large headings
   use weight 650 and letter-spacing -0.01em.
5. **One spacing rhythm:** `4 / 8 / 12 / 16 / 20 / 24 / 32px`. Do not add arbitrary
   intermediate spacing without a reason.
6. **Hover is lighter** (surface-2 → surface-3); focus uses one terracotta
   `:focus-visible` ring. Transitions use 140/220ms with `--ease`.
7. **Restrained glass:** `--glass-row / --glass / --glass-strong` are allowed for
   the composer, HUD, popovers, and modals. Messages and persistent panels remain
   almost opaque.
8. **No emoji as icons.** Use only `components/Icon.jsx` (24×24, stroke 1.8). Add a
   new icon to the dictionary instead of inlining an SVG at the call site.
9. Values that update in place (HP, tokens) use `tabular-nums`.
10. Legacy aliases (`--bg/--bg2/--bg3/--line/--tx/--mut/--sub/--acc/--redo`)
    continue to work, but new code must use semantic tokens.

## Transcript voices

- GM narration is plain text without a card, with a quiet "GAME MASTER" kicker in
  text-3.
- The player uses a soft bluish bubble on the right with no border (r-18, 6px tail).
- An NPC uses an island tinted with the character color
  (color-mix 7% + surface-1, r-18), a colored name, and no side notches.
- Tool cards use a surface-1 island; tool color appears only in the icon tile (`--tc`).
- Do not change the dice (3D art). They are physical props, so gold is appropriate.

## Search

- Global search opens from the header or with `Ctrl/Cmd+K`. The palette always
  lives on the top modal layer, regardless of the current screen.
- The library has one search field: the "All" tab searches the complete library,
  while other tabs restrict search to their entity type.
- Game and message search runs on the server. Only player-visible text enters the
  index; hidden reasoning, tool arguments, and internal state are excluded.
- Input must never block the game: requests use a short debounce, stale requests are
  canceled, and a refresh retains previous results.
- Show skeletons only before the first response. While refining a query, use a small
  indicator without flashing the entire list.

## Known test-environment limitations

- Mock preview (`--server` + `GM_BACKEND=mock`) returns `/transcript` as a bare
  array; App accepts both forms. In a hidden tab (headless panel), rAF is frozen, so
  the live feed does not update. This is a browser limitation, not a UI bug.
