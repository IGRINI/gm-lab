import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api.js";
import WorldDetailModal from "./WorldDetailModal.jsx";
import {
  EMPTY_ARCHITECT_USAGE,
  textValue,
  normalizeVisibleMessage,
  AutoTextarea,
  useLiveSegments,
  ArchitectChatPane,
  ArchitectDebugModal,
  accumulateUsage,
  debugFromDone,
} from "./architectShared.jsx";
import "../styles-studio.css";

// The character architect panel (UI_REDESIGN_TZ §Студия персонажа). It is the
// third sibling of WorldArchitectPanel / StoryArchitectPanel and shares their
// chat/SSE machinery (architectShared.jsx). It authors a STANDALONE, portable
// character sheet (the `.gmchar` package's `player_character` object): the draft
// is the flat sheet the backend's draft_player_character / edit_player_character
// tools mutate (name, pronouns, class_role, level, background, abilities, skills,
// hp, inventory, spells, …). There is NO bound world — a character is orthogonal.
//
// The right column is a FULL manual editor: every scalar, map and list is edited
// in place. Two persistence paths coexist:
//   • the architect chat — the edited sheet rides as the `draft` param of the NEXT
//     architect message (the server snapshot-replaces the package with it);
//   • a DIRECT save («Сохранить лист») — POST /characters/{id}/draft for an
//     existing package, or POST /characters for a fresh one, with no chat turn.
//
// NOTE on the CONTENT contract: unlike the story panel (server shallow-MERGES the
// posted draft into `seed`), the character server SNAPSHOT-REPLACES the whole
// `player_character` when a draft is sent. So the panel keeps and posts the FULL
// sheet (preserving unknown keys such as `card_revision`), never a pruned subset
// — dropping a key would erase it from the package.

const DEFAULT_ARCHITECT_MESSAGES = [
  {
    role: "assistant",
    content:
      "Опиши персонажа, которого хочешь собрать, — или дай направление, а лист я соберу сам.\n\nЧто особенно полезно:\n\n1. Имя, роль или класс, уровень.\n2. Характер, ценности, происхождение и внешность.\n3. Характеристики (сила, ловкость и т.д.) и ключевые навыки.\n4. ХП, класс доспеха, снаряжение и инвентарь.\n5. Заклинания и слоты, если персонаж их использует.\n\nЯ собираю один переносимый лист персонажа — его можно будет запустить в любой истории.",
  },
];

// D&D ability keys arrive from the model in English — localize the six core ones.
const ABILITY_SHORT = { STR: "СИЛ", DEX: "ЛОВ", CON: "ТЕЛ", INT: "ИНТ", WIS: "МДР", CHA: "ХАР" };
// The fixed order the six abilities render in (extra keys are preserved but not
// shown — the editor only exposes the core six inputs).
const ABILITY_ORDER = ["STR", "DEX", "CON", "INT", "WIS", "CHA"];

// The stat maps that DEEP-merge key-by-key on a live draft_player_character call
// (mirrors the backend's per-object merge); everything else overwrites.
const DEEP_MERGE_KEYS = new Set([
  "abilities",
  "skills",
  "saving_throws",
  "hp",
  "spell_slots",
  "spell_slots_max",
]);

function asObject(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}

function asArray(value) {
  return Array.isArray(value) ? value : [];
}

function abilityMod(score) {
  const n = Number(score);
  if (!Number.isFinite(n)) return null;
  return Math.floor((n - 10) / 2);
}

function fmtMod(mod) {
  return mod >= 0 ? `+${mod}` : String(mod);
}

// A scalar shown in a text input — strings pass through, arrays are joined so a
// legacy list value (e.g. senses/languages authored as an array) stays editable.
function scalarText(value) {
  if (Array.isArray(value)) return value.filter((x) => x != null).join(", ");
  if (value == null) return "";
  return typeof value === "string" ? value : String(value);
}

// A number-input value: the number itself, or "" for an absent/blank field (so
// the input clears instead of showing 0).
function numText(value) {
  return value == null || String(value).trim() === "" ? "" : value;
}

// Deterministic stringify (keys sorted) for dirty tracking — key order in the
// sheet is not stable across edits, so a plain JSON.stringify would false-flag.
function stableStringify(value) {
  if (Array.isArray(value)) return `[${value.map(stableStringify).join(",")}]`;
  if (value && typeof value === "object") {
    return `{${Object.keys(value)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${stableStringify(value[k])}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

// --- sheet ⇄ editable-row seeders (the map/list/spell/slot editors keep a local
// row buffer so a key rename or an empty row never collapses under the object
// store; the buffer is re-seeded from the sheet only on an EXTERNAL replace). ---
function rowsFromMap(map) {
  return Object.entries(asObject(map) || {}).map(([k, v]) => ({
    k: String(k),
    v: v == null ? "" : String(v),
  }));
}

function stringRowsFrom(list) {
  return asArray(list).map((v) => ({ text: typeof v === "string" ? v : v == null ? "" : String(v) }));
}

function spellRowsFrom(list) {
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

function slotRowsFrom(slots, max) {
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

// Build the panel sheet from a saved character catalog row. The full sheet lives
// in `payload.player_character`; keep EVERY key verbatim (incl. card_revision and
// any field the panel does not render) so the snapshot-replace round-trips it.
function characterSheetFromSaved(character) {
  const payload = asObject(character?.payload) || {};
  const pc = asObject(payload.player_character) || {};
  return { ...pc };
}

// Restore the visible conversation from the server's architect block
// (`GET /characters/{id}/architect` → `{architect: {messages}}`). The chat lives
// in the dialogs SQLite (architect_chats kind='character'), never in the package.
function characterMessagesFromChat(architect) {
  const messages = asArray(architect?.messages).map(normalizeVisibleMessage).filter(Boolean);
  return messages.length > 0 ? messages : DEFAULT_ARCHITECT_MESSAGES;
}

// The sheet POSTed as `draft` (and, verbatim, as the direct-save body). The
// backend REPLACES the whole player_character, so send the FULL sheet — only
// truly-empty values (blank strings, empty lists / objects) are dropped as noise;
// numbers, booleans and unknown keys pass through.
function cleanCharacterDraft(sheet) {
  const out = {};
  for (const [key, value] of Object.entries(asObject(sheet) || {})) {
    if (value == null) continue;
    if (typeof value === "string") {
      const trimmed = value.trim();
      if (trimmed) out[key] = trimmed;
    } else if (Array.isArray(value)) {
      if (value.length > 0) out[key] = value;
    } else if (typeof value === "object") {
      if (Object.keys(value).length > 0) out[key] = value;
    } else {
      out[key] = value; // numbers / booleans (level, ac, passive_perception, card_revision)
    }
  }
  return out;
}

// Merge a draft_player_character tool call's args (or a final draft) into the
// panel sheet. The six stat maps merge key-by-key; scalars and lists (incl. the
// spells array) overwrite — mirrors the backend so the live view matches the store.
function mergeCharacterSheet(current, args) {
  const patch = asObject(args);
  if (!patch) return current;
  const next = { ...current };
  for (const [key, value] of Object.entries(patch)) {
    if (DEEP_MERGE_KEYS.has(key) && asObject(value)) {
      const base = asObject(next[key]) || {};
      next[key] = { ...base, ...value };
    } else {
      next[key] = value;
    }
  }
  return next;
}

// A sheet is launchable/saveable once it has a name (the runtime minimum).
function characterReady(sheet) {
  return !!textValue(sheet.name);
}

export default function CharacterArchitectPanel({
  character,
  locked,
  onArchitectStream,
  onPlayCharacter,
  onCharacterPersisted,
  notify,
  className = "",
}) {
  // The full sheet (the `.gmchar` payload's player_character). Seeded from the
  // catalog row's `payload` (the /characters list carries it); the conversation
  // comes from the architect fetch below. Model history + cache ids are SERVER-
  // side (the dialogs SQLite) — the panel holds only the visible chat.
  const [sheet, setSheet] = useState(() => characterSheetFromSaved(character));
  const [messages, setMessages] = useState(() => characterMessagesFromChat(null));
  const [input, setInput] = useState("");
  const [architectBusy, setArchitectBusy] = useState(false);
  const [architectError, setArchitectError] = useState("");
  // The last message whose turn FAILED — powers the «Повторить» button.
  const [retryText, setRetryText] = useState("");
  const [architectUsage, setArchitectUsage] = useState(EMPTY_ARCHITECT_USAGE);
  const [architectDebug, setArchitectDebug] = useState(null);
  const [debugOpen, setDebugOpen] = useState(false);
  const [architectElapsed, setArchitectElapsed] = useState(0);
  const [sheetOpen, setSheetOpen] = useState(false);
  const { liveSegments, liveSegmentsRef, appendLiveDelta, pushLiveTool, clearLive } =
    useLiveSegments();
  // The character id captured from the last architect_done (a create returns the
  // new id); until then a fresh character sends no character_id.
  const [currentCharacterId, setCurrentCharacterId] = useState(() => textValue(character?.id) || "");
  // Start as `null` (not the mount id) so the load effect ALWAYS runs on mount —
  // for an existing character that means restoring its conversation on open.
  const loadedCharacterIdRef = useRef(null);

  // --- editable-row buffers for the list/map/spell/slot editors (see the seeder
  // helpers above). Scalars, abilities and hp edit the sheet directly (fixed
  // keys, no rename/add/remove), so they need no buffer. ---
  const [skillRows, setSkillRows] = useState(() => rowsFromMap(sheet.skills));
  const [saveRows, setSaveRows] = useState(() => rowsFromMap(sheet.saving_throws));
  const [invRows, setInvRows] = useState(() => stringRowsFrom(sheet.inventory));
  const [equipRows, setEquipRows] = useState(() => stringRowsFrom(sheet.equipment));
  const [featRows, setFeatRows] = useState(() => stringRowsFrom(sheet.features));
  const [spellRows, setSpellRows] = useState(() => spellRowsFrom(sheet.spells));
  const [slotRows, setSlotRows] = useState(() => slotRowsFrom(sheet.spell_slots, sheet.spell_slots_max));
  const [openSpells, setOpenSpells] = useState(() => new Set());

  // A monotonic token bumped only when the sheet is REPLACED wholesale (load,
  // architect_done adopt, direct-save adopt) — the reseed effect below rebuilds
  // the row buffers from it. Ordinary in-place edits never bump it, so typing in
  // a row never triggers a reseed (which would drop focus / empty rows).
  const [externalRev, setExternalRev] = useState(0);
  const sheetRef = useRef(sheet);
  sheetRef.current = sheet;

  // The stable-stringified draft last known to be PERSISTED (server-side). Dirty
  // tracking compares the live draft against it; it is refreshed on every adopt.
  const draftPayload = useMemo(() => cleanCharacterDraft(sheet), [sheet]);
  const [persistedKey, setPersistedKey] = useState(() =>
    stableStringify(cleanCharacterDraft(characterSheetFromSaved(character)))
  );
  const dirty = useMemo(
    () => stableStringify(draftPayload) !== persistedKey,
    [draftPayload, persistedKey]
  );

  const [saveBusy, setSaveBusy] = useState(false);
  const [saveError, setSaveError] = useState("");

  const ready = characterReady(sheet);
  const architectLocked = locked || architectBusy;
  // The manual editor is frozen while an architect turn is in flight (a turn
  // replaces the sheet on `done`) and while a direct save is round-tripping.
  const editDisabled = locked || architectBusy || saveBusy;

  // Adopt a server-authoritative sheet as the new source of truth: replace the
  // sheet, mark it persisted (clears dirty), and reseed the row buffers.
  const adoptSheet = (pc) => {
    const next = { ...(asObject(pc) || {}) };
    setSheet(next);
    setPersistedKey(stableStringify(cleanCharacterDraft(next)));
    setExternalRev((v) => v + 1);
  };

  // Reseed the row buffers whenever the sheet was replaced wholesale.
  useEffect(() => {
    const s = sheetRef.current;
    setSkillRows(rowsFromMap(s.skills));
    setSaveRows(rowsFromMap(s.saving_throws));
    setInvRows(stringRowsFrom(s.inventory));
    setEquipRows(stringRowsFrom(s.equipment));
    setFeatRows(stringRowsFrom(s.features));
    setSpellRows(spellRowsFrom(s.spells));
    setSlotRows(slotRowsFrom(s.spell_slots, s.spell_slots_max));
    setOpenSpells(new Set());
  }, [externalRev]);

  // Reload the sheet + conversation only when the user opens a DIFFERENT character
  // (or switches from a fresh draft to a saved one). The character our own turn
  // just created/updated is already ours — reloading would wipe the live chat.
  //
  // The sheet is restored SYNCHRONOUSLY from the `character` prop's `payload`
  // (the /characters catalog row carries the full sheet); only the conversation
  // is fetched (`GET /characters/{id}/architect` → `{architect.messages}`).
  useEffect(() => {
    const id = textValue(character?.id) || null;
    if (id === loadedCharacterIdRef.current) return undefined;
    loadedCharacterIdRef.current = id;
    const nextSheet = characterSheetFromSaved(character);
    setSheet(nextSheet);
    setPersistedKey(stableStringify(cleanCharacterDraft(nextSheet)));
    setExternalRev((v) => v + 1);
    setMessages(characterMessagesFromChat(null));
    setCurrentCharacterId(id || "");
    clearLive();
    setInput("");
    setArchitectError("");
    setRetryText("");
    setSaveError("");
    setArchitectUsage(EMPTY_ARCHITECT_USAGE);
    setArchitectDebug(null);
    setDebugOpen(false);
    if (!id) return undefined;
    // `cancelled` guards a stale response when the user reopens a different
    // character before this resolves.
    let cancelled = false;
    api
      .characterArchitect(id)
      .then((data) => {
        if (cancelled || loadedCharacterIdRef.current !== id) return;
        if (!data?.ok) {
          throw new Error(data?.error || "не удалось загрузить переписку персонажа");
        }
        setMessages(characterMessagesFromChat(data.architect));
      })
      .catch((error) => {
        if (cancelled || loadedCharacterIdRef.current !== id) return;
        setArchitectError(error?.message || "не удалось загрузить переписку персонажа");
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [character?.id]);

  // Tick an elapsed-seconds counter while the architect works (visible progress).
  useEffect(() => {
    if (!architectBusy) {
      setArchitectElapsed(0);
      return undefined;
    }
    const startedAt = Date.now();
    const id = window.setInterval(() => {
      setArchitectElapsed(Math.floor((Date.now() - startedAt) / 1000));
    }, 1000);
    return () => window.clearInterval(id);
  }, [architectBusy]);

  // --- direct field mutators (edit the sheet in place; numeric fields omit the
  // key on empty / NaN exactly like the original level input). ---
  const updateField = (field, value) => {
    setSheet((current) => ({ ...current, [field]: value }));
  };
  const updateNumberField = (field, text) => {
    const n = parseInt(text, 10);
    setSheet((current) => {
      const next = { ...current };
      if (Number.isFinite(n)) next[field] = n;
      else delete next[field];
      return next;
    });
  };
  const updateAbility = (key, text) => {
    const n = parseInt(text, 10);
    setSheet((current) => {
      const abilities = { ...(asObject(current.abilities) || {}) };
      if (Number.isFinite(n)) abilities[key] = n;
      else delete abilities[key];
      const next = { ...current };
      if (Object.keys(abilities).length > 0) next.abilities = abilities;
      else delete next.abilities;
      return next;
    });
  };
  const updateHp = (key, text) => {
    const n = parseInt(text, 10);
    setSheet((current) => {
      const hp = { ...(asObject(current.hp) || {}) };
      if (Number.isFinite(n)) hp[key] = n;
      else delete hp[key];
      const next = { ...current };
      if (Object.keys(hp).length > 0) next.hp = hp;
      else delete next.hp;
      return next;
    });
  };

  // --- map editors (skills / saving_throws): a row buffer with a name + numeric
  // modifier; the sheet object is rebuilt from the buffer on every edit. ---
  const commitMap = (field, rows) => {
    setSheet((current) => {
      const obj = {};
      for (const r of rows) {
        const key = (r.k || "").trim();
        if (!key) continue;
        const n = parseInt(r.v, 10);
        obj[key] = Number.isFinite(n) ? n : 0;
      }
      const next = { ...current };
      if (Object.keys(obj).length > 0) next[field] = obj;
      else delete next[field];
      return next;
    });
  };
  const mapHandlers = (field, rows, setRows) => ({
    edit: (i, patch) => {
      const next = rows.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
      setRows(next);
      commitMap(field, next);
    },
    add: () => setRows([...rows, { k: "", v: "" }]),
    remove: (i) => {
      const next = rows.filter((_, idx) => idx !== i);
      setRows(next);
      commitMap(field, next);
    },
  });

  // --- string-list editors (inventory / equipment / features). ---
  const commitList = (field, rows) => {
    setSheet((current) => {
      const list = rows.map((r) => r.text).filter((t) => t.trim() !== "");
      const next = { ...current };
      if (list.length > 0) next[field] = list;
      else delete next[field];
      return next;
    });
  };
  const listHandlers = (field, rows, setRows) => ({
    edit: (i, text) => {
      const next = rows.map((r, idx) => (idx === i ? { text } : r));
      setRows(next);
      commitList(field, next);
    },
    add: () => setRows([...rows, { text: "" }]),
    remove: (i) => {
      const next = rows.filter((_, idx) => idx !== i);
      setRows(next);
      commitList(field, next);
    },
  });

  // --- spell editor (5-field cards). ---
  const commitSpells = (rows) => {
    setSheet((current) => {
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
      const next = { ...current };
      if (list.length > 0) next.spells = list;
      else delete next.spells;
      return next;
    });
  };
  const editSpell = (i, patch) => {
    const next = spellRows.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    setSpellRows(next);
    commitSpells(next);
  };
  const addSpell = () => {
    const newIndex = spellRows.length;
    setSpellRows([
      ...spellRows,
      { name: "", level: "0", effect: "", concentration: false, ritual: false },
    ]);
    setOpenSpells((prev) => new Set(prev).add(newIndex));
  };
  const removeSpell = (i) => {
    const next = spellRows.filter((_, idx) => idx !== i);
    setSpellRows(next);
    commitSpells(next);
    setOpenSpells((prev) => {
      const out = new Set();
      for (const idx of prev) {
        if (idx < i) out.add(idx);
        else if (idx > i) out.add(idx - 1);
      }
      return out;
    });
  };
  const toggleSpell = (i) =>
    setOpenSpells((prev) => {
      const next = new Set(prev);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });

  // --- spell-slot editor (flat level→count maps; levels 1-9, no duplicates). ---
  const commitSlots = (rows) => {
    setSheet((current) => {
      const slots = {};
      const max = {};
      for (const r of rows) {
        if (!(Number.isInteger(r.level) && r.level >= 1 && r.level <= 9)) continue;
        const c = parseInt(r.cur, 10);
        const m = parseInt(r.max, 10);
        if (Number.isFinite(c)) slots[String(r.level)] = c;
        if (Number.isFinite(m)) max[String(r.level)] = m;
      }
      const next = { ...current };
      if (Object.keys(slots).length > 0) next.spell_slots = slots;
      else delete next.spell_slots;
      if (Object.keys(max).length > 0) next.spell_slots_max = max;
      else delete next.spell_slots_max;
      return next;
    });
  };
  const editSlot = (i, patch) => {
    const next = slotRows.map((r, idx) => (idx === i ? { ...r, ...patch } : r));
    setSlotRows(next);
    commitSlots(next);
  };
  const addSlot = (level) => {
    const next = [...slotRows, { level, cur: "", max: "" }].sort((a, b) => a.level - b.level);
    setSlotRows(next);
    commitSlots(next);
  };
  const removeSlot = (i) => {
    const next = slotRows.filter((_, idx) => idx !== i);
    setSlotRows(next);
    commitSlots(next);
  };
  const missingSlotLevels = useMemo(() => {
    const present = new Set(slotRows.map((r) => r.level));
    const out = [];
    for (let l = 1; l <= 9; l += 1) if (!present.has(l)) out.push(l);
    return out;
  }, [slotRows]);

  // One architect turn. `appendUser=false` is the RETRY path: the visible chat
  // already carries the user message (and the failure note) from the failed
  // attempt, so only the request is repeated.
  const runArchitectTurn = async (text, appendUser) => {
    const visibleMessages = appendUser
      ? [...messages, { role: "user", content: text }]
      : [...messages];
    setArchitectError("");
    setArchitectBusy(true);
    clearLive();
    setMessages(visibleMessages);
    let adopted = false;
    let failure = "";
    try {
      // The server owns the conversation (model history + cache ids live in the
      // dialogs SQLite). The body carries only the message, the target id, and
      // the FULL sheet — the server snapshot-replaces the package with it before
      // the turn, so hand-edited fields are never lost.
      await onArchitectStream?.(
        {
          message: text,
          draft: draftPayload,
          // A create sends no id; an edit carries the resolved character_id.
          ...(currentCharacterId ? { character_id: currentCharacterId } : {}),
        },
        (ev) => {
          if (ev.kind === "architect_delta") {
            const d = ev.data || {};
            const sid = textValue(d.sid) || "arch";
            const role = d.channel === "thinking" ? "think" : "assistant";
            appendLiveDelta(sid, role, String(d.text || ""));
          } else if (ev.kind === "architect_tool") {
            const call = ev.data || {};
            const name = textValue(call.name);
            if (!name) return;
            const args = asObject(call.arguments) || {};
            const sid = textValue(call.sid) || "arch";
            pushLiveTool(sid, name, args);
            // draft_player_character args merge live; the targeted
            // edit_player_character patch is folded from the authoritative sheet
            // in the done payload (its ops are non-trivial to replay client-side).
            if (name === "draft_player_character") {
              setSheet((current) => mergeCharacterSheet(current, args));
            }
          } else if (ev.kind === "architect_error") {
            failure = textValue(ev.data) || "Архитектор не ответил";
          } else if (ev.kind === "architect_done") {
            adopted = true;
            const data = ev.data || {};
            const usage = asObject(data.usage);
            if (usage) setArchitectUsage((current) => accumulateUsage(current, usage));
            setArchitectDebug(debugFromDone(data, usage));
            // Adopt the persisted sheet as the source of truth: the server folded
            // the model's draft (draft_player_character merge OR edit ops) into the
            // package, so restore from it rather than replay ops.
            const savedChar = asObject(data.character);
            const savedPayload = savedChar ? asObject(savedChar.payload) : null;
            const savedPc = savedPayload ? asObject(savedPayload.player_character) : null;
            if (savedPc) {
              adoptSheet(savedPc);
            } else if (asObject(data.draft)) {
              adoptSheet(mergeCharacterSheet(sheetRef.current, data.draft));
            }
            // The conversation: fold this turn's live segments into the visible
            // chat — the same shape the server just persisted.
            setMessages([...visibleMessages, ...liveSegmentsRef.current]);
            // The character we just created/updated is ours — pin its id so a
            // parent characters-list refresh (which may re-key the `character`
            // prop) does not wipe this live conversation, and route the next turn
            // as an edit.
            const persistedId = textValue(data.character_id) || textValue(savedChar?.id);
            if (persistedId) {
              setCurrentCharacterId(persistedId);
              loadedCharacterIdRef.current = persistedId;
            }
            clearLive();
          }
        }
      );
      if (failure) throw new Error(failure);
      setRetryText("");
    } catch (error) {
      const message = error?.message || "Не удалось вызвать архитектора";
      setArchitectError(message);
      setRetryText(text);
      if (!adopted) {
        setMessages((current) => [
          ...current,
          ...liveSegmentsRef.current,
          { role: "assistant", content: `Не получилось обновить персонажа: ${message}` },
        ]);
        clearLive();
      }
    } finally {
      setArchitectBusy(false);
    }
  };

  const sendArchitectMessage = async () => {
    const text = input.trim();
    if (!text || architectLocked) return;
    setInput("");
    await runArchitectTurn(text, true);
  };

  const retryArchitectTurn = async () => {
    if (!retryText || architectLocked) return;
    await runArchitectTurn(retryText, false);
  };

  // Direct manual save — no architect turn. An existing package is snapshotted
  // (POST /characters/{id}/draft); a fresh draft is created (POST /characters),
  // whose returned id is pinned via `onCharacterPersisted` (App re-selects it
  // WITHOUT bumping the studio epoch, so this live panel is not remounted).
  const saveSheet = async () => {
    if (editDisabled || !dirty) return;
    const payload = draftPayload;
    setSaveBusy(true);
    setSaveError("");
    try {
      let character;
      if (currentCharacterId) {
        const data = await api.saveCharacterDraft(currentCharacterId, payload);
        if (!data?.ok) throw new Error(data?.error || "не удалось сохранить лист");
        character = data.character;
      } else {
        const title = textValue(payload.name) || "Персонаж";
        const data = await api.createCharacter({
          title,
          payload: { player_character: payload },
        });
        if (!data?.ok) throw new Error(data?.error || "не удалось создать персонажа");
        character = data.character;
        const newId = textValue(character?.id);
        if (newId) {
          setCurrentCharacterId(newId);
          loadedCharacterIdRef.current = newId;
        }
      }
      // Adopt the server-authoritative sheet (clears dirty, reseeds the buffers);
      // fall back to what we sent if the response omits the payload.
      const savedPc =
        asObject(asObject(character?.payload)?.player_character) || payload;
      adoptSheet(savedPc);
      onCharacterPersisted?.(character);
    } catch (error) {
      const message = error?.message || "не удалось сохранить лист";
      setSaveError(message);
      notify?.(message);
    } finally {
      setSaveBusy(false);
    }
  };

  const heroName = textValue(sheet.name);
  const abilitiesObj = asObject(sheet.abilities) || {};
  const hpObj = asObject(sheet.hp) || {};
  const skillOps = mapHandlers("skills", skillRows, setSkillRows);
  const saveOps = mapHandlers("saving_throws", saveRows, setSaveRows);
  const invOps = listHandlers("inventory", invRows, setInvRows);
  const equipOps = listHandlers("equipment", equipRows, setEquipRows);
  const featOps = listHandlers("features", featRows, setFeatRows);

  const renderMapEditor = (label, rows, ops, keyPlaceholder) => (
    <div className="world-bible">
      <div className="world-bible-fields">
        <p className="world-bible-hint">{label}</p>
        <div className="sheet-rows">
          {rows.map((r, i) => (
            <div className="sheet-map-row" key={i}>
              <input
                className="sheet-map-key"
                value={r.k}
                onChange={(e) => ops.edit(i, { k: e.target.value })}
                placeholder={keyPlaceholder}
                disabled={editDisabled}
              />
              <input
                className="sheet-map-val"
                type="number"
                value={r.v}
                onChange={(e) => ops.edit(i, { v: e.target.value })}
                placeholder="±0"
                disabled={editDisabled}
              />
              <button
                type="button"
                className="sheet-row-del"
                onClick={() => ops.remove(i)}
                disabled={editDisabled}
                aria-label="Удалить строку"
              >
                ✕
              </button>
            </div>
          ))}
          <button
            type="button"
            className="sheet-add-btn"
            onClick={ops.add}
            disabled={editDisabled}
          >
            + добавить
          </button>
        </div>
      </div>
    </div>
  );

  const renderListEditor = (label, rows, ops, placeholder) => (
    <div className="world-bible">
      <div className="world-bible-fields">
        <p className="world-bible-hint">{label}</p>
        <div className="sheet-rows">
          {rows.map((r, i) => (
            <div className="sheet-list-row" key={i}>
              <input
                className="sheet-list-input"
                value={r.text}
                onChange={(e) => ops.edit(i, e.target.value)}
                placeholder={placeholder}
                disabled={editDisabled}
              />
              <button
                type="button"
                className="sheet-row-del"
                onClick={() => ops.remove(i)}
                disabled={editDisabled}
                aria-label="Удалить строку"
              >
                ✕
              </button>
            </div>
          ))}
          <button
            type="button"
            className="sheet-add-btn"
            onClick={ops.add}
            disabled={editDisabled}
          >
            + добавить
          </button>
        </div>
      </div>
    </div>
  );

  // Two-field variant for the §И1 «имя — описание» string convention (space +
  // EM DASH + space, split on the FIRST separator — mirrors gml-world
  // helpers::item_head/item_tail/item_entry_string). The stored value stays a
  // single string so the engine's head-matching keeps working; the editor only
  // splits/joins for display.
  const ITEM_DESC_SEP = " — ";
  const entryName = (text) => {
    const idx = String(text).indexOf(ITEM_DESC_SEP);
    return (idx >= 0 ? String(text).slice(0, idx) : String(text)).trim();
  };
  const entryDesc = (text) => {
    const idx = String(text).indexOf(ITEM_DESC_SEP);
    return idx >= 0 ? String(text).slice(idx + ITEM_DESC_SEP.length).trim() : "";
  };
  const entryJoin = (name, desc) => {
    const n = name.trim();
    const d = desc.trim();
    return d ? `${n}${ITEM_DESC_SEP}${d}` : n;
  };
  const renderNamedListEditor = (label, rows, ops, namePh, descPh) => (
    <div className="world-bible">
      <div className="world-bible-fields">
        <p className="world-bible-hint">{label}</p>
        <div className="sheet-rows">
          {rows.map((r, i) => (
            <div className="sheet-named-row" key={i}>
              <input
                className="sheet-list-input sheet-named-name"
                value={entryName(r.text)}
                onChange={(e) => ops.edit(i, entryJoin(e.target.value, entryDesc(r.text)))}
                placeholder={namePh}
                disabled={editDisabled}
              />
              <input
                className="sheet-list-input sheet-named-desc"
                value={entryDesc(r.text)}
                onChange={(e) => ops.edit(i, entryJoin(entryName(r.text), e.target.value))}
                placeholder={descPh}
                disabled={editDisabled}
              />
              <button
                type="button"
                className="sheet-row-del"
                onClick={() => ops.remove(i)}
                disabled={editDisabled}
                aria-label="Удалить строку"
              >
                ✕
              </button>
            </div>
          ))}
          <button
            type="button"
            className="sheet-add-btn"
            onClick={ops.add}
            disabled={editDisabled}
          >
            + добавить
          </button>
        </div>
      </div>
    </div>
  );

  return (
    <div className={`world-studio character-studio${className ? ` ${className}` : ""}`}>
      <header className="world-studio-head">
        <div className="world-studio-id">
          <span className="world-studio-emblem" aria-hidden="true">✦</span>
          <div className="world-studio-title">
            <span className="world-studio-kicker">создание персонажа</span>
            <b>Студия персонажей</b>
            <p className="world-studio-sub">
              Соберите переносимый лист героя с архитектором или отредактируйте каждое поле вручную
              и сохраните лист напрямую.
            </p>
          </div>
        </div>
        <span className={`world-studio-chip${ready ? " ready" : ""}`}>
          {ready ? "готов к запуску" : "черновик не готов"}
        </span>
      </header>

      <div className="world-studio-body">
        <ArchitectChatPane
          headKicker="архитектор"
          headTitle="Собрать персонажа"
          usageTitle="Токены архитектора персонажа"
          helpTitle="Архитектор персонажа"
          helpSubtitle="Отдельный AI-контур до старта игры."
          helpNote="Он собирает один переносимый лист персонажа: имя и роль, характеристики, навыки, ХП, инвентарь, снаряжение и заклинания. Готового героя можно запустить в любой истории или заменить им протагониста."
          thinkLabel="🧠 Архитектор рассуждает"
          placeholder="Например: суровый следопыт-полуэльф, выросший на болотах, мастер лука и трав… (Enter — отправить)"
          messages={messages}
          liveSegments={liveSegments}
          busy={architectBusy}
          elapsed={architectElapsed}
          error={architectError}
          usage={architectUsage}
          debug={architectDebug}
          onOpenDebug={() => setDebugOpen(true)}
          input={input}
          onInputChange={setInput}
          onSend={sendArchitectMessage}
          onRetry={retryText ? retryArchitectTurn : undefined}
          locked={architectLocked}
        />

        <section
          className={`world-studio-pane world-inspector character-sheet${ready ? " is-live" : ""}`}
          aria-label="Лист персонажа"
        >
          <div className="world-inspector-head">
            <span className="world-inspector-kicker">персонаж</span>
            <b>{heroName || "Без имени"}</b>
          </div>

          <div className="world-inspector-body">
            <div className="world-field-grid">
              <label className="world-field">
                <span>Имя</span>
                <input
                  value={textValue(sheet.name)}
                  onChange={(event) => updateField("name", event.target.value)}
                  placeholder="Например: Кара Вент"
                  disabled={editDisabled}
                />
              </label>
              <label className="world-field">
                <span>Роль / класс</span>
                <input
                  value={textValue(sheet.class_role)}
                  onChange={(event) => updateField("class_role", event.target.value)}
                  placeholder="Например: следопыт"
                  disabled={editDisabled}
                />
              </label>
            </div>

            <div className="world-field-grid">
              <label className="world-field">
                <span>Местоимения</span>
                <input
                  value={textValue(sheet.pronouns)}
                  onChange={(event) => updateField("pronouns", event.target.value)}
                  placeholder="она/её"
                  disabled={editDisabled}
                />
              </label>
              <label className="world-field">
                <span>Уровень</span>
                <input
                  type="number"
                  value={numText(sheet.level)}
                  onChange={(event) => updateNumberField("level", event.target.value)}
                  placeholder="1"
                  disabled={editDisabled}
                />
              </label>
            </div>

            <label className="world-field">
              <span>Возраст</span>
              <input
                value={textValue(sheet.age)}
                onChange={(event) => updateField("age", event.target.value)}
                placeholder="Например: 27"
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Внешность</span>
              <AutoTextarea
                value={textValue(sheet.physical_type)}
                onChange={(event) => updateField("physical_type", event.target.value)}
                placeholder="Телосложение, черты, как выглядит."
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Особые приметы</span>
              <AutoTextarea
                value={textValue(sheet.distinctive_features)}
                onChange={(event) => updateField("distinctive_features", event.target.value)}
                placeholder="Шрамы, татуировки, что запоминается."
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Происхождение</span>
              <AutoTextarea
                value={textValue(sheet.background)}
                onChange={(event) => updateField("background", event.target.value)}
                placeholder="Откуда герой, что его сформировало."
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Характер</span>
              <AutoTextarea
                value={textValue(sheet.personality)}
                onChange={(event) => updateField("personality", event.target.value)}
                placeholder="Как ведёт себя, что движет героем."
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Ценности</span>
              <AutoTextarea
                value={textValue(sheet.values)}
                onChange={(event) => updateField("values", event.target.value)}
                placeholder="Во что верит, чем не поступится."
                disabled={editDisabled}
              />
            </label>

            {/* Характеристики — the six core abilities as number inputs. */}
            <div className="world-bible">
              <div className="world-bible-fields">
                <p className="world-bible-hint">Характеристики</p>
                <div className="character-abilities">
                  {ABILITY_ORDER.map((key) => {
                    const raw = abilitiesObj[key];
                    const mod = abilityMod(raw);
                    return (
                      <label className="character-ability character-ability-edit" key={key}>
                        <span className="character-ability-k">{ABILITY_SHORT[key]}</span>
                        <input
                          type="number"
                          className="character-ability-input"
                          value={numText(raw)}
                          onChange={(e) => updateAbility(key, e.target.value)}
                          placeholder="—"
                          disabled={editDisabled}
                        />
                        <span className="character-ability-mod">
                          {mod != null ? fmtMod(mod) : "—"}
                        </span>
                      </label>
                    );
                  })}
                </div>
              </div>
            </div>

            {/* Боевые параметры — КД, ХП, пассивное восприятие, скорость, чувства, языки. */}
            <div className="world-bible">
              <div className="world-bible-fields">
                <p className="world-bible-hint">Боевые параметры</p>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>КД</span>
                    <input
                      type="number"
                      value={numText(sheet.ac)}
                      onChange={(e) => updateNumberField("ac", e.target.value)}
                      placeholder="—"
                      disabled={editDisabled}
                    />
                  </label>
                  <label className="world-field">
                    <span>Пасс. восприятие</span>
                    <input
                      type="number"
                      value={numText(sheet.passive_perception)}
                      onChange={(e) => updateNumberField("passive_perception", e.target.value)}
                      placeholder="—"
                      disabled={editDisabled}
                    />
                  </label>
                </div>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>ХП сейчас</span>
                    <input
                      type="number"
                      value={numText(hpObj.current)}
                      onChange={(e) => updateHp("current", e.target.value)}
                      placeholder="—"
                      disabled={editDisabled}
                    />
                  </label>
                  <label className="world-field">
                    <span>ХП максимум</span>
                    <input
                      type="number"
                      value={numText(hpObj.max)}
                      onChange={(e) => updateHp("max", e.target.value)}
                      placeholder="—"
                      disabled={editDisabled}
                    />
                  </label>
                </div>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>Скорость</span>
                    <input
                      value={scalarText(sheet.speed)}
                      onChange={(e) => updateField("speed", e.target.value)}
                      placeholder="Например: 30 фт"
                      disabled={editDisabled}
                    />
                  </label>
                  <label className="world-field">
                    <span>Чувства</span>
                    <input
                      value={scalarText(sheet.senses)}
                      onChange={(e) => updateField("senses", e.target.value)}
                      placeholder="Тёмное зрение 18 м"
                      disabled={editDisabled}
                    />
                  </label>
                </div>
                <label className="world-field">
                  <span>Языки</span>
                  <input
                    value={scalarText(sheet.languages)}
                    onChange={(e) => updateField("languages", e.target.value)}
                    placeholder="Общий, эльфийский"
                    disabled={editDisabled}
                  />
                </label>
              </div>
            </div>

            {renderMapEditor("Навыки", skillRows, skillOps, "Навык")}
            {renderMapEditor("Спасброски", saveRows, saveOps, "Спасбросок")}
            {renderNamedListEditor("Инвентарь", invRows, invOps, "Название", "Описание (необязательно)")}
            {renderNamedListEditor("Снаряжение", equipRows, equipOps, "Название", "Описание (необязательно)")}
            {renderNamedListEditor("Особенности", featRows, featOps, "Название", "Что даёт (необязательно)")}

            {/* Заклинания — 5-field cards, collapsed to «{name} · {level} круг». */}
            <div className="world-bible">
              <div className="world-bible-fields">
                <p className="world-bible-hint">Заклинания</p>
                <div className="sheet-rows">
                  {spellRows.map((r, i) => {
                    const open = openSpells.has(i);
                    const lvlN = parseInt(r.level, 10);
                    const lvl = Number.isFinite(lvlN) ? Math.max(0, Math.min(9, lvlN)) : 0;
                    return (
                      <div className={`spell-edit${open ? " open" : ""}`} key={i}>
                        <div className="spell-edit-head">
                          <button
                            type="button"
                            className="spell-edit-toggle"
                            onClick={() => toggleSpell(i)}
                          >
                            <span className="mark">{open ? "▾" : "▸"}</span>
                            <span className="spell-edit-label">
                              {(r.name || "").trim() || "Без названия"} · {lvl} круг
                            </span>
                          </button>
                          <button
                            type="button"
                            className="sheet-row-del"
                            onClick={() => removeSpell(i)}
                            disabled={editDisabled}
                            aria-label="Удалить заклинание"
                          >
                            ✕
                          </button>
                        </div>
                        {open && (
                          <div className="spell-edit-body">
                            <div className="world-field-grid">
                              <label className="world-field">
                                <span>Название</span>
                                <input
                                  value={r.name}
                                  onChange={(e) => editSpell(i, { name: e.target.value })}
                                  placeholder="Огненный снаряд"
                                  disabled={editDisabled}
                                />
                              </label>
                              <label className="world-field">
                                <span>Круг (0–9)</span>
                                <input
                                  type="number"
                                  min="0"
                                  max="9"
                                  value={r.level}
                                  onChange={(e) => editSpell(i, { level: e.target.value })}
                                  placeholder="0"
                                  disabled={editDisabled}
                                />
                              </label>
                            </div>
                            <label className="world-field">
                              <span>Эффект</span>
                              <AutoTextarea
                                value={r.effect}
                                onChange={(e) => editSpell(i, { effect: e.target.value })}
                                placeholder="Что делает заклинание — коротко."
                                disabled={editDisabled}
                              />
                            </label>
                            <div className="spell-edit-flags">
                              <label className="sheet-check">
                                <input
                                  type="checkbox"
                                  checked={r.concentration}
                                  onChange={(e) =>
                                    editSpell(i, { concentration: e.target.checked })
                                  }
                                  disabled={editDisabled}
                                />
                                <span>Концентрация</span>
                              </label>
                              <label className="sheet-check">
                                <input
                                  type="checkbox"
                                  checked={r.ritual}
                                  onChange={(e) => editSpell(i, { ritual: e.target.checked })}
                                  disabled={editDisabled}
                                />
                                <span>Ритуал</span>
                              </label>
                            </div>
                          </div>
                        )}
                      </div>
                    );
                  })}
                  <button
                    type="button"
                    className="sheet-add-btn"
                    onClick={addSpell}
                    disabled={editDisabled}
                  >
                    + добавить заклинание
                  </button>
                </div>
              </div>
            </div>

            {/* Слоты заклинаний — flat level→текущие/макс maps. */}
            <div className="world-bible">
              <div className="world-bible-fields">
                <p className="world-bible-hint">Слоты заклинаний</p>
                <div className="sheet-rows">
                  {slotRows.map((r, i) => (
                    <div className="slot-row" key={r.level}>
                      <span className="slot-level">{r.level} круг</span>
                      <input
                        type="number"
                        className="slot-num"
                        value={r.cur}
                        onChange={(e) => editSlot(i, { cur: e.target.value })}
                        placeholder="тек."
                        disabled={editDisabled}
                      />
                      <span className="slot-sep">/</span>
                      <input
                        type="number"
                        className="slot-num"
                        value={r.max}
                        onChange={(e) => editSlot(i, { max: e.target.value })}
                        placeholder="макс"
                        disabled={editDisabled}
                      />
                      <button
                        type="button"
                        className="sheet-row-del"
                        onClick={() => removeSlot(i)}
                        disabled={editDisabled}
                        aria-label="Удалить круг"
                      >
                        ✕
                      </button>
                    </div>
                  ))}
                  {missingSlotLevels.length > 0 && (
                    <div className="slot-add">
                      {missingSlotLevels.map((lvl) => (
                        <button
                          key={lvl}
                          type="button"
                          className="sheet-add-btn"
                          onClick={() => addSlot(lvl)}
                          disabled={editDisabled}
                        >
                          + круг {lvl}
                        </button>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            </div>

            {/* gm_notes — GM-only scratch, not shown to the player in combat. */}
            <div className="world-bible">
              <div className="world-bible-fields">
                <label className="world-field">
                  <span>Тайна героя (видит только ГМ — публичный образ остаётся легендой)</span>
                  <AutoTextarea
                    value={textValue(sheet.gm_notes)}
                    onChange={(e) => updateField("gm_notes", e.target.value)}
                    placeholder="Служебные заметки для ведущего."
                    disabled={editDisabled}
                  />
                </label>
              </div>
            </div>
          </div>

          <div className="world-inspector-foot">
            <div className="sheet-save-row">
              <button
                type="button"
                className="btn primary sheet-save-btn"
                onClick={saveSheet}
                disabled={editDisabled || !dirty}
              >
                {saveBusy ? "Сохранение…" : "Сохранить лист"}
              </button>
              {saveError ? (
                <span className="sheet-save-status is-error">{saveError}</span>
              ) : dirty ? (
                <span className="sheet-save-status">Есть несохранённые правки</span>
              ) : (
                <span className="sheet-save-status is-ok">Всё сохранено</span>
              )}
            </div>
            <div className="world-inspector-launch">
              <button
                type="button"
                className="btn"
                onClick={() => setSheetOpen(true)}
                disabled={!ready}
              >
                Лист персонажа
              </button>
              {currentCharacterId && (
                <button
                  type="button"
                  className="btn primary"
                  onClick={() => onPlayCharacter?.(currentCharacterId)}
                  disabled={locked || !ready}
                >
                  ▶ Играть им
                </button>
              )}
            </div>
            <p className="world-manager-note">
              Правьте лист вручную и жмите «Сохранить лист» — либо продолжайте диалог с
              архитектором: черновик уезжает с каждым сообщением. Для запуска нужно имя.
            </p>
          </div>
        </section>
      </div>

      <ArchitectDebugModal
        debug={debugOpen ? architectDebug : null}
        onClose={() => setDebugOpen(false)}
      />
      {sheetOpen && (
        <WorldDetailModal
          kind="character"
          playerCharacter={sheet}
          onClose={() => setSheetOpen(false)}
        />
      )}
    </div>
  );
}
