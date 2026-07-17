import Icon from "./Icon.jsx";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api.js";
import { ZoomableImage } from "./ImagePreview.jsx";
import WorldDetailModal from "./WorldDetailModal.jsx";
import useConnectorModelBinding from "../useConnectorModelBinding.js";
import { bindingReady } from "../connectorCatalog.js";
import {
  EMPTY_ARCHITECT_USAGE,
  textValue,
  normalizeVisibleMessage,
  AutoTextarea,
  lastUserMessageText,
  useLiveSegments,
  useLocalizedFallbackMessage,
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
  PronounsSelect,
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
function defaultArchitectMessages(t, {
  worldId = "",
  storyId = "",
  worldTitle = "",
  storyTitle = "",
} = {}) {
  // Prefer whichever base is actually AVAILABLE (a dangling story must not
  // hide a live world); only when every recorded base is gone say so.
  const closing =
    storyId && storyTitle
      ? t("character.architect.closing.story", { story: storyTitle })
      : worldId && worldTitle
        ? t("character.architect.closing.world", { world: worldTitle })
        : worldId || storyId
          ? t("character.architect.closing.missing")
          : t("character.architect.closing.standalone");
  return [
    {
      role: "assistant",
      content: t("character.architect.intro", { closing }),
      uiFallback: true,
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
  onArchitectAttach,
  onPlayCharacter,
  onCharacterPersisted,
  notify,
  className = "",
}) {
  const { t } = useTranslation("studio");
  // The full sheet (the `.gmchar` payload's player_character). Seeded from the
  // catalog row's `payload` (the /characters list carries it); the conversation
  // comes from the architect fetch below. Model history + cache ids are SERVER-
  // side (the dialogs SQLite) — the panel holds only the visible chat.
  const greeting = defaultArchitectMessages(t, { worldId, storyId, worldTitle, storyTitle });
  const [sheet, setSheet] = useState(() => characterSheetFromSaved(character));
  const [messages, setMessages] = useState(() => characterMessagesFromChat(null, greeting));
  useLocalizedFallbackMessage(setMessages, greeting[0].content);
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
  // Current-value mirror of `architectBusy` for async attach flows whose
  // closures predate the latest render.
  const architectBusyRef = useRef(false);
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
          throw new Error(data?.error || t("character.errors.loadChat"));
        }
        const restored = characterMessagesFromChat(data.architect, greeting);
        setMessages(restored);
        resetModelBinding(data.architect?.model_binding);
        // The server keeps generating after a closed tab; if a turn is still
        // running for this character, re-attach to its live feed.
        void maybeAttachArchitect(id, restored);
      })
      .catch((error) => {
        if (cancelled || loadedCharacterIdRef.current !== id) return;
        setBindingLoading(false);
        setBindingLoadFailed(true);
        setArchitectError(error?.message || t("character.errors.loadChat"));
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
  const runArchitectTurn = async (text, appendUser, { attach = false, baseMessages = null } = {}) => {
    const source = baseMessages || messages;
    const visibleMessages = appendUser
      ? [...source, { role: "user", content: text }]
      : [...source];
    setArchitectError("");
    setArchitectBusy(true);
    architectBusyRef.current = true;
    clearLive();
    setMessages(visibleMessages);
    let adopted = false;
    let failure = "";
    let attachResult;
    lockConnector();
    try {
      // The server owns the conversation (model history + cache ids live in the
      // dialogs SQLite). The body carries only the message, the target id, and
      // the FULL sheet — the server snapshot-replaces the package with it before
      // the turn, so hand-edited fields are never lost.
      // An attach sends nothing: it replays the live feed.
      const transport = attach
        ? (handler) => onArchitectAttach?.(handler)
        : (handler) =>
            onArchitectStream?.(
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
              handler
            );
      attachResult = await transport(
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
            failure = textValue(ev.data) || t("architect.errors.noResponse");
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
      const message = error?.message || t("architect.errors.callFailed");
      setArchitectError(message);
      // A re-attached turn never saw the original send; seed the retry with
      // the last persisted user message instead of the (empty) attach text.
      setRetryText(attach ? lastUserMessageText(visibleMessages) : text);
      if (!adopted) {
        setMessages((current) => [
          ...current,
          ...liveSegmentsRef.current,
          { role: "assistant", content: t("character.errors.updateFailed", { message }) },
        ]);
        clearLive();
      }
    } finally {
      setArchitectBusy(false);
      architectBusyRef.current = false;
    }
    return attachResult;
  };

  // Reopened panel: if the server still runs an architect turn for this
  // character, join its feed; a false attach (the turn ended between the
  // active check and the GET) refetches the now-complete conversation.
  const maybeAttachArchitect = async (id, restoredMessages) => {
    if (!id || architectBusyRef.current || typeof onArchitectAttach !== "function") return;
    let active = null;
    try {
      active = await api.architectActive("character", id);
    } catch {
      return; // discovery is best-effort; the stored chat is already shown
    }
    if (loadedCharacterIdRef.current !== id || architectBusyRef.current) return;
    if (active?.active !== true) return;
    const attached = await runArchitectTurn("", false, {
      attach: true,
      baseMessages: restoredMessages,
    });
    if (attached === false && loadedCharacterIdRef.current === id) {
      try {
        const data = await api.characterArchitect(id);
        if (data?.ok && loadedCharacterIdRef.current === id) {
          setMessages(characterMessagesFromChat(data.architect, greeting));
        }
      } catch {
        // keep the restored view; the user can reload the panel
      }
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
        if (!data?.ok) throw new Error(data?.error || t("character.errors.saveSheet"));
        character = data.character;
      } else {
        const title = textValue(payload.name) || t("character.defaultTitle");
        const data = await api.createCharacter({
          title,
          payload: { player_character: payload },
          // The picked base rides with the create so the package pins its
          // world_ref/story_ref even on the no-chat manual path.
          ...(worldId ? { world_id: worldId } : {}),
          ...(storyId ? { story_id: storyId } : {}),
        });
        if (!data?.ok) throw new Error(data?.error || t("character.errors.createCharacter"));
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
      const message = error?.message || t("character.errors.saveSheet");
      setSaveError(message);
      notify?.(message);
    } finally {
      setSaveBusy(false);
    }
  };

  const heroName = textValue(sheet.name);
  const portraitUrl = textValue(sheet.portrait_url);
  const hpObj = asObject(sheet.hp) || {};
  const baseLabels = [];
  if (worldId) {
    baseLabels.push(
      worldMissing
        ? t("character.base.worldMissing")
        : t("character.base.world", { world: worldTitle })
    );
  }
  if (storyId) {
    baseLabels.push(
      storyMissing
        ? t("character.base.storyMissing")
        : t("character.base.story", { story: storyTitle })
    );
  }
  const hasWorldCanon = Boolean(worldId && !worldMissing);
  const hasStoryCanon = Boolean(storyId && !storyMissing);
  const canonScope =
    hasWorldCanon && hasStoryCanon
      ? t("character.base.scope.both")
      : hasStoryCanon
        ? t("character.base.scope.story")
        : hasWorldCanon
          ? t("character.base.scope.world")
          : "";
  const subtitle =
    worldId || storyId
      ? canonScope
        ? t("character.base.summaryAvailable", {
            bases: baseLabels.join(" · "),
            scope: canonScope,
          })
        : t("character.base.summaryMissing", { bases: baseLabels.join(" · ") })
      : t("character.subtitle");

  return (
    <div className={`world-studio character-studio${className ? ` ${className}` : ""}`}>
      <header className="world-studio-head">
        <div className="world-studio-id">
          <span className="world-studio-emblem" aria-hidden="true"><Icon name="user" size={18} /></span>
          <div className="world-studio-title">
            <span className="world-studio-kicker">{t("character.kicker")}</span>
            <b>{t("character.title")}</b>
            <p className="world-studio-sub">{subtitle}</p>
          </div>
        </div>
        <span className={`world-studio-chip${ready ? " ready" : ""}`}>
          {ready ? t("character.readiness.ready") : t("character.readiness.notReady")}
        </span>
      </header>

      <div className="world-studio-body">
        <ArchitectChatPane
          headKicker={t("architect.kicker")}
          headTitle={t("character.architect.title")}
          usageTitle={t("character.architect.usageTitle")}
          helpTitle={t("character.architect.helpTitle")}
          helpSubtitle={t("architect.helpSubtitle")}
          helpNote={t("character.architect.helpNote")}
          thinkLabel={t("architect.thinking")}
          placeholder={t("character.architect.placeholder")}
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
          aria-label={t("character.inspector.ariaLabel")}
        >
          <div className="world-inspector-head">
            <span className="world-inspector-kicker">{t("character.inspector.kicker")}</span>
            <b>{heroName || t("character.unnamed")}</b>
          </div>

          <div className="world-inspector-body">
            {portraitUrl && (
              <ZoomableImage
                className="character-architect-portrait"
                src={portraitUrl}
                alt={heroName || t("character.unnamed")}
                title={heroName || t("character.unnamed")}
              />
            )}
            <div className="world-field-grid">
              <label className="world-field">
                <span>{t("character.fields.name.label")}</span>
                <input
                  value={scalarText(sheet.name)}
                  onChange={(event) => updateField("name", event.target.value)}
                  placeholder={t("character.fields.name.placeholder")}
                  disabled={editDisabled}
                />
              </label>
              <label className="world-field">
                <span>{t("character.fields.classRole.label")}</span>
                <input
                  value={scalarText(sheet.class_role)}
                  onChange={(event) => updateField("class_role", event.target.value)}
                  placeholder={t("character.fields.classRole.placeholder")}
                  disabled={editDisabled}
                />
              </label>
            </div>

            <div className="world-field-grid">
              <label className="world-field">
                <span>{t("character.fields.pronouns.label")}</span>
                <PronounsSelect
                  value={sheet.pronouns}
                  onChange={(value) => updateField("pronouns", value)}
                  disabled={editDisabled}
                />
              </label>
              <label className="world-field">
                <span>{t("character.fields.level.label")}</span>
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
              <span>{t("character.fields.age.label")}</span>
              <input
                value={scalarText(sheet.age)}
                onChange={(event) => updateField("age", event.target.value)}
                placeholder={t("character.fields.age.placeholder")}
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>{t("character.fields.appearance.label")}</span>
              <AutoTextarea
                value={scalarText(sheet.physical_type)}
                onChange={(event) => updateField("physical_type", event.target.value)}
                placeholder={t("character.fields.appearance.placeholder")}
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>{t("character.fields.currentAppearance.label")}</span>
              <AutoTextarea
                value={scalarText(sheet.current_appearance)}
                onChange={(event) => updateField("current_appearance", event.target.value)}
                placeholder={t("character.fields.currentAppearance.placeholder")}
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>{t("character.fields.features.label")}</span>
              <AutoTextarea
                value={scalarText(sheet.distinctive_features)}
                onChange={(event) => updateField("distinctive_features", event.target.value)}
                placeholder={t("character.fields.features.placeholder")}
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>{t("character.fields.background.label")}</span>
              <AutoTextarea
                value={scalarText(sheet.background)}
                onChange={(event) => updateField("background", event.target.value)}
                placeholder={t("character.fields.background.placeholder")}
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>{t("character.fields.personality.label")}</span>
              <AutoTextarea
                value={scalarText(sheet.personality)}
                onChange={(event) => updateField("personality", event.target.value)}
                placeholder={t("character.fields.personality.placeholder")}
                disabled={editDisabled}
              />
            </label>

            <label className="world-field">
              <span>{t("character.fields.values.label")}</span>
              <AutoTextarea
                value={scalarText(sheet.values)}
                onChange={(event) => updateField("values", event.target.value)}
                placeholder={t("character.fields.values.placeholder")}
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
                <p className="world-bible-hint">{t("character.combat.title")}</p>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>{t("character.combat.armorClass")}</span>
                    <input
                      type="number"
                      value={numText(sheet.ac)}
                      onChange={(e) => updateNumberField("ac", e.target.value)}
                      placeholder="—"
                      disabled={editDisabled}
                    />
                  </label>
                  <label className="world-field">
                    <span>{t("character.combat.passivePerception")}</span>
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
                    <span>{t("character.combat.hpCurrent")}</span>
                    <input
                      type="number"
                      value={numText(hpObj.current)}
                      onChange={(e) => updateHp("current", e.target.value)}
                      placeholder="—"
                      disabled={editDisabled}
                    />
                  </label>
                  <label className="world-field">
                    <span>{t("character.combat.hpMax")}</span>
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
                    <span>{t("character.combat.speed.label")}</span>
                    <input
                      value={scalarText(sheet.speed)}
                      onChange={(e) => updateField("speed", e.target.value)}
                      placeholder={t("character.combat.speed.placeholder")}
                      disabled={editDisabled}
                    />
                  </label>
                  <label className="world-field">
                    <span>{t("character.combat.senses.label")}</span>
                    {/* Чувства часто длиннее строки («острое зрение, острый слух,
                        идеальный нюх…») — растущая textarea вместо input. */}
                    <AutoTextarea
                      value={scalarText(sheet.senses)}
                      onChange={(e) => updateField("senses", e.target.value)}
                      placeholder={t("character.combat.senses.placeholder")}
                      disabled={editDisabled}
                    />
                  </label>
                </div>
                <label className="world-field">
                  <span>{t("character.combat.languages.label")}</span>
                  <input
                    value={scalarText(sheet.languages)}
                    onChange={(e) => updateField("languages", e.target.value)}
                    placeholder={t("character.combat.languages.placeholder")}
                    disabled={editDisabled}
                  />
                </label>
              </div>
            </div>

            <MapRowsEditor
              label={t("character.editors.skills.label")}
              rows={skillRows.rows}
              onEdit={skillRows.edit}
              onAdd={skillRows.add}
              onRemove={skillRows.remove}
              keyPlaceholder={t("character.editors.skills.placeholder")}
              disabled={editDisabled}
            />
            <MapRowsEditor
              label={t("character.editors.savingThrows.label")}
              rows={saveRows.rows}
              onEdit={saveRows.edit}
              onAdd={saveRows.add}
              onRemove={saveRows.remove}
              keyPlaceholder={t("character.editors.savingThrows.placeholder")}
              disabled={editDisabled}
            />
            <NamedListEditor
              label={t("character.editors.inventory")}
              rows={invRows.rows}
              onEdit={invRows.edit}
              onAdd={invRows.add}
              onRemove={invRows.remove}
              disabled={editDisabled}
            />
            <NamedListEditor
              label={t("character.editors.equipment")}
              rows={equipRows.rows}
              onEdit={equipRows.edit}
              onAdd={equipRows.add}
              onRemove={equipRows.remove}
              disabled={editDisabled}
            />
            <NamedListEditor
              label={t("character.editors.features")}
              rows={featRows.rows}
              onEdit={featRows.edit}
              onAdd={featRows.add}
              onRemove={featRows.remove}
              descPlaceholder={t("character.editors.featureDescription")}
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
                  <span>{t("character.gmNotes.label")}</span>
                  <AutoTextarea
                    value={scalarText(sheet.gm_notes)}
                    onChange={(e) => updateField("gm_notes", e.target.value)}
                    placeholder={t("character.gmNotes.placeholder")}
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
                {saveBusy ? t("character.save.saving") : t("character.save.action")}
              </button>
              {saveError ? (
                <span className="sheet-save-status is-error">{saveError}</span>
              ) : dirty ? (
                <span className="sheet-save-status">{t("character.save.dirty")}</span>
              ) : (
                <span className="sheet-save-status is-ok">{t("character.save.saved")}</span>
              )}
            </div>
            <div className="world-inspector-launch">
              <button
                type="button"
                className="btn"
                onClick={() => setSheetOpen(true)}
                disabled={!ready}
              >
                {t("character.actions.sheet")}
              </button>
              {currentCharacterId && (
                <button
                  type="button"
                  className="btn primary"
                  onClick={() => onPlayCharacter?.(currentCharacterId)}
                  disabled={locked || !ready}
                >
                  {t("character.actions.play")}
                </button>
              )}
            </div>
            <p className="world-manager-note">
              {t("character.saveNote")}
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
