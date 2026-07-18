import Icon from "./Icon.jsx";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api.js";
import ImageThumbnail from "./ImagePreview.jsx";
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
import { DEFAULT_LANGUAGE } from "../i18n/catalog.js";
import { availableLanguages } from "../i18n/localeCatalog.js";
import { createServerMessageError, localizeServerMessage } from "../serverMessages.js";
import { localizedWorldPresetValues, WORLD_PRESETS } from "./worldPresets.js";

const DEFAULT_WORLD_DRAFT = {
  title: "",
  genre: "fantasy",
  tone: "tense",
  worldSize: "",
  population: "",
  publicPremise: "",
  worldLore: null,
};

const LORE_PREVIEW_FIELDS = [
  ["dogmas", "dogmas"],
  ["world_laws", "worldLaws"],
  ["inhabitants", "inhabitants"],
  ["creatures", "creatures"],
  ["power_sources", "powerSources"],
  ["technologies", "technologies"],
  ["taboos", "taboos"],
  ["conflicts", "conflicts"],
  ["inspirations", "inspirations"],
  ["regions", "regions"],
  ["power_centers", "powerCenters"],
  ["religions", "religions"],
  ["gods", "gods"],
  ["cultures", "cultures"],
  ["history", "history"],
  ["economy", "economy"],
  ["daily_life", "dailyLife"],
  ["story_hooks", "storyHooks"],
  ["hidden_secrets", "hiddenSecrets"],
  ["location_rules", "locationRules"],
  ["prohibited_elements", "prohibitedElements"],
];

const VISUAL_PROMPT_FIELDS = [
  [
    "world_image_prompt_en",
    "worldImage",
    "world_image_url",
  ],
  [
    "world_map_prompt_en",
    "worldMap",
    "world_map_url",
  ],
];
const VISUAL_OUTPUT_FIELDS = VISUAL_PROMPT_FIELDS.map(([, , outputField]) => [outputField]);

function cleanWorldDraft(draft) {
  return {
    title: textValue(draft.title),
    genre: textValue(draft.genre),
    tone: textValue(draft.tone),
    worldSize: textValue(draft.worldSize),
    population: textValue(draft.population),
    publicPremise: textValue(draft.publicPremise),
    worldLore: draft.worldLore && typeof draft.worldLore === "object" ? draft.worldLore : null,
  };
}

function worldDraftFromSaved(world, defaults, defaultTitles) {
  if (!world || typeof world !== "object") return { ...defaults };
  const isDraft = textValue(world.status) === "draft";
  const savedTitle = textValue(world.title);
  const title = isDraft && defaultTitles.has(savedTitle) ? "" : savedTitle;
  return {
    title: title || textValue(world.world_lore?.name),
    genre: textValue(world.genre) || defaults.genre,
    tone: textValue(world.tone) || defaults.tone,
    worldSize: textValue(world.world_size),
    population: textValue(world.population),
    publicPremise: textValue(world.public_premise) || textValue(world.world_lore?.public_premise),
    worldLore: world.world_lore && typeof world.world_lore === "object" ? world.world_lore : null,
  };
}

// Restore the visible conversation from the server's architect block
// (`GET /worlds/{id}/architect` → `{architect: {messages}}`). The chat lives in
// the package's architect.json now — never inside the world row.
function architectMessagesFromChat(architect, intro) {
  const raw = Array.isArray(architect?.messages) ? architect.messages : [];
  const messages = raw.map(normalizeVisibleMessage).filter(Boolean);
  return messages.length > 0
    ? messages
    : [{ role: "assistant", content: intro, uiFallback: true }];
}

function mergeArchitectDraft(current, draft) {
  if (!draft || typeof draft !== "object") return current;
  const lore = draft.world_lore && typeof draft.world_lore === "object" ? draft.world_lore : null;
  return {
    ...current,
    title: textValue(draft.title) || current.title,
    genre: textValue(draft.genre) || current.genre,
    tone: textValue(draft.tone) || current.tone,
    worldSize: textValue(draft.world_size) || current.worldSize,
    population: textValue(draft.population) || current.population,
    publicPremise: textValue(draft.public_premise) || current.publicPremise,
    worldLore: lore ? normalizeWorldLore(lore, draft) : current.worldLore,
  };
}

function normalizeWorldLore(lore, draft) {
  const next = { ...lore };
  if (!textValue(next.name)) next.name = textValue(draft.title);
  if (!textValue(next.genre)) next.genre = textValue(draft.genre);
  if (!textValue(next.tone)) next.tone = textValue(draft.tone);
  if (!textValue(next.world_size)) next.world_size = textValue(draft.world_size);
  if (!textValue(next.population)) next.population = textValue(draft.population);
  if (!textValue(next.public_premise)) next.public_premise = textValue(draft.public_premise);
  return next;
}

function loreArray(value) {
  return Array.isArray(value) ? value.map(textValue).filter(Boolean) : [];
}

function applyPresetValues(current, preset) {
  return {
    ...current,
    ...preset.values,
  };
}

// A world is "creatable" once it has any real lore — a public/hidden premise or
// at least one filled list field. Used both for the gate and the readiness chip.
function loreHasContent(lore) {
  if (!lore || typeof lore !== "object") return false;
  if (textValue(lore.public_premise) || textValue(lore.hidden_premise)) return true;
  return LORE_PREVIEW_FIELDS.some(([field]) => loreArray(lore[field]).length > 0);
}

// Render a list lore field as newline-separated text for the manual textareas.
function loreFieldText(lore, field) {
  if (Array.isArray(lore?.[field])) return lore[field].join("\n");
  if (typeof lore?.[field] === "string") return lore[field];
  return "";
}

// Build the final world_lore object on submit: clean list fields (trim + drop
// empties), keep a non-empty hidden premise, and backfill name/genre/tone/
// world-size/public premise from the top-level draft so manual worlds are valid.
function finalizeWorldLore(payload) {
  const source = payload.worldLore && typeof payload.worldLore === "object" ? payload.worldLore : {};
  const lore = { ...source };
  for (const [field] of LORE_PREVIEW_FIELDS) {
    if (field in lore) {
      const items = loreArray(lore[field]);
      if (items.length) lore[field] = items;
      else delete lore[field];
    }
  }
  // Open questions are conversational (the architect asks them in chat), never a
  // stored bible field.
  delete lore.open_questions;
  const hidden = textValue(lore.hidden_premise);
  if (hidden) lore.hidden_premise = hidden;
  else delete lore.hidden_premise;
  for (const [field] of VISUAL_PROMPT_FIELDS) {
    const prompt = textValue(lore[field]);
    if (prompt) lore[field] = prompt;
    else delete lore[field];
  }
  for (const [field] of VISUAL_OUTPUT_FIELDS) {
    const url = textValue(lore[field]);
    if (url) lore[field] = url;
    else delete lore[field];
  }
  if (!textValue(lore.public_premise) && textValue(payload.publicPremise)) {
    lore.public_premise = textValue(payload.publicPremise);
  }
  if (!textValue(lore.name)) lore.name = textValue(payload.title);
  if (!textValue(lore.genre)) lore.genre = textValue(payload.genre);
  if (!textValue(lore.tone)) lore.tone = textValue(payload.tone);
  if (!textValue(lore.world_size)) lore.world_size = textValue(payload.worldSize);
  if (!textValue(lore.population)) lore.population = textValue(payload.population);
  return lore;
}

function visualPromptSnapshot(lore) {
  return VISUAL_PROMPT_FIELDS.map(([promptField, , outputField]) => ({
    promptField,
    outputField,
    prompt: textValue(lore?.[promptField]),
    imageUrl: textValue(lore?.[outputField]),
  }));
}

function visualJobLabel(job, prompt, imageUrl, t) {
  if (job?.loading) return t("world.visual.status.generating");
  if (job?.queued) return t("world.visual.status.queued");
  if (imageUrl) return t("world.visual.status.ready");
  if (prompt) return t("world.visual.status.pending");
  return t("world.visual.status.noPrompt");
}

export default function WorldArchitectPanel({
  world,
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
  onCreateWorld,
  onArchitectStream,
  onArchitectAttach,
  onGenerateImage,
  onPlayWorld,
  onCreateStory,
  responseLanguage = DEFAULT_LANGUAGE,
  className = "",
}) {
  const { t } = useTranslation("studio");
  const presetLanguage = String(responseLanguage || "").trim() || DEFAULT_LANGUAGE;
  const architectIntro = t("world.architect.intro", { lng: presetLanguage });
  const worldDraftDefaults = useMemo(() => ({
    ...DEFAULT_WORLD_DRAFT,
    genre: t("world.defaults.genre", { lng: presetLanguage }),
    tone: t("world.defaults.tone", { lng: presetLanguage }),
  }), [presetLanguage, t]);
  const defaultWorldTitles = useMemo(() => new Set(
    availableLanguages.map(({ code }) => t("world.defaults.title", { lng: code }))
  ), [t]);
  // The model history and prompt-cache ids are SERVER-side (the package's
  // architect.json); the panel holds only the visible conversation.
  const [worldDraft, setWorldDraft] = useState(
    () => worldDraftFromSaved(world, worldDraftDefaults, defaultWorldTitles)
  );
  const previousWorldDraftDefaultsRef = useRef(worldDraftDefaults);
  const [messages, setMessages] = useState(() => architectMessagesFromChat(null, architectIntro));
  useLocalizedFallbackMessage(setMessages, architectIntro);
  const [input, setInput] = useState("");
  const [architectBusy, setArchitectBusy] = useState(false);
  const [architectError, setArchitectError] = useState("");
  // The last message whose turn FAILED — powers the «Повторить» button.
  const [retryText, setRetryText] = useState("");
  const [bibleOpen, setBibleOpen] = useState(false);
  const [architectUsage, setArchitectUsage] = useState(EMPTY_ARCHITECT_USAGE);
  const [architectDebug, setArchitectDebug] = useState(null);
  const [debugOpen, setDebugOpen] = useState(false);
  const [imageJobs, setImageJobs] = useState({});
  const imageAutoRequestsRef = useRef({});
  const imagePromptLatestRef = useRef({});
  const imageQueueRef = useRef([]);
  const imageQueueRunningRef = useRef(false);
  const imageScopeRef = useRef(0);
  const [architectElapsed, setArchitectElapsed] = useState(0);
  // In-flight segments for the current turn (think / reply text / tool), folded
  // from the SSE stream in production order. Mirrors the main chat's live view.
  const { liveSegments, liveSegmentsRef, appendLiveDelta, pushLiveTool, clearLive } =
    useLiveSegments();

  useEffect(() => {
    const previous = previousWorldDraftDefaultsRef.current;
    previousWorldDraftDefaultsRef.current = worldDraftDefaults;
    if (
      previous.genre === worldDraftDefaults.genre
      && previous.tone === worldDraftDefaults.tone
    ) return;
    setWorldDraft((current) => {
      const genre = current.genre === previous.genre ? worldDraftDefaults.genre : current.genre;
      const tone = current.tone === previous.tone ? worldDraftDefaults.tone : current.tone;
      return genre === current.genre && tone === current.tone
        ? current
        : { ...current, genre, tone };
    });
  }, [worldDraftDefaults]);
  // Start as `null` (not the mount id) so the load effect ALWAYS runs on mount —
  // for an existing world that means fetching its architect conversation on open.
  const loadedWorldIdRef = useRef(null);
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
  const bindingContextPending = (world?.id ?? null) !== loadedWorldIdRef.current;
  const worldPayload = useMemo(() => cleanWorldDraft(worldDraft), [worldDraft]);
  // "Filled" for the bible label / auto-open = real DETAIL (hidden premise or any
  // list field), not just a public premise mirrored from the top-level field —
  // otherwise the label reads "заполнена" while every detail field is empty.
  const loreFilled = useMemo(() => {
    const lore = worldPayload.worldLore;
    if (!lore || typeof lore !== "object") return false;
    if (textValue(lore.hidden_premise)) return true;
    return LORE_PREVIEW_FIELDS.some(([field]) => loreArray(lore[field]).length > 0);
  }, [worldPayload.worldLore]);
  // Creatable manually too: the basics plus either a public premise or any lore.
  const loreReady = !!textValue(worldPayload.publicPremise) || loreFilled;
  const worldCreateLocked =
    locked ||
    !worldPayload.title ||
    !worldPayload.genre ||
    !worldPayload.tone ||
    !worldPayload.worldSize ||
    !worldPayload.population ||
    !loreReady;
  const architectLocked =
    locked || architectBusy || bindingContextPending || bindingLoading || bindingLoadFailed
    || !bindingReady(modelBinding, connectors, models);

  useEffect(() => {
    const id = world?.id ?? null;
    // Only reload when the user switches to a DIFFERENT world. The world our own
    // turn just created/updated (App syncs selectedWorldId) is already ours —
    // reloading it would wipe the live conversation.
    if (id === loadedWorldIdRef.current) return undefined;
    loadedWorldIdRef.current = id;
    const nextDraft = worldDraftFromSaved(world, worldDraftDefaults, defaultWorldTitles);
    setWorldDraft(nextDraft);
    setMessages(architectMessagesFromChat(null, architectIntro));
    clearLive();
    setInput("");
    setArchitectError("");
    setRetryText("");
    setArchitectUsage(EMPTY_ARCHITECT_USAGE);
    setArchitectDebug(null);
    setDebugOpen(false);
    resetModelBinding(null);
    setImageJobs({});
    imageScopeRef.current += 1;
    imageAutoRequestsRef.current = {};
    imagePromptLatestRef.current = {};
    imageQueueRef.current = [];
    setBibleOpen(loreHasContent(nextDraft.worldLore));
    if (!id) return undefined;
    setBindingLoading(true);
    // Restore the conversation from the server. A failed fetch is a VISIBLE
    // error (a silently-default intro would look like the chat never existed).
    // `cancelled` guards a stale response when the user switches worlds
    // mid-flight.
    let cancelled = false;
    api
      .worldArchitect(id)
      .then((data) => {
        if (cancelled || loadedWorldIdRef.current !== id) return;
        if (!data?.ok) {
          throw createServerMessageError(data);
        }
        const restored = architectMessagesFromChat(data.architect, architectIntro);
        setMessages(restored);
        resetModelBinding(data.architect?.model_binding);
        // The server keeps generating after a closed tab; if a turn is still
        // running for this world, re-attach to its live feed.
        void maybeAttachArchitect(id, restored);
      })
      .catch((error) => {
        if (cancelled || loadedWorldIdRef.current !== id) return;
        setBindingLoading(false);
        setBindingLoadFailed(true);
        setArchitectError(localizeServerMessage(error, t, { fallbackCode: "architect_load_failed" }));
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [world?.id]);

  // Reveal the bible editor the first time real lore appears (architect draft or
  // manual entry); the user can still collapse it afterwards.
  useEffect(() => {
    if (loreFilled) setBibleOpen(true);
  }, [loreFilled]);

  // Tick an elapsed-seconds counter while the architect works, so a slow model
  // still shows visible progress instead of a frozen-looking screen.
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

  useEffect(() => {
    const latest = {};
    for (const { promptField, prompt } of visualPromptSnapshot(worldDraft.worldLore)) {
      latest[promptField] = prompt;
    }
    imagePromptLatestRef.current = latest;
  }, [worldDraft.worldLore]);

  const updateWorldDraft = (field, value) => {
    setWorldDraft((current) => ({ ...current, [field]: value }));
  };

  const applyPreset = (preset) => {
    const localizedPreset = {
      ...preset,
      values: localizedWorldPresetValues(preset, t, presetLanguage),
    };
    setWorldDraft((current) => applyPresetValues(current, localizedPreset));
  };

  const updateWorldLore = useCallback((field, value) => {
    setWorldDraft((current) => {
      const lore = current.worldLore && typeof current.worldLore === "object" ? { ...current.worldLore } : {};
      lore[field] = value;
      return { ...current, worldLore: lore };
    });
  }, []);
  const updateLoreText = (field, text) => updateWorldLore(field, text);
  const updateLoreList = (field, text) => updateWorldLore(field, text.split("\n"));

  const setImageJob = useCallback((field, patch) => {
    setImageJobs((current) => ({ ...current, [field]: { ...(current[field] || {}), ...patch } }));
  }, []);

  const runVisualGeneration = useCallback(async (promptField, outputField, prompt, scope) => {
    const isCurrentScope = () => scope === imageScopeRef.current;
    const setScopedImageJob = (patch) => {
      if (isCurrentScope()) setImageJob(promptField, patch);
    };
    const releaseAutoRequest = () => {
      if (imageAutoRequestsRef.current[promptField] === prompt) delete imageAutoRequestsRef.current[promptField];
    };
    if (!isCurrentScope() || imagePromptLatestRef.current[promptField] !== prompt) {
      releaseAutoRequest();
      setScopedImageJob({ queued: false, loading: false });
      return;
    }
    setScopedImageJob({ queued: false, loading: true, error: "" });
    try {
      if (typeof onGenerateImage !== "function") {
        throw new Error(t("world.errors.imageUnavailable"));
      }
      const isMap = outputField === "world_map_url";
      const data = await onGenerateImage({
        prompt,
        model: "nvfp4",
        width: isMap ? 1536 : 1024,
        height: 1024,
      });
      if (!data.ok) throw new Error(data.error || t("world.errors.imageNotGenerated"));
      const image = Array.isArray(data.images) ? data.images.find((item) => textValue(item?.url)) : null;
      const url = textValue(image?.url);
      if (!url) throw new Error(t("world.errors.imageMissingUrl"));
      if (!isCurrentScope() || imagePromptLatestRef.current[promptField] !== prompt) {
        releaseAutoRequest();
        setScopedImageJob({ queued: false, loading: false });
        return;
      }
      updateWorldLore(outputField, url);
      setScopedImageJob({ queued: false, loading: false, error: "", seed: data.seed, url });
    } catch (error) {
      setScopedImageJob({
        queued: false,
        loading: false,
        error: error?.message || t("world.errors.imageFailed"),
      });
    }
  }, [onGenerateImage, setImageJob, t, updateWorldLore]);

  const drainVisualQueue = useCallback(async () => {
    if (imageQueueRunningRef.current) return;
    const next = imageQueueRef.current.shift();
    if (!next) return;
    imageQueueRunningRef.current = true;
    try {
      await runVisualGeneration(next.promptField, next.outputField, next.prompt, next.scope);
    } finally {
      imageQueueRunningRef.current = false;
      if (imageQueueRef.current.length > 0) {
        window.setTimeout(() => {
          void drainVisualQueue();
        }, 0);
      }
    }
  }, [runVisualGeneration]);

  const enqueueVisualGeneration = useCallback((job) => {
    const duplicate = imageQueueRef.current.some(
      (queued) => queued.promptField === job.promptField && queued.prompt === job.prompt
    );
    if (duplicate) return;
    imageQueueRef.current = imageQueueRef.current.filter((queued) => queued.promptField !== job.promptField);
    imageQueueRef.current.push({ ...job, scope: imageScopeRef.current });
    setImageJob(job.promptField, { queued: true, loading: false, error: "" });
    void drainVisualQueue();
  }, [drainVisualQueue, setImageJob]);

  useEffect(() => {
    if (locked || architectBusy || typeof onGenerateImage !== "function") return undefined;
    const runnable = visualPromptSnapshot(worldDraft.worldLore).filter(({ promptField, prompt, imageUrl }) => {
      if (!prompt || imageUrl || imageJobs[promptField]?.loading) return false;
      return imageAutoRequestsRef.current[promptField] !== prompt;
    });
    if (!runnable.length) return undefined;

    const timer = window.setTimeout(() => {
      for (const { promptField, outputField, prompt } of runnable) {
        imageAutoRequestsRef.current[promptField] = prompt;
        enqueueVisualGeneration({ promptField, outputField, prompt });
      }
    }, 900);
    return () => window.clearTimeout(timer);
  }, [worldDraft.worldLore, imageJobs, locked, architectBusy, onGenerateImage, enqueueVisualGeneration]);

  const submitWorld = async (event) => {
    event.preventDefault();
    if (worldCreateLocked) return;
    const saved = await onCreateWorld?.({ ...worldPayload, worldLore: finalizeWorldLore(worldPayload) });
    // Adopt the server-rewritten image URLs (/world-assets/<id>/<file>) so the
    // preview points at the package asset instead of the volatile sidecar URL.
    adoptPersistedImageUrls(saved);
  };

  // Overlay the persisted world's image fields onto the live draft. The server
  // copies generated images into the package and returns same-origin
  // /world-assets URLs; keeping the old sidecar URL would 404 once the sidecar
  // run dir clears. Empty fields stay empty (a valid "no image" state).
  const adoptPersistedImageUrls = useCallback((savedWorld) => {
    const lore = savedWorld?.world_lore;
    if (!lore || typeof lore !== "object") return;
    setWorldDraft((current) => {
      const currentLore =
        current.worldLore && typeof current.worldLore === "object" ? current.worldLore : {};
      const nextLore = { ...currentLore };
      let changed = false;
      for (const [outputField] of VISUAL_OUTPUT_FIELDS) {
        const persisted = textValue(lore[outputField]);
        if (persisted && persisted !== textValue(currentLore[outputField])) {
          nextLore[outputField] = persisted;
          changed = true;
        }
      }
      return changed ? { ...current, worldLore: nextLore } : current;
    });
  }, []);

  // One architect turn. `appendUser=false` is the RETRY path: the visible chat
  // already carries the user message (and the failure note) from the failed
  // attempt, so only the request is repeated. `attach` joins a turn the server
  // is already running (after a reload) instead of starting one; the restored
  // conversation rides in via `baseMessages` because the closure's `messages`
  // may predate the restore.
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
      // package's architect.json). The body carries only the message and the
      // form's CONTENT draft — the server applies it as a normal world update
      // before the turn, so hand-edited fields are never lost. App injects the
      // selected world_id. An attach sends nothing: it replays the live feed.
      const transport = attach
        ? (handler) => onArchitectAttach?.(handler)
        : (handler) =>
            onArchitectStream?.(
              {
                message: text,
                draft: worldPayload,
                connector_id: modelBinding.connector_id,
                model_id: modelBinding.model_id,
              },
              handler
            );
      attachResult = await transport(
        (ev) => {
          if (ev.kind === "architect_delta") {
            // Per-hop content/thinking delta. Reasoning streams into a collapsed
            // spoiler; reply text streams into its own bubble — like the main chat.
            const d = ev.data || {};
            const sid = textValue(d.sid) || "arch";
            const role = d.channel === "thinking" ? "think" : "assistant";
            appendLiveDelta(sid, role, String(d.text || ""));
          } else if (ev.kind === "architect_tool") {
            // Surface each tool call inline, in order, and fill the inspector live.
            const call = ev.data || {};
            const name = textValue(call.name);
            if (!name) return;
            const args = call.arguments && typeof call.arguments === "object" ? call.arguments : {};
            const sid = textValue(call.sid) || "arch";
            pushLiveTool(sid, name, args);
            if (name === "draft_world_bible") {
              setWorldDraft((current) => mergeArchitectDraft(current, args));
            }
          } else if (ev.kind === "architect_error") {
            failure = ev;
            if (ev.model_binding) resetModelBinding(ev.model_binding);
          } else if (ev.kind === "architect_done") {
            adopted = true;
            const data = ev.data || {};
            if (data.model_binding) resetModelBinding(data.model_binding);
            const usage = data.usage && typeof data.usage === "object" ? data.usage : null;
            if (usage) setArchitectUsage((current) => accumulateUsage(current, usage));
            setArchitectDebug(debugFromDone(data, usage));
            if (data.draft && typeof data.draft === "object") {
              setWorldDraft((current) => mergeArchitectDraft(current, data.draft));
            }
            // The architect draft carries volatile sidecar image URLs; the
            // persisted world (data.world) carries the package /world-assets
            // URLs the server rewrote to. Adopt those last so the preview is
            // stable across sidecar restarts and image-gen toggles.
            adoptPersistedImageUrls(data.world);
            // The world we just created/updated is ours — keep the `world` prop
            // sync (App.setSelectedWorldId) from wiping this live conversation.
            if (data.world?.id) loadedWorldIdRef.current = data.world.id;
            // Fold this turn's live segments into the visible chat — the same
            // shape the server just persisted to architect.json.
            setMessages([...visibleMessages, ...liveSegmentsRef.current]);
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
        // Keep whatever streamed before the failure, then append the error note.
        setMessages((current) => [
          ...current,
          ...liveSegmentsRef.current,
          { role: "assistant", content: t("world.errors.updateFailed", { message }) },
        ]);
        clearLive();
      }
    } finally {
      setArchitectBusy(false);
      architectBusyRef.current = false;
    }
    return attachResult;
  };

  // Reopened panel: if the server still runs an architect turn for this world,
  // join its feed; a false attach (the turn ended between the active check and
  // the GET) refetches the now-complete conversation instead.
  const maybeAttachArchitect = async (id, restoredMessages) => {
    if (!id || architectBusyRef.current || typeof onArchitectAttach !== "function") return;
    let active = null;
    try {
      active = await api.architectActive("world", id);
    } catch {
      return; // discovery is best-effort; the stored chat is already shown
    }
    if (loadedWorldIdRef.current !== id || architectBusyRef.current) return;
    if (active?.active !== true) return;
    const attached = await runArchitectTurn("", false, {
      attach: true,
      baseMessages: restoredMessages,
    });
    if (attached === false && loadedWorldIdRef.current === id) {
      try {
        const data = await api.worldArchitect(id);
        if (data?.ok && loadedWorldIdRef.current === id) {
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

  // One renderer for both the committed log and the in-flight segments, so the
  // live view and the reloaded history look identical: reasoning → spoiler,
  // tool → detailed card, user/assistant → bubble (with a caret while streaming).
  return (
    <form className={`world-studio${className ? ` ${className}` : ""}`} onSubmit={submitWorld}>
      <header className="world-studio-head">
        <div className="world-studio-id">
          <span className="world-studio-emblem" aria-hidden="true"><Icon name="globe" size={18} /></span>
          <div className="world-studio-title">
            <span className="world-studio-kicker">{t("world.kicker")}</span>
            <b>{t("world.title")}</b>
            <p className="world-studio-sub">
              {t("world.subtitle")}
            </p>
          </div>
        </div>
        <span className={`world-studio-chip${worldCreateLocked ? "" : " ready"}`}>
          {worldCreateLocked ? t("world.readiness.notReady") : t("world.readiness.ready")}
        </span>
      </header>

      <div className="world-studio-body">
        <ArchitectChatPane
          headKicker={t("architect.kicker")}
          headTitle={t("world.architect.title")}
          helpTitle={t("world.architect.helpTitle")}
          helpSubtitle={t("architect.helpSubtitle")}
          helpNote={t("world.architect.helpNote")}
          thinkLabel={t("architect.thinking")}
          placeholder={t("world.architect.placeholder")}
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
          className={`world-studio-pane world-inspector${loreReady ? " is-live" : ""}`}
          aria-label={t("world.inspector.ariaLabel")}
        >
          <div className="world-inspector-head">
            <span className="world-inspector-kicker">{t("world.inspector.kicker")}</span>
            <b>{textValue(worldPayload.worldLore?.name) || worldDraft.title || t("common.untitled")}</b>
          </div>

          <div className="world-inspector-body">
            <div className="world-inspector-section">
              <span className="world-inspector-label">{t("world.presets.title")}</span>
              <div className="world-manager-presets" aria-label={t("world.presets.ariaLabel")}>
                {WORLD_PRESETS.map((preset) => (
                  <button
                    key={preset.id}
                    type="button"
                    className="world-preset"
                    onClick={() => applyPreset(preset)}
                    disabled={locked}
                  >
                    <b>{t(`world.presets.${preset.id}.label`)}</b>
                    <span>{t(`world.presets.${preset.id}.description`)}</span>
                  </button>
                ))}
              </div>
            </div>

            <label className="world-field">
              <span>{t("world.fields.title.label")}</span>
              <input
                value={worldDraft.title}
                onChange={(event) => updateWorldDraft("title", event.target.value)}
                placeholder={t("world.fields.title.placeholder")}
                disabled={locked}
              />
            </label>

            <div className="world-field-grid">
              <label className="world-field">
                <span>{t("world.fields.genre.label")}</span>
                <input
                  value={worldDraft.genre}
                  onChange={(event) => updateWorldDraft("genre", event.target.value)}
                  placeholder={t("world.fields.genre.placeholder")}
                  disabled={locked}
                />
              </label>
              <label className="world-field">
                <span>{t("world.fields.tone.label")}</span>
                <input
                  value={worldDraft.tone}
                  onChange={(event) => updateWorldDraft("tone", event.target.value)}
                  placeholder={t("world.fields.tone.placeholder")}
                  disabled={locked}
                />
              </label>
            </div>

            <label className="world-field">
              <span>{t("world.fields.worldSize.label")}</span>
              <AutoTextarea
                value={worldDraft.worldSize}
                onChange={(event) => updateWorldDraft("worldSize", event.target.value)}
                placeholder={t("world.fields.worldSize.placeholder")}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>{t("world.fields.population.label")}</span>
              <AutoTextarea
                value={worldDraft.population}
                onChange={(event) => updateWorldDraft("population", event.target.value)}
                placeholder={t("world.fields.population.placeholder")}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>{t("world.fields.publicPremise.label")}</span>
              <AutoTextarea
                value={worldDraft.publicPremise}
                onChange={(event) => updateWorldDraft("publicPremise", event.target.value)}
                placeholder={t("world.fields.publicPremise.placeholder")}
                disabled={locked}
              />
            </label>

            {visualPromptSnapshot(worldDraft.worldLore).some(({ prompt, imageUrl }) => prompt || imageUrl) && (
              <div className="world-visual-gallery" aria-label={t("world.visual.galleryAriaLabel")}>
                <div className="world-visual-gallery-head">
                  <span className="world-inspector-label">{t("world.visual.galleryTitle")}</span>
                </div>
                <div className="world-visual-gallery-grid">
                  {VISUAL_PROMPT_FIELDS.map(([field, visualKey, outputField]) => {
                    const prompt = textValue(worldDraft.worldLore?.[field]);
                    const imageUrl = textValue(worldDraft.worldLore?.[outputField]);
                    const job = imageJobs[field] || {};
                    const outputLabel = t(`world.visual.${visualKey}.outputLabel`);
                    if (!prompt && !imageUrl && !job.loading && !job.error) return null;
                    return (
                      <div key={field} className="world-visual-card">
                        <div className="world-visual-card-head">
                          <b>{outputLabel}</b>
                          <span className="world-visual-state">{visualJobLabel(job, prompt, imageUrl, t)}</span>
                        </div>
                        {imageUrl ? (
                          <ImageThumbnail
                            src={imageUrl}
                            alt={outputLabel}
                            caption={outputLabel}
                            className="world-visual-thumb"
                          />
                        ) : (
                          <div className="world-visual-pending">
                            {visualJobLabel(job, prompt, imageUrl, t)}
                          </div>
                        )}
                        {job.seed != null && (
                          <span className="world-visual-seed">
                            {t("world.visual.seed", { seed: job.seed })}
                          </span>
                        )}
                        {job.error && <div className="world-visual-error">{job.error}</div>}
                      </div>
                    );
                  })}
                </div>
              </div>
            )}

            <div className="world-bible">
              <button
                type="button"
                className="world-bible-toggle"
                onClick={() => setBibleOpen((open) => !open)}
                aria-expanded={bibleOpen}
                disabled={locked}
              >
                <span className="world-bible-toggle-label">
                  <b>{t("world.lore.title")}</b>
                  <small>
                    {loreFilled ? t("world.lore.filled") : t("world.lore.empty")}
                  </small>
                </span>
                <span className="world-bible-caret" aria-hidden="true"><Icon name={bibleOpen ? "chevron-down" : "chevron-right"} size={12} /></span>
              </button>
              {bibleOpen && (
                <div className="world-bible-fields">
                  <p className="world-bible-hint">
                    {t("world.lore.hint")}
                  </p>
                  <label className="world-field">
                    <span>{t("world.lore.hiddenPremise.label")}</span>
                    <AutoTextarea
                      value={worldDraft.worldLore?.hidden_premise || ""}
                      onChange={(event) => updateLoreText("hidden_premise", event.target.value)}
                      placeholder={t("world.lore.hiddenPremise.placeholder")}
                      disabled={locked}
                    />
                  </label>
                  {VISUAL_PROMPT_FIELDS.map(([field, visualKey, outputField]) => {
                    const prompt = textValue(worldDraft.worldLore?.[field]);
                    const imageUrl = textValue(worldDraft.worldLore?.[outputField]);
                    const job = imageJobs[field] || {};
                    const label = t(`world.visual.${visualKey}.promptLabel`);
                    const placeholder = t(`world.visual.${visualKey}.promptPlaceholder`);
                    const outputLabel = t(`world.visual.${visualKey}.outputLabel`);
                    return (
                      <div key={field} className="world-visual-field">
                        <label className="world-field">
                          <span>{label}</span>
                          <AutoTextarea
                            value={worldDraft.worldLore?.[field] || ""}
                            onChange={(event) => updateLoreText(field, event.target.value)}
                            placeholder={placeholder}
                            disabled={locked}
                          />
                        </label>
                        <div className="world-visual-actions">
                          <span className="world-visual-state">{visualJobLabel(job, prompt, imageUrl, t)}</span>
                          {job.seed != null && (
                            <span className="world-visual-seed">
                              {t("world.visual.seed", { seed: job.seed })}
                            </span>
                          )}
                        </div>
                        {job.error && <div className="world-visual-error">{job.error}</div>}
                        {imageUrl && (
                          <ImageThumbnail
                            src={imageUrl}
                            alt={outputLabel}
                            caption={outputLabel}
                            className="world-visual-thumb"
                          />
                        )}
                      </div>
                    );
                  })}
                  {LORE_PREVIEW_FIELDS.map(([field, labelKey]) => (
                    <label key={field} className="world-field">
                      <span>{t(`world.lore.fields.${labelKey}`)}</span>
                      <AutoTextarea
                        value={loreFieldText(worldDraft.worldLore, field)}
                        onChange={(event) => updateLoreList(field, event.target.value)}
                        placeholder={t("world.lore.listPlaceholder")}
                        disabled={locked}
                      />
                    </label>
                  ))}
                </div>
              )}
            </div>
          </div>

          <div className="world-inspector-foot">
            <button type="submit" className="btn primary world-create-btn" disabled={worldCreateLocked}>
              {t("world.actions.save")}
            </button>
            {world?.id && (
              <div className="world-inspector-launch">
                <button
                  type="button"
                  className="btn"
                  onClick={() => onPlayWorld?.(world.id)}
                  disabled={locked}
                >
                  {t("world.actions.play")}
                </button>
                <button
                  type="button"
                  className="btn"
                  onClick={() => onCreateStory?.(world.id)}
                  disabled={locked}
                >
                  {t("world.actions.createStory")}
                </button>
              </div>
            )}
            <p className="world-manager-note">
              {t("world.saveNote")}
            </p>
          </div>
        </section>
      </div>

      <ArchitectDebugModal
        debug={debugOpen ? architectDebug : null}
        onClose={() => setDebugOpen(false)}
      />
    </form>
  );
}
