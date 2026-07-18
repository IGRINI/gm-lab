import Icon from "./Icon.jsx";
import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api.js";
import Spoiler from "./Spoiler.jsx";
import useConnectorModelBinding from "../useConnectorModelBinding.js";
import { bindingReady } from "../connectorCatalog.js";
import { DEFAULT_LANGUAGE } from "../i18n/catalog.js";
import { createServerMessageError, localizeServerMessage } from "../serverMessages.js";
import {
  EMPTY_ARCHITECT_USAGE,
  textValue,
  rawText,
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
import { PronounsSelect } from "./sheetEditors.jsx";

// The story architect panel (docs/CHARACTERS_AND_STORY_TZ.md §С1.3). It is the
// story-level sibling of WorldArchitectPanel and shares its chat/SSE machinery
// (architectShared.jsx). It authors a reusable PLOT on top of an EXISTING world:
// the draft is the plot object the backend's draft_story_plot / edit_story_plot
// tools mutate (title, story_brief, public_intro, hidden_truth, player_character,
// scene, npcs[], public_facts[], state_records[], proper_nouns[], time). The
// bound world is fixed (`worldId`); an existing story is edited by `storyId`.

const DEFAULT_STORY_DRAFT = {
  title: "",
  description: "",
  story_brief: "",
  public_intro: "",
  hidden_truth: "",
  player_character: null,
  scene: null,
  npcs: [],
  public_facts: [],
  state_records: [],
  proper_nouns: [],
  time: null,
};

function defaultArchitectMessages(intro) {
  return [{ role: "assistant", content: intro, uiFallback: true }];
}

// The list sections rendered as read-only JSON summaries in v1 (complex nested
// object cards the architect authors; the panel shows a compact preview and the
// full JSON in a spoiler — editing them is a future track, per §С1.3).
const OBJECT_LIST_SECTIONS = [
  ["npcs", "npcs", (e) => textValue(e?.name) || textValue(e?.id)],
  ["public_facts", "publicFacts", (e) => textValue(e?.text) || textValue(e?.id)],
  ["state_records", "stateRecords", (e) => textValue(e?.text) || textValue(e?.id)],
];

// Scene sub-lists rendered as newline-editable text. `items` and `exits` are
// OBJECT lists (the runtime/seed contract); each line renders/parses a
// convention — items: «имя — детали», exits: «имя -> куда». Other object
// fields (portable, visible, owner, id...) are preserved by name on parse; a
// NEW line defaults to a takeable visible item / visible exit.
const SCENE_LIST_FIELDS = [
  ["present_npcs", "presentNpcs"],
  ["exits", "exits"],
  ["items", "items"],
  ["constraints", "constraints"],
];

function asObject(value) {
  return value && typeof value === "object" && !Array.isArray(value) ? value : null;
}

function asArray(value) {
  return Array.isArray(value) ? value : [];
}

// Build the panel draft from a saved story envelope. The plot lives in the
// story's `seed` (the backend folds draft_story_plot into `seed`), so restore the
// form from `seed` with `title`/`description` overlaid from the envelope.
function storyDraftFromSaved(story) {
  const seed = asObject(story?.seed) || {};
  return {
    title: textValue(story?.title) || textValue(seed.title),
    description: textValue(story?.description) || textValue(seed.description),
    story_brief: textValue(seed.story_brief),
    public_intro: textValue(seed.public_intro),
    hidden_truth: textValue(seed.hidden_truth),
    player_character: asObject(seed.player_character),
    scene: asObject(seed.scene),
    npcs: asArray(seed.npcs),
    public_facts: asArray(seed.public_facts),
    state_records: asArray(seed.state_records),
    proper_nouns: asArray(seed.proper_nouns).map(textValue).filter(Boolean),
    time: typeof seed.time === "number" ? seed.time : null,
  };
}

// Restore the visible conversation from the server's architect block
// (`GET /stories/{id}/draft` → `{architect: {messages}}`). The chat lives in the
// package's architect.json now — never inside the story row.
function architectMessagesFromChat(architect, intro) {
  const messages = asArray(architect?.messages).map(normalizeVisibleMessage).filter(Boolean);
  return messages.length > 0 ? messages : defaultArchitectMessages(intro);
}

// The plot object POSTed as `draft` to the story architect (snake_case, matching
// the runtime contract + the tool schema). Empty scalars/lists are dropped so a
// blank field never clobbers an existing value on the shallow server-side merge.
// Trim string values of a nested object at the payload boundary (the form binds
// RAW values so typing spaces works — см. rawText); non-strings pass through.
function cleanScalarStrings(obj) {
  if (!obj) return obj;
  const out = {};
  for (const [key, value] of Object.entries(obj)) {
    if (typeof value === "string") {
      const trimmed = value.trim();
      if (trimmed) out[key] = trimmed;
    } else if (value != null) {
      out[key] = value;
    }
  }
  return out;
}

function cleanStoryDraft(draft) {
  const plot = {};
  for (const key of ["title", "description", "story_brief", "public_intro", "hidden_truth"]) {
    const v = textValue(draft[key]);
    if (v) plot[key] = v;
  }
  const pc = cleanScalarStrings(asObject(draft.player_character));
  if (pc && Object.keys(pc).length > 0) plot.player_character = pc;
  const scene = cleanScalarStrings(asObject(draft.scene));
  if (scene && Object.keys(scene).length > 0) plot.scene = scene;
  for (const key of ["npcs", "public_facts", "state_records"]) {
    const arr = asArray(draft[key]);
    if (arr.length > 0) plot[key] = arr;
  }
  const nouns = asArray(draft.proper_nouns).map(textValue).filter(Boolean);
  if (nouns.length > 0) plot.proper_nouns = nouns;
  if (typeof draft.time === "number" && Number.isFinite(draft.time)) plot.time = draft.time;
  return plot;
}

// Merge a draft_story_plot tool call's args (or the final draft) into the panel
// state. Top-level scalars/lists overwrite; `scene` and `player_character` merge
// key-by-key — mirrors the backend merge_plot so the live view matches the store.
function mergeStoryDraft(current, args) {
  const patch = asObject(args);
  if (!patch) return current;
  const next = { ...current };
  for (const [key, value] of Object.entries(patch)) {
    if ((key === "scene" || key === "player_character") && asObject(value)) {
      const base = asObject(next[key]) || {};
      next[key] = { ...base, ...value };
    } else if (key === "proper_nouns") {
      next[key] = asArray(value).map(textValue).filter(Boolean);
    } else if (["npcs", "public_facts", "state_records"].includes(key)) {
      next[key] = asArray(value);
    } else if (key === "time") {
      next[key] = typeof value === "number" ? value : next.time;
    } else if (typeof value === "string") {
      next[key] = value;
    } else if (value != null) {
      next[key] = value;
    }
  }
  return next;
}

// A plot is "launchable-ready" once it has the runtime minimum
// (story_brief + public_intro); title comes from the draft/message fallback.
function plotReady(draft) {
  return !!textValue(draft.story_brief) && !!textValue(draft.public_intro);
}

// Render a list section as newline-separated text for the editable textareas.
function listText(arr) {
  return asArray(arr).map(textValue).filter(Boolean).join("\n");
}

// §И1 item name↔details separator (space, em dash, space) — the same convention
// the runtime uses for inventory entries.
const ITEM_DESC_SEP = " — ";

// One editable line for a scene items/exits entry: objects render through the
// line conventions, legacy string entries render as-is.
function sceneEntryLine(field, entry) {
  if (entry && typeof entry === "object" && !Array.isArray(entry)) {
    const name = textValue(entry.name) || textValue(entry.id);
    if (!name) return "";
    if (field === "items") {
      const details = textValue(entry.details);
      return details ? `${name}${ITEM_DESC_SEP}${details}` : name;
    }
    if (field === "exits") {
      const destination = textValue(entry.destination);
      return destination ? `${name} -> ${destination}` : name;
    }
    return name;
  }
  return textValue(entry);
}

function sceneListText(field, arr) {
  return asArray(arr)
    .map((entry) => sceneEntryLine(field, entry))
    .filter(Boolean)
    .join("\n");
}

// Parse one edited line back into an object entry, preserving the fields the
// line convention does not carry (portable/visible/owner/id...) by matching the
// prior entry with the same name. A brand-new item line defaults to a takeable
// visible object; a new exit line to a visible exit.
function sceneEntryFromLine(field, line, priorByName) {
  if (field === "items") {
    const sep = line.indexOf(ITEM_DESC_SEP);
    const name = (sep >= 0 ? line.slice(0, sep) : line).trim();
    const details = sep >= 0 ? line.slice(sep + ITEM_DESC_SEP.length).trim() : "";
    const prior = asObject(priorByName.get(name)) || {};
    const entry = {
      ...prior,
      name,
      portable: typeof prior.portable === "boolean" ? prior.portable : true,
      visible: prior.visible !== false,
    };
    if (details) entry.details = details;
    else delete entry.details;
    return entry;
  }
  if (field === "exits") {
    const idx = line.lastIndexOf("->");
    const name = (idx >= 0 ? line.slice(0, idx) : line).trim() || line.trim();
    const destination = idx >= 0 ? line.slice(idx + 2).trim() : "";
    const prior = asObject(priorByName.get(name)) || {};
    const entry = { ...prior, name };
    if (destination) entry.destination = destination;
    else if (!textValue(prior.destination)) entry.destination = "";
    return entry;
  }
  return line;
}

export default function StoryArchitectPanel({
  story,
  worldId,
  worldTitle,
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
  onPlayStory,
  onSaveProtagonist,
  responseLanguage = DEFAULT_LANGUAGE,
  className = "",
}) {
  const { t } = useTranslation("studio");
  const architectLanguage = String(responseLanguage || "").trim() || DEFAULT_LANGUAGE;
  const architectIntro = t("story.architect.intro", { lng: architectLanguage });
  // Seed the form from the catalog row's scalars only (title/description); the
  // GM-only seed comes from the draft fetch below (the `story` prop is the
  // minimal catalog row, §С1.3). The model history and prompt-cache ids are
  // SERVER-side now (the package's architect.json) — the panel holds only the
  // visible conversation.
  const [storyDraft, setStoryDraft] = useState(() => storyDraftFromSaved(story));
  const [messages, setMessages] = useState(() => architectMessagesFromChat(null, architectIntro));
  useLocalizedFallbackMessage(setMessages, architectIntro);
  const [input, setInput] = useState("");
  const [architectBusy, setArchitectBusy] = useState(false);
  const [architectError, setArchitectError] = useState("");
  // The last message whose turn FAILED — powers the «Повторить» button.
  const [retryText, setRetryText] = useState("");
  const [architectUsage, setArchitectUsage] = useState(EMPTY_ARCHITECT_USAGE);
  const [architectDebug, setArchitectDebug] = useState(null);
  const [debugOpen, setDebugOpen] = useState(false);
  const [architectElapsed, setArchitectElapsed] = useState(0);
  const { liveSegments, liveSegmentsRef, appendLiveDelta, pushLiveTool, clearLive } =
    useLiveSegments();
  // The story id captured from the last architect_done (a create returns the new
  // id); until then a new story sends no story_id and relies on `worldId`.
  const [currentStoryId, setCurrentStoryId] = useState(() => textValue(story?.id) || "");
  // Start as `null` (not the mount id) so the load effect ALWAYS runs on mount —
  // for an existing story that means fetching its GM draft row on open, not just
  // when the id later changes.
  const loadedStoryIdRef = useRef(null);
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
  const bindingContextPending = (textValue(story?.id) || null) !== loadedStoryIdRef.current;

  const draftPayload = useMemo(() => cleanStoryDraft(storyDraft), [storyDraft]);
  const ready = plotReady(storyDraft);
  const architectLocked =
    locked || architectBusy || bindingContextPending || bindingLoading || bindingLoadFailed
    || !bindingReady(modelBinding, connectors, models);

  // Reload the form + conversation only when the user opens a DIFFERENT story
  // (or switches from a fresh draft to a saved one). The story our own turn just
  // created/updated is already ours — reloading would wipe the live chat.
  //
  // The `story` prop is the MINIMAL player-facing catalog row (id/title/
  // description/kind/world_ref) — it deliberately carries NO seed (hidden_truth
  // is GM-only, §С1.3). For an EXISTING story we fetch the GM-scoped draft via
  // `api.storyDraft(id)`: `{story}` restores the form, `{architect.messages}`
  // the conversation; a fresh draft (no id) resets to the empty defaults.
  useEffect(() => {
    const id = textValue(story?.id) || null;
    if (id === loadedStoryIdRef.current) return undefined;
    loadedStoryIdRef.current = id;
    // Reset synchronously to the catalog row's scalars (title/description) so the
    // form never flashes a stale story while the draft fetch is in flight.
    setStoryDraft(storyDraftFromSaved(story));
    setMessages(architectMessagesFromChat(null, architectIntro));
    setCurrentStoryId(id || "");
    clearLive();
    setInput("");
    setArchitectError("");
    setRetryText("");
    setArchitectUsage(EMPTY_ARCHITECT_USAGE);
    setArchitectDebug(null);
    setDebugOpen(false);
    resetModelBinding(null);
    if (!id) return undefined;
    setBindingLoading(true);
    // Fetch the GM draft row for an existing story. `cancelled` guards a stale
    // response when the user reopens a different story before this resolves.
    let cancelled = false;
    api
      .storyDraft(id)
      .then((data) => {
        if (cancelled || loadedStoryIdRef.current !== id) return;
        if (!data?.ok || !data.story) {
          throw createServerMessageError(data);
        }
        setStoryDraft(storyDraftFromSaved(data.story));
        const restored = architectMessagesFromChat(data.architect, architectIntro);
        setMessages(restored);
        resetModelBinding(data.architect?.model_binding);
        // The server keeps generating after a closed tab; if a turn is still
        // running for this story, re-attach to its live feed.
        void maybeAttachArchitect(id, restored);
      })
      .catch((error) => {
        if (cancelled || loadedStoryIdRef.current !== id) return;
        setBindingLoading(false);
        setBindingLoadFailed(true);
        setArchitectError(localizeServerMessage(error, t, { fallbackCode: "architect_load_failed" }));
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [story?.id]);

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

  const updateDraft = (field, value) => {
    setStoryDraft((current) => ({ ...current, [field]: value }));
  };
  const updatePc = (field, value) => {
    setStoryDraft((current) => {
      const pc = asObject(current.player_character) ? { ...current.player_character } : {};
      pc[field] = value;
      return { ...current, player_character: pc };
    });
  };
  const updateScene = (field, value) => {
    setStoryDraft((current) => {
      const scene = asObject(current.scene) ? { ...current.scene } : {};
      scene[field] = value;
      return { ...current, scene };
    });
  };
  const updateSceneList = (field, text) => {
    const lines = text.split("\n").map((s) => s.trim()).filter(Boolean);
    if (field !== "items" && field !== "exits") {
      updateScene(field, lines);
      return;
    }
    // Object list sections: parse each line through the convention, keeping
    // unrendered fields of the entry with the same name.
    setStoryDraft((current) => {
      const scene = asObject(current.scene) ? { ...current.scene } : {};
      const priorByName = new Map();
      for (const entry of asArray(scene[field])) {
        const key = textValue(entry?.name) || textValue(entry);
        if (key && !priorByName.has(key)) priorByName.set(key, entry);
      }
      scene[field] = lines.map((line) => sceneEntryFromLine(field, line, priorByName));
      return { ...current, scene };
    });
  };
  const updateProperNouns = (text) =>
    updateDraft("proper_nouns", text.split("\n").map((s) => s.trim()).filter(Boolean));
  const updateTime = (text) => {
    const n = parseInt(text, 10);
    updateDraft("time", Number.isFinite(n) ? n : null);
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
    let failure = null;
    let attachResult;
    lockConnector();
    try {
      // The server owns the conversation (model history + cache ids live in the
      // package's architect.json). The body carries only the message, the target
      // ids, and the form's CONTENT draft — the server applies it as a normal
      // story update before the turn, so hand-edited fields are never lost.
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
                // A create relies on world_id; an edit carries the resolved story_id.
                ...(currentStoryId ? { story_id: currentStoryId } : {}),
                ...(worldId ? { world_id: worldId } : {}),
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
            // draft_story_plot args merge live like the world panel's bible; the
            // targeted edit_story_plot patch is folded from the authoritative
            // `draft` in the done payload (its set/add/remove ops are non-trivial
            // to replay client-side, so we adopt the server's result instead).
            if (name === "draft_story_plot") {
              setStoryDraft((current) => mergeStoryDraft(current, args));
            }
          } else if (ev.kind === "architect_error") {
            failure = ev;
            if (ev.model_binding) resetModelBinding(ev.model_binding);
            // The story is created BEFORE the model call; error events carry
            // the persisted story_id as a sibling of `data`. Pin it so a retry
            // edits that story instead of minting a duplicate package.
            const errId = textValue(ev.story_id);
            if (errId && !currentStoryId) {
              setCurrentStoryId(errId);
              loadedStoryIdRef.current = errId;
            }
          } else if (ev.kind === "architect_done") {
            adopted = true;
            const data = ev.data || {};
            if (data.model_binding) resetModelBinding(data.model_binding);
            const usage = asObject(data.usage);
            if (usage) setArchitectUsage((current) => accumulateUsage(current, usage));
            setArchitectDebug(debugFromDone(data, usage));
            // Adopt the persisted story as the source of truth: the server folded
            // the plot into `seed` (draft_story_plot merge OR edit_story_plot
            // set/add/remove), so restore the form from it rather than replay ops.
            const savedStory = asObject(data.story);
            if (savedStory) {
              setStoryDraft(storyDraftFromSaved(savedStory));
            } else if (asObject(data.draft)) {
              setStoryDraft((current) => mergeStoryDraft(current, data.draft));
            }
            // The conversation: fold this turn's live segments into the visible
            // chat — the same shape the server just persisted to architect.json.
            setMessages([...visibleMessages, ...liveSegmentsRef.current]);
            // The story we just created/updated is ours — pin its id so a parent
            // stories-list refresh (which may re-key the `story` prop) does not
            // wipe this live conversation, and route the next turn as an edit.
            const persistedId = textValue(data.story_id) || textValue(savedStory?.id);
            if (persistedId) {
              setCurrentStoryId(persistedId);
              loadedStoryIdRef.current = persistedId;
            }
            clearLive();
          }
        }
      );
      if (failure !== null) throw createServerMessageError(failure);
      setRetryText("");
    } catch (error) {
      const message = localizeServerMessage(error, t, { fallbackCode: "architect_turn_failed" });
      setArchitectError(message);
      // A re-attached turn never saw the original send; seed the retry with
      // the last persisted user message instead of the (empty) attach text.
      setRetryText(attach ? lastUserMessageText(visibleMessages) : text);
      if (!adopted) {
        setMessages((current) => [
          ...current,
          ...liveSegmentsRef.current,
          { role: "assistant", content: t("story.errors.updateFailed", { message }) },
        ]);
        clearLive();
      }
    } finally {
      setArchitectBusy(false);
      architectBusyRef.current = false;
    }
    return attachResult;
  };

  // Reopened panel: if the server still runs an architect turn for this story,
  // join its feed; a false attach (the turn ended between the active check and
  // the GET) refetches the now-complete conversation instead.
  const maybeAttachArchitect = async (id, restoredMessages) => {
    if (!id || architectBusyRef.current || typeof onArchitectAttach !== "function") return;
    let active = null;
    try {
      active = await api.architectActive("story", id);
    } catch {
      return; // discovery is best-effort; the stored chat is already shown
    }
    if (loadedStoryIdRef.current !== id || architectBusyRef.current) return;
    if (active?.active !== true) return;
    const attached = await runArchitectTurn("", false, {
      attach: true,
      baseMessages: restoredMessages,
    });
    if (attached === false && loadedStoryIdRef.current === id) {
      try {
        const data = await api.storyDraft(id);
        if (data?.ok && data.story && loadedStoryIdRef.current === id) {
          setStoryDraft(storyDraftFromSaved(data.story));
          setMessages(architectMessagesFromChat(data.architect, architectIntro));
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

  const pc = asObject(storyDraft.player_character) || {};
  const scene = asObject(storyDraft.scene) || {};

  return (
    <div className={`world-studio${className ? ` ${className}` : ""}`}>
      <header className="world-studio-head">
        <div className="world-studio-id">
          <span className="world-studio-emblem" aria-hidden="true"><Icon name="book" size={18} /></span>
          <div className="world-studio-title">
            <span className="world-studio-kicker">{t("story.kicker")}</span>
            <b>{t("story.title")}</b>
            <p className="world-studio-sub">
              {worldTitle
                ? t("story.subtitleWithWorld", { world: worldTitle })
                : t("story.subtitle")}
            </p>
          </div>
        </div>
        <span className={`world-studio-chip${ready ? " ready" : ""}`}>
          {ready ? t("story.readiness.ready") : t("story.readiness.notReady")}
        </span>
      </header>

      <div className="world-studio-body">
        <ArchitectChatPane
          headKicker={t("architect.kicker")}
          headTitle={t("story.architect.title")}
          usageTitle={t("story.architect.usageTitle")}
          helpTitle={t("story.architect.helpTitle")}
          helpSubtitle={t("architect.helpSubtitle")}
          helpNote={t("story.architect.helpNote")}
          thinkLabel={t("architect.thinking")}
          placeholder={t("story.architect.placeholder")}
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
          className={`world-studio-pane world-inspector${ready ? " is-live" : ""}`}
          aria-label={t("story.inspector.ariaLabel")}
        >
          <div className="world-inspector-head">
            <span className="world-inspector-kicker">{t("story.inspector.kicker")}</span>
            <b>{textValue(storyDraft.title) || t("common.untitled")}</b>
          </div>

          <div className="world-inspector-body">
            <label className="world-field">
              <span>{t("story.fields.title.label")}</span>
              <input
                value={storyDraft.title}
                onChange={(event) => updateDraft("title", event.target.value)}
                placeholder={t("story.fields.title.placeholder")}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>{t("story.fields.description.label")}</span>
              <input
                value={storyDraft.description}
                onChange={(event) => updateDraft("description", event.target.value)}
                placeholder={t("story.fields.description.placeholder")}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>{t("story.fields.storyBrief.label")}</span>
              <AutoTextarea
                value={storyDraft.story_brief}
                onChange={(event) => updateDraft("story_brief", event.target.value)}
                placeholder={t("story.fields.storyBrief.placeholder")}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>{t("story.fields.publicIntro.label")}</span>
              <AutoTextarea
                value={storyDraft.public_intro}
                onChange={(event) => updateDraft("public_intro", event.target.value)}
                placeholder={t("story.fields.publicIntro.placeholder")}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>{t("story.fields.hiddenTruth.label")}</span>
              <AutoTextarea
                value={storyDraft.hidden_truth}
                onChange={(event) => updateDraft("hidden_truth", event.target.value)}
                placeholder={t("story.fields.hiddenTruth.placeholder")}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>{t("story.fields.time.label")}</span>
              <input
                type="number"
                value={storyDraft.time == null ? "" : storyDraft.time}
                onChange={(event) => updateTime(event.target.value)}
                placeholder="480"
                disabled={locked}
              />
            </label>

            <div className="world-bible">
              <div className="world-bible-fields">
                <p className="world-bible-hint">{t("story.protagonist.hint")}</p>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>{t("story.protagonist.name.label")}</span>
                    <input
                      value={rawText(pc.name)}
                      onChange={(event) => updatePc("name", event.target.value)}
                      placeholder={t("story.protagonist.name.placeholder")}
                      disabled={locked}
                    />
                  </label>
                  <label className="world-field">
                    <span>{t("story.protagonist.classRole.label")}</span>
                    <input
                      value={rawText(pc.class_role)}
                      onChange={(event) => updatePc("class_role", event.target.value)}
                      placeholder={t("story.protagonist.classRole.placeholder")}
                      disabled={locked}
                    />
                  </label>
                </div>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>{t("story.protagonist.pronouns.label")}</span>
                    <PronounsSelect
                      value={pc.pronouns}
                      onChange={(value) => updatePc("pronouns", value)}
                      disabled={locked}
                    />
                  </label>
                  <label className="world-field">
                    <span>{t("story.protagonist.background.label")}</span>
                    <input
                      value={rawText(pc.background)}
                      onChange={(event) => updatePc("background", event.target.value)}
                      placeholder={t("story.protagonist.background.placeholder")}
                      disabled={locked}
                    />
                  </label>
                </div>
              </div>
            </div>

            <div className="world-bible">
              <div className="world-bible-fields">
                <p className="world-bible-hint">{t("story.scene.hint")}</p>
                <label className="world-field">
                  <span>{t("story.scene.title.label")}</span>
                  <input
                    value={rawText(scene.title)}
                    onChange={(event) => updateScene("title", event.target.value)}
                    placeholder={t("story.scene.title.placeholder")}
                    disabled={locked}
                  />
                </label>
                <label className="world-field">
                  <span>{t("story.scene.description.label")}</span>
                  <AutoTextarea
                    value={rawText(scene.description)}
                    onChange={(event) => updateScene("description", event.target.value)}
                    placeholder={t("story.scene.description.placeholder")}
                    disabled={locked}
                  />
                </label>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>{t("story.scene.locationId")}</span>
                    <input
                      value={rawText(scene.location_id)}
                      onChange={(event) => updateScene("location_id", event.target.value)}
                      placeholder="salt_port_gate"
                      disabled={locked}
                    />
                  </label>
                  <label className="world-field">
                    <span>{t("story.scene.tension.label")}</span>
                    <input
                      value={rawText(scene.tension)}
                      onChange={(event) => updateScene("tension", event.target.value)}
                      placeholder={t("story.scene.tension.placeholder")}
                      disabled={locked}
                    />
                  </label>
                </div>
                {SCENE_LIST_FIELDS.map(([field, labelKey]) => (
                  <label key={field} className="world-field">
                    <span>{t(`story.scene.lists.${labelKey}`)}</span>
                    <AutoTextarea
                      value={sceneListText(field, scene[field])}
                      onChange={(event) => updateSceneList(field, event.target.value)}
                      placeholder={t("story.scene.listPlaceholder")}
                      disabled={locked}
                    />
                  </label>
                ))}
              </div>
            </div>

            <label className="world-field">
              <span>{t("story.properNouns.label")}</span>
              <AutoTextarea
                value={listText(storyDraft.proper_nouns)}
                onChange={(event) => updateProperNouns(event.target.value)}
                placeholder={t("story.properNouns.placeholder")}
                disabled={locked}
              />
            </label>

            {OBJECT_LIST_SECTIONS.map(([field, labelKey, summarize]) => {
              const entries = asArray(storyDraft[field]);
              if (entries.length === 0) return null;
              return (
                <div key={field} className="world-bible">
                  <div className="world-bible-fields">
                    <Spoiler label={`${t(`story.objectSections.${labelKey}`)} · ${entries.length}`}>
                      <ul className="story-plot-list">
                        {entries.map((entry, index) => (
                          <li key={index}>{summarize(entry) || `#${index + 1}`}</li>
                        ))}
                      </ul>
                      <pre className="arch-debug-json">{JSON.stringify(entries, null, 2)}</pre>
                    </Spoiler>
                  </div>
                </div>
              );
            })}
          </div>

          <div className="world-inspector-foot">
            {currentStoryId && (
              <div className="world-inspector-launch">
                {onSaveProtagonist && (
                  <button
                    type="button"
                    className="btn"
                    onClick={() => onSaveProtagonist(currentStoryId)}
                    disabled={locked || !ready}
                    title={t("story.actions.saveProtagonistTitle")}
                  >
                    {t("story.actions.saveProtagonist")}
                  </button>
                )}
                <button
                  type="button"
                  className="btn primary"
                  onClick={() => onPlayStory?.(currentStoryId)}
                  disabled={locked || !ready}
                >
                  {t("story.actions.play")}
                </button>
              </div>
            )}
            <p className="world-manager-note">
              {t("story.saveNote")}
            </p>
          </div>
        </section>
      </div>

      <ArchitectDebugModal
        debug={debugOpen ? architectDebug : null}
        onClose={() => setDebugOpen(false)}
      />
    </div>
  );
}
