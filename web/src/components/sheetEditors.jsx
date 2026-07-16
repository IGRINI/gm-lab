import { useState } from "react";
import Icon from "./Icon.jsx";
import { AutoTextarea, textValue } from "./architectShared.jsx";

// Структурные редакторы листа персонажа, общие для студии архитектора
// (CharacterArchitectPanel) и дебаг-панели (PlayerEditor/NpcEditor в
// DebugPanel). Здесь живут презентационные компоненты, хуки строкового буфера
// и чистые конвертеры «строки ⇄ значение листа»; куда складывать результат
// (живой sheet-стейт против payload на момент сохранения) решает панель-хозяин.

// D&D ability keys arrive from the model in English — localize the six core ones.
export const ABILITY_SHORT = { STR: "СИЛ", DEX: "ЛОВ", CON: "ТЕЛ", INT: "ИНТ", WIS: "МДР", CHA: "ХАР" };
// The fixed order the six abilities render in (extra keys are preserved but not
// shown — the editor only exposes the core six inputs).
export const ABILITY_ORDER = ["STR", "DEX", "CON", "INT", "WIS", "CHA"];

// §И1 разделитель «имя — описание» (пробел + EM DASH + пробел, сплит по ПЕРВОМУ
// вхождению — зеркалит gml-world helpers::item_head/item_tail).
export const ITEM_DESC_SEP = " — ";

export function asObject(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}

export function asArray(value) {
  return Array.isArray(value) ? value : [];
}

export function abilityMod(score) {
  const n = Number(score);
  if (!Number.isFinite(n)) return null;
  return Math.floor((n - 10) / 2);
}

export function fmtMod(mod) {
  return mod >= 0 ? `+${mod}` : String(mod);
}

// A scalar shown in a text input — strings pass through, arrays are joined so a
// legacy list value (e.g. senses/languages authored as an array) stays editable.
export function scalarText(value) {
  if (Array.isArray(value)) return value.filter((x) => x != null).join(", ");
  if (value == null) return "";
  return typeof value === "string" ? value : String(value);
}

// A number-input value: the number itself, or "" for an absent/blank field (so
// the input clears instead of showing 0).
export function numText(value) {
  return value == null || String(value).trim() === "" ? "" : value;
}

// --- sheet ⇄ editable-row seeders (the map/list/spell/slot editors keep a local
// row buffer so a key rename or an empty row never collapses under the object
// store; the buffer is re-seeded from the sheet only on an EXTERNAL replace). ---
export function rowsFromMap(map) {
  return Object.entries(asObject(map) || {}).map(([k, v]) => ({
    k: String(k),
    v: v == null ? "" : String(v),
  }));
}

export function stringRowsFrom(list) {
  return asArray(list).map((v) => ({ text: typeof v === "string" ? v : v == null ? "" : String(v) }));
}

export function spellRowsFrom(list) {
  return asArray(list).map((sp) => {
    const o = asObject(sp) || {};
    return {
      name: textValue(o.name) || (typeof sp === "string" ? sp : ""),
      level: o.level == null ? "" : String(o.level),
      effect: textValue(o.effect),
      concentration: !!o.concentration,
      ritual: !!o.ritual,
    };
  });
}

export function slotRowsFrom(slots, max) {
  const levels = new Set();
  for (const m of [asObject(slots) || {}, asObject(max) || {}]) {
    for (const key of Object.keys(m)) {
      const n = parseInt(key, 10);
      if (Number.isInteger(n) && n >= 1 && n <= 9) levels.add(n);
    }
  }
  const cur = asObject(slots) || {};
  const cap = asObject(max) || {};
  return [...levels]
    .sort((a, b) => a - b)
    .map((level) => ({
      level,
      cur: cur[level] == null ? "" : String(cur[level]),
      max: cap[level] == null ? "" : String(cap[level]),
    }));
}

// --- row → sheet-value builders (the save/commit boundary). ---
// Навыки/спасброски: имя + модификатор. Целые числа сохраняются числами;
// всё остальное («advantage», «+5 (в лесу)», дробные) уходит СТРОКОЙ как есть —
// бэкенд (normalize_stat_dict) сам коэрсит числовые строки и по контракту
// «никогда не разрушает» текстовые пометки. Пустое значение при заполненном
// имени — 0 (историческое поведение студии).
export function mapFromRows(rows) {
  const obj = {};
  for (const r of rows) {
    const key = (r.k || "").trim();
    if (!key) continue;
    const raw = (r.v == null ? "" : String(r.v)).trim();
    if (!raw) {
      obj[key] = 0;
      continue;
    }
    const n = Number(raw);
    obj[key] = Number.isInteger(n) ? n : raw;
  }
  return obj;
}

// Normalize an «имя — описание» entry at the payload boundary: the editors bind
// RAW values while typing (trim in a controlled input eats the trailing space
// the user just typed), so head/tail are trimmed only here.
export function normalizeEntryString(text) {
  const idx = text.indexOf(ITEM_DESC_SEP);
  if (idx < 0) return text.trim();
  const head = text.slice(0, idx).trim();
  const tail = text.slice(idx + ITEM_DESC_SEP.length).trim();
  return tail ? `${head}${ITEM_DESC_SEP}${tail}` : head;
}

// Инвентарь/снаряжение/особенности как строки «имя — описание», нормализованные
// на границе сохранения (см. normalizeEntryString) — для панелей, собирающих
// payload разово; живой sheet-стейт студии хранит сырые строки до cleanDraft.
export function namedListFromRows(rows) {
  return rows.map((r) => normalizeEntryString(r.text || "")).filter((t) => t !== "");
}

export function spellsFromRows(rows) {
  const list = [];
  for (const r of rows) {
    const name = (r.name || "").trim();
    if (!name) continue;
    const lvlN = parseInt(r.level, 10);
    const level = Number.isFinite(lvlN) ? Math.max(0, Math.min(9, lvlN)) : 0;
    const sp = { name, level, concentration: !!r.concentration, ritual: !!r.ritual };
    const effect = (r.effect || "").trim();
    if (effect) sp.effect = effect;
    list.push(sp);
  }
  return list;
}

export function slotsFromRows(rows) {
  const slots = {};
  const max = {};
  for (const r of rows) {
    if (!(Number.isInteger(r.level) && r.level >= 1 && r.level <= 9)) continue;
    const c = parseInt(r.cur, 10);
    const m = parseInt(r.max, 10);
    if (Number.isFinite(c)) slots[String(r.level)] = c;
    if (Number.isFinite(m)) max[String(r.level)] = m;
  }
  return { slots, max };
}

export function missingSlotLevels(rows) {
  const present = new Set(rows.map((r) => r.level));
  const out = [];
  for (let l = 1; l <= 9; l += 1) if (!present.has(l)) out.push(l);
  return out;
}

// --- row-buffer hooks. onCommit (опционально) получает СЛЕДУЮЩИЙ буфер после
// правки/удаления — студия пересобирает из него sheet-поле на каждый ввод;
// разовые редакторы (дебаг-модалка) не передают его и читают rows на save.
// «Добавить» не коммитит: пустая строка не значима, пока в неё не начали писать.
export function useMapRows(initial, onCommit) {
  const [rows, setRows] = useState(() => rowsFromMap(initial));
  const edit = (i, patch) => {
    const next = rows.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    setRows(next);
    onCommit?.(next);
  };
  const add = () => setRows([...rows, { k: "", v: "" }]);
  const remove = (i) => {
    const next = rows.filter((_, idx) => idx !== i);
    setRows(next);
    onCommit?.(next);
  };
  const reseed = (map) => setRows(rowsFromMap(map));
  return { rows, edit, add, remove, reseed };
}

export function useStringRows(initial, onCommit) {
  const [rows, setRows] = useState(() => stringRowsFrom(initial));
  const edit = (i, text) => {
    const next = rows.map((r, idx) => (idx === i ? { text } : r));
    setRows(next);
    onCommit?.(next);
  };
  const add = () => setRows([...rows, { text: "" }]);
  const remove = (i) => {
    const next = rows.filter((_, idx) => idx !== i);
    setRows(next);
    onCommit?.(next);
  };
  const reseed = (list) => setRows(stringRowsFrom(list));
  return { rows, edit, add, remove, reseed };
}

export function useSpellRows(initial, onCommit) {
  const [rows, setRows] = useState(() => spellRowsFrom(initial));
  const [open, setOpen] = useState(() => new Set());
  const edit = (i, patch) => {
    const next = rows.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    setRows(next);
    onCommit?.(next);
  };
  const add = () => {
    setOpen((prev) => new Set(prev).add(rows.length));
    setRows([...rows, { name: "", level: "0", effect: "", concentration: false, ritual: false }]);
  };
  const remove = (i) => {
    const next = rows.filter((_, idx) => idx !== i);
    setRows(next);
    onCommit?.(next);
    setOpen((prev) => {
      const out = new Set();
      for (const idx of prev) {
        if (idx < i) out.add(idx);
        else if (idx > i) out.add(idx - 1);
      }
      return out;
    });
  };
  const toggle = (i) =>
    setOpen((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  const reseed = (spells) => {
    setRows(spellRowsFrom(spells));
    setOpen(new Set());
  };
  return { rows, open, edit, add, remove, toggle, reseed };
}

export function useSlotRows(initialSlots, initialMax, onCommit) {
  const [rows, setRows] = useState(() => slotRowsFrom(initialSlots, initialMax));
  const edit = (i, patch) => {
    const next = rows.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    setRows(next);
    onCommit?.(next);
  };
  const addLevel = (level) => {
    const next = [...rows, { level, cur: "", max: "" }].sort((a, b) => a.level - b.level);
    setRows(next);
    onCommit?.(next);
  };
  const remove = (i) => {
    const next = rows.filter((_, idx) => idx !== i);
    setRows(next);
    onCommit?.(next);
  };
  const reseed = (slots, max) => setRows(slotRowsFrom(slots, max));
  return { rows, edit, addLevel, remove, reseed, missing: missingSlotLevels(rows) };
}

// --- presentational blocks (world-bible / sheet-* классы — глобальный CSS,
// одинаково рендерятся и в студии, и в дебаг-модалке). ---
function EditorBlock({ hint, children }) {
  return (
    <div className="world-bible">
      <div className="world-bible-fields">
        {hint ? <p className="world-bible-hint">{hint}</p> : null}
        {children}
      </div>
    </div>
  );
}

// Характеристики — the six core abilities as number inputs (extra keys of the
// abilities object survive untouched — only the core six are editable).
export function AbilitiesEditor({ label = "Характеристики", abilities, onChange, disabled = false }) {
  const obj = asObject(abilities) || {};
  return (
    <EditorBlock hint={label}>
      <div className="character-abilities">
        {ABILITY_ORDER.map((key) => {
          const raw = obj[key];
          const mod = abilityMod(raw);
          return (
            <label className="character-ability character-ability-edit" key={key}>
              <span className="character-ability-k">{ABILITY_SHORT[key]}</span>
              <input
                type="number"
                className="character-ability-input"
                value={numText(raw)}
                onChange={(e) => onChange(key, e.target.value)}
                placeholder="—"
                disabled={disabled}
              />
              <span className="character-ability-mod">{mod != null ? fmtMod(mod) : "—"}</span>
            </label>
          );
        })}
      </div>
    </EditorBlock>
  );
}

// Навыки / спасброски: строка = имя + числовой модификатор.
export function MapRowsEditor({ label, rows, onEdit, onAdd, onRemove, keyPlaceholder, disabled = false }) {
  return (
    <EditorBlock hint={label}>
      <div className="sheet-rows">
        {rows.map((r, i) => (
          <div className="sheet-map-row" key={i}>
            <input
              className="sheet-map-key"
              value={r.k}
              onChange={(e) => onEdit(i, { k: e.target.value })}
              placeholder={keyPlaceholder}
              disabled={disabled}
            />
            {/* type="text": текстовые модификаторы («advantage», «+5 (в лесу)»)
                должны быть ВИДНЫ и редактируемы — number-инпут рендерит их
                пустым полем, и сохранение молча теряло бы значение. */}
            <input
              className="sheet-map-val"
              type="text"
              inputMode="numeric"
              value={r.v}
              onChange={(e) => onEdit(i, { v: e.target.value })}
              placeholder="±0"
              disabled={disabled}
            />
            <button
              type="button"
              className="sheet-row-del"
              onClick={() => onRemove(i)}
              disabled={disabled}
              aria-label="Удалить строку"
            >
              <Icon name="x" size={12} />
            </button>
          </div>
        ))}
        <button type="button" className="sheet-add-btn" onClick={onAdd} disabled={disabled}>
          + добавить
        </button>
      </div>
    </EditorBlock>
  );
}

// Two-field variant for the §И1 «имя — описание» string convention (space +
// EM DASH + space, split on the FIRST separator — mirrors gml-world
// helpers::item_head/item_tail/item_entry_string). The stored value stays a
// single string so the engine's head-matching keeps working; the editor only
// splits/joins for display. NO trim here: a trimmed controlled input eats the
// trailing space the user just typed — normalization lives at the payload
// boundary (cleanCharacterDraft / namedListFromRows).
const entryName = (text) => {
  const idx = String(text).indexOf(ITEM_DESC_SEP);
  return idx >= 0 ? String(text).slice(0, idx) : String(text);
};
const entryDesc = (text) => {
  const idx = String(text).indexOf(ITEM_DESC_SEP);
  return idx >= 0 ? String(text).slice(idx + ITEM_DESC_SEP.length) : "";
};
const entryJoin = (name, desc) => (desc ? `${name}${ITEM_DESC_SEP}${desc}` : name);

export function NamedListEditor({
  label,
  rows,
  onEdit,
  onAdd,
  onRemove,
  namePlaceholder = "Название",
  descPlaceholder = "Описание (необязательно)",
  disabled = false,
}) {
  return (
    <EditorBlock hint={label}>
      <div className="sheet-rows">
        {rows.map((r, i) => (
          <div className="sheet-named-row" key={i}>
            <input
              className="sheet-list-input sheet-named-name"
              value={entryName(r.text)}
              onChange={(e) => onEdit(i, entryJoin(e.target.value, entryDesc(r.text)))}
              placeholder={namePlaceholder}
              disabled={disabled}
            />
            <textarea
              className="sheet-list-input sheet-named-desc"
              value={entryDesc(r.text)}
              onChange={(e) => onEdit(i, entryJoin(entryName(r.text), e.target.value))}
              placeholder={descPlaceholder}
              disabled={disabled}
              rows={1}
            />
            <button
              type="button"
              className="sheet-row-del"
              onClick={() => onRemove(i)}
              disabled={disabled}
              aria-label="Удалить строку"
            >
              <Icon name="x" size={12} />
            </button>
          </div>
        ))}
        <button type="button" className="sheet-add-btn" onClick={onAdd} disabled={disabled}>
          + добавить
        </button>
      </div>
    </EditorBlock>
  );
}

// Заклинания — 5-field cards, collapsed to «{name} · {level} круг».
export function SpellsEditor({
  label = "Заклинания",
  rows,
  openSet,
  onToggle,
  onEdit,
  onAdd,
  onRemove,
  disabled = false,
}) {
  return (
    <EditorBlock hint={label}>
      <div className="sheet-rows">
        {rows.map((r, i) => {
          const open = openSet.has(i);
          const lvlN = parseInt(r.level, 10);
          const lvl = Number.isFinite(lvlN) ? Math.max(0, Math.min(9, lvlN)) : 0;
          return (
            <div className={`spell-edit${open ? " open" : ""}`} key={i}>
              <div className="spell-edit-head">
                <button type="button" className="spell-edit-toggle" onClick={() => onToggle(i)}>
                  <span className="mark">
                    <Icon name={open ? "chevron-down" : "chevron-right"} size={11} />
                  </span>
                  <span className="spell-edit-label">
                    {(r.name || "").trim() || "Без названия"} · {lvl} круг
                  </span>
                </button>
                <button
                  type="button"
                  className="sheet-row-del"
                  onClick={() => onRemove(i)}
                  disabled={disabled}
                  aria-label="Удалить заклинание"
                >
                  <Icon name="x" size={12} />
                </button>
              </div>
              {open && (
                <div className="spell-edit-body">
                  <div className="world-field-grid spell-name-grid">
                    <label className="world-field">
                      <span>Название</span>
                      <input
                        value={r.name}
                        onChange={(e) => onEdit(i, { name: e.target.value })}
                        placeholder="Огненный снаряд"
                        disabled={disabled}
                      />
                    </label>
                    <label className="world-field">
                      <span>Круг (0–9)</span>
                      <input
                        type="number"
                        min="0"
                        max="9"
                        value={r.level}
                        onChange={(e) => onEdit(i, { level: e.target.value })}
                        placeholder="0"
                        disabled={disabled}
                      />
                    </label>
                  </div>
                  <label className="world-field">
                    <span>Эффект</span>
                    <AutoTextarea
                      value={r.effect}
                      onChange={(e) => onEdit(i, { effect: e.target.value })}
                      placeholder="Что делает заклинание — коротко."
                      disabled={disabled}
                    />
                  </label>
                  <div className="spell-edit-flags">
                    <label className="sheet-check">
                      <input
                        type="checkbox"
                        checked={r.concentration}
                        onChange={(e) => onEdit(i, { concentration: e.target.checked })}
                        disabled={disabled}
                      />
                      <span>Концентрация</span>
                    </label>
                    <label className="sheet-check">
                      <input
                        type="checkbox"
                        checked={r.ritual}
                        onChange={(e) => onEdit(i, { ritual: e.target.checked })}
                        disabled={disabled}
                      />
                      <span>Ритуал</span>
                    </label>
                  </div>
                </div>
              )}
            </div>
          );
        })}
        <button type="button" className="sheet-add-btn" onClick={onAdd} disabled={disabled}>
          + добавить заклинание
        </button>
      </div>
    </EditorBlock>
  );
}

// Слоты заклинаний — flat level→текущие/макс maps.
export function SlotsEditor({
  label = "Слоты заклинаний",
  rows,
  missing,
  onEdit,
  onAddLevel,
  onRemove,
  disabled = false,
}) {
  return (
    <EditorBlock hint={label}>
      <div className="sheet-rows">
        {rows.map((r, i) => (
          <div className="slot-row" key={r.level}>
            <span className="slot-level">{r.level} круг</span>
            <input
              type="number"
              className="slot-num"
              value={r.cur}
              onChange={(e) => onEdit(i, { cur: e.target.value })}
              placeholder="тек."
              disabled={disabled}
            />
            <span className="slot-sep">/</span>
            <input
              type="number"
              className="slot-num"
              value={r.max}
              onChange={(e) => onEdit(i, { max: e.target.value })}
              placeholder="макс"
              disabled={disabled}
            />
            <button
              type="button"
              className="sheet-row-del"
              onClick={() => onRemove(i)}
              disabled={disabled}
              aria-label="Удалить круг"
            >
              <Icon name="x" size={12} />
            </button>
          </div>
        ))}
        {missing.length > 0 && (
          <div className="slot-add">
            {missing.map((lvl) => (
              <button
                key={lvl}
                type="button"
                className="sheet-add-btn"
                onClick={() => onAddLevel(lvl)}
                disabled={disabled}
              >
                + круг {lvl}
              </button>
            ))}
          </div>
        )}
      </div>
    </EditorBlock>
  );
}
