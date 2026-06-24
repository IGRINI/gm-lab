import { useMemo, useState, useEffect, useRef, useCallback, useSyncExternalStore } from "react";
import { api, streamTurn } from "./api.js";
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
import WorldDetailModal from "./components/WorldDetailModal.jsx";
import Tooltip, { TipContent } from "./components/Tooltip.jsx";
import { normalizeEntities } from "./entityContext.js";
import { useDevSettings, computeVisibility, VisibilityContext, isMessageVisible } from "./devSettings.js";

const EMPTY_SRV = {
  backend: "",
  model: "",
  stream_gm_content: false,
  storyBrief: null,
  scene: "",
  time: null,
  playerCharacter: null,
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

function WorldHud({ time, scene, playerCharacter, npcs, statusLabels }) {
  const [detail, setDetail] = useState(null);
  const dateLabel = textValue(time?.current_date_label) || (time?.day_number ? `День ${time.day_number}` : "");
  const timeOfDay = textValue(time?.time_of_day);
  const calendar = textValue(time?.calendar_name);
  const sceneTitle = textValue(scene?.title);
  const pcName = textValue(playerCharacter?.name);
  if (!dateLabel && !timeOfDay && !sceneTitle && !pcName) return null;

  const sceneClickable = !!scene && !!(sceneTitle || textValue(scene?.description));
  const pcClickable = !!playerCharacter && !!pcName;

  return (
    <>
      <aside className="world-hud" aria-label="Текущее состояние мира">
        <div className="world-hud-kicker">мир</div>
        <div className="world-hud-row">
          <span>дата</span>
          <b>{calendar ? `${calendar}, ${dateLabel || "—"}` : dateLabel || "—"}</b>
        </div>
        <div className="world-hud-row">
          <span>время</span>
          <b>{timeOfDay || "—"}</b>
        </div>
        <div className="world-hud-row">
          <span>сцена</span>
          {sceneClickable ? (
            <Tooltip
              className="world-hud-tip"
              tipClassName="ui-tip-wrap"
              focusable={false}
              content={
                <TipContent
                  title="Локация"
                  subtitle={sceneTitle || "Текущая сцена"}
                  note="Открыть подробности: описание, персонажи, выходы и предметы."
                />
              }
            >
              <button
                type="button"
                className="world-hud-link"
                onClick={() => setDetail("scene")}
              >
                {sceneTitle || "—"}
              </button>
            </Tooltip>
          ) : (
            <b>{sceneTitle || "—"}</b>
          )}
        </div>
        {pcName && (
          <div className="world-hud-row">
            <span>персонаж</span>
            {pcClickable ? (
              <Tooltip
                className="world-hud-tip"
                tipClassName="ui-tip-wrap"
                focusable={false}
                content={
                  <TipContent
                    title="Персонаж игрока"
                    subtitle={pcName}
                    note="Открыть лист персонажа: характеристики, навыки, инвентарь и особенности."
                  />
                }
              >
                <button
                  type="button"
                  className="world-hud-link"
                  onClick={() => setDetail("character")}
                >
                  {pcName}
                </button>
              </Tooltip>
            ) : (
              <b>{pcName}</b>
            )}
          </div>
        )}
      </aside>
      {detail && (
        <WorldDetailModal
          kind={detail}
          scene={scene}
          playerCharacter={playerCharacter}
          npcs={npcs}
          statusLabels={statusLabels}
          onClose={() => setDetail(null)}
        />
      )}
    </>
  );
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
  const [chatActionBusy, setChatActionBusy] = useState(false);
  const [stories, setStories] = useState([]);
  const [selectedStoryId, setSelectedStoryId] = useState("");
  const [storiesLoading, setStoriesLoading] = useState(false);
  const [storiesError, setStoriesError] = useState("");
  const [playerOptions, setPlayerOptions] = useState(null);
  const [debugOpen, setDebugOpen] = useState(false);
  const [mainView, setMainView] = useState("chat");

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
      storyBrief: s.story_brief || null,
      scene: s.scene || s.public,
      time: s.time || null,
      playerCharacter: s.player_character || null,
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
      if (!data.ok) throw new Error(data.error || "список чатов не загружен");
      setChatsFromServer(data);
      return data;
    } catch (e) {
      setChatsError(e.message || "список чатов не загружен");
      return null;
    } finally {
      setChatsLoading(false);
    }
  }, [setChatsFromServer]);

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
      const defaultStoryId = data?.default_story_id == null ? "" : String(data.default_story_id).trim();
      setStories(nextStories);
      setSelectedStoryId((current) => {
        if (current && nextStories.some((story) => story.id === current)) return current;
        if (defaultStoryId && nextStories.some((story) => story.id === defaultStoryId)) {
          return defaultStoryId;
        }
        return nextStories[0]?.id || "";
      });
      return nextStories;
    } catch (e) {
      setStories([]);
      setSelectedStoryId("");
      setStoriesError(e.message || "истории не загружены");
      return [];
    } finally {
      setStoriesLoading(false);
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
      await Promise.all([refreshChats(), loadStories()]);
      try {
        const s = await api.state();
        setStateFromServer(s);
      } catch (e) {
        store.dispatch({ kind: "error", agent: "состояние", data: e.message });
      }
      await loadModels();
      try {
        const t = await api.transcript();
        store.clear();
        const events = t.events || [];
        store.dispatchMany(events);
        setPlayerOptions(playerOptionsFromEvents(events));
      } catch (e) {
        store.dispatch({ kind: "error", agent: "история", data: e.message });
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
        store.dispatch({ kind: "error", agent: "сеть", data: e.message });
      } finally {
        setBusy(false);
        setStatus("");
      }
    },
    [store, setStateFromServer, refreshChats]
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
          store.dispatch({ kind: "error", agent: "команда", data: data.error || "команда не выполнена" });
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
        store.dispatch({ kind: "error", agent: "команда", data: e.message });
      } finally {
        setBusy(false);
      }
    },
    [store, setStateFromServer, refreshChats]
  );

  const closeChats = useCallback(() => setChatsOpen(false), []);
  const toggleChats = useCallback(() => setChatsOpen((value) => !value), []);
  const showChatView = useCallback(() => {
    setMainView("chat");
  }, []);
  // On desktop the sidebar is a docked, collapsible column, so it must stay open after
  // picking a chat; only the mobile drawer should auto-close on selection.
  const closeChatsOnMobile = useCallback(() => {
    if (typeof window !== "undefined" && window.matchMedia("(max-width: 700px)").matches) {
      setChatsOpen(false);
    }
  }, []);

  const openWorldCreator = useCallback(() => {
    if (busy || chatActionBusy) return;
    setPlayerOptions(null);
    setStatus("");
    setMainView("world");
    closeChatsOnMobile();
  }, [busy, chatActionBusy, closeChatsOnMobile]);

  // Remember the collapse/expand choice across reloads.
  useEffect(() => {
    try {
      window.localStorage.setItem("gmlab.chatsOpen", chatsOpen ? "1" : "0");
    } catch {
      /* localStorage unavailable (private mode) — non-fatal */
    }
  }, [chatsOpen]);

  const onCreateChat = useCallback(async () => {
    if (busy || chatActionBusy) return;
    if (storiesLoading) {
      store.dispatch({ kind: "error", agent: "чаты", data: "Истории еще загружаются" });
      return;
    }
    if (storiesError) {
      store.dispatch({ kind: "error", agent: "чаты", data: storiesError });
      return;
    }
    const storyId = selectedStoryId && stories.some((story) => story.id === selectedStoryId) ? selectedStoryId : "";
    if (!storyId) {
      store.dispatch({ kind: "error", agent: "чаты", data: "Выберите историю для нового чата" });
      return;
    }
    setChatActionBusy(true);
    setStatus("Создаю чат...");
    try {
      const data = await api.createChat({ activate: true, story_id: storyId });
      if (!data.ok) throw new Error(data.error || "чат не создан");
      restoreChatSession(data);
      await refreshChats();
      closeChatsOnMobile();
    } catch (e) {
      store.dispatch({ kind: "error", agent: "чаты", data: e.message });
    } finally {
      setChatActionBusy(false);
      setStatus("");
    }
  }, [
    busy,
    chatActionBusy,
    storiesLoading,
    storiesError,
    selectedStoryId,
    stories,
    store,
    restoreChatSession,
    refreshChats,
    closeChatsOnMobile,
  ]);

  const onCreateWorld = useCallback(
    async (draft) => {
      if (busy || chatActionBusy) return;
      const payload = {
        activate: true,
        story_id: "procedural",
        title: textValue(draft?.title) || "Процедурный мир",
        story_title: textValue(draft?.title) || "Процедурный мир",
        seed: textValue(draft?.seed),
        genre: textValue(draft?.genre) || "fantasy",
        tone: textValue(draft?.tone) || "tense",
        scale: textValue(draft?.scale) || "village",
        story_brief: textValue(draft?.storyBrief),
        public_intro: textValue(draft?.publicIntro),
        world_lore: draft?.worldLore && typeof draft.worldLore === "object" ? draft.worldLore : null,
      };
      setChatActionBusy(true);
      setStatus("Создаю мир...");
      try {
        const data = await api.createChat(payload);
        if (!data.ok) throw new Error(data.error || "мир не создан");
        restoreChatSession(data);
        setMainView("chat");
        await refreshChats();
        closeChatsOnMobile();
      } catch (e) {
        store.dispatch({ kind: "error", agent: "мир", data: e.message });
      } finally {
        setChatActionBusy(false);
        setStatus("");
      }
    },
    [busy, chatActionBusy, store, restoreChatSession, refreshChats, closeChatsOnMobile]
  );

  const onWorldArchitectTurn = useCallback(async (body) => api.worldArchitectChat(body), []);

  const onActivateChat = useCallback(
    async (chatId) => {
      if (!chatId || busy || chatActionBusy) return;
      if (sameChatId(chatId, activeChatId)) {
        setMainView("chat");
        closeChatsOnMobile();
        return;
      }
      setChatActionBusy(true);
      setStatus("Открываю чат...");
      try {
        const data = await api.activateChat(chatId);
        if (!data.ok) throw new Error(data.error || "чат не открыт");
        restoreChatSession(data);
        setMainView("chat");
        await refreshChats();
        closeChatsOnMobile();
      } catch (e) {
        store.dispatch({ kind: "error", agent: "чаты", data: e.message });
      } finally {
        setChatActionBusy(false);
        setStatus("");
      }
    },
    [activeChatId, busy, chatActionBusy, store, restoreChatSession, refreshChats, closeChatsOnMobile]
  );

  const onDeleteChat = useCallback(
    async (chatId) => {
      if (!chatId) return;
      const wasActive = sameChatId(chatId, activeChatId);
      try {
        const data = await api.deleteChat(chatId);
        if (!data.ok) throw new Error(data.error || "чат не удалён");
        // If the open chat was deleted, switch to the active session the server returned
        // (it creates a fresh chat when none remain). Otherwise keep the current view.
        if (wasActive && data.chat && data.state && data.transcript) {
          restoreChatSession(data);
        }
        await refreshChats();
      } catch (e) {
        store.dispatch({ kind: "error", agent: "чаты", data: e.message });
      }
    },
    [activeChatId, restoreChatSession, refreshChats, store]
  );

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
        store.dispatch({ kind: "error", agent: "модель", data: e.message });
      }
    },
    [store, setStateFromServer]
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
        store.dispatch({ kind: "error", agent: "настройки", data: e.message });
      }
    },
    [settings, store, setStateFromServer]
  );

  const onCodex = useCallback(async () => {
    setStatus("Жду авторизацию Codex в браузере…");
    try {
      const data = await api.codexLogin();
      if (!data.ok) throw new Error(data.error || "Codex OAuth не выполнен");
      setStateFromServer(await api.state());
      await loadModels();
    } catch (e) {
      store.dispatch({ kind: "error", agent: "Codex", data: e.message });
    } finally {
      setStatus("");
    }
  }, [store, setStateFromServer, loadModels]);

  const onLogout = useCallback(async () => {
    try {
      const data = await api.codexLogout();
      if (!data.ok) throw new Error(data.error || "не вышло отключить Codex");
      setStateFromServer(await api.state());
    } catch (e) {
      store.dispatch({ kind: "error", agent: "Codex", data: e.message });
    }
  }, [store, setStateFromServer]);

  const onReset = useCallback(async () => {
    try {
      const data = await api.command("reset");
      store.clear();
      setPlayerOptions(null);
      if (data.ok) setStateFromServer(data.state);
      await refreshChats();
    } catch (e) {
      store.dispatch({ kind: "error", agent: "команда", data: e.message });
    }
  }, [store, setStateFromServer, refreshChats]);

  const currentModel = useMemo(
    () => (models || []).find((m) => m.id === srv.model || m.slug === srv.model) || null,
    [models, srv.model]
  );
  const interactionBusy = busy || chatActionBusy;

  return (
    <VisibilityContext.Provider value={visibility}>
    <div className="app">
      <Header
        onToggleChats={toggleChats}
        chatsOpen={chatsOpen}
        srv={srv}
        sidecarStatus={sidecarStatus}
        models={models}
        settings={settings}
        settingsOptions={settingsOptions}
        onModelChange={onModelChange}
        onSettingsChange={onSettingsChange}
        onCodex={onCodex}
        onLogout={onLogout}
        onExport={() => api.export()}
        onReset={onReset}
      />
      <div className={"app-body" + (debugOpen ? " debug-open" : "") + (chatsOpen ? "" : " chats-collapsed")}>
        {mainView === "chat" && (
          <WorldHud
            time={srv.time}
            scene={srv.scene}
            playerCharacter={srv.playerCharacter}
            npcs={srv.npcs}
            statusLabels={srv.statusLabels}
          />
        )}
        <ChatHistorySidebar
          chats={chats}
          activeChatId={activeChatId}
          open={chatsOpen}
          busy={interactionBusy}
          loading={chatsLoading}
          error={chatsError}
          stories={stories}
          selectedStoryId={selectedStoryId}
          storiesLoading={storiesLoading}
          storiesError={storiesError}
          onSelectStory={setSelectedStoryId}
          onClose={closeChats}
          onCreate={onCreateChat}
          onCreateWorld={openWorldCreator}
          onShowChats={showChatView}
          onActivate={onActivateChat}
          onDelete={onDeleteChat}
        />
        {mainView === "world" ? (
          <main className="world-creation-pane">
            <WorldArchitectPanel
              className="world-manager-main"
              locked={interactionBusy}
              onCreateWorld={onCreateWorld}
              onArchitectTurn={onWorldArchitectTurn}
            />
          </main>
        ) : (
          <main className={"chat-pane" + (playerOptions ? " has-options" : "")}>
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
          </main>
        )}
      </div>
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
