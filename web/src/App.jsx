import { useMemo, useState, useEffect, useCallback, useSyncExternalStore } from "react";
import { api, streamTurn } from "./api.js";
import { createTimeline } from "./timelineStore.js";
import Header from "./components/Header.jsx";
import Chat from "./components/Chat.jsx";
import Composer from "./components/Composer.jsx";
import DebugPanel from "./components/DebugPanel.jsx";
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
      } catch (e) {
        store.dispatch({ kind: "error", agent: "сеть", data: e.message });
      } finally {
        setBusy(false);
        setStatus("");
      }
    },
    [store, setStateFromServer]
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
      } catch (e) {
        store.dispatch({ kind: "error", agent: "команда", data: e.message });
      } finally {
        setBusy(false);
      }
    },
    [store, setStateFromServer]
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
    } catch (e) {
      store.dispatch({ kind: "error", agent: "команда", data: e.message });
    }
  }, [store, setStateFromServer]);

  const currentModel = useMemo(
    () => (models || []).find((m) => m.id === srv.model || m.slug === srv.model) || null,
    [models, srv.model]
  );

  return (
    <div className="app">
      <Header
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
      <Chat messages={messages} scene={srv.scene} npcs={srv.npcs} entities={srv.entities} statusLabels={srv.statusLabels} />
      <Composer
        onSend={send}
        busy={busy}
        status={status}
        runUsage={runUsage}
        contextUsage={contextUsage}
        modelWindow={currentModel?.context_window || currentModel?.max_context_window || 0}
      />
      <DebugPanel refreshKey={`${runUsage.turns}:${srv.model}:${srv.scene?.scene_id || ""}`} />
    </div>
  );
}
