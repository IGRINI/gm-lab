import Icon from "./Icon.jsx";
import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api.js";
import WorldDetailModal from "./WorldDetailModal.jsx";
import useConnectorModelBinding from "../useConnectorModelBinding.js";
import { bindingReady } from "../connectorCatalog.js";
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
import {
  asObject,
  scalarText,
  numText,
  mapFromRows,
  normalizeEntryString,
  spellsFromRows,
  slotsFromRows,
  useMapRows,
  useStringRows,
  useSpellRows,
  useSlotRows,
  AbilitiesEditor,
  MapRowsEditor,
  NamedListEditor,
  SpellsEditor,
  SlotsEditor,
} from "./sheetEditors.jsx";

// The character architect panel (UI_REDESIGN_TZ §Студия персонажа). It is the
// third sibling of WorldArchitectPanel / StoryArchitectPanel and shares their
// chat/SSE machinery (architectShared.jsx). It authors a portable character
// sheet (the `.gmchar` package's `player_character` object): the draft is the
// flat sheet the backend's draft_player_character / edit_player_character tools
// mutate (name, pronouns, class_role, level, background, abilities, skills, hp,
// inventory, spells, …). A hero MAY be based on a world and/or story (the
// optional worldId/storyId props, picked in BasePickerModal): the ids ride with
// the CREATE only — the backend pins them into the package (world_ref/story_ref)
// and feeds the architect the base's public canon. Without a base the hero is
// standalone/orthogonal, as before.
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

// The panel's greeting (UI-only — never part of the model history). The closing
// line reflects the base: a standalone hero is pitched as portable, a based one
// as grounded in its world/story — the old «запустить в любой истории» would
// read as a lie right under an «Основа: …» header. Keyed on the base IDS (not
// titles): a dangling base still IS a base — its material is just unavailable,
// and the greeting says so instead of pretending the hero is standalone.
function defaultArchitectMessages({
  worldId = "",
  storyId = "",
  worldTitle = "",
  storyTitle = "",
} = {}) {
  // Prefer whichever base is actually AVAILABLE (a dangling story must not
  // hide a live world); only when every recorded base is gone say so.
  const closing =
    storyId && storyTitle
      ? `Я соберу героя под историю «${storyTitle}» с опорой на её публичную завязку — при этом лист останется переносимым.`
      : worldId && worldTitle
        ? `Я соберу героя под мир «${worldTitle}» с опорой на его публичный канон — при этом лист останется переносимым.`
        : worldId || storyId
          ? "Основа героя записана в пакете, но её материал сейчас недоступен — я сохраню существующие связи листа."
          : "Я собираю один переносимый лист персонажа — его можно будет запустить в любой истории.";
  return [
    {
      role: "assistant",
      content:
        "Опиши персонажа, которого хочешь собрать, — или дай направление, а лист я соберу сам.\n\nЧто особенно полезно:\n\n1. Имя, роль или класс, уровень.\n2. Характер, ценности, происхождение и внешность.\n3. Характеристики (сила, ловкость и т.д.) и ключевые навыки.\n4. ХП, класс доспеха, снаряжение и инвентарь.\n5. Заклинания и слоты, если персонаж их использует.\n\n" +
        closing,
    },
  ];
}

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

function asArray(value) {
  return Array.isArray(value) ? value : [];
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
// `fallback` is the base-aware greeting for an empty conversation.
function characterMessagesFromChat(architect, fallback) {
  const messages = asArray(architect?.messages).map(normalizeVisibleMessage).filter(Boolean);
  return messages.length > 0 ? messages : fallback;
}

// The sheet POSTed as `draft` (and, verbatim, as the direct-save body). The
// backend REPLACES the whole player_character, so send the FULL sheet — only
// truly-empty values (blank strings, empty lists / objects) are dropped as noise;
// numbers, booleans and unknown keys pass through. Строки инвентаря/снаряжения
// нормализуются по §И1 «имя — описание» (см. sheetEditors normalizeEntryString).
function cleanCharacterDraft(sheet) {
  const out = {};
  for (const [key, value] of Object.entries(asObject(sheet) || {})) {
    if (value == null) continue;
    if (typeof value === "string") {
      const trimmed = value.trim();
      if (trimmed) out[key] = trimmed;
    } else if (Array.isArray(value)) {
      // Инвентарь/снаряжение/особенности — строки «имя — описание»; заклинания
      // (объекты) проходят как есть.
      const list = value
        .map((item) => (typeof item === "string" ? normalizeEntryString(item) : item))
        .filter((item) => item != null && item !== "");
      if (list.length > 0) out[key] = list;
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
  // The OPTIONAL base the hero is built on. For a fresh draft these come from
  // the BasePickerModal or the wizard's «Создать персонажа» ctx (App state) and
  // ride with the create (first architect turn / manual save) so the backend
  // pins world_ref/story_ref and feeds the architect the base's public context;
  // for an existing character they mirror the refs already stored in the
  // package (display-only — the binding is fixed at creation). A missing flag
  // marks a ref whose package is gone from the library (dangling by design):
  // the title is then empty and the header/greeting say so honestly.
  worldId = "",
  storyId = "",
  worldTitle = "",
  storyTitle = "",
  worldMissing = false,
  storyMissing = false,
  locked,
  connectors = [],
  models = [],
  connectorModelsLoadingIds = [],
  onEnsureConnectorModels,
  initialModelBinding = null,
  connectorAuthBusyIds = [],
  connectorAuthCancellingIds = [],
  connectorAuthPrompts = {},
  onConnectorAuthStart,
  onConnectorAuthCancel,
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
  const greeting = defaultArchitectMessages({ worldId, storyId, worldTitle, storyTitle });
  const [sheet, setSheet] = useState(() => characterSheetFromSaved(character));
  const [messages, setMessages] = useState(() => characterMessagesFromChat(null, greeting));
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
  const {
    modelBinding,
    setModelBinding,
    connectorLocked,
    bindingLoading,
    setBindingLoading,
    bindingLoadFailed,
    setBindingLoadFailed,
    lockConnector,
    resetModelBinding,
  } = useConnectorModelBinding(initialModelBinding, connectors, models);
  const bindingContextPending =
    (textValue(character?.id) || null) !== loadedCharacterIdRef.current;

  // --- editable-row buffers for the list/map/spell/slot editors (sheetEditors
  // hooks). Scalars, abilities and hp edit the sheet directly (fixed keys, no
  // rename/add/remove), so they need no buffer. Each commit rebuilds its sheet
  // field from the NEXT row buffer; an empty result drops the key entirely. ---
  const commitMapField = (field) => (rows) => {
    setSheet((current) => {
      const obj = mapFromRows(rows);
      const next = { ...current };
      if (Object.keys(obj).length > 0) next[field] = obj;
      else delete next[field];
      return next;
    });
  };
  // RAW strings ride the sheet while typing — «имя — описание» normalization
  // lives in cleanCharacterDraft at the payload boundary.
  const commitListField = (field) => (rows) => {
    setSheet((current) => {
      const list = rows.map((r) => r.text).filter((t) => t.trim() !== "");
      const next = { ...current };
      if (list.length > 0) next[field] = list;
      else delete next[field];
      return next;
    });
  };
  const commitSpellRows = (rows) => {
    setSheet((current) => {
      const list = spellsFromRows(rows);
      const next = { ...current };
      if (list.length > 0) next.spells = list;
      else delete next.spells;
      return next;
    });
  };
  const commitSlotRows = (rows) => {
    setSheet((current) => {
      const { slots, max } = slotsFromRows(rows);
      const next = { ...current };
      if (Object.keys(slots).length > 0) next.spell_slots = slots;
      else delete next.spell_slots;
      if (Object.keys(max).length > 0) next.spell_slots_max = max;
      else delete next.spell_slots_max;
      return next;
    });
  };
  const skillRows = useMapRows(sheet.skills, commitMapField("skills"));
  const saveRows = useMapRows(sheet.saving_throws, commitMapField("saving_throws"));
  const invRows = useStringRows(sheet.inventory, commitListField("inventory"));
  const equipRows = useStringRows(sheet.equipment, commitListField("equipment"));
  const featRows = useStringRows(sheet.features, commitListField("features"));
  const spellRows = useSpellRows(sheet.spells, commitSpellRows);
  const slotRows = useSlotRows(sheet.spell_slots, sheet.spell_slots_max, commitSlotRows);

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
  const architectLocked =
    locked || architectBusy || bindingContextPending || bindingLoading || bindingLoadFailed
    || !bindingReady(modelBinding, connectors, models);
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
    skillRows.reseed(s.skills);
    saveRows.reseed(s.saving_throws);
    invRows.reseed(s.inventory);
    equipRows.reseed(s.equipment);
    featRows.reseed(s.features);
    spellRows.reseed(s.spells);
    slotRows.reseed(s.spell_slots, s.spell_slots_max);
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
    setMessages(characterMessagesFromChat(null, greeting));
    setCurrentCharacterId(id || "");
    clearLive();
    setInput("");
    setArchitectError("");
    setRetryText("");
    setSaveError("");
    setArchitectUsage(EMPTY_ARCHITECT_USAGE);
    setArchitectDebug(null);
    setDebugOpen(false);
    resetModelBinding(null);
    if (!id) return undefined;
    setBindingLoading(true);
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
        setMessages(characterMessagesFromChat(data.architect, greeting));
        resetModelBinding(data.architect?.model_binding);
      })
      .catch((error) => {
        if (cancelled || loadedCharacterIdRef.current !== id) return;
        setBindingLoading(false);
        setBindingLoadFailed(true);
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
    lockConnector();
    try {
      // The server owns the conversation (model history + cache ids live in the
      // dialogs SQLite). The body carries only the message, the target id, and
      // the FULL sheet — the server snapshot-replaces the package with it before
      // the turn, so hand-edited fields are never lost.
      await onArchitectStream?.(
        {
          message: text,
          draft: draftPayload,
          connector_id: modelBinding.connector_id,
          model_id: modelBinding.model_id,
          // A create sends no id; an edit carries the resolved character_id.
          // The base world/story ids ride ONLY with the create (the server pins
          // them into the new package; for an existing character it reads the
          // stored refs and ignores request ids).
          ...(currentCharacterId
            ? { character_id: currentCharacterId }
            : {
                ...(worldId ? { world_id: worldId } : {}),
                ...(storyId ? { story_id: storyId } : {}),
              }),
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
            if (ev.model_binding) resetModelBinding(ev.model_binding);
            // The server creates the package (and saves the user message into
            // its conversation) BEFORE the model call, and its error events
            // carry the persisted character_id as a SIBLING of `data`. Pin it,
            // or «Повторить» would re-post without an id and mint a DUPLICATE
            // package with the same base refs, stranding this one.
            const errId = textValue(ev.character_id);
            if (errId && !currentCharacterId) {
              setCurrentCharacterId(errId);
              loadedCharacterIdRef.current = errId;
            }
          } else if (ev.kind === "architect_done") {
            adopted = true;
            const data = ev.data || {};
            if (data.model_binding) resetModelBinding(data.model_binding);
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
          // The picked base rides with the create so the package pins its
          // world_ref/story_ref even on the no-chat manual path.
          ...(worldId ? { world_id: worldId } : {}),
          ...(storyId ? { story_id: storyId } : {}),
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
  const hpObj = asObject(sheet.hp) || {};

  return (
    <div className={`world-studio character-studio${className ? ` ${className}` : ""}`}>
      <header className="world-studio-head">
        <div className="world-studio-id">
          <span className="world-studio-emblem" aria-hidden="true"><Icon name="user" size={18} /></span>
          <div className="world-studio-title">
            <span className="world-studio-kicker">создание персонажа</span>
            <b>Студия персонажей</b>
            <p className="world-studio-sub">
              {worldId || storyId ? (
                <>
                  Основа:{" "}
                  {worldId && (
                    <>
                      {worldMissing ? "мир (пакет недоступен)" : `мир «${worldTitle}»`}
                      {storyId ? " · " : ""}
                    </>
                  )}
                  {storyId &&
                    (storyMissing ? "история (пакет недоступен)" : `история «${storyTitle}»`)}
                  {(worldId && !worldMissing) || (storyId && !storyMissing)
                    ? // Pronoun agrees with what is actually available: их (оба),
                      // её (история), его (мир).
                      ` — архитектор опирается на ${
                        worldId && !worldMissing && storyId && !storyMissing
                          ? "их"
                          : storyId && !storyMissing
                            ? "её"
                            : "его"
                      } публичный канон.`
                    : " — материал основы недоступен: архитектор сохранит записанные связи листа."}
                </>
              ) : (
                <>
                  Соберите переносимый лист героя с архитектором или отредактируйте каждое поле
                  вручную и сохраните лист напрямую.
                </>
              )}
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
          helpNote="Он собирает один переносимый лист персонажа: имя и роль, характеристики, навыки, ХП, инвентарь, снаряжение и заклинания. Без основы герой универсален; с основой архитектор опирается на её публичный канон. Лист в любом случае переносим — героя можно запустить и в другой истории (при несовпадении мира придёт предупреждение)."
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
          connectors={connectors}
          models={models}
          connectorModelsLoadingIds={connectorModelsLoadingIds}
          onEnsureConnectorModels={onEnsureConnectorModels}
          modelBinding={modelBinding}
          onModelBindingChange={setModelBinding}
          connectorLocked={connectorLocked}
          modelPickerDisabled={
            locked || architectBusy || bindingContextPending || bindingLoading || bindingLoadFailed
          }
          connectorAuthBusyIds={connectorAuthBusyIds}
          connectorAuthCancellingIds={connectorAuthCancellingIds}
          connectorAuthPrompts={connectorAuthPrompts}
          onConnectorAuthStart={onConnectorAuthStart}
          onConnectorAuthCancel={onConnectorAuthCancel}
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
                  value={scalarText(sheet.name)}
                  onChange={(event) => updateField("name", event.target.value)}
                  placeholder="Например: Кара Вент"
                  disabled={editDisabled}
                />
              </label>
              <label className="world-field">
                <span>Роль / класс</span>
                <input
                  value={scalarText(sheet.class_role)}
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
                  value={scalarText(sheet.pronouns)}
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
                value={scalarText(sheet.age)}
                onChange={(event) => updateField("age", event.target.value)}
                placeholder="Например: 27"
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Внешность</span>
              <AutoTextarea
                value={scalarText(sheet.physical_type)}
                onChange={(event) => updateField("physical_type", event.target.value)}
                placeholder="Телосложение, черты, как выглядит."
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Особые приметы</span>
              <AutoTextarea
                value={scalarText(sheet.distinctive_features)}
                onChange={(event) => updateField("distinctive_features", event.target.value)}
                placeholder="Шрамы, татуировки, что запоминается."
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Происхождение</span>
              <AutoTextarea
                value={scalarText(sheet.background)}
                onChange={(event) => updateField("background", event.target.value)}
                placeholder="Откуда герой, что его сформировало."
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Характер</span>
              <AutoTextarea
                value={scalarText(sheet.personality)}
                onChange={(event) => updateField("personality", event.target.value)}
                placeholder="Как ведёт себя, что движет героем."
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>Ценности</span>
              <AutoTextarea
                value={scalarText(sheet.values)}
                onChange={(event) => updateField("values", event.target.value)}
                placeholder="Во что верит, чем не поступится."
                disabled={editDisabled}
              />
            </label>

            {/* Характеристики — the six core abilities as number inputs. */}
            <AbilitiesEditor
              abilities={sheet.abilities}
              onChange={updateAbility}
              disabled={editDisabled}
            />

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

            <MapRowsEditor
              label="Навыки"
              rows={skillRows.rows}
              onEdit={skillRows.edit}
              onAdd={skillRows.add}
              onRemove={skillRows.remove}
              keyPlaceholder="Навык"
              disabled={editDisabled}
            />
            <MapRowsEditor
              label="Спасброски"
              rows={saveRows.rows}
              onEdit={saveRows.edit}
              onAdd={saveRows.add}
              onRemove={saveRows.remove}
              keyPlaceholder="Спасбросок"
              disabled={editDisabled}
            />
            <NamedListEditor
              label="Инвентарь"
              rows={invRows.rows}
              onEdit={invRows.edit}
              onAdd={invRows.add}
              onRemove={invRows.remove}
              disabled={editDisabled}
            />
            <NamedListEditor
              label="Снаряжение"
              rows={equipRows.rows}
              onEdit={equipRows.edit}
              onAdd={equipRows.add}
              onRemove={equipRows.remove}
              disabled={editDisabled}
            />
            <NamedListEditor
              label="Особенности"
              rows={featRows.rows}
              onEdit={featRows.edit}
              onAdd={featRows.add}
              onRemove={featRows.remove}
              descPlaceholder="Что даёт (необязательно)"
              disabled={editDisabled}
            />

            {/* Заклинания — 5-field cards, collapsed to «{name} · {level} круг». */}
            <SpellsEditor
              rows={spellRows.rows}
              openSet={spellRows.open}
              onToggle={spellRows.toggle}
              onEdit={spellRows.edit}
              onAdd={spellRows.add}
              onRemove={spellRows.remove}
              disabled={editDisabled}
            />

            {/* Слоты заклинаний — flat level→текущие/макс maps. */}
            <SlotsEditor
              rows={slotRows.rows}
              missing={slotRows.missing}
              onEdit={slotRows.edit}
              onAddLevel={slotRows.addLevel}
              onRemove={slotRows.remove}
              disabled={editDisabled}
            />

            {/* gm_notes — GM-only scratch, not shown to the player in combat. */}
            <div className="world-bible">
              <div className="world-bible-fields">
                <label className="world-field">
                  <span>Тайна героя (видит только ГМ — публичный образ остаётся легендой)</span>
                  <AutoTextarea
                    value={scalarText(sheet.gm_notes)}
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
