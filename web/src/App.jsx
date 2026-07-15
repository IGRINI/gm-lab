import { useMemo, useState, useEffect, useRef, useCallback, useSyncExternalStore } from "react";
import {
  api,
  streamTurn,
  streamArchitect,
  streamStoryArchitect,
  streamCharacterArchitect,
} from "./api.js";
import { createTimeline } from "./timelineStore.js";
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
import { normalizeEntities } from "./entityContext.js";
import { useDevSettings, computeVisibility, VisibilityContext, isMessageVisible } from "./devSettings.js";

const EMPTY_SRV = {
  backend: "",
  model: "",
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
  codex_auth: null,
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
  next_compact: { label: "ГМ", used: 0, limit: 0, remaining: 0 },
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
    question: textValue(payload.question) || "Что ты делаешь дальше?",
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
    throw new Error(`Сервер вернул неполный payload чата: нет ${missing.join(" и ")}`);
  }
  return { chatId: payload.chat.id, state: payload.state, transcript: payload.transcript };
}

export default function App() {
  const store = useMemo(createTimeline, []);
  const messages = useSyncExternalStore(store.subscribe, store.getSnapshot);
  const dev = useDevSettings();
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
  // Server failures are `{ok:false, code?, error}`. Keep the machine `code` so
  // Toasts maps it to a human headline and tucks the raw server text behind the
  // «детали» expander — never lose the code by throwing `data.error` as bare text.
  // `fallback` covers a body with no `error`. Wire this into every `!data.ok`
  // branch instead of `notify(data.error)` / `throw new Error(data.error)`.
  const notifyApiError = useCallback(
    (data, fallback = "") => {
      const rawCode = data && typeof data.code === "string" ? data.code.trim() : "";
      pushToast({
        kind: "error",
        code: rawCode || undefined,
        message: textValue(data?.error) || textValue(fallback),
      });
    },
    [pushToast]
  );

  const [srv, setSrv] = useState(EMPTY_SRV);
  const [settings, setSettings] = useState(EMPTY_SETTINGS);
  const [settingsOptions, setSettingsOptions] = useState(EMPTY_SETTINGS_OPTIONS);
  const [runUsage, setRunUsage] = useState(EMPTY_RUN_USAGE);
  const [contextUsage, setContextUsage] = useState(EMPTY_CONTEXT_USAGE);
  const [models, setModels] = useState([]);
  const [sidecarStatus, setSidecarStatus] = useState(EMPTY_SIDECAR_STATUS);
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState(false);
  const [chats, setChats] = useState([]);
  const [activeChatId, setActiveChatId] = useState("");
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
  const [chatsLoading, setChatsLoading] = useState(false);
  const [chatsError, setChatsError] = useState("");
  const [worlds, setWorlds] = useState([]);
  const [worldsLoading, setWorldsLoading] = useState(false);
  const [worldsError, setWorldsError] = useState("");
  const [selectedWorldId, setSelectedWorldId] = useState("");
  const [chatActionBusy, setChatActionBusy] = useState(false);
  const [stories, setStories] = useState([]);
  const [storiesLoading, setStoriesLoading] = useState(false);
  const [storiesError, setStoriesError] = useState("");
  const [characters, setCharacters] = useState([]);
  const [charactersLoading, setCharactersLoading] = useState(false);
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

  const setStateFromServer = useCallback((s) => {
    setSrv({
      backend: s.backend,
      model: s.model,
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
      entities: normalizeEntities(s.entities),
      statusLabels: s.status_labels || {},
      codex_auth: s.codex_auth || null,
    });
    if (s.settings) setSettings((prev) => ({ ...prev, ...s.settings }));
    if (s.settings_options) {
      setSettingsOptions((prev) => ({ ...prev, ...s.settings_options }));
    }
    setRunUsage(s.run_usage || EMPTY_RUN_USAGE);
    setContextUsage(s.context_usage || EMPTY_CONTEXT_USAGE);
  }, []);

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
      if (!data.ok) throw new Error(data.error || "список игр не загружен");
      setChatsFromServer(data);
      return data;
    } catch (e) {
      setChatsError(e.message || "список игр не загружен");
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
      if (!data.ok) throw new Error(data.error || "список миров не загружен");
      setWorlds(Array.isArray(data.worlds) ? data.worlds : []);
      return data;
    } catch (e) {
      setWorldsError(e.message || "список миров не загружен");
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
          error: e.message || "статус sidecar недоступен",
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
      if (!data.ok) throw new Error(data.error || "истории не загружены");
      const nextStories = normalizeStories(data);
      setStories(nextStories);
      return nextStories;
    } catch (e) {
      setStories([]);
      setStoriesError(e.message || "истории не загружены");
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
      if (!data.ok) throw new Error(data.error || "персонажи не загружены");
      const next = Array.isArray(data.characters) ? data.characters : [];
      setCharacters(next);
      return next;
    } catch (e) {
      setCharacters([]);
      setCharactersError(e.message || "персонажи не загружены");
      return [];
    } finally {
      setCharactersLoading(false);
    }
  }, []);

  const restoreChatSession = useCallback(
    (payload) => {
      const { chatId: nextChatId, state: nextState, transcript: nextTranscript } =
        requireChatSessionPayload(payload);

      store.clear();
      setStateFromServer(nextState);
      const events = nextTranscript?.events || [];
      store.dispatchMany(events);
      setPlayerOptions(playerOptionsFromEvents(events));
      setActiveChatId(nextChatId || "");
      if (payload?.chat || nextChatId) {
        setChats((prev) => mergeChatList(prev, payload?.chat, nextChatId));
      }
    },
    [store, setStateFromServer]
  );

  const loadModels = useCallback(async () => {
    try {
      const data = await api.models();
      if (!data.ok) throw new Error(data.error || "модели не загружены");
      setModels(data.models || []);
      if (data.model) setSrv((p) => ({ ...p, model: data.model }));
      if (data.settings) setSettings((prev) => ({ ...prev, ...data.settings }));
      if (data.settings_options) {
        setSettingsOptions((prev) => ({ ...prev, ...data.settings_options }));
      }
    } catch {
      setModels([]); // Header falls back to the current model as the only option
    }
  }, []);

  // initial load
  useEffect(() => {
    (async () => {
      await Promise.all([refreshChats(), refreshWorlds(), loadStories(), loadCharacters()]);
      try {
        const s = await api.state();
        setStateFromServer(s);
      } catch (e) {
        notify(e.message || "состояние не загружено");
      }
      await loadModels();
      try {
        const t = await api.transcript();
        store.clear();
        // Прод отдаёт {events:[...]}, мок-бэкенд — голый массив; принимаем оба.
        const events = Array.isArray(t) ? t : t?.events || [];
        store.dispatchMany(events);
        setPlayerOptions(playerOptionsFromEvents(events));
      } catch (e) {
        notify(e.message || "история не загружена");
      }
      setStatus("");
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const sendTurn = useCallback(
    async (text) => {
      ttsUnlock(); // unlock audio inside the send gesture so auto-play can sound
      store.beginTurn();
      ttsAutoReset(); // each turn's auto-play chain starts fresh
      setPlayerOptions(null);
      setBusy(true);
      setStatus("ГМ думает…");
      try {
        await streamTurn(text, (ev) => {
          store.dispatch(ev);
          const auto = ttsAutoplayRef.current;
          if (auto || ttsEnabledRef.current) {
            const emit = (key, segs) => (auto ? ttsAutoEnqueue(key, segs) : ttsPrime(key, segs));
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
          if (ev.kind === "player") setPlayerOptions(null);
          if (ev.kind === "meta_total") {
            if (ev.data?.run) setRunUsage(ev.data.run);
            if (ev.data?.context) setContextUsage(ev.data.context);
          }
          if (ev.kind === "gm_tool_call") setStatus("ГМ: " + ev.data.name + "…");
          else if (ev.kind === "npc_start") setStatus(ev.agent + " печатает…");
          else if (ev.kind === "npc_speech") setStatus("");
        });
        setStateFromServer(await api.state());
        await refreshChats();
      } catch (e) {
        notify(e.message || "ход не выполнен");
      } finally {
        setBusy(false);
        setStatus("");
      }
    },
    [store, setStateFromServer, refreshChats, notify]
  );

  const sendCommand = useCallback(
    async (text) => {
      const [rawCmd, ...rest] = text.slice(1).split(" ");
      const cmd = rawCmd.trim().toLowerCase();
      const arg = rest.join(" ").trim();
      // No client-side allow-list: the backend /cmd handler validates the command set
      // and returns a structured {ok:false,error} for unknown/incomplete commands.
      setBusy(true);
      try {
        const data = await api.command(cmd, arg);
        if (!data.ok) {
          notifyApiError(data, "команда не выполнена");
          return;
        }
        if (cmd === "reset") {
          store.clear();
          setPlayerOptions(null);
          setStateFromServer(data.state);
          store.pushLocal({ type: "command", text: "Новая партия" });
        } else if (cmd === "new") {
          store.clear();
          setPlayerOptions(null);
          setStateFromServer(data.state);
          store.pushLocal({
            type: "command",
            text: "Новая история: " + (data.state.scene?.title || "стартовая сцена"),
          });
        } else if (cmd === "constraint") {
          store.pushLocal({ type: "command", text: "Ограничение сцены добавлено" });
        } else if (cmd === "event") {
          store.pushLocal({ type: "command", text: "Событие добавлено в мир" });
        }
        await refreshChats();
      } catch (e) {
        notify(e.message || "команда не выполнена");
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
    async ({ storyId, worldId, characterId, title }) => {
      if (chatActionBusy) return;
      setChatActionBusy(true);
      setStatus("Запускаю игру...");
      try {
        const body = { activate: true, story_id: storyId };
        if (worldId) body.world_id = worldId;
        if (characterId) body.character_id = characterId;
        if (title) body.title = title;
        const data = await api.createChat(body);
        if (!data.ok) {
          notifyApiError(data, "игра не создана");
          return;
        }
        restoreChatSession(data);
        // Launch warnings (story_pc_override, world_version_drift…) are
        // «warn-but-allow» notices on a SUCCESSFUL launch — show them as
        // non-sticky warning toasts, never as sticky red errors.
        for (const w of data.warnings ?? []) {
          pushToast({ kind: "warning", code: w.code, message: w.message });
        }
        setWizardOpen(false);
        setMainView("chat");
        await refreshChats();
        closeChatsOnMobile();
      } catch (e) {
        notify(e.message || "игра не создана");
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
        title: textValue(draft?.title) || "Новый мир",
        genre: textValue(draft?.genre) || "fantasy",
        tone: textValue(draft?.tone) || "tense",
        world_size: textValue(draft?.worldSize),
        population: textValue(draft?.population),
        public_premise: textValue(draft?.publicPremise),
        world_lore: draft?.worldLore && typeof draft.worldLore === "object" ? draft.worldLore : null,
        status: "ready",
      };
      setChatActionBusy(true);
      setStatus("Сохраняю мир...");
      try {
        const data = selectedWorldId
          ? await api.updateWorld(selectedWorldId, payload)
          : await api.createWorld(payload);
        if (!data.ok) {
          notifyApiError(data, "мир не создан");
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
        notify(e.message || "мир не создан");
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
      const title = (typeof c.title === "string" && c.title.trim()) || "Персонаж";
      const version = c.version == null ? "?" : c.version;
      pushToast({ kind: "success", message: `Лист «${title}» сохранён (v${version})` });
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
      setStatus("Открываю игру...");
      try {
        const data = await api.activateChat(chatId);
        if (!data.ok) {
          notifyApiError(data, "игра не открыта");
          return;
        }
        restoreChatSession(data);
        setMainView("chat");
        await refreshChats();
        closeChatsOnMobile();
      } catch (e) {
        notify(e.message || "игра не открыта");
      } finally {
        setChatActionBusy(false);
        setStatus("");
      }
    },
    [activeChatId, busy, chatActionBusy, restoreChatSession, refreshChats, closeChatsOnMobile, notify, notifyApiError]
  );

  const onDeleteChat = useCallback(
    async (chatId) => {
      if (!chatId) return;
      const wasActive = sameChatId(chatId, activeChatId);
      try {
        const data = await api.deleteChat(chatId);
        if (!data.ok) {
          notifyApiError(data, "игра не удалена");
          return;
        }
        // If the open game was deleted, switch to the session the server returned
        // (it creates a fresh chat when none remain).
        if (wasActive && data.chat && data.state && data.transcript) {
          restoreChatSession(data);
        }
        await refreshChats();
      } catch (e) {
        notify(e.message || "игра не удалена");
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
          notifyApiError(data, "мир не удалён");
          return;
        }
        if (sameChatId(worldId, selectedWorldId)) setSelectedWorldId("");
        if (Array.isArray(data.worlds)) setWorlds(data.worlds);
        else await refreshWorlds();
      } catch (e) {
        notify(e.message || "мир не удалён");
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
          notifyApiError(data, "история не удалена");
          return;
        }
        await loadStories();
      } catch (e) {
        notify(e.message || "история не удалена");
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
        notify(e.message || "экспорт не выполнен");
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
        notify(e.message || "экспорт не выполнен");
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
        notify(e.message || "экспорт не выполнен");
      }
    },
    [notify]
  );

  // §К1.5: rename a character via a metadata patch (v1 = native prompt).
  const onRenameCharacter = useCallback(
    async (characterId, currentTitle) => {
      if (!characterId || typeof window === "undefined") return;
      const next = window.prompt("Новое имя персонажа", currentTitle || "");
      if (next == null) return; // cancelled
      const title = next.trim();
      if (!title || title === (currentTitle || "").trim()) return;
      try {
        const data = await api.updateCharacter(characterId, { title });
        if (!data.ok) {
          notifyApiError(data, "не удалось переименовать персонажа");
          return;
        }
        await loadCharacters();
      } catch (e) {
        notify(e.message || "не удалось переименовать персонажа");
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
          notifyApiError(data, "не удалось удалить персонажа");
          return;
        }
        await loadCharacters();
      } catch (e) {
        notify(e.message || "не удалось удалить персонажа");
      }
    },
    [loadCharacters, notify, notifyApiError]
  );

  // §К1.5: export the active game's hero snapshot into the library. `characterId`
  // -> snapshot the existing package (+version bump); omitted -> create a new one.
  const onSaveCharacter = useCallback(
    async (characterId) => {
      if (!activeChatId) {
        notify("Нет активной игры");
        return;
      }
      try {
        const body = characterId ? { character_id: characterId } : {};
        const data = await api.saveCharacterFromChat(activeChatId, body);
        if (!data.ok) {
          notifyApiError(data, "не удалось сохранить персонажа");
          return;
        }
        await loadCharacters();
        const c = data.character || {};
        const title = (typeof c.title === "string" && c.title.trim()) || "Персонаж";
        const version = c.version == null ? "?" : c.version;
        pushToast({ kind: "success", message: `Персонаж «${title}» сохранён (v${version})` });
      } catch (e) {
        notify(e.message || "не удалось сохранить персонажа");
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
        if (!data.ok) throw new Error(data.error || "не удалось сохранить протагониста");
        await loadCharacters();
        const c = data.character || {};
        const title = (typeof c.title === "string" && c.title.trim()) || "Персонаж";
        pushToast({ kind: "success", message: `Протагонист «${title}» сохранён в библиотеку` });
      } catch (e) {
        notify(e.message || "не удалось сохранить протагониста");
      }
    },
    [loadCharacters, pushToast, notify]
  );

  const onRevealLibrary = useCallback(async () => {
    try {
      const data = await api.revealLibrary();
      if (!data.ok) throw new Error(data.error || "не удалось открыть папку библиотеки");
    } catch (e) {
      notify(e.message || "не удалось открыть папку библиотеки");
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
    (text) => {
      if (text.startsWith("/")) sendCommand(text);
      else sendTurn(text);
    },
    [sendCommand, sendTurn]
  );

  const onModelChange = useCallback(
    async (model) => {
      if (!model) return;
      try {
        const data = await api.setModel(model);
        if (!data.ok) throw new Error(data.error || "модель не переключена");
        setStateFromServer(data.state);
      } catch (e) {
        notify(e.message || "модель не переключена");
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
        if (!data.ok) throw new Error(data.error || "настройки не сохранены");
        if (data.settings) setSettings((prev) => ({ ...prev, ...data.settings }));
        if (data.settings_options) {
          setSettingsOptions((prev) => ({ ...prev, ...data.settings_options }));
        }
        if (data.state) setStateFromServer(data.state);
      } catch (e) {
        notify(e.message || "настройки не сохранены");
      }
    },
    [settings, setStateFromServer, notify]
  );

  const onCodex = useCallback(async () => {
    setStatus("Жду авторизацию Codex в браузере…");
    try {
      const data = await api.codexLogin();
      if (!data.ok) throw new Error(data.error || "Codex OAuth не выполнен");
      setStateFromServer(await api.state());
      await loadModels();
    } catch (e) {
      notify(e.message || "Codex OAuth не выполнен");
    } finally {
      setStatus("");
    }
  }, [setStateFromServer, loadModels, notify]);

  const onLogout = useCallback(async () => {
    try {
      const data = await api.codexLogout();
      if (!data.ok) throw new Error(data.error || "не вышло отключить Codex");
      setStateFromServer(await api.state());
    } catch (e) {
      notify(e.message || "не вышло отключить Codex");
    }
  }, [setStateFromServer, notify]);

  // «Сброс партии» from the game-context ⋯ menu (its own confirm dialog).
  const onReset = useCallback(async () => {
    try {
      const data = await api.command("reset");
      if (!data.ok) {
        notifyApiError(data, "не удалось сбросить партию");
        return;
      }
      store.clear();
      setPlayerOptions(null);
      setStateFromServer(data.state);
      await refreshChats();
    } catch (e) {
      notify(e.message || "не удалось сбросить партию");
    }
  }, [store, setStateFromServer, refreshChats, notify, notifyApiError]);

  const onExportJson = useCallback(() => api.export(), []);

  const currentModel = useMemo(
    () => (models || []).find((m) => m.id === srv.model || m.slug === srv.model) || null,
    [models, srv.model]
  );
  const interactionBusy = busy || chatActionBusy;
  const isGame = mainView === "chat";

  return (
    <VisibilityContext.Provider value={visibility}>
    <div className="app">
      <Header
        onToggleChats={toggleChats}
        chatsOpen={chatsOpen}
        mainView={mainView}
        onNavGame={showGame}
        onNavLibrary={showLibrary}
        onNavImage={showImage}
        imageLabEnabled={imageLabEnabled}
        srv={srv}
        sidecarStatus={sidecarStatus}
        models={models}
        settings={settings}
        settingsOptions={settingsOptions}
        onModelChange={onModelChange}
        onSettingsChange={onSettingsChange}
        onCodex={onCodex}
        onLogout={onLogout}
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
                ← Библиотека
              </button>
              <WorldArchitectPanel
                world={selectedWorld}
                locked={interactionBusy}
                onCreateWorld={onCreateWorld}
                onArchitectStream={onWorldArchitectStream}
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
                ← Библиотека
              </button>
              <StoryArchitectPanel
                key={storyArchitectWorldId || "new-story"}
                story={storyArchitectStory}
                worldId={storyArchitectWorldId}
                worldTitle={
                  textValue(storyArchitectWorld?.title) ||
                  textValue(storyArchitectWorld?.world_lore?.name)
                }
                locked={interactionBusy}
                onArchitectStream={onStoryArchitectStream}
                onPlayStory={onPlayStory}
                onSaveProtagonist={onSaveProtagonist}
              />
            </div>
          </main>
        ) : mainView === "character-studio" ? (
          <main className="world-creation-pane">
            <div className="studio-frame">
              <button type="button" className="studio-back" onClick={showLibrary}>
                ← Библиотека
              </button>
              <CharacterArchitectPanel
                key={`char-studio-${characterStudioEpoch}`}
                character={characterArchitectCharacter}
                worldId={characterStudioRefs.worldId}
                storyId={characterStudioRefs.storyId}
                worldTitle={characterStudioRefs.worldTitle}
                storyTitle={characterStudioRefs.storyTitle}
                worldMissing={characterStudioRefs.worldMissing}
                storyMissing={characterStudioRefs.storyMissing}
                locked={interactionBusy}
                onArchitectStream={onCharacterArchitectStream}
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
              />
              <Composer
                onSend={send}
                busy={interactionBusy}
                status={status}
                playerOptions={playerOptions}
                runUsage={runUsage}
                contextUsage={contextUsage}
                modelWindow={currentModel?.context_window || currentModel?.max_context_window || 0}
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
