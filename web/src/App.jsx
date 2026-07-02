import { useMemo, useState, useEffect, useRef, useCallback, useSyncExternalStore } from "react";
import { api, streamTurn, streamArchitect } from "./api.js";
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
import CreateStoryModal from "./components/CreateStoryModal.jsx";
import ImageLabPanel from "./components/ImageLabPanel.jsx";
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
  charRef: null,
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

function WorldHud({
  time,
  scene,
  playerCharacter,
  npcs,
  statusLabels,
  charRef = null,
  characters = [],
  canSaveCharacter = false,
  onSaveCharacter,
}) {
  const [detail, setDetail] = useState(null);
  // Save-hero control state: null (idle), true (choice row open), "busy" (in flight).
  const [saveState, setSaveState] = useState(null);
  const dateLabel = textValue(time?.current_date_label) || (time?.day_number ? `День ${time.day_number}` : "");
  const timeOfDay = textValue(time?.time_of_day);
  const calendar = textValue(time?.calendar_name);
  const sceneTitle = textValue(scene?.title);
  const pcName = textValue(playerCharacter?.name);
  if (!dateLabel && !timeOfDay && !sceneTitle && !pcName) return null;

  const sceneClickable = !!scene && !!(sceneTitle || textValue(scene?.description));
  const pcClickable = !!playerCharacter && !!pcName;

  // §К1.5: "update the source" is offered only when char_ref resolves to a
  // character still present in the loaded library; otherwise only "save as new".
  const sourceId = charRef && charRef.id != null ? String(charRef.id) : "";
  const source =
    sourceId && Array.isArray(characters)
      ? characters.find((c) => c != null && String(c.id) === sourceId) || null
      : null;
  const sourceTitle = textValue(source?.title) || textValue(source?.preview) || pcName || "исходный";
  const saveBusy = saveState === "busy";

  const runSave = async (characterId) => {
    if (saveBusy || !onSaveCharacter) return;
    setSaveState("busy");
    try {
      await onSaveCharacter(characterId);
    } finally {
      // The transcript notice reports the result; collapse the control either way.
      setSaveState(null);
    }
  };

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
        {canSaveCharacter && (
          <div className="world-hud-save">
            {saveState === true ? (
              <div className="world-hud-save-choice">
                <button
                  type="button"
                  className="btn world-hud-save-btn"
                  onClick={() => runSave(sourceId)}
                  disabled={saveBusy}
                  title={`Перезаписать пакет «${sourceTitle}» текущим состоянием`}
                >
                  Обновить «{sourceTitle}»
                </button>
                <button
                  type="button"
                  className="btn world-hud-save-btn"
                  onClick={() => runSave("")}
                  disabled={saveBusy}
                >
                  Сохранить как нового
                </button>
                <button
                  type="button"
                  className="btn world-hud-save-cancel"
                  onClick={() => setSaveState(null)}
                  disabled={saveBusy}
                >
                  Отмена
                </button>
              </div>
            ) : source ? (
              <button
                type="button"
                className="btn world-hud-save-btn"
                onClick={() => setSaveState(true)}
                disabled={saveBusy}
              >
                {saveBusy ? "Сохраняю…" : "Сохранить ГГ в библиотеку"}
              </button>
            ) : (
              <button
                type="button"
                className="btn world-hud-save-btn"
                onClick={() => runSave("")}
                disabled={saveBusy}
              >
                {saveBusy ? "Сохраняю…" : "Сохранить ГГ в библиотеку"}
              </button>
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
  const [worlds, setWorlds] = useState([]);
  const [worldsLoading, setWorldsLoading] = useState(false);
  const [worldsError, setWorldsError] = useState("");
  const [selectedWorldId, setSelectedWorldId] = useState("");
  const [chatActionBusy, setChatActionBusy] = useState(false);
  const [stories, setStories] = useState([]);
  const [selectedStoryId, setSelectedStoryId] = useState("");
  const [storiesLoading, setStoriesLoading] = useState(false);
  const [storiesError, setStoriesError] = useState("");
  const [characters, setCharacters] = useState([]);
  const [selectedCharacterId, setSelectedCharacterId] = useState("");
  const [charactersLoading, setCharactersLoading] = useState(false);
  const [charactersError, setCharactersError] = useState("");
  const [createStoryWorldId, setCreateStoryWorldId] = useState("");
  const [playerOptions, setPlayerOptions] = useState(null);
  const [debugOpen, setDebugOpen] = useState(false);
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
      storyBrief: s.story_brief || null,
      scene: s.scene || s.public,
      time: s.time || null,
      playerCharacter: s.player_character || null,
      // K1 (§К1.5): the launched CHARACTER package provenance, when present.
      // Absent -> null (the "save hero" control offers "save as new" only).
      charRef: s.char_ref || null,
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

  // K1 (§К1.5): load the CHARACTER packages (mirror of loadStories). The picker
  // is optional (empty = story/default hero), so a stale/removed selection just
  // falls back to "no character" rather than auto-selecting.
  const loadCharacters = useCallback(async () => {
    setCharactersLoading(true);
    setCharactersError("");
    try {
      const data = await api.characters();
      if (!data.ok) throw new Error(data.error || "персонажи не загружены");
      const next = Array.isArray(data.characters) ? data.characters : [];
      setCharacters(next);
      setSelectedCharacterId((current) =>
        current && next.some((c) => sameChatId(c.id, current)) ? current : ""
      );
      return next;
    } catch (e) {
      setCharacters([]);
      setSelectedCharacterId("");
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

  const showWorldCreator = useCallback(() => {
    if (busy || chatActionBusy) return;
    setPlayerOptions(null);
    setStatus("");
    setMainView("world");
    closeChatsOnMobile();
  }, [busy, chatActionBusy, closeChatsOnMobile]);

  const showImageLab = useCallback(() => {
    if (busy || chatActionBusy || !imageLabEnabled) return;
    setPlayerOptions(null);
    setStatus("");
    setMainView("image");
    closeChatsOnMobile();
  }, [busy, chatActionBusy, closeChatsOnMobile, imageLabEnabled]);

  const openNewWorldCreator = useCallback(() => {
    if (busy || chatActionBusy) return;
    setSelectedWorldId("");
    setPlayerOptions(null);
    setStatus("");
    setMainView("world");
    closeChatsOnMobile();
  }, [busy, chatActionBusy, closeChatsOnMobile]);

  const onSelectWorld = useCallback(
    (worldId) => {
      if (!worldId || busy || chatActionBusy) return;
      setSelectedWorldId(worldId);
      setPlayerOptions(null);
      setStatus("");
      setMainView("world");
      closeChatsOnMobile();
    },
    [busy, chatActionBusy, closeChatsOnMobile]
  );

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
    // §К1.5: an optional CHARACTER package overlays the hero at launch; empty
    // selection = the story's/default hero. A stale id is filtered out here.
    const characterId =
      selectedCharacterId && characters.some((c) => sameChatId(c.id, selectedCharacterId))
        ? selectedCharacterId
        : "";
    setChatActionBusy(true);
    setStatus("Создаю чат...");
    try {
      const body = { activate: true, story_id: storyId };
      if (characterId) body.character_id = characterId;
      const data = await api.createChat(body);
      if (!data.ok) throw new Error(data.error || "чат не создан");
      restoreChatSession(data);
      // Surface any structured launch warnings (e.g. world_version_drift) in the
      // transcript via the existing error channel.
      for (const w of data.warnings ?? []) {
        store.dispatch({ kind: "error", agent: "мир", data: w.message });
      }
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
    selectedCharacterId,
    characters,
    store,
    restoreChatSession,
    refreshChats,
    closeChatsOnMobile,
  ]);

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
        if (!data.ok) throw new Error(data.error || "мир не создан");
        if (Array.isArray(data.worlds)) setWorlds(data.worlds);
        else await refreshWorlds();
        if (data.world?.id) setSelectedWorldId(data.world.id);
        setMainView("world");
        // Return the persisted world so the editor can adopt the server-rewritten
        // image URLs (/world-assets/...) instead of keeping volatile sidecar URLs.
        return data.world || null;
      } catch (e) {
        store.dispatch({ kind: "error", agent: "мир", data: e.message });
        return null;
      } finally {
        setChatActionBusy(false);
        setStatus("");
      }
    },
    [busy, chatActionBusy, selectedWorldId, store, refreshWorlds]
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

  const onDeleteWorld = useCallback(
    async (worldId) => {
      if (!worldId) return;
      try {
        const data = await api.deleteWorld(worldId);
        if (!data.ok) throw new Error(data.error || "мир не удалён");
        if (sameChatId(worldId, selectedWorldId)) setSelectedWorldId("");
        if (Array.isArray(data.worlds)) setWorlds(data.worlds);
        else await refreshWorlds();
      } catch (e) {
        store.dispatch({ kind: "error", agent: "мир", data: e.message });
      }
    },
    [refreshWorlds, selectedWorldId, store]
  );

  // Play a saved world: launch a procedural campaign from the world package
  // (the backend resolves the world's lore and stamps world_ref). A missing
  // world surfaces as a 400 error — never a default/empty world.
  const onPlayWorld = useCallback(
    async (worldId) => {
      if (!worldId || busy || chatActionBusy) return;
      // §К1.5: carry the optional CHARACTER package selection into a direct play.
      const characterId =
        selectedCharacterId && characters.some((c) => sameChatId(c.id, selectedCharacterId))
          ? selectedCharacterId
          : "";
      setChatActionBusy(true);
      setStatus("Запускаю мир...");
      try {
        const body = {
          activate: true,
          story_id: "procedural",
          world_id: worldId,
        };
        if (characterId) body.character_id = characterId;
        const data = await api.createChat(body);
        if (!data.ok) throw new Error(data.error || "мир не запущен");
        restoreChatSession(data);
        // Mirror the launch-warning surface from onCreateChat. Direct world plays
        // carry no authored pin today, so this is a harmless no-op — future-proof.
        for (const w of data.warnings ?? []) {
          store.dispatch({ kind: "error", agent: "мир", data: w.message });
        }
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
    [
      busy,
      chatActionBusy,
      selectedCharacterId,
      characters,
      restoreChatSession,
      refreshChats,
      closeChatsOnMobile,
      store,
    ]
  );

  const onCreateStory = useCallback(
    (worldId) => {
      if (!worldId || busy || chatActionBusy) return;
      setCreateStoryWorldId(worldId);
    },
    [busy, chatActionBusy]
  );

  // Phase 5: download a world/story package zip via a fetch-based blob download.
  // A failed export reads the backend JSON error and surfaces it through the same
  // in-app error channel as import/reveal (never navigates the SPA away).
  const onExportWorld = useCallback(
    async (worldId) => {
      if (!worldId) return;
      try {
        await api.downloadExport(api.exportWorldUrl(worldId), `${worldId}.gmworld.zip`);
      } catch (e) {
        store.dispatch({ kind: "error", agent: "экспорт", data: e.message || "экспорт не выполнен" });
      }
    },
    [store]
  );

  const onExportStory = useCallback(
    async (storyId, bake) => {
      if (!storyId) return;
      try {
        await api.downloadExport(api.exportStoryUrl(storyId, !!bake), `${storyId}.gmstory.zip`);
      } catch (e) {
        store.dispatch({ kind: "error", agent: "экспорт", data: e.message || "экспорт не выполнен" });
      }
    },
    [store]
  );

  // §К1.5: download a character package zip (mirror of onExportWorld/onExportStory).
  const onExportCharacter = useCallback(
    async (characterId) => {
      if (!characterId) return;
      try {
        await api.downloadExport(api.exportCharacterUrl(characterId), `${characterId}.gmchar.zip`);
      } catch (e) {
        store.dispatch({ kind: "error", agent: "экспорт", data: e.message || "экспорт не выполнен" });
      }
    },
    [store]
  );

  // §К1.5: rename a character via a metadata patch (v1 = native prompt; no inline
  // editor exists yet). Refreshes the list on success so the new title appears.
  const onRenameCharacter = useCallback(
    async (characterId, currentTitle) => {
      if (!characterId || typeof window === "undefined") return;
      const next = window.prompt("Новое имя персонажа", currentTitle || "");
      if (next == null) return; // cancelled
      const title = next.trim();
      if (!title || title === (currentTitle || "").trim()) return;
      try {
        const data = await api.updateCharacter(characterId, { title });
        if (!data.ok) throw new Error(data.error || "не удалось переименовать персонажа");
        await loadCharacters();
      } catch (e) {
        store.dispatch({ kind: "error", agent: "персонаж", data: e.message });
      }
    },
    [store, loadCharacters]
  );

  // §К1.5: delete a character package. NEVER touches saves (a char_ref may dangle).
  const onDeleteCharacter = useCallback(
    async (characterId) => {
      if (!characterId) return;
      try {
        const data = await api.deleteCharacter(characterId);
        if (!data.ok) throw new Error(data.error || "не удалось удалить персонажа");
        await loadCharacters();
      } catch (e) {
        store.dispatch({ kind: "error", agent: "персонаж", data: e.message });
      }
    },
    [store, loadCharacters]
  );

  // §К1.5: export the active chat's current hero snapshot into the library.
  // `characterId` -> snapshot the existing package (+version bump); omitted ->
  // create a new package (title = hero name). On success: refresh the list and
  // post a transcript notice through the established "персонаж" channel.
  const onSaveCharacter = useCallback(
    async (characterId) => {
      if (!activeChatId) {
        store.dispatch({ kind: "error", agent: "персонаж", data: "Нет активного чата" });
        return;
      }
      try {
        const body = characterId ? { character_id: characterId } : {};
        const data = await api.saveCharacterFromChat(activeChatId, body);
        if (!data.ok) throw new Error(data.error || "не удалось сохранить персонажа");
        await loadCharacters();
        const c = data.character || {};
        const title = (typeof c.title === "string" && c.title.trim()) || "Персонаж";
        const version = c.version == null ? "?" : c.version;
        store.dispatch({
          kind: "error",
          agent: "персонаж",
          data: `Персонаж «${title}» сохранён (v${version})`,
        });
      } catch (e) {
        store.dispatch({ kind: "error", agent: "персонаж", data: e.message });
      }
    },
    [activeChatId, store, loadCharacters]
  );

  // Open the library root folder in the OS file manager. A failed open surfaces
  // as an error (never a silent success).
  const onRevealLibrary = useCallback(async () => {
    try {
      const data = await api.revealLibrary();
      if (!data.ok) throw new Error(data.error || "не удалось открыть папку библиотеки");
    } catch (e) {
      store.dispatch({ kind: "error", agent: "библиотека", data: e.message });
    }
  }, [store]);

  // Import a picked .zip package, then refresh worlds + stories so the imported
  // world/story appears. Backend errors (collision, malformed) propagate to the
  // caller so the sidebar can show them inline.
  const onImportPackage = useCallback(
    async (file, overwrite) => {
      const data = await api.importPackage(file, overwrite);
      // §К1.5: import is shared across kinds, so refresh characters too.
      await Promise.all([refreshWorlds(), loadStories(), loadCharacters()]);
      return data;
    },
    [refreshWorlds, loadStories, loadCharacters]
  );

  const closeCreateStory = useCallback(() => setCreateStoryWorldId(""), []);

  // Create a story package bound to a world, then refresh + select it so it
  // appears in the new-chat picker. Throws on failure so the modal can show it.
  const onSubmitCreateStory = useCallback(
    async (body) => {
      const data = await api.createStory(body);
      if (!data.ok) throw new Error(data.error || "история не создана");
      const nextStories = await loadStories();
      const newId = data.story?.id == null ? "" : String(data.story.id).trim();
      if (newId && nextStories.some((story) => story.id === newId)) {
        setSelectedStoryId(newId);
      }
      return data.story || null;
    },
    [loadStories]
  );

  const createStoryWorld = useMemo(
    () => (Array.isArray(worlds) ? worlds : []).find((world) => sameChatId(world.id, createStoryWorldId)) || null,
    [worlds, createStoryWorldId]
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
            charRef={srv.charRef}
            characters={characters}
            canSaveCharacter={!!activeChatId && !!srv.playerCharacter}
            onSaveCharacter={onSaveCharacter}
          />
        )}
        <ChatHistorySidebar
          chats={chats}
          worlds={worlds}
          activeChatId={activeChatId}
          selectedWorldId={selectedWorldId}
          open={chatsOpen}
          busy={interactionBusy}
          loading={chatsLoading}
          error={chatsError}
          worldsLoading={worldsLoading}
          worldsError={worldsError}
          stories={stories}
          selectedStoryId={selectedStoryId}
          storiesLoading={storiesLoading}
          storiesError={storiesError}
          onSelectStory={setSelectedStoryId}
          characters={characters}
          selectedCharacterId={selectedCharacterId}
          charactersLoading={charactersLoading}
          charactersError={charactersError}
          onSelectCharacter={setSelectedCharacterId}
          onExportCharacter={onExportCharacter}
          onRenameCharacter={onRenameCharacter}
          onDeleteCharacter={onDeleteCharacter}
          onClose={closeChats}
          onCreate={onCreateChat}
          onCreateWorld={openNewWorldCreator}
          onShowWorldCreator={showWorldCreator}
          onShowChats={showChatView}
          onShowImageLab={showImageLab}
          onSelectWorld={onSelectWorld}
          onPlayWorld={onPlayWorld}
          onCreateStory={onCreateStory}
          onExportWorld={onExportWorld}
          onExportStory={onExportStory}
          onRevealLibrary={onRevealLibrary}
          onImportPackage={onImportPackage}
          onActivate={onActivateChat}
          onDelete={onDeleteChat}
          onDeleteWorld={onDeleteWorld}
          sidebarMode={mainView === "world" ? "world" : mainView === "image" ? "image" : "chats"}
          imageLabEnabled={imageLabEnabled}
        />
        {mainView === "world" ? (
          <main className="world-creation-pane">
            <WorldArchitectPanel
              world={selectedWorld}
              locked={interactionBusy}
              onCreateWorld={onCreateWorld}
              onArchitectStream={onWorldArchitectStream}
              onGenerateImage={onGenerateImage}
              onPlayWorld={onPlayWorld}
              onCreateStory={onCreateStory}
            />
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
      {createStoryWorld && (
        <CreateStoryModal
          world={createStoryWorld}
          busy={interactionBusy}
          onClose={closeCreateStory}
          onCreate={onSubmitCreateStory}
        />
      )}
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
