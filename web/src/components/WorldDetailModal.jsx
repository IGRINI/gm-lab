import { useEffect } from "react";
import Modal from "./Modal.jsx";
import Tooltip, { TipContent } from "./Tooltip.jsx";

// Player-facing detail modal for the world HUD: shows either the current
// location (scene) or the player's own character sheet. All data comes from
// the public state payload (scene_export / player_character_export).

const PRONOUN_LABELS = {
  M: "мужской", F: "женский", N: "средний", PL: "множественное",
  HE: "он", SHE: "она", THEY: "они", IT: "оно", OTHER: "иное",
};
const LIFE_STATUS_LABELS = {
  alive: "жив", dead: "мёртв", unconscious: "без сознания",
  dying: "при смерти", wounded: "ранен", injured: "ранен", stable: "стабилен",
};

// D&D ability / skill keys come from the model in English — localize for display.
const ABILITY_SHORT = {
  STR: "СИЛ", DEX: "ЛОВ", CON: "ТЕЛ", INT: "ИНТ", WIS: "МДР", CHA: "ХАР",
};
const ABILITY_FULL = {
  STR: "Сила", DEX: "Ловкость", CON: "Телосложение",
  INT: "Интеллект", WIS: "Мудрость", CHA: "Харизма",
};
const SKILL_LABELS = {
  acrobatics: "Акробатика",
  "animal handling": "Уход за животными",
  arcana: "Магия",
  athletics: "Атлетика",
  deception: "Обман",
  history: "История",
  insight: "Проницательность",
  intimidation: "Запугивание",
  investigation: "Анализ",
  medicine: "Медицина",
  nature: "Природа",
  perception: "Внимательность",
  performance: "Выступление",
  persuasion: "Убеждение",
  religion: "Религия",
  "sleight of hand": "Ловкость рук",
  stealth: "Скрытность",
  survival: "Выживание",
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
function spellLevelLabel(level) {
  const n = Number(level);
  return Number.isFinite(n) && n > 0 ? `ур. ${n}` : "заговор";
}
function spellLine(sp) {
  if (!sp || typeof sp !== "object") return "";
  const marks = [sp.concentration ? "конц." : "", sp.ritual ? "ритуал" : ""].filter(Boolean);
  const head = `${sp.name || "—"} (${[spellLevelLabel(sp.level), ...marks].join(", ")})`;
  const effect = txt(sp.effect);
  return effect ? `${head}: ${effect}` : head;
}
function slotsLine(slots, max) {
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
    return `${lvl}-й: ${cur}/${cap}`;
  }).join(", ");
}
function statEntries(v) {
  return objEntries(v).filter(([, x]) => x != null && String(x).trim() !== "");
}

function abilityShort(k) {
  return ABILITY_SHORT[String(k).toUpperCase()] || k;
}
function abilityFull(k) {
  return ABILITY_FULL[String(k).toUpperCase()] || k;
}
function skillLabel(k) {
  return SKILL_LABELS[String(k).toLowerCase()] || k;
}
function ruUnits(s) {
  return txt(s).replace(/\bfeet\b/gi, "фт").replace(/\bft\b/gi, "фт");
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
                title={abilityFull(k)}
                subtitle="Характеристика персонажа"
                rows={[
                  ["значение", String(v)],
                  ["модификатор", mod == null ? "—" : fmtMod(mod)],
                ]}
              />
            }
          >
            <span className="wd-stat-k">{abilityShort(k)}</span>
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
  const entries = statEntries(data);
  if (!entries.length) return null;
  const label = kind === "skill" ? skillLabel : abilityShort;
  return (
    <div className="wd-chips">
      {entries.map(([k, v]) => (
        <span className={"wd-chip wd-chip--" + kind + " tone-" + bonusTone(v)} key={k}>
          <span className="wd-chip-k">{label(k)}</span>
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

function npcName(n) {
  return n?.label || n?.name || n?.public_label || n?.id || "персонаж";
}
function npcColor(n) {
  return n?.color || "var(--entity-unknown)";
}
function npcHint(n) {
  return [n?.role, n?.physical_type, n?.distinctive_features, n?.condition].filter(Boolean).join(" · ");
}

function LocationDetail({ scene, npcs, statusLabels }) {
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
  const statusText = (s) => (statusLabels && statusLabels[s]) || s || "неизвестно";

  const empty = !description && !present.length && !offscreen.length && !exits.length && !items.length;

  return (
    <div className="wd">
      {description && <p className="wd-desc">{description}</p>}

      <Section title="Персонажи в сцене" when={present.length > 0} tone="green">
        <div className="wd-npcs">
          {present.map((n) => (
            <div className="wd-npc" key={n.id || npcName(n)}>
              <span className="dot" style={{ "--c": npcColor(n) }} />
              <b style={{ color: npcColor(n) }}>{npcName(n)}</b>
              {npcHint(n) && <span className="wd-npc-hint">{npcHint(n)}</span>}
            </div>
          ))}
        </div>
      </Section>

      <Section title="Где искать остальных" when={offscreen.length > 0} tone="amber">
        <div className="wd-npcs">
          {offscreen.map((n) => {
            const w = whereabouts[n.id] || {};
            const place = txt(w.location_name) || txt(w.location_id) || "место не установлено";
            return (
              <div className="wd-npc" key={n.id || npcName(n)}>
                <span className="dot" style={{ "--c": npcColor(n) }} />
                <b style={{ color: npcColor(n) }}>{npcName(n)}</b>
                <span className="wd-npc-hint">{statusText(w.status)} · {place}</span>
              </div>
            );
          })}
        </div>
      </Section>

      <Section title="Выходы" when={exits.length > 0} tone="blue">
        <div className="wd-list2">
          {exits.map((e, i) => (
            <div className={"wd-item tone-" + (txt(e.blocked_by) ? "bad" : "blue")} key={e.exit_id || i}>
              <b>{txt(e.name) || "выход"}</b>
              {txt(e.destination) && <span>→ {txt(e.destination)}</span>}
              {txt(e.blocked_by) && <em className="wd-blocked">заблокировано: {txt(e.blocked_by)}</em>}
            </div>
          ))}
        </div>
      </Section>

      <Section title="Предметы" when={items.length > 0} tone="green">
        <div className="wd-list2">
          {items.map((it, i) => (
            <div className="wd-item tone-green" key={it.item_id || i}>
              <b>{txt(it.name) || "предмет"}</b>
              {txt(it.details) && <span>{txt(it.details)}</span>}
            </div>
          ))}
        </div>
      </Section>

      {empty && <p className="wd-empty">Подробностей о локации пока нет.</p>}
    </div>
  );
}

function CharacterDetail({ pc }) {
  const pronoun = PRONOUN_LABELS[String(pc?.pronouns || "").toUpperCase()] || txt(pc?.pronouns);
  const life = LIFE_STATUS_LABELS[String(pc?.life_status || "").toLowerCase()] || txt(pc?.life_status);
  const hp = pc?.hp && typeof pc.hp === "object" ? pc.hp : null;
  const hpText = hp ? [hp.current, hp.max].filter((x) => x != null).join(" / ") : "";
  const personality = txt(pc?.personality);
  const values = txt(pc?.values);

  const ac = pc?.ac != null ? String(pc.ac) : "";
  const pp = pc?.passive_perception != null ? String(pc.passive_perception) : "";
  const speed = ruUnits(pc?.speed);
  const senses = txt(pc?.senses);
  const languages = txt(pc?.languages);
  const hasVitals = [ac, hpText, pp, speed].some((v) => v && v.trim() !== "");

  return (
    <div className="wd wd-char">
      {hasVitals && (
        <div className="wd-vitals">
          <Vital label="Класс доспеха" value={ac} tone="blue" />
          <Vital label="Хиты" value={hpText} tone={hpTone(hp)} />
          <Vital label="Пасс. внимательность" value={pp} tone="purple" />
          <Vital label="Скорость" value={speed} tone="teal" />
        </div>
      )}

      <Section title="О персонаже">
        <div className="wd-fields">
          <FieldCard label="Местоимения" value={pronoun} />
          <FieldCard label="Класс / роль" value={pc?.class_role} />
          <FieldCard label="Уровень" value={pc?.level != null ? String(pc.level) : ""} />
          <FieldCard label="Возраст" value={pc?.age} />
          <FieldCard label="Жизнь" value={life} tone={lifeTone(pc?.life_status)} />
          <FieldCard label="Происхождение" value={pc?.background} wide />
          <FieldCard label="Внешность" value={pc?.physical_type} wide />
          <FieldCard label="Особые приметы" value={pc?.distinctive_features} wide />
          <FieldCard label="Состояние" value={pc?.condition} wide tone="amber" />
          <FieldCard label="Заметка о статусе" value={pc?.life_status_note} wide />
        </div>
      </Section>

      <Section title="Личность" when={!!(personality || values)} tone="purple">
        {personality && <p className="wd-desc"><span className="wd-inline-k">Характер. </span>{personality}</p>}
        {values && <p className="wd-desc"><span className="wd-inline-k">Ценности. </span>{values}</p>}
      </Section>

      <Section title="Характеристики" when={statEntries(pc?.abilities).length > 0} tone="teal">
        <StatBlocks data={pc?.abilities} />
      </Section>

      <Section title="Навыки" when={statEntries(pc?.skills).length > 0} tone="teal">
        <BonusChips data={pc?.skills} kind="skill" />
      </Section>

      <Section title="Спасброски" when={statEntries(pc?.saving_throws).length > 0} tone="blue">
        <BonusChips data={pc?.saving_throws} kind="save" />
      </Section>

      <Section title="Чувства и языки" when={!!(senses || languages)}>
        <div className="wd-fields">
          <FieldCard label="Чувства" value={senses} wide />
          <FieldCard label="Языки" value={languages} wide />
        </div>
      </Section>

      <Section title="Инвентарь" when={arr(pc?.inventory).length > 0} tone="green">
        <Lines items={pc?.inventory} />
      </Section>

      <Section title="Снаряжение" when={arr(pc?.equipment).length > 0} tone="green">
        <Lines items={pc?.equipment} />
      </Section>

      <Section title="Особенности" when={arr(pc?.features).length > 0} tone="amber">
        <Lines items={pc?.features} cards />
      </Section>

      <Section
        title="Заклинания"
        when={arr(pc?.spells).length > 0 || !!slotsLine(pc?.spell_slots, pc?.spell_slots_max) || !!txt(pc?.concentration)}
        tone="purple"
      >
        {arr(pc?.spells).length > 0 && <Lines items={arr(pc?.spells).map(spellLine)} />}
        {!!slotsLine(pc?.spell_slots, pc?.spell_slots_max) && (
          <p className="wd-desc"><span className="wd-inline-k">Слоты. </span>{slotsLine(pc?.spell_slots, pc?.spell_slots_max)}</p>
        )}
        {!!txt(pc?.concentration) && (
          <p className="wd-desc"><span className="wd-inline-k">Концентрация. </span>{txt(pc?.concentration)}</p>
        )}
      </Section>
    </div>
  );
}

export default function WorldDetailModal({ kind, scene, playerCharacter, npcs, statusLabels, onClose }) {
  // Standalone modal (not part of the DebugPanel stack) — handle ESC itself.
  useEffect(() => {
    const onKey = (e) => {
      if (e.key === "Escape") onClose?.();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  if (kind === "scene") {
    return (
      <Modal title={txt(scene?.title) || "Локация"} subtitle="Локация" onClose={onClose} className="wd-modal">
        <LocationDetail scene={scene} npcs={npcs} statusLabels={statusLabels} />
      </Modal>
    );
  }

  if (kind === "character") {
    const pc = playerCharacter;
    const subtitle =
      [txt(pc?.class_role), pc?.level != null ? `ур. ${pc.level}` : ""].filter(Boolean).join(" · ") ||
      "Персонаж игрока";
    return (
      <Modal title={txt(pc?.name) || "Персонаж игрока"} subtitle={subtitle} onClose={onClose} className="wd-modal">
        <CharacterDetail pc={pc} />
      </Modal>
    );
  }

  return null;
}
