import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { ZoomableImage } from "./ImagePreview.jsx";
import Modal from "./Modal.jsx";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import NpcTooltip from "./NpcTooltip.jsx";

// Player-facing detail modal for the world HUD: shows either the current
// location (scene) or the player's own character sheet. All data comes from
// the public state payload (scene_export / player_character_export).

// D&D ability / skill keys come from the model in English — localize for display.
const SKILL_KEYS = {
  acrobatics: "acrobatics",
  "animal handling": "animalHandling",
  arcana: "arcana",
  athletics: "athletics",
  deception: "deception",
  history: "history",
  insight: "insight",
  intimidation: "intimidation",
  investigation: "investigation",
  medicine: "medicine",
  nature: "nature",
  perception: "perception",
  performance: "performance",
  persuasion: "persuasion",
  religion: "religion",
  "sleight of hand": "sleightOfHand",
  stealth: "stealth",
  survival: "survival",
};

function txt(v) {
  if (v == null) return "";
  if (typeof v === "string") return v.trim();
  return String(v);
}
function arr(v) {
  return Array.isArray(v) ? v : [];
}
function objEntries(v) {
  return v && typeof v === "object" && !Array.isArray(v) ? Object.entries(v) : [];
}
// Фаза С (ITEMS_AND_SPELLS_TZ §С3): read-only spell rendering.
function spellLevelLabel(level, t) {
  const n = Number(level);
  return Number.isFinite(n) && n > 0
    ? t("worldDetail.spells.level", { level: n })
    : t("worldDetail.spells.cantrip");
}
function spellLine(sp, t) {
  if (!sp || typeof sp !== "object") return "";
  const marks = [
    sp.concentration ? t("worldDetail.spells.concentrationShort") : "",
    sp.ritual ? t("worldDetail.spells.ritual") : "",
  ].filter(Boolean);
  const head = `${sp.name || "—"} (${[spellLevelLabel(sp.level, t), ...marks].join(", ")})`;
  const effect = txt(sp.effect);
  return effect ? `${head}: ${effect}` : head;
}
function slotsLine(slots, max, t) {
  const levels = new Set();
  for (const m of [slots, max]) {
    for (const [k] of objEntries(m)) {
      const n = parseInt(k, 10);
      if (Number.isInteger(n) && n > 0) levels.add(n);
    }
  }
  const slotNum = (v) => { const n = Number(v); return Number.isFinite(n) ? n : 0; };
  return [...levels].sort((a, b) => a - b).map((lvl) => {
    const cur = slotNum((slots || {})[lvl]);
    const capRaw = (max || {})[lvl];
    const cap = capRaw == null ? "?" : slotNum(capRaw);
    return t("worldDetail.spells.slot", { level: lvl, current: cur, max: cap });
  }).join(", ");
}
function statEntries(v) {
  return objEntries(v).filter(([, x]) => x != null && String(x).trim() !== "");
}

function abilityShort(k, t) {
  const key = String(k).toUpperCase();
  return ["STR", "DEX", "CON", "INT", "WIS", "CHA"].includes(key)
    ? t(`worldDetail.abilities.short.${key}`)
    : k;
}
function abilityFull(k, t) {
  const key = String(k).toUpperCase();
  return ["STR", "DEX", "CON", "INT", "WIS", "CHA"].includes(key)
    ? t(`worldDetail.abilities.full.${key}`)
    : k;
}
function skillLabel(k, t) {
  const key = SKILL_KEYS[String(k).toLowerCase()];
  return key ? t(`worldDetail.skills.${key}`) : k;
}
function displayUnits(s, t) {
  const feet = t("worldDetail.units.feetShort");
  return txt(s).replace(/\bfeet\b/gi, feet).replace(/\bft\b/gi, feet);
}

// --- semantic color tones (see .tone-* in styles.css) -----------------------
function abilityMod(score) {
  const n = Number(score);
  if (!Number.isFinite(n)) return null;
  return Math.floor((n - 10) / 2);
}
function fmtMod(mod) {
  return mod >= 0 ? `+${mod}` : String(mod);
}
function modTone(mod) {
  if (mod == null) return "mid";
  if (mod >= 2) return "great";
  if (mod === 1) return "good";
  if (mod === 0) return "mid";
  if (mod === -1) return "weak";
  return "bad";
}
function bonusTone(v) {
  const n = Number(v);
  if (!Number.isFinite(n)) return "mid";
  if (n >= 4) return "great";
  if (n >= 2) return "good";
  if (n >= 1) return "mid";
  if (n === 0) return "weak";
  return "bad";
}
function hpTone(hp) {
  if (!hp) return "great";
  const cur = Number(hp.current);
  const max = Number(hp.max);
  if (!Number.isFinite(cur) || !Number.isFinite(max) || max <= 0) return "great";
  const r = cur / max;
  if (r <= 0.25) return "bad";
  if (r < 1) return "weak";
  return "great";
}
function lifeTone(status) {
  const s = String(status || "").toLowerCase();
  if (/dead|мёртв|мертв|погиб|killed/.test(s)) return "bad";
  if (/unconscious|dying|без созн|при смерт/.test(s)) return "bad";
  if (/wounded|injured|ранен|bleed/.test(s)) return "weak";
  if (/alive|жив|stable|стабил|здоров/.test(s)) return "great";
  return "mid";
}

function Section({ title, when = true, tone, children }) {
  if (!when) return null;
  return (
    <section className={"wd-section" + (tone ? " tone-" + tone : "")}>
      <h4>{title}</h4>
      {children}
    </section>
  );
}

function FieldCard({ label, value, wide = false, tone }) {
  const v = txt(value);
  if (!v) return null;
  return (
    <div className={"wd-field" + (wide ? " wd-field--wide" : "") + (tone ? " tone-" + tone : "")}>
      <span className="wd-field-k">{label}</span>
      <b className="wd-field-v">{v}</b>
    </div>
  );
}

function Vital({ label, value, tone }) {
  const v = txt(value);
  if (!v) return null;
  return (
    <div className={"wd-vital" + (tone ? " tone-" + tone : "")}>
      <b className="wd-vital-v">{v}</b>
      <span className="wd-vital-k">{label}</span>
    </div>
  );
}

function StatBlocks({ data }) {
  const { t } = useTranslation("studio");
  const entries = statEntries(data);
  if (!entries.length) return null;
  return (
    <div className="wd-stats">
      {entries.map(([k, v]) => {
        const mod = abilityMod(v);
        return (
          <Tooltip
            as="div"
            className={"wd-stat tone-" + modTone(mod)}
            tipClassName="ui-tip-wrap"
            key={k}
            content={
              <TipContent
                title={abilityFull(k, t)}
                subtitle={t("worldDetail.abilities.subtitle")}
                rows={[
                  [t("worldDetail.abilities.value"), String(v)],
                  [t("worldDetail.abilities.modifier"), mod == null ? "—" : fmtMod(mod)],
                ]}
              />
            }
          >
            <span className="wd-stat-k">{abilityShort(k, t)}</span>
            <b className="wd-stat-v">{String(v)}</b>
            {mod != null && <span className="wd-stat-mod">{fmtMod(mod)}</span>}
          </Tooltip>
        );
      })}
    </div>
  );
}

// Compact bonus chips. `kind="save"` keeps short ability abbreviations uppercased;
// `kind="skill"` shows the full localized skill name. Both tint by bonus size.
function BonusChips({ data, kind }) {
  const { t } = useTranslation("studio");
  const entries = statEntries(data);
  if (!entries.length) return null;
  const label = kind === "skill" ? skillLabel : abilityShort;
  return (
    <div className="wd-chips">
      {entries.map(([k, v]) => (
        <span className={"wd-chip wd-chip--" + kind + " tone-" + bonusTone(v)} key={k}>
          <span className="wd-chip-k">{label(k, t)}</span>
          <b className="wd-chip-v">{String(v)}</b>
        </span>
      ))}
    </div>
  );
}

function Lines({ items, cards = false }) {
  const xs = arr(items).map(txt).filter(Boolean);
  if (!xs.length) return null;
  if (cards) {
    return (
      <div className="wd-tags">
        {xs.map((x, i) => <span className="wd-tag" key={i}>{x}</span>)}
      </div>
    );
  }
  return (
    <ul className="wd-list">
      {xs.map((x, i) => <li key={i}>{x}</li>)}
    </ul>
  );
}

function npcName(n, t) {
  return n?.label || n?.name || n?.public_label || n?.id || t("worldDetail.location.character");
}
function npcColor(n) {
  return n?.color || "var(--entity-unknown)";
}
function npcHint(n) {
  return [n?.role, n?.physical_type, n?.current_appearance, n?.distinctive_features, n?.condition].filter(Boolean).join(" · ");
}

function LocationDetail({ scene, npcs, statusLabels }) {
  const { t } = useTranslation("studio");
  const description = txt(scene?.description);
  const roster = arr(npcs);
  const presentIds = new Set(arr(scene?.present_npcs));
  const present = roster.filter((n) => presentIds.has(n.id));
  const whereabouts = scene?.npc_whereabouts && typeof scene.npc_whereabouts === "object" ? scene.npc_whereabouts : {};
  const offscreen = roster.filter((n) => {
    if (presentIds.has(n.id)) return false;
    const w = whereabouts[n.id] || {};
    return (w.status && w.status !== "unknown") || w.location_name || w.details;
  });
  const exits = arr(scene?.exits).filter((e) => e && e.visible !== false);
  const items = arr(scene?.items).filter((it) => it && it.visible !== false);
  const statusText = (s) => {
    const fallback = (statusLabels && statusLabels[s]) || s || t("worldDetail.location.unknown");
    return s ? t(`worldDetail.whereaboutsStatus.${s}`, { defaultValue: fallback }) : fallback;
  };

  const empty = !description && !present.length && !offscreen.length && !exits.length && !items.length;

  return (
    <div className="wd">
      {txt(scene?.image_url) && (
        <ZoomableImage
          className="wd-location-art"
          src={scene.image_url}
          alt={txt(scene?.title) || t("worldDetail.location.title")}
          title={txt(scene?.title)}
        />
      )}
      {description && <p className="wd-desc">{description}</p>}

      <Section title={t("worldDetail.location.presentCharacters")} when={present.length > 0} tone="green">
        <div className="wd-npcs">
          {present.map((n) => (
            <div className="wd-npc" key={n.id || npcName(n, t)}>
              {txt(n?.portrait_url) ? (
                <ZoomableImage
                  className="wd-npc-portrait"
                  src={n.portrait_url}
                  alt={npcName(n, t)}
                  title={npcName(n, t)}
                  loading="lazy"
                />
              ) : (
                <span className="dot" style={{ "--c": npcColor(n) }} />
              )}
              <NpcTooltip npc={n} label={npcName(n, t)}>
                <b style={{ color: npcColor(n) }}>{npcName(n, t)}</b>
              </NpcTooltip>
              {npcHint(n) && <span className="wd-npc-hint">{npcHint(n)}</span>}
            </div>
          ))}
        </div>
      </Section>

      <Section title={t("worldDetail.location.offscreenCharacters")} when={offscreen.length > 0} tone="amber">
        <div className="wd-npcs">
          {offscreen.map((n) => {
            const w = whereabouts[n.id] || {};
            const place = txt(w.location_name) || txt(w.location_id) || t("worldDetail.location.placeUnknown");
            return (
              <div className="wd-npc" key={n.id || npcName(n, t)}>
                {txt(n?.portrait_url) ? (
                  <ZoomableImage
                    className="wd-npc-portrait"
                    src={n.portrait_url}
                    alt={npcName(n, t)}
                    title={npcName(n, t)}
                    loading="lazy"
                  />
                ) : (
                  <span className="dot" style={{ "--c": npcColor(n) }} />
                )}
                <NpcTooltip
                  npc={n}
                  label={npcName(n, t)}
                  status={statusText(w.status)}
                  place={place}
                >
                  <b style={{ color: npcColor(n) }}>{npcName(n, t)}</b>
                </NpcTooltip>
                <span className="wd-npc-hint">{statusText(w.status)} · {place}</span>
              </div>
            );
          })}
        </div>
      </Section>

      <Section title={t("worldDetail.location.exits")} when={exits.length > 0} tone="blue">
        <div className="wd-list2">
          {exits.map((e, i) => (
            <div className={"wd-item tone-" + (txt(e.blocked_by) ? "bad" : "blue")} key={e.exit_id || i}>
              <b>{txt(e.name) || t("worldDetail.location.exit")}</b>
              {txt(e.destination) && <span>→ {txt(e.destination)}</span>}
              {txt(e.blocked_by) && (
                <em className="wd-blocked">
                  {t("worldDetail.location.blocked", { reason: txt(e.blocked_by) })}
                </em>
              )}
            </div>
          ))}
        </div>
      </Section>

      <Section title={t("worldDetail.location.items")} when={items.length > 0} tone="green">
        <div className="wd-list2">
          {items.map((it, i) => (
            <div className="wd-item tone-green" key={it.item_id || i}>
              <b>{txt(it.name) || t("worldDetail.location.item")}</b>
              {txt(it.details) && <span>{txt(it.details)}</span>}
            </div>
          ))}
        </div>
      </Section>

      {empty && <p className="wd-empty">{t("worldDetail.location.empty")}</p>}
    </div>
  );
}

function CharacterDetail({ pc }) {
  const { t } = useTranslation("studio");
  const pronounCode = String(pc?.pronouns || "").toUpperCase();
  const lifeCode = String(pc?.life_status || "").toLowerCase();
  const pronoun = t(`worldDetail.pronouns.${pronounCode}`, { defaultValue: txt(pc?.pronouns) });
  const life = t(`worldDetail.lifeStatus.${lifeCode}`, { defaultValue: txt(pc?.life_status) });
  const hp = pc?.hp && typeof pc.hp === "object" ? pc.hp : null;
  const hpText = hp ? [hp.current, hp.max].filter((x) => x != null).join(" / ") : "";
  const personality = txt(pc?.personality);
  const values = txt(pc?.values);

  const ac = pc?.ac != null ? String(pc.ac) : "";
  const pp = pc?.passive_perception != null ? String(pc.passive_perception) : "";
  const speed = displayUnits(pc?.speed, t);
  const senses = txt(pc?.senses);
  const languages = txt(pc?.languages);
  const hasVitals = [ac, hpText, pp, speed].some((v) => v && v.trim() !== "");

  return (
    <div className="wd wd-char">
      {txt(pc?.portrait_url) && (
        <ZoomableImage
          className="wd-character-portrait"
          src={pc.portrait_url}
          alt={txt(pc?.name) || t("worldDetail.character.about")}
          title={txt(pc?.name)}
        />
      )}
      {hasVitals && (
        <div className="wd-vitals">
          <Vital label={t("worldDetail.character.armorClass")} value={ac} tone="blue" />
          <Vital label={t("worldDetail.character.hp")} value={hpText} tone={hpTone(hp)} />
          <Vital label={t("worldDetail.character.passivePerception")} value={pp} tone="purple" />
          <Vital label={t("worldDetail.character.speed")} value={speed} tone="teal" />
        </div>
      )}

      <Section title={t("worldDetail.character.about")}>
        <div className="wd-fields">
          <FieldCard label={t("worldDetail.character.pronouns")} value={pronoun} />
          <FieldCard label={t("worldDetail.character.classRole")} value={pc?.class_role} />
          <FieldCard label={t("worldDetail.character.level")} value={pc?.level != null ? String(pc.level) : ""} />
          <FieldCard label={t("worldDetail.character.age")} value={pc?.age} />
          <FieldCard label={t("worldDetail.character.life")} value={life} tone={lifeTone(pc?.life_status)} />
          <FieldCard label={t("worldDetail.character.background")} value={pc?.background} wide />
          <FieldCard label={t("worldDetail.character.appearance")} value={pc?.physical_type} wide />
          <FieldCard label={t("worldDetail.character.currentAppearance")} value={pc?.current_appearance} wide />
          <FieldCard label={t("worldDetail.character.features")} value={pc?.distinctive_features} wide />
          <FieldCard label={t("worldDetail.character.condition")} value={pc?.condition} wide tone="amber" />
          <FieldCard label={t("worldDetail.character.statusNote")} value={pc?.life_status_note} wide />
        </div>
      </Section>

      <Section title={t("worldDetail.character.personalitySection")} when={!!(personality || values)} tone="purple">
        {personality && <p className="wd-desc"><span className="wd-inline-k">{t("worldDetail.character.personalityPrefix")} </span>{personality}</p>}
        {values && <p className="wd-desc"><span className="wd-inline-k">{t("worldDetail.character.valuesPrefix")} </span>{values}</p>}
      </Section>

      <Section title={t("worldDetail.character.abilities")} when={statEntries(pc?.abilities).length > 0} tone="teal">
        <StatBlocks data={pc?.abilities} />
      </Section>

      <Section title={t("worldDetail.character.skills")} when={statEntries(pc?.skills).length > 0} tone="teal">
        <BonusChips data={pc?.skills} kind="skill" />
      </Section>

      <Section title={t("worldDetail.character.savingThrows")} when={statEntries(pc?.saving_throws).length > 0} tone="blue">
        <BonusChips data={pc?.saving_throws} kind="save" />
      </Section>

      <Section title={t("worldDetail.character.sensesAndLanguages")} when={!!(senses || languages)}>
        <div className="wd-fields">
          <FieldCard label={t("worldDetail.character.senses")} value={senses} wide />
          <FieldCard label={t("worldDetail.character.languages")} value={languages} wide />
        </div>
      </Section>

      <Section title={t("worldDetail.character.inventory")} when={arr(pc?.inventory).length > 0} tone="green">
        <Lines items={pc?.inventory} />
      </Section>

      <Section title={t("worldDetail.character.equipment")} when={arr(pc?.equipment).length > 0} tone="green">
        <Lines items={pc?.equipment} />
      </Section>

      <Section title={t("worldDetail.character.traits")} when={arr(pc?.features).length > 0} tone="amber">
        <Lines items={pc?.features} cards />
      </Section>

      <Section
        title={t("worldDetail.character.spells")}
        when={arr(pc?.spells).length > 0 || !!slotsLine(pc?.spell_slots, pc?.spell_slots_max, t) || !!txt(pc?.concentration)}
        tone="purple"
      >
        {arr(pc?.spells).length > 0 && <Lines items={arr(pc?.spells).map((spell) => spellLine(spell, t))} />}
        {!!slotsLine(pc?.spell_slots, pc?.spell_slots_max, t) && (
          <p className="wd-desc"><span className="wd-inline-k">{t("worldDetail.spells.slotsPrefix")} </span>{slotsLine(pc?.spell_slots, pc?.spell_slots_max, t)}</p>
        )}
        {!!txt(pc?.concentration) && (
          <p className="wd-desc"><span className="wd-inline-k">{t("worldDetail.spells.concentrationPrefix")} </span>{txt(pc?.concentration)}</p>
        )}
      </Section>
    </div>
  );
}

export default function WorldDetailModal({
  kind,
  scene,
  playerCharacter,
  npcs,
  statusLabels,
  onClose,
  closeOnEscape = true,
  footer = null,
}) {
  const { t } = useTranslation("studio");
  // Standalone usages handle Escape here; embedded owners may keep control of
  // their own overlay stack by disabling this listener.
  useEffect(() => {
    if (!closeOnEscape) return undefined;
    const onKey = (e) => {
      if (e.key === "Escape") onClose?.();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [closeOnEscape, onClose]);

  if (kind === "scene") {
    return (
      <Modal
        title={txt(scene?.title) || t("worldDetail.location.title")}
        subtitle={t("worldDetail.location.title")}
        onClose={onClose}
        className="wd-modal"
        footer={footer}
      >
        <LocationDetail scene={scene} npcs={npcs} statusLabels={statusLabels} />
      </Modal>
    );
  }

  if (kind === "character") {
    const pc = playerCharacter;
    const subtitle =
      [txt(pc?.class_role), pc?.level != null ? t("worldDetail.character.levelShort", { level: pc.level }) : ""]
        .filter(Boolean)
        .join(" · ") || t("worldDetail.character.playerCharacter");
    return (
      <Modal
        title={txt(pc?.name) || t("worldDetail.character.playerCharacter")}
        subtitle={subtitle}
        onClose={onClose}
        className="wd-modal"
        footer={footer}
      >
        <CharacterDetail pc={pc} />
      </Modal>
    );
  }

  return null;
}
