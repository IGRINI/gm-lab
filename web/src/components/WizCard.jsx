import Icon from "./Icon.jsx";
import Tooltip from "./Tooltip.jsx";

// WizCard — the shared selection card of the New-Game wizard and the
// BasePickerModal (they used to carry identical local copies). Besides the card
// itself this module owns the card-TEXT helpers (world/story/character titles,
// meta lines, previews) and the hover-TIP builders: a styled Tooltip (the same
// floating-ui portal the rest of the UI uses) with the FULL, un-clamped
// description of the world/story/hero behind the card.
//
// PRIVACY: tips render only player-visible material. For worlds that is the
// public premise + flavor meta (never `hidden_premise`/`hidden_secrets`); for
// stories the catalog row itself is already public (`story_brief`, the
// whitelisted `pc` summary); for characters the .gmchar sheet fields listed in
// PC_TIP_ROWS — a fixed field list, so any ad-hoc GM key stays out.

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

// ---- card text helpers (shared by NewGameWizard / BasePickerModal) ----

export function worldTitle(world) {
  return textValue(world?.title) || textValue(world?.world_lore?.name) || "Без названия";
}

export function worldMeta(world) {
  return [world?.genre, world?.tone].map((v) => textValue(v)).filter(Boolean).join(" · ");
}

export function worldPreview(world) {
  return textValue(world?.preview) || textValue(world?.public_premise) || "";
}

export function storyTitle(story) {
  return textValue(story?.title) || "Без названия";
}

export function storyDescription(story) {
  return textValue(story?.story_brief) || textValue(story?.description) || "";
}

export function characterTitle(character) {
  return textValue(character?.title) || "Персонаж";
}

export function characterMeta(character) {
  const pc = character?.payload?.player_character || {};
  return pcMeta(pc) || textValue(character?.preview) || "персонаж";
}

// The catalog row's public protagonist summary (whitelisted server-side; see
// StoryStore::metadata) — `null` when the story ships no authored hero.
export function storyPc(story) {
  const pc = story?.pc;
  return pc && typeof pc === "object" && !Array.isArray(pc) ? pc : null;
}

// «класс/роль · ур. N» from any player_character-shaped sheet. Library .gmchar
// sheets are NOT server-whitelisted (LLM-authored/imported), so only scalar
// levels render — an object would stringify to «ур. [object Object]».
export function pcMeta(pc) {
  const parts = [];
  const role = textValue(pc?.class_role);
  if (role) parts.push(role);
  const level = pc?.level;
  if (
    (typeof level === "number" && Number.isFinite(level)) ||
    (typeof level === "string" && level.trim() !== "")
  ) {
    parts.push(`ур. ${level}`);
  }
  return parts.join(" · ");
}

// ---- hover tips ----

function CardTip({ kicker, title, meta, desc, rows = [] }) {
  const cleanRows = rows.filter((row) => row && row[0] && row[1]);
  return (
    <div className="wiz-tip">
      <div className="wiz-tip-head">
        {kicker && <span>{kicker}</span>}
        <b>{title}</b>
        {meta && <em>{meta}</em>}
      </div>
      {desc && <div className="wiz-tip-desc">{desc}</div>}
      {cleanRows.length > 0 && (
        <div className="wiz-tip-rows">
          {cleanRows.map(([label, value]) => (
            <div className="wiz-tip-row" key={label}>
              <span>{label}</span>
              <b>{value}</b>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// The world's PUBLIC face: premise in full + flavor meta. `world_lore` rides in
// the /worlds row, but only these player-safe fields ever render.
export function worldTip(world) {
  if (!world) return null;
  const lore = world.world_lore && typeof world.world_lore === "object" ? world.world_lore : {};
  const premise =
    textValue(world.public_premise) || textValue(lore.public_premise) || textValue(world.preview);
  const rows = [
    ["Жанр", textValue(world.genre) || textValue(lore.genre)],
    ["Тон", textValue(world.tone) || textValue(lore.tone)],
    ["Масштаб", textValue(world.scale) || textValue(lore.scale)],
  ];
  if (!premise && !rows.some((r) => r[1])) return null;
  return <CardTip kicker="мир" title={worldTitle(world)} desc={premise} rows={rows} />;
}

// The story's public premise in full; an authored protagonist gets a one-line
// mention (his own card/tip carries the details).
export function storyTip(story, { kicker = "история" } = {}) {
  if (!story) return null;
  const brief = textValue(story.story_brief);
  // No separate description row: App's normalizeStory folds story_brief into
  // description, so in practice the two never differ by the time we render.
  const description = textValue(story.description);
  const pc = storyPc(story);
  const rows = [];
  if (pc) {
    const name = textValue(pc.name);
    const role = textValue(pc.class_role);
    rows.push(["Протагонист", name ? (role ? `${name} — ${role}` : name) : role]);
  }
  const desc = brief || description;
  if (!desc && rows.length === 0) return null;
  return <CardTip kicker={kicker} title={storyTitle(story)} desc={desc} rows={rows} />;
}

// Sheet fields a hero tip presents, in order — presentation only, no mechanics.
const PC_TIP_ROWS = [
  ["age", "Возраст"],
  ["physical_type", "Внешность"],
  ["distinctive_features", "Приметы"],
  ["personality", "Характер"],
  ["values", "Ценности"],
  ["condition", "Состояние"],
];

function pcTip({ title, sheet, fallbackDesc = "" }) {
  const rows = PC_TIP_ROWS.map(([key, label]) => [label, textValue(sheet?.[key])]);
  const desc = textValue(sheet?.background) || fallbackDesc;
  if (!desc && !rows.some((r) => r[1])) return null;
  return <CardTip kicker="персонаж" title={title} meta={pcMeta(sheet)} desc={desc} rows={rows} />;
}

// The story's own authored protagonist, from the catalog row's public summary.
export function protagonistTip(story) {
  const pc = storyPc(story);
  if (!pc) return null;
  return pcTip({ title: textValue(pc.name) || "Протагонист истории", sheet: pc });
}

// A library .gmchar hero, from the full sheet the /characters row carries.
export function characterTip(character) {
  if (!character) return null;
  const sheet = character.payload?.player_character || {};
  return pcTip({
    title: characterTitle(character),
    sheet,
    fallbackDesc: textValue(character.preview),
  });
}

// ---- the card ----

// On touch, the tap that SELECTS a card would also pin its tip over the grid
// until the next tap (emulated mouseenter + focus) — suppress tips entirely,
// matching the (pointer: coarse) guards in Composer/DiceRoll.
const COARSE_POINTER =
  typeof window !== "undefined" &&
  !!window.matchMedia &&
  window.matchMedia("(hover: none), (pointer: coarse)").matches;

export default function WizCard({
  selected,
  disabled,
  onClick,
  kicker,
  title,
  badge,
  meta,
  desc,
  add,
  tip,
}) {
  const className =
    "wiz-card" + (add ? " wiz-card-add" : "") + (selected ? " is-selected" : "");
  const card = (
    <button
      type="button"
      className={className}
      onClick={onClick}
      disabled={disabled}
      aria-pressed={add ? undefined : selected}
    >
      {add ? (
        <>
          <span className="wiz-card-add-icon" aria-hidden="true"><Icon name="plus" size={20} /></span>
          <span className="wiz-card-add-label">{title}</span>
        </>
      ) : (
        <>
          {(kicker || badge) && (
            <span className="wiz-card-top">
              {kicker && <span className="wiz-card-kicker">{kicker}</span>}
              {badge && <span className="wiz-badge">{badge}</span>}
            </span>
          )}
          <span className="wiz-card-title">{title}</span>
          {meta && <span className="wiz-card-meta">{meta}</span>}
          {desc && <span className="wiz-card-desc">{desc}</span>}
          {selected && (
            <span className="wiz-card-check" aria-hidden="true">
              <Icon name="check" size={13} strokeWidth={2.4} />
            </span>
          )}
        </>
      )}
    </button>
  );
  if (!tip || COARSE_POINTER) return card;
  // The wrapper (not the button) is the Tooltip trigger: the card transforms on
  // hover, and a transformed ancestor would re-anchor the portal's fixed tip.
  return (
    <Tooltip as="div" className="wiz-cell" tipClassName="wiz-tip-wrap" content={tip} focusable={false}>
      {card}
    </Tooltip>
  );
}
