import { useMemo, useState, useEffect, useRef, useCallback, useSyncExternalStore } from "react";
import { useTranslation } from "react-i18next";
import i18n from "./i18n/index.js";
import {
  api,
  attachArchitect,
  attachTurn,
  createTurnRequestId,
  streamTurn,
  streamArchitect,
  streamStoryArchitect,
  streamCharacterArchitect,
} from "./api.js";
import { createTimeline } from "./timelineStore.js";
import { historicalFailedTurn } from "./turnRetry.js";
import {
  ttsPrime,
  ttsAutoEnqueue,
  ttsAutoReset,
  ttsUnlock,
  gmSegments,
  npcSegments,
  genderVoice,
} from "./ttsStore.js";
import Header from "./components/Header.jsx";
import Chat from "./components/Chat.jsx";
import Composer from "./components/Composer.jsx";
import DebugPanel from "./components/DebugPanel.jsx";
import ChatHistorySidebar from "./components/ChatHistorySidebar.jsx";
import WorldArchitectPanel from "./components/WorldArchitectPanel.jsx";
import StoryArchitectPanel from "./components/StoryArchitectPanel.jsx";
import CharacterArchitectPanel from "./components/CharacterArchitectPanel.jsx";
import LibraryScreen from "./components/LibraryScreen.jsx";
import NewGameWizard from "./components/NewGameWizard.jsx";
import BasePickerModal from "./components/BasePickerModal.jsx";
import GameContextBar from "./components/GameContextBar.jsx";
import ScenePanel from "./components/ScenePanel.jsx";
import Toasts, { useToasts } from "./components/Toasts.jsx";
import ImageLabPanel from "./components/ImageLabPanel.jsx";
import GlobalSearchPalette from "./components/GlobalSearchPalette.jsx";
import LocationMapOverlay from "./components/LocationMapOverlay.jsx";
import { normalizeEntities } from "./entityContext.js";
import { useDevSettings, computeVisibility, VisibilityContext, isMessageVisible } from "./devSettings.js";
import { useInterfaceSettings } from "./interfaceSettings.js";
import {
  createLocationTransition,
  hasLocationGraph,
  locationTravelIntent,
} from "./locationTransition.js";
import { acceptStateSyncEvent, applyStateSyncEvent } from "./liveTurnState.js";
import { localizeServerMessage } from "./serverMessages.js";
import {
  connectorAuthState,
  connectorAuthUrl,
  connectorById,
  connectorIdOf,
  connectorName,
  modelConnectorId,
  modelIdOf,
  modelsForConnector,
  normalizeModelBinding,
  normalizeModels,
} from "./connectorCatalog.js";

const CONNECTOR_AUTH_DEFAULT_TIMEOUT_MS = 5 * 60 * 1000;
const CONNECTOR_AUTH_CANCEL_TIMEOUT_MS = 15 * 1000;

// The server keeps a turn running after this client loses the stream, so a
// dropped connection is recovered by re-attaching to the live feed instead of
// failing the turn.
const TURN_RECONNECT_ATTEMPTS = 2;
const TURN_RECONNECT_DELAY_MS = 1500;

function appText(key, options = {}) {
  return i18n.t(key, { ns: "app", ...options });
}

function userErrorText(value, fallback) {
  return localizeServerMessage(value, (key, options) => i18n.t(key, options), {
    fallbackText: fallback,
  });
}

function connectorAuthPollInterval(start) {
  const seconds = Number(start?.interval_seconds);
  if (!Number.isFinite(seconds) || seconds <= 0) return 2000;
  return Math.min(10, Math.max(1, seconds)) * 1000;
}

function connectorAuthTimeout(start) {
  const seconds = Number(start?.expires_in_seconds);
  return Number.isFinite(seconds) && seconds >= 0
    ? Math.max(1, seconds) * 1000
    : CONNECTOR_AUTH_DEFAULT_TIMEOUT_MS;
}

function waitForAbortable(milliseconds, signal) {
  if (signal?.aborted) {
    const error = new Error();
    error.name = "AbortError";
    return Promise.reject(error);
  }
  return new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, milliseconds);
    const onAbort = () => {
      window.clearTimeout(timer);
      const error = new Error();
      error.name = "AbortError";
      reject(error);
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

function isAbortError(error) {
  return error?.name === "AbortError";
}

const EMPTY_SRV = {
  backend: "",
  model: "",
  modelBinding: { connector_id: "", model_id: "" },
  stream_gm_content: false,
  storyId: "",
  storyTitle: "",
  storyBrief: null,
  scene: "",
  time: null,
  playerCharacter: null,
  charRef: null,
  worldRef: null,
  npcs: [],
  entities: { byKey: {} },
  statusLabels: {},
  locationGraph: null,
};

// Settings values and option enums are owned by the backend and delivered through
// /state and /models. These are inert pre-load placeholders, NOT a second copy of
// the defaults/enums — the server payload is merged over them on first load.
const EMPTY_SETTINGS = {};
const EMPTY_SETTINGS_OPTIONS = {};

const EMPTY_RUN_USAGE = {
  turns: 0,
  calls: 0,
  in: 0,
  out: 0,
  cached: 0,
  tokens: 0,
  secs: 0,
  peak_context: 0,
  gm_calls: 0,
  gm_tokens: 0,
  npc_calls: 0,
  npc_tokens: 0,
};

const EMPTY_CONTEXT_USAGE = {
  current: 0,
  world: 0,
  next_compact: { label: "GM", used: 0, limit: 0, remaining: 0 },
  gm: { active: 0, history: 0, summary: 0, limit: 0, remaining: 0 },
  npc: { name: "", active: 0, history: 0, summary: 0, limit: 0, remaining: 0 },
  npcs: [],
};

const EMPTY_SIDECAR_STATUS = {
  ok: false,
  enabled: false,
  ready: false,
  state: "disabled",
  manager_state: "disabled",
  components: {},
};

function sameChatId(a, b) {
  return a != null && b != null && String(a) === String(b);
}

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function storyIdFrom(story) {
  const id = story?.story_id ?? story?.id;
  return id == null ? "" : String(id).trim();
}

function normalizeStory(story) {
  const id = storyIdFrom(story);
  if (!id) return null;
  const title = textValue(story?.title) || textValue(story?.name) || id;
  const description =
    textValue(story?.story_brief) ||
    textValue(story?.description) ||
    textValue(story?.summary) ||
    textValue(story?.public_intro) ||
    "";
  const storyBrief = textValue(story?.story_brief);
  return { ...story, id, story_id: id, title, description, story_brief: storyBrief };
}

// The library / wizard need the FULL story catalog (incl. self-contained builtin
// bundles); only the procedural pseudo-row is dropped. Builtins have no world_ref
// and the wizard renders them as world cards labeled «встроенная классика».
function normalizeStories(data) {
  if (!Array.isArray(data?.stories)) return [];
  return data.stories
    .map(normalizeStory)
    .filter(Boolean)
    .filter((story) => story.id !== "procedural");
}

function activeChatIdFrom(data) {
  return data?.active_chat_id || data?.chats?.find((chat) => chat.active)?.id || "";
}

function normalizePlayerOptions(payload) {
  if (!payload || typeof payload !== "object") return null;
  const options = Array.isArray(payload.options)
    ? payload.options
        .map((option) => ({
          label: textValue(option?.label),
          message: textValue(option?.message),
        }))
        .filter((option) => option.label && option.message)
    : [];
  if (!options.length) return null;
  return {
    question: textValue(payload.question) || appText("defaults.nextQuestion"),
    options,
  };
}

function playerOptionsFromEvents(events) {
  let current = null;
  for (const ev of Array.isArray(events) ? events : []) {
    if (ev?.kind === "player") current = null;
    if (ev?.kind === "player_options") current = normalizePlayerOptions(ev.data);
  }
  return current;
}

function mergeChatList(prevChats, chat, activeChatId) {
  const list = Array.isArray(prevChats) ? prevChats : [];
  if (!chat) {
    return list.map((item) => ({ ...item, active: sameChatId(item.id, activeChatId) }));
  }

  let found = false;
  const merged = list.map((item) => {
    if (sameChatId(item.id, chat.id)) {
      found = true;
      return { ...item, ...chat, active: true };
    }
    return { ...item, active: sameChatId(item.id, activeChatId) };
  });

  return found ? merged : [{ ...chat, active: true }, ...merged];
}

function requireChatSessionPayload(payload) {
  const missing = [];
  if (payload?.chat?.id == null) missing.push("chat.id");
  if (payload?.state == null) missing.push("state");
  if (payload?.transcript == null) missing.push("transcript");
  if (missing.length > 0) {
    throw new Error(appText("errors.incompleteChatPayload", { fields: missing.join(", ") }));
  }
  return { chatId: payload.chat.id, state: payload.state, transcript: payload.transcript };
}

export default function App() {
  const { t } = useTranslation("app");
  const store = useMemo(createTimeline, []);
  const messages = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const dev = useDevSettings();
  const interfaceSettings = useInterfaceSettings();
  const visibility = useMemo(() => computeVisibility(dev), [dev]);
  const visibleMessages = useMemo(
    () => messages.filter((m) => isMessageVisible(m, visibility)),
    [messages, visibility]
  );

  // Bottom-right toast stack replaces the old error-into-transcript channel for
  // ACTION failures (API calls, launches, CRUD). GM/agent errors that arrive as
  // `error` events on the SSE stream still render inline in the transcript.
  const { toasts, pushToast, dismissToast } = useToasts();
  const notify = useCallback(
    (message, opts = {}) => pushToast({ kind: "error", message: textValue(message), ...opts }),
    [pushToast]
  );
  // Server failures are `{ok:false, code?, params?}`. Resolve the stable code in
  // the browser locale and never surface raw transport/backend details here.
  const notifyApiError = useCallback(
    (data, fallback = "") => {
      const rawCode = data && typeof data.code === "string" ? data.code.trim() : "";
      pushToast({
        kind: "error",
        code: rawCode || undefined,
        message: userErrorText(data, textValue(fallback)),
      });
    },
    [pushToast]
  );

  const [srv, setSrv] = useState(EMPTY_SRV);
  const srvRef = useRef(EMPTY_SRV);
  const locationTransitionSequenceRef = useRef(0);
  const [locationTransition, setLocationTransition] = useState(null);
  const [locationMapOpen, setLocationMapOpen] = useState(false);
  const [settings, setSettings] = useState(EMPTY_SETTINGS);
  const [settingsOptions, setSettingsOptions] = useState(EMPTY_SETTINGS_OPTIONS);
  const [runUsage, setRunUsage] = useState(EMPTY_RUN_USAGE);
  const [contextUsage, setContextUsage] = useState(EMPTY_CONTEXT_USAGE);
  const [connectors, setConnectors] = useState([]);
  const [models, setModels] = useState([]);
  const [connectorModelsLoadingIds, setConnectorModelsLoadingIds] = useState([]);
  const connectorModelsLoadedRef = useRef(new Set());
  const connectorModelsRequestsRef = useRef(new Map());
  const [connectorAuthBusyIds, setConnectorAuthBusyIds] = useState([]);
  const [connectorAuthCancellingIds, setConnectorAuthCancellingIds] = useState([]);
  const [connectorAuthPrompts, setConnectorAuthPrompts] = useState({});
  const connectorAuthOperationsRef = useRef(new Map());
  const [sidecarStatus, setSidecarStatus] = useState(EMPTY_SIDECAR_STATUS);
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState(false);
  const [turnGenerating, setTurnGenerating] = useState(false);
  const [failedTurn, setFailedTurn] = useState(null);
  const turnInFlightRef = useRef(false);
  const turnAbortRef = useRef(null);
  const activeTurnRef = useRef(null);
  useEffect(() => () => turnAbortRef.current?.abort(), []);
  const [chats, setChats] = useState([]);
  const [activeChatId, setActiveChatId] = useState("");
  // Current value for async flows (turn re-attach) that outlive the closure
  // they were created in.
  const activeChatIdRef = useRef("");
  useEffect(() => {
    activeChatIdRef.current = activeChatId;
  }, [activeChatId]);
  const [chatsOpen, setChatsOpen] = useState(() => {
    // Desktop: docked sidebar starts expanded; mobile: drawer starts closed.
    // A saved choice (localStorage) wins so a collapse/expand sticks across reloads.
    if (typeof window === "undefined") return false;
    try {
      const saved = window.localStorage.getItem("gmlab.chatsOpen");
      if (saved === "0") return false;
      if (saved === "1") return true;
    } catch {
      /* localStorage unavailable — fall through to breakpoint default */
    }
    return window.matchMedia("(min-width: 701px)").matches;
  });
  const [chatsLoading, setChatsLoading] = useState(true);
  const [chatsError, setChatsError] = useState("");
  const [worlds, setWorlds] = useState([]);
  const [worldsLoading, setWorldsLoading] = useState(true);
  const [worldsError, setWorldsError] = useState("");
  const [selectedWorldId, setSelectedWorldId] = useState("");
  const [chatActionBusy, setChatActionBusy] = useState(false);
  const [stories, setStories] = useState([]);
  const [storiesLoading, setStoriesLoading] = useState(true);
  const [storiesError, setStoriesError] = useState("");
  const [characters, setCharacters] = useState([]);
  const [charactersLoading, setCharactersLoading] = useState(true);
  const [charactersError, setCharactersError] = useState("");
  // Studio targets: the world the story architect plots over, the story it edits,
  // and the character package the character architect edits ("" = a fresh draft).
  const [storyArchitectWorldId, setStoryArchitectWorldId] = useState("");
  const [selectedStoryArchitectId, setSelectedStoryArchitectId] = useState("");
  const [selectedCharacterArchitectId, setSelectedCharacterArchitectId] = useState("");
  // Mount epoch for the character studio: bumped ONLY on an explicit open, so
  // the create-on-first-turn id assignment ("" -> new id) does NOT remount the
  // panel and wipe its live stream (review finding).
  const [characterStudioEpoch, setCharacterStudioEpoch] = useState(0);
  // The base a NEW character is built on ({worldId, storyId}, both optional) —
  // picked in BasePickerModal (or passed by the wizard) and sent with the
  // create-on-first-turn / manual save. An EXISTING character ignores this: its
  // binding is fixed in the package (world_ref/story_ref).
  const [characterStudioBase, setCharacterStudioBase] = useState(null);
  // Which creation base picker is open: null | "story" | "character".
  const [basePickerKind, setBasePickerKind] = useState(null);
  // The New-Game wizard: the ONLY way to create a game. `wizardPreselect` seeds a
  // step from a Library «Играть» click ({worldId}|{storyId}|{characterId}).
  const [wizardOpen, setWizardOpen] = useState(false);
  const [wizardPreselect, setWizardPreselect] = useState(null);
  const [playerOptions, setPlayerOptions] = useState(null);
  const [debugOpen, setDebugOpen] = useState(false);
  const [globalSearchOpen, setGlobalSearchOpen] = useState(false);
  // Router: chat | library | world-studio | story-studio | character-studio | image
  const [mainView, setMainView] = useState("chat");
  const selectedWorld = useMemo(
    () => (Array.isArray(worlds) ? worlds : []).find((world) => sameChatId(world.id, selectedWorldId)) || null,
    [worlds, selectedWorldId]
  );
  const imageLabEnabled = !!dev.developerMode && settings.image_enabled === true;

  // Auto-generate TTS as GM/NPC lines finalize, read live inside the stream closure.
  const ttsEnabledRef = useRef(false);
  useEffect(() => {
    ttsEnabledRef.current = !!settings.tts_enabled;
  }, [settings.tts_enabled]);
  const ttsAutoplayRef = useRef(false);
  useEffect(() => {
    ttsAutoplayRef.current = !!settings.tts_autoplay;
  }, [settings.tts_autoplay]);
  const npcsRef = useRef([]); // roster, for voice resolution inside the stream closure
  useEffect(() => {
    npcsRef.current = srv.npcs || [];
  }, [srv.npcs]);

  useEffect(() => () => {
    for (const operation of connectorAuthOperationsRef.current.values()) {
      operation.disposed = true;
      if (operation.timeoutId) window.clearTimeout(operation.timeoutId);
      if (operation.cancelTimeoutId) window.clearTimeout(operation.cancelTimeoutId);
      operation.controller?.abort();
      operation.cancelController?.abort();
    }
    connectorAuthOperationsRef.current.clear();
  }, []);

  const publishServerState = useCallback(
    (nextSrv, { animateLocationChange = false, clearLocationTransition = false } = {}) => {
      const transition = createLocationTransition(
        srvRef.current,
        nextSrv,
        animateLocationChange
      );
      srvRef.current = nextSrv;
      setSrv(nextSrv);
      if (clearLocationTransition) {
        setLocationTransition(null);
      } else if (transition) {
        locationTransitionSequenceRef.current += 1;
        setLocationMapOpen(false);
        setLocationTransition({
          ...transition,
          sequence: locationTransitionSequenceRef.current,
        });
      }
    },
    []
  );

  const setStateFromServer = useCallback((s, { animateLocationChange = false } = {}) => {
    const binding = normalizeModelBinding(s.model_binding || {
      connector_id: s.backend,
      model_id: s.model,
    });
    const nextSrv = {
      backend: binding.connector_id || s.backend,
      model: binding.model_id || s.model,
      modelBinding: binding,
      stream_gm_content: s.stream_gm_content,
      storyId: s.story_id || "",
      storyTitle: s.story_title || "",
      storyBrief: s.story_brief || null,
      scene: s.scene || s.public,
      time: s.time || null,
      playerCharacter: s.player_character || null,
      // K1 (§К1.5): the launched CHARACTER package provenance, when present.
      charRef: s.char_ref || null,
      // The launched WORLD package provenance — resolves the game-context bar's
      // world badge against the loaded worlds list.
      worldRef: s.world_ref || null,
      npcs: s.npcs || [],
      entities: normalizeEntities(s.entities, s.npcs),
      statusLabels: s.status_labels || {},
      locationGraph: s.location_graph || null,
    };
    publishServerState(nextSrv, { animateLocationChange });
    if (s.settings) setSettings((prev) => ({ ...prev, ...s.settings }));
    if (s.settings_options) {
      setSettingsOptions((prev) => ({ ...prev, ...s.settings_options }));
    }
    setRunUsage(s.run_usage || EMPTY_RUN_USAGE);
    setContextUsage(s.context_usage || EMPTY_CONTEXT_USAGE);
  }, [publishServerState]);

  const applyLiveStateSync = useCallback(
    (event, { animateLocationChange = true } = {}) => {
      const nextSrv = applyStateSyncEvent(srvRef.current, event);
      if (nextSrv === srvRef.current) return false;
      publishServerState(nextSrv, { animateLocationChange });
      return true;
    },
    [publishServerState]
  );

  const restoreLiveStateCheckpoint = useCallback(
    (snapshot) => {
      if (!snapshot) return;
      publishServerState(snapshot, {
        animateLocationChange: false,
        clearLocationTransition: true,
      });
    },
    [publishServerState]
  );

  const setChatsFromServer = useCallback((data) => {
    const list = Array.isArray(data?.chats) ? data.chats : [];
    const nextActiveChatId = activeChatIdFrom(data);
    setChats(list);
    setActiveChatId(nextActiveChatId || "");
    return nextActiveChatId;
  }, []);

  const refreshChats = useCallback(async () => {
    setChatsLoading(true);
    setChatsError("");
    try {
      const data = await api.chats();
      if (!data.ok) throw new Error(data.error || appText("errors.chatsLoad"));
      setChatsFromServer(data);
      return data;
    } catch (e) {
      setChatsError(userErrorText(e, appText("errors.chatsLoad")));
      return null;
    } finally {
      setChatsLoading(false);
    }
  }, [setChatsFromServer]);

  const refreshWorlds = useCallback(async () => {
    setWorldsLoading(true);
    setWorldsError("");
    try {
      const data = await api.worlds();
      if (!data.ok) throw new Error(data.error || appText("errors.worldsLoad"));
      setWorlds(Array.isArray(data.worlds) ? data.worlds : []);
      return data;
    } catch (e) {
      setWorldsError(userErrorText(e, appText("errors.worldsLoad")));
      return null;
    } finally {
      setWorldsLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!selectedWorldId) return;
    if (!worlds.some((world) => sameChatId(world.id, selectedWorldId))) {
      setSelectedWorldId("");
    }
  }, [worlds, selectedWorldId]);

  useEffect(() => {
    let stopped = false;
    let timer = null;

    const poll = async () => {
      let delay = 5000;
      try {
        const data = await api.sidecarStatus();
        if (stopped) return;
        setSidecarStatus(data || EMPTY_SIDECAR_STATUS);
        const loading = data?.enabled && !data?.ready && data?.state !== "failed";
        delay = loading ? 1000 : 5000;
      } catch (e) {
        if (stopped) return;
        setSidecarStatus({
          ...EMPTY_SIDECAR_STATUS,
          enabled: true,
          state: "unavailable",
        error: userErrorText(e, appText("errors.sidecarUnavailable")),
        });
      }
      timer = window.setTimeout(poll, delay);
    };

    poll();
    return () => {
      stopped = true;
      if (timer) window.clearTimeout(timer);
    };
  }, []);

  const loadStories = useCallback(async () => {
    setStoriesLoading(true);
    setStoriesError("");
    try {
      const data = await api.stories();
      if (!data.ok) throw new Error(data.error || appText("errors.storiesLoad"));
      const nextStories = normalizeStories(data);
      setStories(nextStories);
      return nextStories;
    } catch (e) {
      setStories([]);
      setStoriesError(userErrorText(e, appText("errors.storiesLoad")));
      return [];
    } finally {
      setStoriesLoading(false);
    }
  }, []);

  const loadCharacters = useCallback(async () => {
    setCharactersLoading(true);
    setCharactersError("");
    try {
      const data = await api.characters();
      if (!data.ok) throw new Error(data.error || appText("errors.charactersLoad"));
      const next = Array.isArray(data.characters) ? data.characters : [];
      setCharacters(next);
      return next;
    } catch (e) {
      setCharacters([]);
      setCharactersError(userErrorText(e, appText("errors.charactersLoad")));
      return [];
    } finally {
      setCharactersLoading(false);
    }
  }, []);

  const restoreChatSession = useCallback(
    (payload, { animateLocationChange = false } = {}) => {
      const { chatId: nextChatId, state: nextState, transcript: nextTranscript } =
        requireChatSessionPayload(payload);

      store.clear();
      setFailedTurn(null);
      setStateFromServer(nextState, { animateLocationChange });
      const events = nextTranscript?.events || [];
      store.dispatchMany(events);
      setPlayerOptions(playerOptionsFromEvents(events));
      setActiveChatId(nextChatId || "");
      if (Array.isArray(payload?.chats)) {
        setChats(payload.chats);
      } else if (payload?.chat || nextChatId) {
        setChats((prev) => mergeChatList(prev, payload?.chat, nextChatId));
      }
      return nextChatId;
    },
    [store, setStateFromServer]
  );

  const loadConnectorModels = useCallback((rawConnectorId, { force = false } = {}) => {
    const connectorId = textValue(rawConnectorId);
    if (!connectorId) return Promise.resolve([]);

    const pending = connectorModelsRequestsRef.current.get(connectorId);
    if (pending) {
      return force
        ? pending.then(() => loadConnectorModels(connectorId, { force: true }))
        : pending;
    }
    if (!force && connectorModelsLoadedRef.current.has(connectorId)) {
      return Promise.resolve([]);
    }

    setConnectorModelsLoadingIds((current) => (
      current.includes(connectorId) ? current : [...current, connectorId]
    ));

    const request = (async () => {
      try {
        const data = await api.connectorModels(connectorId);
        if (!data.ok) throw new Error(data.error || appText("errors.connectorModelsLoad"));
        if (data.connector_id && data.connector_id !== connectorId) {
          throw new Error(appText("errors.connectorModelsMismatch"));
        }
        const nextModels = normalizeModels([], data.models, connectorId);
        setModels((current) => [
          ...current.filter((model) => modelConnectorId(model) !== connectorId),
          ...nextModels,
        ]);
        connectorModelsLoadedRef.current.add(connectorId);
        return nextModels;
      } catch (error) {
      notify(userErrorText(error, appText("errors.connectorModelsLoad")));
        return [];
      } finally {
        connectorModelsRequestsRef.current.delete(connectorId);
        setConnectorModelsLoadingIds((current) => current.filter((id) => id !== connectorId));
      }
    })();

    connectorModelsRequestsRef.current.set(connectorId, request);
    return request;
  }, [notify]);

  const loadConnectors = useCallback(async () => {
    try {
      const data = await api.connectors();
      if (!data.ok) throw new Error(data.error || appText("errors.connectorsLoad"));
      const nextConnectors = Array.isArray(data.connectors) ? data.connectors : [];
      const binding = normalizeModelBinding(data.model_binding);
      const catalogModels = normalizeModels(nextConnectors, data.models, binding.connector_id);
      setConnectors(nextConnectors);
      if (catalogModels.length > 0) {
        const connectorIds = new Set(catalogModels.map(modelConnectorId).filter(Boolean));
        setModels((current) => [
          ...current.filter((model) => !connectorIds.has(modelConnectorId(model))),
          ...catalogModels,
        ]);
      }
      if (binding.connector_id || binding.model_id) {
        setSrv((current) => ({
          ...current,
          backend: binding.connector_id || current.backend,
          model: binding.model_id || current.model,
          modelBinding: {
            connector_id: binding.connector_id || current.modelBinding.connector_id,
            model_id: binding.model_id || current.modelBinding.model_id,
          },
        }));
      }
    } catch {
      connectorModelsLoadedRef.current.clear();
      setConnectors([]);
      setModels([]);
    }
  }, []);

  useEffect(() => {
    const connectorId = srv.modelBinding.connector_id;
    if (connectorId && connectorById(connectors, connectorId)) {
      loadConnectorModels(connectorId);
    }
  }, [connectors, loadConnectorModels, srv.modelBinding.connector_id]);

  // initial load
  useEffect(() => {
    (async () => {
      await Promise.all([refreshChats(), refreshWorlds(), loadStories(), loadCharacters()]);
      try {
        const s = await api.state();
        setStateFromServer(s);
      } catch (e) {
      notify(userErrorText(e, appText("errors.stateLoad")));
      }
      await loadConnectors();
      try {
        const t = await api.transcript();
        store.clear();
        // Прод отдаёт {events:[...]}, мок-бэкенд — голый массив; принимаем оба.
        const events = Array.isArray(t) ? t : t?.events || [];
        store.dispatchMany(events);
        setPlayerOptions(playerOptionsFromEvents(events));
      } catch (e) {
      notify(userErrorText(e, appText("errors.transcriptLoad")));
      }
      setStatus("");
      // The server may still be running a turn started before this page load;
      // re-attach to its live feed instead of showing a silently missing
      // turn. Via the ref: the awaits above re-rendered the app with loaded
      // usage/options, and the attach must run against that fresh closure.
      await resumeActiveTurnRef.current("");
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const sendTurn = useCallback(
    async (rawText, previousRequestId = "", retryOptions = {}) => {
      const text = textValue(rawText);
      const historyMutation = retryOptions?.historyMutation === true;
      // Attach mode joins a turn the server is already running (after a page
      // reload or a lost stream); the player text is unknown until the feed
      // replays the `player` event.
      const attach = retryOptions?.attach === true;
      if (
        (!text && !attach) ||
        turnInFlightRef.current ||
        busy ||
        (!historyMutation && chatActionBusy)
      ) return;
      const legacyResume = retryOptions?.legacyResume === true;

      let requestId = textValue(previousRequestId);
      if (attach && !requestId) return;
      try {
        if (!requestId) requestId = createTurnRequestId();
      } catch (error) {
      notify(userErrorText(error, appText("errors.turnFailed")));
        return;
      }

      turnInFlightRef.current = true;
      // A retryable attempt is deliberately left visible. Remove its optimistic
      // rows before either retrying it or starting a different player action.
      store.rollbackTurn();
      store.beginTurn();
      setFailedTurn(null);
      const controller = new AbortController();
      turnAbortRef.current = controller;

      const attemptChatId = textValue(retryOptions?.chatId) || activeChatId;
      const history = retryOptions?.history || null;
      const failedAttempt = {
        chatId: attemptChatId,
        text,
        requestId,
        legacyResume,
        history,
        attach,
      };
      const stateCheckpoint = srvRef.current;
      const activeTurn = {
        chatId: attemptChatId,
        requestId,
        controller,
        canonicalRestored: false,
        cancelling: false,
        cancelPromise: null,
        animateLocationChange: !historyMutation,
        stateCheckpoint,
        stateSyncSequences: new Set(),
      };
      activeTurnRef.current = activeTurn;
      const previousPlayerOptions = playerOptions;
      const previousRunUsage = runUsage;
      const previousContextUsage = contextUsage;
      const restoreAttemptUi = () => {
        setPlayerOptions(previousPlayerOptions);
        setRunUsage(previousRunUsage);
        setContextUsage(previousContextUsage);
      };

      ttsUnlock(); // unlock audio inside the send gesture so auto-play can sound
      ttsAutoReset(); // each turn's auto-play chain starts fresh
      setPlayerOptions(null);
      setBusy(true);
      setTurnGenerating(true);
      setStatus(appText("status.gmThinking"));
      let streamError = null;
      let playerEventSeen = false;
      let terminal = null;
      // A reconnect replays the feed from the start; each narration/speech may
      // reach this handler several times but must be voiced at most once.
      const ttsSeenKeys = new Set();
      const handleTurnEvent = (ev) => {
        if (ev.kind === "state_sync") {
          if (!acceptStateSyncEvent(activeTurn.stateSyncSequences, ev)) return;
          applyLiveStateSync(ev, {
            animateLocationChange: activeTurn.animateLocationChange,
          });
        }
        store.dispatch(ev);
        if (ev.kind === "error") {
          streamError = {
            agent: textValue(ev.agent) || "GM",
            text: textValue(ev.data) || appText("errors.turnFailed"),
          };
        }
        const auto = ttsAutoplayRef.current;
        if (auto || ttsEnabledRef.current) {
          const emit = (key, segs) => {
            if (ttsSeenKeys.has(key)) return;
            ttsSeenKeys.add(key);
            if (auto) ttsAutoEnqueue(key, segs);
            else ttsPrime(key, segs);
          };
          if (ev.kind === "gm_narration" && typeof ev.data === "string" && ev.data.trim())
            emit(`${ev.sid}:narration`, gmSegments(ev.data));
          else if (ev.kind === "npc_speech" && (ev.data?.response || ev.data?.speech || ev.data?.action)) {
            const npc = (npcsRef.current || []).find(
              (n) => (ev.data.npc_id && n.id === ev.data.npc_id) || n.name === ev.agent
            );
            emit(
              `${ev.sid}:npc`,
              npcSegments({
                name: ev.agent,
                response: ev.data.response,
                beats: ev.data.beats,
                speech: ev.data.speech,
                action: ev.data.action,
                voice: genderVoice(npc?.pronouns ?? npc?.gender),
              })
            );
          }
        }
        if (ev.kind === "player_options") setPlayerOptions(normalizePlayerOptions(ev.data));
        if (ev.kind === "player") {
          playerEventSeen = true;
          // The replayed feed is the only place an attached client learns
          // the player text; keep it for the manual-retry checkpoint.
          if (typeof ev.data === "string" && ev.data.trim()) {
            failedAttempt.text = ev.data;
          }
          setPlayerOptions(null);
        }
        if (ev.kind === "meta_total") {
          if (ev.data?.run) setRunUsage(ev.data.run);
          if (ev.data?.context) setContextUsage(ev.data.context);
        }
        if (ev.kind === "gm_tool_call") {
          const toolName = ev.data?.name === "invoke_loaded_tool"
            ? textValue(ev.data?.arguments?.name) || ev.data.name
            : ev.data?.name;
          if (toolName !== "ask_player") {
            setStatus(appText("status.gmTool", { tool: toolName }));
          }
        } else if (ev.kind === "npc_start") {
          setStatus(appText("status.npcTyping", { name: ev.agent }));
        } else if (ev.kind === "npc_speech") setStatus("");
      };
      // First attempt starts (or replays) the turn; reconnect attempts join
      // the live feed the server kept running. Every attempt replays the feed
      // from the beginning, so partial optimistic rows are rebuilt each time.
      const streamAttempt = (attemptIndex) =>
        attemptIndex === 0 && !attach
          ? streamTurn(text, requestId, handleTurnEvent, {
              signal: controller.signal,
              legacyResume,
              chatId: attemptChatId,
              history,
            })
          : attachTurn(attemptChatId, requestId, handleTurnEvent, {
              signal: controller.signal,
            });
      try {
        try {
          for (let attempt = 0; ; attempt += 1) {
            try {
              terminal = await streamAttempt(attempt);
              break;
            } catch (error) {
              const reconnectable =
                error?.retryable !== false &&
                !controller.signal.aborted &&
                !isAbortError(error) &&
                attempt < TURN_RECONNECT_ATTEMPTS;
              if (!reconnectable) throw error;
              streamError = null;
              playerEventSeen = false;
              restoreLiveStateCheckpoint(activeTurn.stateCheckpoint);
              activeTurn.stateSyncSequences.clear();
              store.rollbackTurn();
              store.beginTurn();
              setStatus(appText("status.turnReconnecting"));
              await waitForAbortable(
                TURN_RECONNECT_DELAY_MS * (attempt + 1),
                controller.signal
              );
            }
          }
          setTurnGenerating(false);
        } catch (e) {
          if (controller.signal.aborted || isAbortError(e)) {
            if (!activeTurn.canonicalRestored) {
              restoreAttemptUi();
              restoreLiveStateCheckpoint(activeTurn.stateCheckpoint);
              store.rollbackTurn();
            }
            setFailedTurn(null);
            return {
              ok: false,
              cancelled: true,
              retryable: false,
              request_id: requestId,
              chat_id: attemptChatId,
            };
          }
          restoreAttemptUi();
          restoreLiveStateCheckpoint(activeTurn.stateCheckpoint);
          const errorRow = streamError || {
            agent: "GM",
            text: userErrorText(e, appText("errors.turnFailed")),
          };
          if (e?.code === "turn_not_running") {
            // The server neither runs nor committed this request, so the
            // pre-turn state is canonical. Keep the checkpoint open (like the
            // retryable-failure path): the retry's rollbackTurn then removes
            // these rows, so the replayed player event is not duplicated.
            const retryText = textValue(failedAttempt.text);
            if (!playerEventSeen && retryText)
              store.pushLocal({ type: "player", text: retryText });
            store.pushLocal({ type: "error", ...errorRow });
            setFailedTurn(retryText ? { ...failedAttempt, attach: false } : null);
            return {
              ok: false,
              cancelled: false,
              retryable: Boolean(retryText),
              error: errorRow.text,
              request_id: requestId,
              chat_id: attemptChatId,
            };
          }
          if (e?.retryable === false) {
            store.rollbackTurn();
            store.pushLocal({ type: "error", ...errorRow });
            setFailedTurn(null);
          } else if (legacyResume) {
            // The persisted player/error/meta tail is the retry checkpoint.
            // Resume attempts must never add another local player or error row.
            store.rollbackTurn();
            setFailedTurn(failedAttempt);
          } else {
            if (!playerEventSeen && textValue(failedAttempt.text))
              store.pushLocal({ type: "player", text: failedAttempt.text });
            if (!streamError) store.pushLocal({ type: "error", ...errorRow });
            else store.flush();
            setFailedTurn(failedAttempt);
          }
          return {
            ok: false,
            cancelled: false,
            retryable: e?.retryable !== false,
            error: errorRow.text,
            request_id: requestId,
            chat_id: attemptChatId,
          };
        }

        if (terminal.cancelled === true) {
          if (!activeTurn.canonicalRestored) {
            restoreAttemptUi();
            restoreLiveStateCheckpoint(activeTurn.stateCheckpoint);
            store.rollbackTurn();
          }
          setFailedTurn(null);
          return terminal;
        }

        if (!terminal.ok) {
          restoreAttemptUi();
          restoreLiveStateCheckpoint(activeTurn.stateCheckpoint);
          const errorRow = streamError || {
            agent: "GM",
            text: textValue(terminal.error) || appText("errors.turnFailed"),
          };
          if (!legacyResume && !playerEventSeen && textValue(failedAttempt.text))
            store.pushLocal({ type: "player", text: failedAttempt.text });
          if (!legacyResume && !streamError) {
            streamError = errorRow;
            store.pushLocal({ type: "error", ...errorRow });
          }
          if (terminal.retryable) {
            if (legacyResume) store.rollbackTurn();
            else store.flush();
            // The turn itself ended; a manual retry must re-send, not attach.
            setFailedTurn(
              textValue(failedAttempt.text)
                ? { ...failedAttempt, attach: false }
                : null
            );
          } else {
            // The server did not commit this attempt and explicitly forbids a
            // replay. Drop partial rows, retaining only an explanatory error.
            store.rollbackTurn();
            store.pushLocal({ type: "error", ...errorRow });
            setFailedTurn(null);
          }
          return { ...terminal, error: errorRow.text, chat_id: attemptChatId };
        }

        const committedChatId = textValue(terminal.chat_id) || attemptChatId;
        if (!sameChatId(committedChatId, activeChatId)) {
          setActiveChatId(committedChatId);
        }

        if (terminal.replayed || legacyResume) {
          try {
            // Replayed requests stream no duplicate events, and a legacy resume
            // replaces its persisted error tail. In both cases only the server's
            // committed transcript is canonical.
            const [nextState, transcript] = await Promise.all([api.state(), api.transcript()]);
            const events = Array.isArray(transcript) ? transcript : transcript?.events || [];
            store.clear();
            store.dispatchMany(events);
            setPlayerOptions(playerOptionsFromEvents(events));
            setStateFromServer(nextState, { animateLocationChange: !historyMutation });
          } catch (error) {
            restoreAttemptUi();
            if (legacyResume) {
              store.rollbackTurn();
              notify(userErrorText(error, appText("errors.turnCommittedTranscriptRefresh")));
            } else {
              if (!playerEventSeen && textValue(failedAttempt.text))
                store.pushLocal({ type: "player", text: failedAttempt.text });
              store.pushLocal({
                type: "error",
                agent: "GM",
                text: userErrorText(error, appText("errors.turnCommittedTranscriptRefresh")),
              });
            }
            // Retain the current checkpoint. Repeating the same id is safe and
            // gives the client another chance to fetch the committed transcript.
            setFailedTurn({
              ...failedAttempt,
              chatId: committedChatId,
              history: null,
            });
            return terminal;
          }
        } else {
          store.commitTurn();
          try {
            // Reload the canonical transcript after every committed turn. Besides
            // exact state, it carries the rolling rewind window, so the 11th-oldest
            // player action loses its edit/branch controls immediately.
            const [nextState, transcript] = await Promise.all([api.state(), api.transcript()]);
            const events = Array.isArray(transcript) ? transcript : transcript?.events || [];
            store.clear();
            store.dispatchMany(events);
            setPlayerOptions(playerOptionsFromEvents(events));
            setStateFromServer(nextState, { animateLocationChange: !historyMutation });
          } catch (error) {
        notify(userErrorText(error, appText("errors.turnTranscriptRefresh")));
          }
        }

        setFailedTurn(null);
        try {
          await refreshChats();
        } catch (error) {
        notify(userErrorText(error, appText("errors.turnChatsRefresh")));
        }
        return terminal;
      } finally {
        if (activeTurn.cancelPromise) {
          await activeTurn.cancelPromise;
        }
        if (turnAbortRef.current === controller) turnAbortRef.current = null;
        if (activeTurnRef.current === activeTurn) activeTurnRef.current = null;
        turnInFlightRef.current = false;
        setTurnGenerating(false);
        setBusy(false);
        setStatus("");
      }
    },
    [
      activeChatId,
      busy,
      chatActionBusy,
      contextUsage,
      notify,
      playerOptions,
      refreshChats,
      applyLiveStateSync,
      restoreLiveStateCheckpoint,
      runUsage,
      setStateFromServer,
      store,
    ]
  );

  const stopTurn = useCallback(async () => {
    const activeTurn = activeTurnRef.current;
    if (
      !activeTurn ||
      activeTurn.cancelling ||
      activeTurn.controller.signal.aborted
    ) return;
    activeTurn.cancelling = true;
    ttsAutoReset();
    setStatus(appText("status.stopping"));
    const cancelPromise = (async () => {
      try {
        const data = await api.cancelTurn(activeTurn.chatId, activeTurn.requestId);
        if (
          !data?.ok ||
          (data.status !== "cancelled" && data.status !== "committed")
        ) {
          throw new Error(data?.error || appText("errors.stopNotConfirmed"));
        }

        // The cancel endpoint crosses the same commit fence as SQLite and returns
        // the only canonical outcome: either the pre-turn snapshot or a turn that
        // had already committed. Apply it before closing the SSE stream.
        restoreChatSession(data, {
          animateLocationChange:
            data.status === "committed" && activeTurn.animateLocationChange,
        });
        activeTurn.canonicalRestored = true;
        activeTurn.controller.abort();
        if (data.status === "committed") {
          notify(appText("notices.turnAlreadyCommitted"), { kind: "info" });
        }
      } catch (error) {
        activeTurn.cancelling = false;
        setStatus(appText("status.gmThinking"));
      notify(userErrorText(error, appText("errors.stopFailed")));
      }
    })();
    activeTurn.cancelPromise = cancelPromise;
    await cancelPromise;
  }, [notify, restoreChatSession]);

  // The server owns turn execution, so a turn survives page reloads and chat
  // switches. Ask whether one is still running for this chat and re-attach to
  // its live feed; without this a reopened tab would show the pre-turn state
  // and invite a double send. The discovery response carries the turn's
  // fingerprint (text/history/legacy_resume) so a later manual retry repeats
  // the exact same work instead of degrading an edit/branch into a plain turn.
  const resumeActiveTurn = useCallback(
    async (chatId = "") => {
      if (turnInFlightRef.current) return;
      let active = null;
      try {
        active = await api.activeTurn(chatId);
      } catch {
        return; // discovery is best-effort; the turn stays reachable later
      }
      const requestId = textValue(active?.request_id);
      if (!requestId || turnInFlightRef.current) return;
      const targetChatId = textValue(active?.chat_id) || chatId;
      // The user may have switched chats while discovery was in flight; do
      // not stream another chat's turn into the current timeline.
      if (
        activeChatIdRef.current &&
        !sameChatId(targetChatId, activeChatIdRef.current)
      ) {
        return;
      }
      const history =
        active?.history &&
        (active.history.kind === "edit" || active.history.kind === "branch")
          ? {
              kind: active.history.kind,
              turn: active.history.turn,
              ...(textValue(active.history.title)
                ? { title: textValue(active.history.title) }
                : {}),
            }
          : null;
      await sendTurn(textValue(active?.text), requestId, {
        attach: true,
        chatId: targetChatId,
        legacyResume: active?.legacy_resume === true,
        history,
      });
    },
    [sendTurn]
  );
  // Async flows (mount effect, chat activation) must call the LATEST closure:
  // the first-render sendTurn would restore pre-load usage/options on failure.
  const resumeActiveTurnRef = useRef(resumeActiveTurn);
  useEffect(() => {
    resumeActiveTurnRef.current = resumeActiveTurn;
  }, [resumeActiveTurn]);

  const sendCommand = useCallback(
    async (text) => {
      const [rawCmd, ...rest] = text.slice(1).split(" ");
      const cmd = rawCmd.trim().toLowerCase();
      const arg = rest.join(" ").trim();
      store.rollbackTurn();
      setFailedTurn(null);
      // No client-side allow-list: the backend /cmd handler validates the command set
      // and returns a structured {ok:false,error} for unknown/incomplete commands.
      setBusy(true);
      try {
        const data = await api.command(cmd, arg);
        if (!data.ok) {
          notifyApiError(data, appText("errors.commandFailed"));
          return;
        }
        if (cmd === "reset") {
          store.clear();
          setPlayerOptions(null);
          setStateFromServer(data.state);
          store.pushLocal({ type: "command", text: appText("commands.newGame") });
        } else if (cmd === "new") {
          store.clear();
          setPlayerOptions(null);
          setStateFromServer(data.state);
          store.pushLocal({
            type: "command",
            text: appText("commands.newStory", {
              scene: data.state.scene?.title || appText("defaults.startingScene"),
            }),
          });
        } else if (cmd === "constraint") {
          store.pushLocal({ type: "command", text: appText("commands.constraintAdded") });
        } else if (cmd === "event") {
          store.pushLocal({ type: "command", text: appText("commands.worldEventAdded") });
        }
        await refreshChats();
      } catch (e) {
      notify(userErrorText(e, appText("errors.commandFailed")));
      } finally {
        setBusy(false);
      }
    },
    [store, setStateFromServer, refreshChats, notify, notifyApiError]
  );

  const closeChats = useCallback(() => setChatsOpen(false), []);
  const toggleChats = useCallback(() => setChatsOpen((value) => !value), []);
  // On desktop the sidebar is a docked, collapsible column, so it must stay open
  // after picking a game; only the mobile drawer should auto-close on selection.
  const closeChatsOnMobile = useCallback(() => {
    if (typeof window !== "undefined" && window.matchMedia("(max-width: 700px)").matches) {
      setChatsOpen(false);
    }
  }, []);

  const regenerateFromTurn = useCallback(
    async (mode, turn, rawText, previousRequestId = "") => {
      const text = textValue(rawText);
      if (!text || !Number.isInteger(turn) || turn < 1) {
        throw new Error(appText("errors.invalidRewindMessage"));
      }
      if (!activeChatId) throw new Error(appText("errors.openGameFirst"));
      if (turnInFlightRef.current || busy || chatActionBusy) {
        throw new Error(appText("errors.waitForCurrentAction"));
      }

      // Only the view is rewound here. The server loads the same checkpoint
      // into a staged runtime and leaves the durable source chat untouched
      // until the replacement turn completes successfully.
      store.rollbackTurn();
      if (!store.truncateFromPlayerTurn(turn)) {
        throw new Error(appText("errors.turnNoLongerEditable"));
      }

      setChatActionBusy(true);
      ttsAutoReset();
      setPlayerOptions(null);
      setMainView("chat");
      closeChatsOnMobile();
      try {
        const history = { kind: mode, turn };
        const result = await sendTurn(text, previousRequestId, {
          chatId: activeChatId,
          historyMutation: true,
          history,
        });
        if (result?.ok || result?.cancelled) return;

        // A failed staged mutation did not touch persistence. Restore the
        // canonical chat, but keep one local retry affordance using the same
        // idempotency key. If a lost terminal receipt hid a successful commit,
        // the request id in the canonical transcript proves it and no error is
        // added.
        const [nextState, transcript, chatList] = await Promise.all([
          api.state(),
          api.transcript(),
          api.chats(),
        ]);
        const events = Array.isArray(transcript) ? transcript : transcript?.events || [];
        const nextChatId = textValue(chatList?.active_chat_id) || activeChatId;
        const nextChats = Array.isArray(chatList?.chats) ? chatList.chats : chats;
        const nextChat =
          nextChats.find((chat) => sameChatId(chat?.id, nextChatId)) || { id: nextChatId };
        restoreChatSession({
          ok: true,
          active_chat_id: nextChatId,
          chat: nextChat,
          chats: nextChats,
          state: nextState,
          transcript: Array.isArray(transcript) ? { events } : transcript,
        });

        const committed = events.some(
          (event) =>
            event?.kind === "player" &&
            textValue(event?.request_id) === textValue(result?.request_id)
        );
        if (!committed) {
          const errorText = textValue(result?.error) || appText("errors.historyMutationFailed");
          store.beginTurn();
          store.pushLocal({ type: "error", agent: "GM", text: errorText });
          if (result?.retryable !== false && result?.request_id) {
            setFailedTurn({
              chatId: activeChatId,
              text,
              requestId: result.request_id,
              legacyResume: false,
              history,
            });
          }
        }
      } finally {
        setChatActionBusy(false);
        setStatus("");
      }
    },
    [
      activeChatId,
      busy,
      chats,
      chatActionBusy,
      closeChatsOnMobile,
      restoreChatSession,
      sendTurn,
      store,
    ]
  );

  const editFromTurn = useCallback(
    (turn, text) => regenerateFromTurn("edit", turn, text),
    [regenerateFromTurn]
  );
  const branchFromTurn = useCallback(
    (turn, text) => regenerateFromTurn("branch", turn, text),
    [regenerateFromTurn]
  );

  // ---- top-level navigation (header) ----
  const showGame = useCallback(() => {
    setMainView("chat");
  }, []);
  const showLibrary = useCallback(() => {
    setStatus("");
    setMainView("library");
  }, []);
  const showImage = useCallback(() => {
    if (!imageLabEnabled) return;
    setStatus("");
    setMainView("image");
  }, [imageLabEnabled]);
  const openGlobalSearch = useCallback(() => setGlobalSearchOpen(true), []);
  const closeGlobalSearch = useCallback(() => setGlobalSearchOpen(false), []);

  // Remember the collapse/expand choice across reloads.
  useEffect(() => {
    try {
      window.localStorage.setItem("gmlab.chatsOpen", chatsOpen ? "1" : "0");
    } catch {
      /* localStorage unavailable (private mode) — non-fatal */
    }
  }, [chatsOpen]);

  useEffect(() => {
    if (mainView === "image" && !imageLabEnabled) {
      setMainView("chat");
    }
  }, [imageLabEnabled, mainView]);

  // ---- studios ----
  const openWorldStudio = useCallback(
    (worldId = "") => {
      if (busy || chatActionBusy) return;
      setSelectedWorldId(worldId || "");
      setStatus("");
      setMainView("world-studio");
    },
    [busy, chatActionBusy]
  );

  const openStoryArchitect = useCallback(
    (worldId, storyId = "") => {
      if (busy || chatActionBusy) return;
      setStoryArchitectWorldId(worldId || "");
      setSelectedStoryArchitectId(storyId || "");
      setStatus("");
      setMainView("story-studio");
    },
    [busy, chatActionBusy]
  );

  const openCharacterStudio = useCallback(
    (characterId = "", base = null) => {
      if (busy || chatActionBusy) return;
      setSelectedCharacterArchitectId(characterId || "");
      // The base only matters for a FRESH draft (create-on-first-turn); an
      // existing character carries its own world_ref/story_ref.
      setCharacterStudioBase(characterId ? null : base);
      setCharacterStudioEpoch((n) => n + 1);
      setStatus("");
      setMainView("character-studio");
    },
    [busy, chatActionBusy]
  );

  // Library «Студия» on a card — open the entity's architect studio.
  const onOpenStudio = useCallback(
    (entity, kind) => {
      const id = entity?.id == null ? "" : String(entity.id);
      if (kind === "world") openWorldStudio(id);
      else if (kind === "story") openStoryArchitect(textValue(entity?.world_ref?.id), id);
      else if (kind === "character") openCharacterStudio(id);
    },
    [openWorldStudio, openStoryArchitect, openCharacterStudio]
  );

  // Library toolbar «+ Создать ▾ → …» and wizard «+ Создать …» — a fresh studio.
  // A story is ALWAYS based on a world and a character MAY be based on a world
  // and/or story: when the context doesn't already carry the user's explicit
  // choice (the wizard passes it), the BasePickerModal asks first — the base is
  // never silently inferred.
  const onCreateEntity = useCallback(
    (kind, ctx = null) => {
      if (kind === "world") {
        openWorldStudio("");
      } else if (kind === "story") {
        const worldId = textValue(ctx?.worldId);
        if (worldId) openStoryArchitect(worldId, "");
        else setBasePickerKind("story");
      } else if (kind === "character") {
        if (ctx && (textValue(ctx.worldId) || textValue(ctx.storyId))) {
          openCharacterStudio("", {
            worldId: textValue(ctx.worldId),
            storyId: textValue(ctx.storyId),
          });
        } else {
          setBasePickerKind("character");
        }
      }
    },
    [openWorldStudio, openStoryArchitect, openCharacterStudio]
  );

  // BasePickerModal «В студию →»: open the matching studio with the picked base.
  const onBasePicked = useCallback(
    (kind, { worldId = "", storyId = "" } = {}) => {
      setBasePickerKind(null);
      if (kind === "story") {
        if (!worldId) return; // the picker enforces this; belt-and-braces
        openStoryArchitect(worldId, "");
      } else if (kind === "character") {
        openCharacterStudio("", worldId || storyId ? { worldId, storyId } : null);
      }
    },
    [openStoryArchitect, openCharacterStudio]
  );

  // BasePickerModal's empty-library escape hatch: straight to the world studio.
  const onBasePickerCreateWorld = useCallback(() => {
    setBasePickerKind(null);
    openWorldStudio("");
  }, [openWorldStudio]);

  // Convenience for panels/back-links.
  const onCreateStory = useCallback((worldId) => openStoryArchitect(worldId, ""), [openStoryArchitect]);

  // ---- New-Game wizard ----
  const openWizard = useCallback(
    (preselect = null) => {
      if (busy || chatActionBusy) return;
      setWizardPreselect(preselect);
      setWizardOpen(true);
    },
    [busy, chatActionBusy]
  );
  const closeWizard = useCallback(() => setWizardOpen(false), []);

  // The single launch seam: story → POST /chats {story_id, character_id?};
  // procedural → POST /chats {story_id:"procedural", world_id, character_id}.
  const onWizardLaunch = useCallback(
    async ({ connectorId, modelId, storyId, worldId, characterId, title }) => {
      if (chatActionBusy) return;
      setChatActionBusy(true);
      setStatus(appText("status.launchingGame"));
      try {
        const body = {
          activate: true,
          connector_id: connectorId,
          model_id: modelId,
          story_id: storyId,
        };
        if (worldId) body.world_id = worldId;
        if (characterId) body.character_id = characterId;
        if (title) body.title = title;
        const data = await api.createChat(body);
        if (!data.ok) {
          notifyApiError(data, appText("errors.gameCreate"));
          return;
        }
        restoreChatSession(data);
        // Launch warnings (story_pc_override, world_version_drift…) are
        // «warn-but-allow» notices on a SUCCESSFUL launch — show them as
        // non-sticky warning toasts, never as sticky red errors.
        for (const w of data.warnings ?? []) {
          pushToast({
            kind: "warning",
            code: w.code,
            message: userErrorText(w, appText("errors.gameCreate")),
          });
        }
        setWizardOpen(false);
        setMainView("chat");
        await refreshChats();
        closeChatsOnMobile();
      } catch (e) {
        notify(userErrorText(e, appText("errors.gameCreate")));
      } finally {
        setChatActionBusy(false);
        setStatus("");
      }
    },
    [chatActionBusy, restoreChatSession, refreshChats, closeChatsOnMobile, pushToast, notify, notifyApiError]
  );

  // Library «Играть» / studio «Играть им/ей» → open the wizard pre-selected.
  const onPlayEntity = useCallback(
    (entity, kind) => {
      const id = entity?.id == null ? "" : String(entity.id);
      if (kind === "world") openWizard({ worldId: id });
      else if (kind === "story") openWizard({ storyId: id });
      else if (kind === "character") openWizard({ characterId: id });
    },
    [openWizard]
  );
  const onPlayWorld = useCallback((worldId) => openWizard({ worldId }), [openWizard]);
  const onPlayStory = useCallback((storyId) => openWizard({ storyId }), [openWizard]);
  const onPlayCharacter = useCallback((characterId) => openWizard({ characterId }), [openWizard]);

  const onCreateWorld = useCallback(
    async (draft) => {
      if (busy || chatActionBusy) return;
      const payload = {
        title: textValue(draft?.title) || appText("defaults.newWorld"),
        genre: textValue(draft?.genre) || "fantasy",
        tone: textValue(draft?.tone) || "tense",
        world_size: textValue(draft?.worldSize),
        population: textValue(draft?.population),
        public_premise: textValue(draft?.publicPremise),
        world_lore: draft?.worldLore && typeof draft.worldLore === "object" ? draft.worldLore : null,
        status: "ready",
      };
      setChatActionBusy(true);
      setStatus(appText("status.savingWorld"));
      try {
        const data = selectedWorldId
          ? await api.updateWorld(selectedWorldId, payload)
          : await api.createWorld(payload);
        if (!data.ok) {
          notifyApiError(data, appText("errors.worldCreate"));
          return null;
        }
        if (Array.isArray(data.worlds)) setWorlds(data.worlds);
        else await refreshWorlds();
        if (data.world?.id) setSelectedWorldId(data.world.id);
        setMainView("world-studio");
        // Return the persisted world so the editor can adopt the server-rewritten
        // image URLs (/world-assets/...) instead of keeping volatile sidecar URLs.
        return data.world || null;
      } catch (e) {
        notify(userErrorText(e, appText("errors.worldCreate")));
        return null;
      } finally {
        setChatActionBusy(false);
        setStatus("");
      }
    },
    [busy, chatActionBusy, selectedWorldId, refreshWorlds, notify, notifyApiError]
  );

  const onWorldArchitectStream = useCallback(
    async (body, onEvent) => {
      await streamArchitect(
        { ...body, ...(selectedWorldId ? { world_id: selectedWorldId } : {}) },
        (ev) => {
          if (ev.kind === "architect_done") {
            const data = ev.data || {};
            if (Array.isArray(data.worlds)) setWorlds(data.worlds);
            if (data.world?.id) setSelectedWorldId(data.world.id);
          }
          onEvent(ev);
        }
      );
    },
    [selectedWorldId]
  );

  // Story-architect turn (§С1.3): fold the persisted stories list into state and
  // pin the selection so a follow-up turn edits the same story. `stories` are the
  // MINIMAL catalog rows (kind/world_ref, no seed/architect_*).
  const onStoryArchitectStream = useCallback(
    async (body, onEvent) => {
      await streamStoryArchitect(body, (ev) => {
        if (ev.kind === "architect_done") {
          const data = ev.data || {};
          if (Array.isArray(data.stories)) {
            setStories(normalizeStories({ stories: data.stories }));
          }
          const newId = data.story_id == null ? "" : String(data.story_id).trim();
          if (newId) setSelectedStoryArchitectId(newId);
        }
        onEvent(ev);
      });
    },
    []
  );

  // Character-architect turn (§Студия персонажа): fold the persisted characters
  // list into state and pin the id so a follow-up turn edits the same package.
  const onCharacterArchitectStream = useCallback(
    async (body, onEvent) => {
      await streamCharacterArchitect(body, (ev) => {
        if (ev.kind === "architect_done") {
          const data = ev.data || {};
          if (Array.isArray(data.characters)) setCharacters(data.characters);
          const newId = data.character_id == null ? "" : String(data.character_id).trim();
          if (newId) setSelectedCharacterArchitectId(newId);
        }
        onEvent(ev);
      });
    },
    []
  );

  // Re-attach wrappers: architect turns run detached on the server, so a
  // reopened panel joins the live feed. The replayed `architect_done` carries
  // the same payload as a live one — apply the identical catalog/selection
  // side effects before handing it to the panel.
  const onWorldArchitectAttach = useCallback(
    (onEvent) =>
      attachArchitect("world", selectedWorldId, (ev) => {
        if (ev.kind === "architect_done") {
          const data = ev.data || {};
          if (Array.isArray(data.worlds)) setWorlds(data.worlds);
          if (data.world?.id) setSelectedWorldId(data.world.id);
        }
        onEvent(ev);
      }),
    [selectedWorldId]
  );

  const onStoryArchitectAttach = useCallback(
    (onEvent) =>
      attachArchitect("story", selectedStoryArchitectId, (ev) => {
        if (ev.kind === "architect_done") {
          const data = ev.data || {};
          if (Array.isArray(data.stories)) {
            setStories(normalizeStories({ stories: data.stories }));
          }
          const newId = data.story_id == null ? "" : String(data.story_id).trim();
          if (newId) setSelectedStoryArchitectId(newId);
        }
        onEvent(ev);
      }),
    [selectedStoryArchitectId]
  );

  const onCharacterArchitectAttach = useCallback(
    (onEvent) =>
      attachArchitect("character", selectedCharacterArchitectId, (ev) => {
        if (ev.kind === "architect_done") {
          const data = ev.data || {};
          if (Array.isArray(data.characters)) setCharacters(data.characters);
          const newId = data.character_id == null ? "" : String(data.character_id).trim();
          if (newId) setSelectedCharacterArchitectId(newId);
        }
        onEvent(ev);
      }),
    [selectedCharacterArchitectId]
  );

  // Direct manual save from the character studio (POST /characters or
  // /characters/{id}/draft, done inside the panel). Pin the returned id WITHOUT
  // bumping `characterStudioEpoch` so the live panel is not remounted, and
  // refresh the catalog so the card's title/preview/version follow.
  const onCharacterPersisted = useCallback(
    (character) => {
      const c = character || {};
      const id = c.id == null ? "" : String(c.id).trim();
      if (id) setSelectedCharacterArchitectId(id);
      loadCharacters();
      const title = (typeof c.title === "string" && c.title.trim()) || appText("defaults.character");
      const version = c.version == null ? "?" : c.version;
      pushToast({
        kind: "success",
        message: appText("notices.sheetSaved", { title, version }),
      });
    },
    [loadCharacters, pushToast]
  );

  const onGenerateImage = useCallback((body) => api.generateImage(body), []);

  const onActivateChat = useCallback(
    async (chatId) => {
      if (!chatId || busy || chatActionBusy) return;
      if (sameChatId(chatId, activeChatId)) {
        setMainView("chat");
        closeChatsOnMobile();
        return;
      }
      setChatActionBusy(true);
      setStatus(appText("status.openingGame"));
      try {
        const data = await api.activateChat(chatId);
        if (!data.ok) {
          notifyApiError(data, appText("errors.gameOpen"));
          return;
        }
        restoreChatSession(data);
        setMainView("chat");
        await refreshChats();
        closeChatsOnMobile();
      } catch (e) {
      notify(userErrorText(e, appText("errors.gameOpen")));
      } finally {
        setChatActionBusy(false);
        setStatus("");
      }
      // The opened chat may have a turn still running server-side. Scheduled
      // after the busy flags clear (macrotask, so React has re-rendered) —
      // the fresh closure would otherwise see chatActionBusy=true and skip.
      window.setTimeout(() => {
        void resumeActiveTurnRef.current(chatId);
      }, 0);
    },
    [activeChatId, busy, chatActionBusy, restoreChatSession, refreshChats, closeChatsOnMobile, notify, notifyApiError]
  );

  const onGlobalSearchSelect = useCallback(
    (item) => {
      const type = textValue(item?.type);
      const id = textValue(item?.id);
      if (!type || !id) return;
      setGlobalSearchOpen(false);
      if (type === "chat") {
        onActivateChat(id);
        return;
      }
      const source = type === "world" ? worlds : type === "story" ? stories : characters;
      const entity = (Array.isArray(source) ? source : []).find((entry) => sameChatId(entry?.id, id));
      if (entity) onOpenStudio(entity, type);
      else showLibrary();
    },
    [onActivateChat, worlds, stories, characters, onOpenStudio, showLibrary]
  );

  const onDeleteChat = useCallback(
    async (chatId) => {
      if (!chatId) return;
      const wasActive = sameChatId(chatId, activeChatId);
      try {
        const data = await api.deleteChat(chatId);
        if (!data.ok) {
          notifyApiError(data, appText("errors.gameDelete"));
          return;
        }
        // If the open game was deleted, switch to the session the server returned
        // (it creates a fresh chat when none remain).
        if (wasActive && data.chat && data.state && data.transcript) {
          restoreChatSession(data);
        }
        await refreshChats();
      } catch (e) {
      notify(userErrorText(e, appText("errors.gameDelete")));
      }
    },
    [activeChatId, restoreChatSession, refreshChats, notify, notifyApiError]
  );

  const onDeleteWorld = useCallback(
    async (worldId) => {
      if (!worldId) return;
      try {
        const data = await api.deleteWorld(worldId);
        if (!data.ok) {
          notifyApiError(data, appText("errors.worldDelete"));
          return;
        }
        if (sameChatId(worldId, selectedWorldId)) setSelectedWorldId("");
        if (Array.isArray(data.worlds)) setWorlds(data.worlds);
        else await refreshWorlds();
      } catch (e) {
      notify(userErrorText(e, appText("errors.worldDelete")));
      }
    },
    [refreshWorlds, selectedWorldId, notify, notifyApiError]
  );

  const onDeleteStory = useCallback(
    async (storyId) => {
      if (!storyId) return;
      try {
        const data = await api.deleteStory(storyId);
        if (!data.ok) {
          notifyApiError(data, appText("errors.storyDelete"));
          return;
        }
        await loadStories();
      } catch (e) {
      notify(userErrorText(e, appText("errors.storyDelete")));
      }
    },
    [loadStories, notify, notifyApiError]
  );

  // Phase 5: download a world/story/character package zip via a fetch blob.
  const onExportWorld = useCallback(
    async (worldId) => {
      if (!worldId) return;
      try {
        await api.downloadExport(api.exportWorldUrl(worldId), `${worldId}.gmworld.zip`);
      } catch (e) {
      notify(userErrorText(e, appText("errors.exportFailed")));
      }
    },
    [notify]
  );

  const onExportStory = useCallback(
    async (storyId, bake) => {
      if (!storyId) return;
      try {
        await api.downloadExport(api.exportStoryUrl(storyId, !!bake), `${storyId}.gmstory.zip`);
      } catch (e) {
      notify(userErrorText(e, appText("errors.exportFailed")));
      }
    },
    [notify]
  );

  const onExportCharacter = useCallback(
    async (characterId) => {
      if (!characterId) return;
      try {
        await api.downloadExport(api.exportCharacterUrl(characterId), `${characterId}.gmchar.zip`);
      } catch (e) {
      notify(userErrorText(e, appText("errors.exportFailed")));
      }
    },
    [notify]
  );

  // §К1.5: rename a character via a metadata patch (v1 = native prompt).
  const onRenameCharacter = useCallback(
    async (characterId, currentTitle) => {
      if (!characterId || typeof window === "undefined") return;
      const next = window.prompt(appText("prompts.characterName"), currentTitle || "");
      if (next == null) return; // cancelled
      const title = next.trim();
      if (!title || title === (currentTitle || "").trim()) return;
      try {
        const data = await api.updateCharacter(characterId, { title });
        if (!data.ok) {
          notifyApiError(data, appText("errors.characterRename"));
          return;
        }
        await loadCharacters();
      } catch (e) {
      notify(userErrorText(e, appText("errors.characterRename")));
      }
    },
    [loadCharacters, notify, notifyApiError]
  );

  const onDeleteCharacter = useCallback(
    async (characterId) => {
      if (!characterId) return;
      try {
        const data = await api.deleteCharacter(characterId);
        if (!data.ok) {
          notifyApiError(data, appText("errors.characterDelete"));
          return;
        }
        await loadCharacters();
      } catch (e) {
      notify(userErrorText(e, appText("errors.characterDelete")));
      }
    },
    [loadCharacters, notify, notifyApiError]
  );

  // §К1.5: export the active game's hero snapshot into the library. `characterId`
  // -> snapshot the existing package (+version bump); omitted -> create a new one.
  const onSaveCharacter = useCallback(
    async (characterId) => {
      if (!activeChatId) {
        notify(appText("errors.noActiveGame"));
        return;
      }
      try {
        const body = characterId ? { character_id: characterId } : {};
        const data = await api.saveCharacterFromChat(activeChatId, body);
        if (!data.ok) {
          notifyApiError(data, appText("errors.characterSave"));
          return;
        }
        await loadCharacters();
        const c = data.character || {};
        const title = (typeof c.title === "string" && c.title.trim()) || appText("defaults.character");
        const version = c.version == null ? "?" : c.version;
        pushToast({
          kind: "success",
          message: appText("notices.characterSaved", { title, version }),
        });
      } catch (e) {
      notify(userErrorText(e, appText("errors.characterSave")));
      }
    },
    [activeChatId, loadCharacters, pushToast, notify, notifyApiError]
  );

  // Story studio: save the story draft's seed protagonist as a portable .gmchar.
  const onSaveProtagonist = useCallback(
    async (storyId) => {
      if (!storyId) return;
      try {
        const data = await api.saveProtagonist(storyId);
        if (!data.ok) throw new Error(data.error || appText("errors.protagonistSave"));
        await loadCharacters();
        const c = data.character || {};
        const title = (typeof c.title === "string" && c.title.trim()) || appText("defaults.character");
        pushToast({
          kind: "success",
          message: appText("notices.protagonistSaved", { title }),
        });
      } catch (e) {
      notify(userErrorText(e, appText("errors.protagonistSave")));
      }
    },
    [loadCharacters, pushToast, notify]
  );

  const onRevealLibrary = useCallback(async () => {
    try {
      const data = await api.revealLibrary();
      if (!data.ok) throw new Error(data.error || appText("errors.libraryReveal"));
    } catch (e) {
      notify(userErrorText(e, appText("errors.libraryReveal")));
    }
  }, [notify]);

  // Import a picked .zip package, then refresh worlds + stories + characters.
  // Backend errors (collision 409, malformed) propagate so LibraryScreen shows them.
  const onImportPackage = useCallback(
    async (file, overwrite) => {
      const data = await api.importPackage(file, overwrite);
      await Promise.all([refreshWorlds(), loadStories(), loadCharacters()]);
      return data;
    },
    [refreshWorlds, loadStories, loadCharacters]
  );

  // Library card handlers (kind-dispatch over the raw entity).
  const onLibraryExport = useCallback(
    (entity, kind, opts = {}) => {
      const id = entity?.id == null ? "" : String(entity.id);
      if (kind === "world") onExportWorld(id);
      else if (kind === "story") onExportStory(id, !!opts.bake);
      else if (kind === "character") onExportCharacter(id);
    },
    [onExportWorld, onExportStory, onExportCharacter]
  );
  const onLibraryDelete = useCallback(
    (entity, kind) => {
      const id = entity?.id == null ? "" : String(entity.id);
      if (kind === "world") return onDeleteWorld(id);
      if (kind === "story") return onDeleteStory(id);
      if (kind === "character") return onDeleteCharacter(id);
      return undefined;
    },
    [onDeleteWorld, onDeleteStory, onDeleteCharacter]
  );
  const onLibraryRename = useCallback(
    (entity, kind) => {
      if (kind !== "character") return;
      onRenameCharacter(entity?.id, textValue(entity?.title));
    },
    [onRenameCharacter]
  );

  const storyArchitectWorld = useMemo(
    () => (Array.isArray(worlds) ? worlds : []).find((world) => sameChatId(world.id, storyArchitectWorldId)) || null,
    [worlds, storyArchitectWorldId]
  );
  const storyArchitectStory = useMemo(
    () =>
      selectedStoryArchitectId
        ? (Array.isArray(stories) ? stories : []).find((s) => s.id === selectedStoryArchitectId) || null
        : null,
    [stories, selectedStoryArchitectId]
  );
  const characterArchitectCharacter = useMemo(
    () =>
      selectedCharacterArchitectId
        ? (Array.isArray(characters) ? characters : []).find((c) =>
            sameChatId(c.id, selectedCharacterArchitectId)
          ) || null
        : null,
    [characters, selectedCharacterArchitectId]
  );
  // The character studio's base world/story: a FRESH draft uses the picker's
  // choice (characterStudioBase); an EXISTING character shows the refs pinned in
  // its package. Titles resolve against the loaded lists; a deleted base keeps
  // its raw id as the label (refs may dangle by design).
  const characterStudioRefs = useMemo(() => {
    const source = characterArchitectCharacter
      ? {
          worldId: textValue(characterArchitectCharacter?.world_ref?.id),
          storyId: textValue(characterArchitectCharacter?.story_ref?.id),
        }
      : {
          worldId: textValue(characterStudioBase?.worldId),
          storyId: textValue(characterStudioBase?.storyId),
        };
    const world = source.worldId
      ? (Array.isArray(worlds) ? worlds : []).find((w) => sameChatId(w.id, source.worldId))
      : null;
    const story = source.storyId
      ? (Array.isArray(stories) ? stories : []).find((s) => sameChatId(s.id, source.storyId))
      : null;
    // A ref whose package is gone keeps its id (the binding stays) but gets NO
    // title — the panel renders an honest «пакет недоступен» instead of leaking
    // a machine id into player-facing prose.
    return {
      worldId: source.worldId,
      storyId: source.storyId,
      worldTitle: world ? textValue(world.title) || textValue(world.world_lore?.name) : "",
      storyTitle: story ? textValue(story.title) : "",
      worldMissing: !!source.worldId && !world,
      storyMissing: !!source.storyId && !story,
    };
  }, [characterArchitectCharacter, characterStudioBase, worlds, stories]);

  // The context bar / scene panel read the launched game's story + world.
  const contextProcedural = !srv.storyId || srv.storyId === "procedural";
  const contextStory = useMemo(() => {
    if (contextProcedural) return null;
    const found = (Array.isArray(stories) ? stories : []).find((s) => sameChatId(s.id, srv.storyId));
    if (found) return found;
    return srv.storyTitle
      ? { title: srv.storyTitle, story_brief: textValue(srv.storyBrief?.text) }
      : null;
  }, [contextProcedural, stories, srv.storyId, srv.storyTitle, srv.storyBrief]);
  const contextWorld = useMemo(() => {
    const wid = srv.worldRef?.id;
    return wid ? (Array.isArray(worlds) ? worlds : []).find((w) => sameChatId(w.id, wid)) || null : null;
  }, [worlds, srv.worldRef]);

  const send = useCallback(
    (rawText) => {
      const text = textValue(rawText);
      if (!text || turnInFlightRef.current || busy || chatActionBusy) return;
      if (text.startsWith("/")) sendCommand(text);
      else sendTurn(text);
    },
    [busy, chatActionBusy, sendCommand, sendTurn]
  );

  const retryableTurn = useMemo(() => {
    if (failedTurn && sameChatId(failedTurn.chatId, activeChatId)) {
      const latestError = [...messages].reverse().find((message) => message?.type === "error");
      return { ...failedTurn, errorId: latestError?.id };
    }
    return historicalFailedTurn(messages, activeChatId);
  }, [activeChatId, failedTurn, messages]);

  const retryFailedTurn = useCallback(() => {
    if (!retryableTurn) return;
    if (retryableTurn.history) {
      void regenerateFromTurn(
        retryableTurn.history.kind,
        retryableTurn.history.turn,
        retryableTurn.text,
        retryableTurn.requestId
      );
      return;
    }
    void sendTurn(retryableTurn.text, retryableTurn.requestId, {
      legacyResume: retryableTurn.legacyResume === true,
      history: retryableTurn.history || null,
      chatId: retryableTurn.chatId,
      historyMutation: Boolean(retryableTurn.history),
      attach: retryableTurn.attach === true,
    });
  }, [regenerateFromTurn, retryableTurn, sendTurn]);

  const onModelChange = useCallback(
    async (model) => {
      if (!model) return;
      try {
        const data = await api.setModel(model);
        if (!data.ok) throw new Error(data.error || appText("errors.modelSwitch"));
        setStateFromServer(data.state);
      } catch (e) {
      notify(userErrorText(e, appText("errors.modelSwitch")));
      }
    },
    [setStateFromServer, notify]
  );

  const onSettingsChange = useCallback(
    async (patch) => {
      const next = { ...settings, ...patch };
      setSettings(next);
      try {
        const data = await api.updateSettings(next);
        if (!data.ok) throw new Error(data.error || appText("errors.settingsSave"));
        if (data.settings) setSettings((prev) => ({ ...prev, ...data.settings }));
        if (data.settings_options) {
          setSettingsOptions((prev) => ({ ...prev, ...data.settings_options }));
        }
        if (data.state) setStateFromServer(data.state);
      } catch (e) {
      notify(userErrorText(e, appText("errors.settingsSave")));
      }
    },
    [settings, setStateFromServer, notify]
  );

  const updateConnectorAuth = useCallback((connectorId, auth) => {
    if (!connectorId || !auth) return;
    setConnectors((current) => current.map((connector) => (
      connectorIdOf(connector) === connectorId ? { ...connector, auth } : connector
    )));
  }, []);

  const setConnectorAuthBusy = useCallback((connectorId, busy) => {
    setConnectorAuthBusyIds((current) => {
      if (busy) return current.includes(connectorId) ? current : [...current, connectorId];
      return current.filter((id) => id !== connectorId);
    });
  }, []);

  const setConnectorAuthCancelling = useCallback((connectorId, cancelling) => {
    setConnectorAuthCancellingIds((current) => {
      if (cancelling) return current.includes(connectorId) ? current : [...current, connectorId];
      return current.filter((id) => id !== connectorId);
    });
  }, []);

  const setConnectorAuthPromptFor = useCallback((connectorId, prompt) => {
    setConnectorAuthPrompts((current) => {
      if (prompt) return { ...current, [connectorId]: prompt };
      if (!(connectorId in current)) return current;
      const next = { ...current };
      delete next[connectorId];
      return next;
    });
  }, []);

  const finishConnectorAuthOperation = useCallback((connectorId, operation) => {
    if (connectorAuthOperationsRef.current.get(connectorId) !== operation) return false;
    if (operation.timeoutId) window.clearTimeout(operation.timeoutId);
    if (operation.cancelTimeoutId) window.clearTimeout(operation.cancelTimeoutId);
    connectorAuthOperationsRef.current.delete(connectorId);
    setConnectorAuthBusy(connectorId, false);
    setConnectorAuthCancelling(connectorId, false);
    setConnectorAuthPromptFor(connectorId, null);
    return true;
  }, [setConnectorAuthBusy, setConnectorAuthCancelling, setConnectorAuthPromptFor]);

  const waitForConnectorAuth = useCallback(async (connectorId, start, initialAuth, operation) => {
    const deadline = Date.now() + connectorAuthTimeout(start);
    const interval = connectorAuthPollInterval(start);
    let auth = initialAuth || {};
    let lastStatusError = null;
    let consecutiveStatusFailures = 0;

    while (!operation.controller.signal.aborted) {
      const authState = connectorAuthState(auth);
      if (authState === "signed_in" || authState === "not_required") return auth;
      if (authState === "expired") {
        throw new Error(userErrorText(auth, appText("errors.authExpired")));
      }
      if (Date.now() >= deadline) {
        throw lastStatusError || new Error(appText("errors.authExpired"));
      }

      await waitForAbortable(
        Math.min(
          Math.min(10_000, interval * Math.max(1, consecutiveStatusFailures + 1)),
          Math.max(0, deadline - Date.now())
        ),
        operation.controller.signal
      );
      try {
        const data = await api.connectorAuthStatus(connectorId, {
          signal: operation.controller.signal,
        });
        if (!data.ok) throw new Error(data.error || appText("errors.authStatusUnavailable"));
        auth = data.auth || {};
        lastStatusError = null;
        consecutiveStatusFailures = 0;
        updateConnectorAuth(connectorId, auth);
      } catch (error) {
        if (isAbortError(error)) throw error;
        lastStatusError = error;
        consecutiveStatusFailures += 1;
      }
    }
    const error = new Error("Operation cancelled");
    error.name = "AbortError";
    throw error;
  }, [updateConnectorAuth]);

  const onConnectorAuthStart = useCallback(async (connectorId, methodId) => {
    if (!connectorId || !methodId || connectorAuthOperationsRef.current.has(connectorId)) return;
    const connector = connectorById(connectors, connectorId);
    const name = connectorName(connector);
    const operation = {
      kind: "login",
      controller: new AbortController(),
      cancelController: null,
      cancelRequested: false,
      disposed: false,
      timedOut: false,
      timeoutId: null,
      cancelTimeoutId: null,
    };
    connectorAuthOperationsRef.current.set(connectorId, operation);
    setConnectorAuthBusy(connectorId, true);
    try {
      const data = await api.connectorAuthStart(connectorId, methodId, {
        signal: operation.controller.signal,
      });
      if (!data.ok) throw new Error(data.error || appText("errors.connectorConnect", { name }));
      updateConnectorAuth(connectorId, data.auth);

      const start = data.start || { kind: "complete" };
      const authUrl = connectorAuthUrl(start);
      if (start.kind === "device_code" || start.kind === "browser") {
        setConnectorAuthPromptFor(connectorId, { ...start, connector_id: connectorId });
        if (authUrl) window.open(authUrl, "_blank", "noopener,noreferrer");
      }
      operation.timeoutId = window.setTimeout(() => {
        operation.timedOut = true;
        operation.controller.abort();
      }, connectorAuthTimeout(start));

      const auth = await waitForConnectorAuth(connectorId, start, data.auth, operation);
      if (!auth || operation.cancelRequested) return;
      void loadConnectorModels(connectorId, { force: true });
    } catch (e) {
      if (operation.cancelRequested || operation.disposed) return;
      if (operation.timedOut) notify(appText("errors.authExpired"));
      else if (!isAbortError(e)) {
      notify(userErrorText(e, appText("errors.connectorConnect", { name })));
      }
    } finally {
      if (!operation.cancelRequested && finishConnectorAuthOperation(connectorId, operation)) {
        await loadConnectors();
      }
    }
  }, [connectors, finishConnectorAuthOperation, loadConnectors, loadConnectorModels, notify, setConnectorAuthBusy, setConnectorAuthPromptFor, updateConnectorAuth, waitForConnectorAuth]);

  const onConnectorAuthCancel = useCallback(async (connectorId) => {
    const operation = connectorAuthOperationsRef.current.get(connectorId);
    if (!operation || operation.kind !== "login" || operation.cancelRequested) return;
    const connector = connectorById(connectors, connectorId);
    const name = connectorName(connector);
    operation.cancelRequested = true;
    setConnectorAuthCancelling(connectorId, true);
    setConnectorAuthPromptFor(connectorId, null);
    if (operation.timeoutId) window.clearTimeout(operation.timeoutId);
    operation.controller.abort();

    const cancelController = new AbortController();
    operation.cancelController = cancelController;
    operation.cancelTimeoutId = window.setTimeout(
      () => cancelController.abort(),
      CONNECTOR_AUTH_CANCEL_TIMEOUT_MS
    );
    try {
      const data = await api.connectorAuthLogout(connectorId, {
        signal: cancelController.signal,
      });
      if (!data.ok) {
        throw new Error(data.error || appText("errors.connectorCancel", { name }));
      }
      updateConnectorAuth(connectorId, { state: "signed_out" });
      void loadConnectorModels(connectorId, { force: true });
    } catch (error) {
      if (operation.disposed) return;
      const message = isAbortError(error)
        ? appText("errors.connectorCancelNotConfirmed", { name })
          : userErrorText(error, appText("errors.connectorCancel", { name }));
      notify(message);
    } finally {
      if (finishConnectorAuthOperation(connectorId, operation)) await loadConnectors();
    }
  }, [connectors, finishConnectorAuthOperation, loadConnectors, loadConnectorModels, notify, setConnectorAuthCancelling, setConnectorAuthPromptFor, updateConnectorAuth]);

  const onConnectorLogout = useCallback(async (connectorId) => {
    if (!connectorId || connectorAuthOperationsRef.current.has(connectorId)) return;
    const connector = connectorById(connectors, connectorId);
    const name = connectorName(connector);
    const operation = {
      kind: "logout",
      controller: new AbortController(),
      disposed: false,
      timeoutId: null,
      cancelTimeoutId: null,
    };
    connectorAuthOperationsRef.current.set(connectorId, operation);
    setConnectorAuthBusy(connectorId, true);
    operation.timeoutId = window.setTimeout(
      () => operation.controller.abort(),
      CONNECTOR_AUTH_CANCEL_TIMEOUT_MS
    );
    try {
      const data = await api.connectorAuthLogout(connectorId, {
        signal: operation.controller.signal,
      });
      if (!data.ok) {
        throw new Error(data.error || appText("errors.connectorDisconnect", { name }));
      }
      updateConnectorAuth(connectorId, { state: "signed_out" });
      void loadConnectorModels(connectorId, { force: true });
    } catch (e) {
      if (operation.disposed) return;
      const message = isAbortError(e)
        ? appText("errors.connectorDisconnectNotConfirmed", { name })
          : userErrorText(e, appText("errors.connectorDisconnect", { name }));
      notify(message);
    } finally {
      if (finishConnectorAuthOperation(connectorId, operation)) await loadConnectors();
    }
  }, [connectors, finishConnectorAuthOperation, loadConnectors, loadConnectorModels, notify, setConnectorAuthBusy, updateConnectorAuth]);

  // «Сброс партии» from the game-context ⋯ menu (its own confirm dialog).
  const onReset = useCallback(async () => {
    try {
      const data = await api.command("reset");
      if (!data.ok) {
        notifyApiError(data, appText("errors.gameReset"));
        return;
      }
      store.clear();
      setFailedTurn(null);
      setPlayerOptions(null);
      setStateFromServer(data.state);
      await refreshChats();
    } catch (e) {
      notify(userErrorText(e, appText("errors.gameReset")));
    }
  }, [store, setStateFromServer, refreshChats, notify, notifyApiError]);

  const onExportJson = useCallback(() => api.export(), []);
  const openLocationMap = useCallback(() => {
    setLocationTransition(null);
    setLocationMapOpen(true);
  }, []);
  const requestMappedLocationTravel = useCallback((node) => {
    const intent = locationTravelIntent(node, (destinationReference) => i18n.t(
      "locationMap.travelIntent",
      { ns: "game", destination: destinationReference }
    ));
    if (!intent || turnInFlightRef.current || busy || chatActionBusy) return;
    setLocationMapOpen(false);
    setMainView("chat");
    void sendTurn(intent);
  }, [busy, chatActionBusy, sendTurn]);

  const currentModel = useMemo(
    () => modelsForConnector(models, srv.modelBinding.connector_id)
      .find((model) => modelIdOf(model) === srv.modelBinding.model_id) || null,
    [models, srv.modelBinding]
  );
  const interactionBusy = busy || chatActionBusy;
  const isGame = mainView === "chat";
  const locationMapAvailable = hasLocationGraph(srv.locationGraph);
  const speechToTextEnabled = Boolean(
    connectorById(connectors, srv.modelBinding.connector_id)?.capabilities?.includes(
      "speech_to_text"
    )
  );

  const sceneBackgroundUrl = interfaceSettings.sceneBackground && mainView === "chat"
    ? textValue(srv.scene?.image_url)
    : "";

  return (
    <VisibilityContext.Provider value={visibility}>
    <div className="app">
      {sceneBackgroundUrl && (
        <div className="scene-background" aria-hidden="true">
          <img src={sceneBackgroundUrl} alt="" decoding="async" />
        </div>
      )}
      <Header
        onToggleChats={toggleChats}
        chatsOpen={chatsOpen}
        mainView={mainView}
        onNavGame={showGame}
        onNavLibrary={showLibrary}
        onNavImage={showImage}
        onOpenSearch={openGlobalSearch}
        imageLabEnabled={imageLabEnabled}
        srv={srv}
        sidecarStatus={sidecarStatus}
        connectors={connectors}
        models={models}
        connectorModelsLoadingIds={connectorModelsLoadingIds}
        onEnsureConnectorModels={loadConnectorModels}
        modelBinding={srv.modelBinding}
        settings={settings}
        settingsOptions={settingsOptions}
        onModelChange={onModelChange}
        onSettingsChange={onSettingsChange}
        connectorAuthBusyIds={connectorAuthBusyIds}
        connectorAuthCancellingIds={connectorAuthCancellingIds}
        connectorAuthPrompts={connectorAuthPrompts}
        onConnectorAuthStart={onConnectorAuthStart}
        onConnectorAuthCancel={onConnectorAuthCancel}
        onConnectorLogout={onConnectorLogout}
      />
      <GlobalSearchPalette
        open={globalSearchOpen}
        onOpen={openGlobalSearch}
        onClose={closeGlobalSearch}
        onSelect={onGlobalSearchSelect}
      />
      <div className={"app-body" + (debugOpen ? " debug-open" : "") + (chatsOpen ? "" : " chats-collapsed")}>
        <ChatHistorySidebar
          chats={chats}
          activeChatId={activeChatId}
          open={chatsOpen}
          busy={interactionBusy}
          loading={chatsLoading}
          error={chatsError}
          onClose={closeChats}
          onNewGame={() => openWizard(null)}
          onActivate={onActivateChat}
          onDelete={onDeleteChat}
          mainView={mainView}
          onNavGame={showGame}
          onNavLibrary={showLibrary}
          onNavImage={showImage}
          imageLabEnabled={imageLabEnabled}
        />
        {/* дровер смонтирован во всех вьюхах: на мобилке он носит навигацию
            разделов; на десктопе вне игры прячется CSS'ом (.sidebar-off-game) */}
        {mainView === "library" ? (
          <main className="world-creation-pane">
            <LibraryScreen
              worlds={worlds}
              stories={stories}
              characters={characters}
              busy={interactionBusy}
              worldsLoading={worldsLoading}
              worldsError={worldsError}
              storiesLoading={storiesLoading}
              storiesError={storiesError}
              charactersLoading={charactersLoading}
              charactersError={charactersError}
              onPlay={onPlayEntity}
              onOpenStudio={onOpenStudio}
              onExport={onLibraryExport}
              onDelete={onLibraryDelete}
              onRename={onLibraryRename}
              onCreate={onCreateEntity}
              onImport={onImportPackage}
              onReveal={onRevealLibrary}
            />
          </main>
        ) : mainView === "world-studio" ? (
          <main className="world-creation-pane">
            <div className="studio-frame">
              <button type="button" className="studio-back" onClick={showLibrary}>
                {t("nav.backToLibrary")}
              </button>
              <WorldArchitectPanel
                world={selectedWorld}
                responseLanguage={settings.response_language}
                locked={interactionBusy}
                connectors={connectors}
                models={models}
                connectorModelsLoadingIds={connectorModelsLoadingIds}
                onEnsureConnectorModels={loadConnectorModels}
                initialModelBinding={srv.modelBinding}
                connectorAuthBusyIds={connectorAuthBusyIds}
                connectorAuthCancellingIds={connectorAuthCancellingIds}
                connectorAuthPrompts={connectorAuthPrompts}
                onConnectorAuthStart={onConnectorAuthStart}
                onConnectorAuthCancel={onConnectorAuthCancel}
                onCreateWorld={onCreateWorld}
                onArchitectStream={onWorldArchitectStream}
                onArchitectAttach={onWorldArchitectAttach}
                onGenerateImage={onGenerateImage}
                onPlayWorld={onPlayWorld}
                onCreateStory={onCreateStory}
              />
            </div>
          </main>
        ) : mainView === "story-studio" ? (
          <main className="world-creation-pane">
            <div className="studio-frame">
              <button type="button" className="studio-back" onClick={showLibrary}>
                {t("nav.backToLibrary")}
              </button>
              <StoryArchitectPanel
                key={storyArchitectWorldId || "new-story"}
                story={storyArchitectStory}
                responseLanguage={settings.response_language}
                worldId={storyArchitectWorldId}
                worldTitle={
                  textValue(storyArchitectWorld?.title) ||
                  textValue(storyArchitectWorld?.world_lore?.name)
                }
                locked={interactionBusy}
                connectors={connectors}
                models={models}
                connectorModelsLoadingIds={connectorModelsLoadingIds}
                onEnsureConnectorModels={loadConnectorModels}
                initialModelBinding={srv.modelBinding}
                connectorAuthBusyIds={connectorAuthBusyIds}
                connectorAuthCancellingIds={connectorAuthCancellingIds}
                connectorAuthPrompts={connectorAuthPrompts}
                onConnectorAuthStart={onConnectorAuthStart}
                onConnectorAuthCancel={onConnectorAuthCancel}
                onArchitectStream={onStoryArchitectStream}
                onArchitectAttach={onStoryArchitectAttach}
                onPlayStory={onPlayStory}
                onSaveProtagonist={onSaveProtagonist}
              />
            </div>
          </main>
        ) : mainView === "character-studio" ? (
          <main className="world-creation-pane">
            <div className="studio-frame">
              <button type="button" className="studio-back" onClick={showLibrary}>
                {t("nav.backToLibrary")}
              </button>
              <CharacterArchitectPanel
                key={`char-studio-${characterStudioEpoch}`}
                character={characterArchitectCharacter}
                responseLanguage={settings.response_language}
                worldId={characterStudioRefs.worldId}
                storyId={characterStudioRefs.storyId}
                worldTitle={characterStudioRefs.worldTitle}
                storyTitle={characterStudioRefs.storyTitle}
                worldMissing={characterStudioRefs.worldMissing}
                storyMissing={characterStudioRefs.storyMissing}
                locked={interactionBusy}
                connectors={connectors}
                models={models}
                connectorModelsLoadingIds={connectorModelsLoadingIds}
                onEnsureConnectorModels={loadConnectorModels}
                initialModelBinding={srv.modelBinding}
                connectorAuthBusyIds={connectorAuthBusyIds}
                connectorAuthCancellingIds={connectorAuthCancellingIds}
                connectorAuthPrompts={connectorAuthPrompts}
                onConnectorAuthStart={onConnectorAuthStart}
                onConnectorAuthCancel={onConnectorAuthCancel}
                onArchitectStream={onCharacterArchitectStream}
                onArchitectAttach={onCharacterArchitectAttach}
                onPlayCharacter={onPlayCharacter}
                onCharacterPersisted={onCharacterPersisted}
                notify={notify}
              />
            </div>
          </main>
        ) : mainView === "image" && imageLabEnabled ? (
          <main className="image-lab-pane">
            <ImageLabPanel
              locked={interactionBusy}
              sidecarStatus={sidecarStatus}
              onGenerateImage={onGenerateImage}
            />
          </main>
        ) : (
          <main className={"chat-pane" + (playerOptions ? " has-options" : "")}>
            <GameContextBar
              story={contextStory}
              world={contextWorld}
              procedural={contextProcedural}
              playerCharacter={srv.playerCharacter}
              scene={srv.scene}
              npcs={srv.npcs}
              statusLabels={srv.statusLabels}
              mapAvailable={locationMapAvailable}
              onOpenMap={openLocationMap}
              onExportJson={onExportJson}
              onReset={onReset}
              locked={interactionBusy}
            />
            <div className="chat-pane-body">
              <ScenePanel
                time={srv.time}
                scene={srv.scene}
                playerCharacter={srv.playerCharacter}
                npcs={srv.npcs}
                statusLabels={srv.statusLabels}
                charRef={srv.charRef}
                characters={characters}
                canSaveCharacter={!!activeChatId && !!srv.playerCharacter}
                onSaveCharacter={onSaveCharacter}
              />
              <Chat
                key={activeChatId || "active-chat"}
                messages={visibleMessages}
                storyBrief={srv.storyBrief}
                scene={srv.scene}
                npcs={srv.npcs}
                entities={srv.entities}
                statusLabels={srv.statusLabels}
                onRetry={retryableTurn ? retryFailedTurn : undefined}
                retryErrorId={retryableTurn?.errorId}
                retryBusy={interactionBusy}
                onEditFrom={editFromTurn}
                onBranchFrom={branchFromTurn}
                historyBusy={interactionBusy}
              />
              <Composer
                onSend={send}
                onStop={stopTurn}
                busy={interactionBusy}
                generating={turnGenerating}
                status={status}
                playerOptions={playerOptions}
                runUsage={runUsage}
                contextUsage={contextUsage}
                modelWindow={currentModel?.context_window || currentModel?.max_context_window || 0}
                speechToTextEnabled={speechToTextEnabled}
              />
            </div>
          </main>
        )}
      </div>
      {wizardOpen && (
        <NewGameWizard
          worlds={worlds}
          stories={stories}
          characters={characters}
          connectors={connectors}
          models={models}
          connectorModelsLoadingIds={connectorModelsLoadingIds}
          onEnsureConnectorModels={loadConnectorModels}
          initialModelBinding={srv.modelBinding}
          connectorAuthBusyIds={connectorAuthBusyIds}
          connectorAuthCancellingIds={connectorAuthCancellingIds}
          connectorAuthPrompts={connectorAuthPrompts}
          onConnectorAuthStart={onConnectorAuthStart}
          onConnectorAuthCancel={onConnectorAuthCancel}
          preselect={wizardPreselect}
          busy={interactionBusy}
          onLaunch={onWizardLaunch}
          onOpenStudio={(kind, ctx) => {
            closeWizard();
            onCreateEntity(kind, ctx);
          }}
          onClose={closeWizard}
        />
      )}
      {basePickerKind && (
        <BasePickerModal
          kind={basePickerKind}
          worlds={worlds}
          stories={stories}
          busy={interactionBusy}
          onConfirm={onBasePicked}
          onCreateWorld={onBasePickerCreateWorld}
          onClose={() => setBasePickerKind(null)}
        />
      )}
      {locationTransition ? (
        <LocationMapOverlay
          key={`location-transition-${locationTransition.sequence}`}
          graph={locationTransition.graph}
          mode="transition"
          fromLocationId={locationTransition.fromLocationId}
          toLocationId={locationTransition.toLocationId}
          onClose={() => {
            setLocationTransition((current) =>
              current?.sequence === locationTransition.sequence ? null : current
            );
          }}
          onTransitionComplete={() => {
            setLocationTransition((current) =>
              current?.sequence === locationTransition.sequence ? null : current
            );
          }}
        />
      ) : locationMapOpen ? (
        <LocationMapOverlay
          key={`location-map-${activeChatId || "active"}`}
          graph={srv.locationGraph}
          mode="map"
          currentScene={srv.scene}
          npcs={srv.npcs}
          statusLabels={srv.statusLabels}
          onTravelRequest={requestMappedLocationTravel}
          travelBusy={interactionBusy}
          onClose={() => setLocationMapOpen(false)}
        />
      ) : null}
      <Toasts toasts={toasts} onDismiss={dismissToast} />
      {visibility.historyDebug && (
        <DebugPanel
          open={debugOpen}
          onOpenChange={setDebugOpen}
          refreshKey={`${activeChatId}:${runUsage.turns}:${srv.model}:${srv.scene?.scene_id || ""}`}
        />
      )}
    </div>
    </VisibilityContext.Provider>
  );
}
