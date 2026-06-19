import { useMemo, useState, useEffect, useCallback, useSyncExternalStore } from "react";
import { api, streamTurn } from "./api.js";
import { createTimeline } from "./timelineStore.js";
import Header from "./components/Header.jsx";
import Chat from "./components/Chat.jsx";
import Composer from "./components/Composer.jsx";
import DebugPanel from "./components/DebugPanel.jsx";
import ChatHistorySidebar from "./components/ChatHistorySidebar.jsx";
import { normalizeEntities } from "./entityContext.js";

const EMPTY_SRV = {
  backend: "",
  model: "",
  stream_gm_content: false,
  scene: "",
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
    textValue(story?.description) ||
    textValue(story?.summary) ||
    textValue(story?.public_intro) ||
    "";
  return { ...story, id, story_id: id, title, description };
}

function normalizeStories(data) {
  if (!Array.isArray(data?.stories)) return [];
  return data.stories.map(normalizeStory).filter(Boolean);
}

function activeChatIdFrom(data) {
  return data?.active_chat_id || data?.chats?.find((chat) => chat.active)?.id || "";
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

  const [srv, setSrv] = useState(EMPTY_SRV);
  const [settings, setSettings] = useState(EMPTY_SETTINGS);
  const [settingsOptions, setSettingsOptions] = useState(EMPTY_SETTINGS_OPTIONS);
  const [runUsage, setRunUsage] = useState(EMPTY_RUN_USAGE);
  const [contextUsage, setContextUsage] = useState(EMPTY_CONTEXT_USAGE);
  const [models, setModels] = useState([]);
  const [status, setStatus] = useState("");
  const [busy, setBusy] = useState(false);
  const [chats, setChats] = useState([]);
  const [activeChatId, setActiveChatId] = useState("");
  const [chatsOpen, setChatsOpen] = useState(false);
  const [chatsLoading, setChatsLoading] = useState(false);
  const [chatsError, setChatsError] = useState("");
  const [chatActionBusy, setChatActionBusy] = useState(false);
  const [stories, setStories] = useState([]);
  const [selectedStoryId, setSelectedStoryId] = useState("");
  const [storiesLoading, setStoriesLoading] = useState(false);
  const [storiesError, setStoriesError] = useState("");

  const setStateFromServer = useCallback((s) => {
    setSrv({
      backend: s.backend,
      model: s.model,
      stream_gm_content: s.stream_gm_content,
      scene: s.scene || s.public,
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
      store.dispatchMany(nextTranscript?.events || []);
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
        store.dispatchMany(t.events || []);
      } catch (e) {
        store.dispatch({ kind: "error", agent: "история", data: e.message });
      }
      setStatus("");
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const sendTurn = useCallback(
    async (text) => {
      store.beginTurn();
      setBusy(true);
      setStatus("ГМ думает…");
      try {
        await streamTurn(text, (ev) => {
          store.dispatch(ev);
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
          setStateFromServer(data.state);
          store.pushLocal({ type: "command", text: "Новая партия" });
        } else if (cmd === "new") {
          store.clear();
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
  const openChats = useCallback(() => setChatsOpen(true), []);

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
      setChatsOpen(false);
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
  ]);

  const onActivateChat = useCallback(
    async (chatId) => {
      if (!chatId || sameChatId(chatId, activeChatId) || busy || chatActionBusy) return;
      setChatActionBusy(true);
      setStatus("Открываю чат...");
      try {
        const data = await api.activateChat(chatId);
        if (!data.ok) throw new Error(data.error || "чат не открыт");
        restoreChatSession(data);
        await refreshChats();
        setChatsOpen(false);
      } catch (e) {
        store.dispatch({ kind: "error", agent: "чаты", data: e.message });
      } finally {
        setChatActionBusy(false);
        setStatus("");
      }
    },
    [activeChatId, busy, chatActionBusy, store, restoreChatSession, refreshChats]
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
    <div className="app">
      <Header
        onToggleChats={openChats}
        srv={srv}
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
      <div className="app-body">
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
          onActivate={onActivateChat}
        />
        <main className="chat-pane">
          <Chat
            key={activeChatId || "active-chat"}
            messages={messages}
            scene={srv.scene}
            npcs={srv.npcs}
            entities={srv.entities}
            statusLabels={srv.statusLabels}
          />
          <Composer
            onSend={send}
            busy={interactionBusy}
            status={status}
            runUsage={runUsage}
            contextUsage={contextUsage}
            modelWindow={currentModel?.context_window || currentModel?.max_context_window || 0}
          />
        </main>
      </div>
      <DebugPanel refreshKey={`${activeChatId}:${runUsage.turns}:${srv.model}:${srv.scene?.scene_id || ""}`} />
    </div>
  );
}
