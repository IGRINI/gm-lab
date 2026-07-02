import { useEffect, useMemo, useRef, useState } from "react";
import { api } from "../api.js";
import Spoiler from "./Spoiler.jsx";
import {
  EMPTY_ARCHITECT_USAGE,
  textValue,
  normalizeModelMessage,
  architectCacheFromSaved,
  visibleMessagesFromSaved,
  modelMessagesFromSaved,
  AutoTextarea,
  useLiveSegments,
  ArchitectChatPane,
  ArchitectDebugModal,
  accumulateUsage,
  debugFromDone,
} from "./architectShared.jsx";

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

const DEFAULT_ARCHITECT_MESSAGES = [
  {
    role: "assistant",
    content:
      "Опиши, какую историю ты хочешь на этом мире — или дай направление, а завязку я соберу сам.\n\nЧто особенно полезно:\n\n1. О чём история и что движет игроком (story brief).\n2. Публичное вступление — что игрок видит и знает в начале.\n3. Скрытая правда истории (секрет только для GM).\n4. Стартовая сцена: место, кто рядом, что создаёт напряжение.\n5. Кого встретит игрок (NPC) и какие факты уже известны миру.\n6. Каким может быть предлагаемый протагонист (игрок сможет заменить его при запуске).\n\nЯ пишу только завязку одной истории поверх канона мира — без глав, целей и концовок.",
  },
];

// The list sections rendered as read-only JSON summaries in v1 (complex nested
// object cards the architect authors; the panel shows a compact preview and the
// full JSON in a spoiler — editing them is a future track, per §С1.3).
const OBJECT_LIST_SECTIONS = [
  ["npcs", "NPC (стартовый состав)", (e) => textValue(e?.name) || textValue(e?.id)],
  ["public_facts", "Публичные факты", (e) => textValue(e?.text) || textValue(e?.id)],
  ["state_records", "Начальные состояния", (e) => textValue(e?.text) || textValue(e?.id)],
];

// Scene sub-lists rendered as newline-editable text.
const SCENE_LIST_FIELDS = [
  ["present_npcs", "NPC в сцене (id, по строке)"],
  ["exits", "Выходы (по строке)"],
  ["items", "Предметы (по строке)"],
  ["constraints", "Ограничения (по строке)"],
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

function architectMessagesFromStory(story) {
  return visibleMessagesFromSaved(story, DEFAULT_ARCHITECT_MESSAGES);
}

// The plot object POSTed as `draft` to the story architect (snake_case, matching
// the runtime contract + the tool schema). Empty scalars/lists are dropped so a
// blank field never clobbers an existing value on the shallow server-side merge.
function cleanStoryDraft(draft) {
  const plot = {};
  for (const key of ["title", "description", "story_brief", "public_intro", "hidden_truth"]) {
    const v = textValue(draft[key]);
    if (v) plot[key] = v;
  }
  const pc = asObject(draft.player_character);
  if (pc && Object.keys(pc).length > 0) plot.player_character = pc;
  const scene = asObject(draft.scene);
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

export default function StoryArchitectPanel({
  story,
  worldId,
  worldTitle,
  locked,
  onArchitectStream,
  onPlayStory,
  className = "",
}) {
  // Seed the form from the catalog row's scalars only (title/description); the
  // GM-only seed + architect chat state come from the draft fetch below (the
  // `story` prop is the minimal catalog row, §С1.3).
  const [architectCache, setArchitectCache] = useState(() =>
    architectCacheFromSaved(null, "story-architect")
  );
  const [storyDraft, setStoryDraft] = useState(() => storyDraftFromSaved(story));
  const [messages, setMessages] = useState(() => architectMessagesFromStory(null));
  const [modelMessages, setModelMessages] = useState(() => modelMessagesFromSaved(null));
  const [input, setInput] = useState("");
  const [architectBusy, setArchitectBusy] = useState(false);
  const [architectError, setArchitectError] = useState("");
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

  const draftPayload = useMemo(() => cleanStoryDraft(storyDraft), [storyDraft]);
  const ready = plotReady(storyDraft);
  const architectLocked = locked || architectBusy;

  // Reload the form + conversation only when the user opens a DIFFERENT story
  // (or switches from a fresh draft to a saved one). The story our own turn just
  // created/updated is already ours — reloading would wipe the live chat.
  //
  // The `story` prop is the MINIMAL player-facing catalog row (id/title/
  // description/kind/world_ref) — it deliberately carries NO seed/architect_*
  // (hidden_truth is GM-only, §С1.3). So for an EXISTING story we fetch the
  // GM-scoped draft row (seed + flattened architect chat state) via
  // `api.storyDraft(id)` and restore the form + conversation from it; a fresh
  // draft (no id) resets to the empty defaults.
  useEffect(() => {
    const id = textValue(story?.id) || null;
    if (id === loadedStoryIdRef.current) return undefined;
    loadedStoryIdRef.current = id;
    // Reset synchronously to the catalog row's scalars (title/description) so the
    // form never flashes a stale story while the draft fetch is in flight.
    setArchitectCache(architectCacheFromSaved(null, "story-architect"));
    setStoryDraft(storyDraftFromSaved(story));
    setMessages(architectMessagesFromStory(null));
    setModelMessages(modelMessagesFromSaved(null));
    setCurrentStoryId(id || "");
    clearLive();
    setInput("");
    setArchitectError("");
    setArchitectUsage(EMPTY_ARCHITECT_USAGE);
    setArchitectDebug(null);
    setDebugOpen(false);
    if (!id) return undefined;
    // Fetch the GM draft row for an existing story. `cancelled` guards a stale
    // response when the user reopens a different story before this resolves.
    let cancelled = false;
    api
      .storyDraft(id)
      .then((data) => {
        if (cancelled || loadedStoryIdRef.current !== id) return;
        if (!data?.ok || !data.story) {
          throw new Error(data?.error || "не удалось загрузить черновик истории");
        }
        const draftRow = data.story;
        setArchitectCache(architectCacheFromSaved(draftRow, "story-architect"));
        setStoryDraft(storyDraftFromSaved(draftRow));
        setMessages(architectMessagesFromStory(draftRow));
        setModelMessages(modelMessagesFromSaved(draftRow));
      })
      .catch((error) => {
        if (cancelled || loadedStoryIdRef.current !== id) return;
        setArchitectError(error?.message || "не удалось загрузить черновик истории");
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
  const updateSceneList = (field, text) =>
    updateScene(field, text.split("\n").map((s) => s.trim()).filter(Boolean));
  const updateProperNouns = (text) =>
    updateDraft("proper_nouns", text.split("\n").map((s) => s.trim()).filter(Boolean));
  const updateTime = (text) => {
    const n = parseInt(text, 10);
    updateDraft("time", Number.isFinite(n) ? n : null);
  };

  const sendArchitectMessage = async () => {
    const text = input.trim();
    if (!text || architectLocked) return;
    const history = modelMessages;
    const userMessage = { role: "user", content: text };
    const visibleMessages = [...messages, userMessage];
    setInput("");
    setArchitectError("");
    setArchitectBusy(true);
    clearLive();
    setMessages(visibleMessages);
    let adopted = false;
    let failure = "";
    try {
      await onArchitectStream?.(
        {
          message: text,
          history,
          draft: draftPayload,
          visible_messages: visibleMessages,
          cache_session_id: architectCache.sessionId,
          cache_thread_id: architectCache.threadId,
          // A create relies on world_id; an edit carries the resolved story_id.
          ...(currentStoryId ? { story_id: currentStoryId } : {}),
          ...(worldId ? { world_id: worldId } : {}),
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
            // draft_story_plot args merge live like the world panel's bible; the
            // targeted edit_story_plot patch is folded from the authoritative
            // `draft` in the done payload (its set/add/remove ops are non-trivial
            // to replay client-side, so we adopt the server's result instead).
            if (name === "draft_story_plot") {
              setStoryDraft((current) => mergeStoryDraft(current, args));
            }
          } else if (ev.kind === "architect_error") {
            failure = textValue(ev.data) || "Архитектор не ответил";
          } else if (ev.kind === "architect_done") {
            adopted = true;
            const data = ev.data || {};
            const usage = asObject(data.usage);
            if (usage) setArchitectUsage((current) => accumulateUsage(current, usage));
            setArchitectDebug(debugFromDone(data, usage));
            const nextSessionId = textValue(data.cache_session_id);
            const nextThreadId = textValue(data.cache_thread_id);
            if (nextSessionId || nextThreadId) {
              setArchitectCache((current) => ({
                sessionId: nextSessionId || current.sessionId,
                threadId: nextThreadId || current.threadId,
              }));
            }
            // Adopt the persisted story as the source of truth: the server folded
            // the plot into `seed` (draft_story_plot merge OR edit_story_plot
            // set/add/remove), so restore the form from it rather than replay ops.
            const savedStory = asObject(data.story);
            if (savedStory) {
              setStoryDraft(storyDraftFromSaved(savedStory));
              setMessages(architectMessagesFromStory(savedStory));
            } else if (asObject(data.draft)) {
              setStoryDraft((current) => mergeStoryDraft(current, data.draft));
            }
            const modelUserMessage = normalizeModelMessage(data.user_message);
            const modelAssistantMessage = normalizeModelMessage(data.assistant_history_message);
            if (modelUserMessage) {
              setModelMessages((current) =>
                modelAssistantMessage
                  ? [...current, modelUserMessage, modelAssistantMessage]
                  : [...current, modelUserMessage]
              );
            }
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
      if (failure) throw new Error(failure);
    } catch (error) {
      const message = error?.message || "Не удалось вызвать архитектора";
      setArchitectError(message);
      if (!adopted) {
        setMessages((current) => [
          ...current,
          ...liveSegmentsRef.current,
          { role: "assistant", content: `Не получилось обновить историю: ${message}` },
        ]);
        clearLive();
      }
    } finally {
      setArchitectBusy(false);
    }
  };

  const pc = asObject(storyDraft.player_character) || {};
  const scene = asObject(storyDraft.scene) || {};

  return (
    <div className={`world-studio${className ? ` ${className}` : ""}`}>
      <header className="world-studio-head">
        <div className="world-studio-id">
          <span className="world-studio-emblem" aria-hidden="true">✧</span>
          <div className="world-studio-title">
            <span className="world-studio-kicker">создание истории</span>
            <b>Студия историй</b>
            <p className="world-studio-sub">
              Соберите завязку истории поверх мира{worldTitle ? ` «${worldTitle}»` : ""} с
              архитектором. Черновик сохраняется автоматически на каждом шаге.
            </p>
          </div>
        </div>
        <span className={`world-studio-chip${ready ? " ready" : ""}`}>
          {ready ? "готова к запуску" : "черновик не готов"}
        </span>
      </header>

      <div className="world-studio-body">
        <ArchitectChatPane
          headKicker="архитектор"
          headTitle="Собрать сюжет"
          usageTitle="Токены архитектора истории"
          helpTitle="Архитектор истории"
          helpSubtitle="Отдельный AI-контур до старта игры."
          helpNote="Он собирает завязку одной истории поверх готового мира: story brief, публичное вступление, скрытую правду GM, стартовую сцену, NPC, факты и предлагаемого протагониста. Без глав, целей и концовок — их движок пока не отслеживает."
          thinkLabel="🧠 Архитектор рассуждает"
          placeholder="Например: интрига в портовом городе, где пропадают досмотрщики, а игрок — новый инспектор… (Enter — отправить)"
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
          locked={architectLocked}
        />

        <section
          className={`world-studio-pane world-inspector${ready ? " is-live" : ""}`}
          aria-label="Параметры истории"
        >
          <div className="world-inspector-head">
            <span className="world-inspector-kicker">сюжет</span>
            <b>{textValue(storyDraft.title) || "Без названия"}</b>
          </div>

          <div className="world-inspector-body">
            <label className="world-field">
              <span>Название истории</span>
              <input
                value={storyDraft.title}
                onChange={(event) => updateDraft("title", event.target.value)}
                placeholder="Например: Досмотр в Соляном порту"
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Короткое описание (для списка историй)</span>
              <input
                value={storyDraft.description}
                onChange={(event) => updateDraft("description", event.target.value)}
                placeholder="Одна строка для списка историй."
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Завязка для игрока (story brief)</span>
              <AutoTextarea
                value={storyDraft.story_brief}
                onChange={(event) => updateDraft("story_brief", event.target.value)}
                placeholder="Кто игрок и что втягивает его в историю (несколько предложений)."
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Публичное вступление</span>
              <AutoTextarea
                value={storyDraft.public_intro}
                onChange={(event) => updateDraft("public_intro", event.target.value)}
                placeholder="Что игрок видит и знает в начале — без секретов GM."
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Скрытая правда (секрет GM)</span>
              <AutoTextarea
                value={storyDraft.hidden_truth}
                onChange={(event) => updateDraft("hidden_truth", event.target.value)}
                placeholder="То, что стоит за историей и чего игрок не должен узнать напрямую."
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Стартовое время (минуты с полуночи, напр. 480 = 08:00)</span>
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
                <p className="world-bible-hint">Предлагаемый протагонист (игрок сможет заменить его при запуске).</p>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>Имя</span>
                    <input
                      value={textValue(pc.name)}
                      onChange={(event) => updatePc("name", event.target.value)}
                      placeholder="Например: Мира"
                      disabled={locked}
                    />
                  </label>
                  <label className="world-field">
                    <span>Роль/архетип</span>
                    <input
                      value={textValue(pc.class_role)}
                      onChange={(event) => updatePc("class_role", event.target.value)}
                      placeholder="Например: морской досмотрщик"
                      disabled={locked}
                    />
                  </label>
                </div>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>Местоимения</span>
                    <input
                      value={textValue(pc.pronouns)}
                      onChange={(event) => updatePc("pronouns", event.target.value)}
                      placeholder="она/её"
                      disabled={locked}
                    />
                  </label>
                  <label className="world-field">
                    <span>Предыстория (одна строка)</span>
                    <input
                      value={textValue(pc.background)}
                      onChange={(event) => updatePc("background", event.target.value)}
                      placeholder="Что связывает героя с этой историей."
                      disabled={locked}
                    />
                  </label>
                </div>
              </div>
            </div>

            <div className="world-bible">
              <div className="world-bible-fields">
                <p className="world-bible-hint">Стартовая сцена — где игрок открывает историю.</p>
                <label className="world-field">
                  <span>Название сцены</span>
                  <input
                    value={textValue(scene.title)}
                    onChange={(event) => updateScene("title", event.target.value)}
                    placeholder="Например: Ворота Соляного порта"
                    disabled={locked}
                  />
                </label>
                <label className="world-field">
                  <span>Описание сцены</span>
                  <AutoTextarea
                    value={textValue(scene.description)}
                    onChange={(event) => updateScene("description", event.target.value)}
                    placeholder="Что игрок видит на старте — конкретно, сенсорно."
                    disabled={locked}
                  />
                </label>
                <div className="world-field-grid">
                  <label className="world-field">
                    <span>location_id</span>
                    <input
                      value={textValue(scene.location_id)}
                      onChange={(event) => updateScene("location_id", event.target.value)}
                      placeholder="salt_port_gate"
                      disabled={locked}
                    />
                  </label>
                  <label className="world-field">
                    <span>Напряжение сцены</span>
                    <input
                      value={textValue(scene.tension)}
                      onChange={(event) => updateScene("tension", event.target.value)}
                      placeholder="Что делает это сценой, а не холлом."
                      disabled={locked}
                    />
                  </label>
                </div>
                {SCENE_LIST_FIELDS.map(([field, label]) => (
                  <label key={field} className="world-field">
                    <span>{label}</span>
                    <AutoTextarea
                      value={listText(scene[field])}
                      onChange={(event) => updateSceneList(field, event.target.value)}
                      placeholder="по пункту на строку"
                      disabled={locked}
                    />
                  </label>
                ))}
              </div>
            </div>

            <label className="world-field">
              <span>Собственные имена (по строке)</span>
              <AutoTextarea
                value={listText(storyDraft.proper_nouns)}
                onChange={(event) => updateProperNouns(event.target.value)}
                placeholder="Имена, которые нужно писать единообразно — по одному на строку."
                disabled={locked}
              />
            </label>

            {OBJECT_LIST_SECTIONS.map(([field, label, summarize]) => {
              const entries = asArray(storyDraft[field]);
              if (entries.length === 0) return null;
              return (
                <div key={field} className="world-bible">
                  <div className="world-bible-fields">
                    <Spoiler label={`${label} · ${entries.length}`}>
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
                <button
                  type="button"
                  className="btn primary"
                  onClick={() => onPlayStory?.(currentStoryId)}
                  disabled={locked || !ready}
                >
                  ▶ Запустить историю
                </button>
              </div>
            )}
            <p className="world-manager-note">
              Черновик сохраняется автоматически на каждом ответе архитектора. Для запуска нужны
              story brief и публичное вступление. Запуск открывает игровой чат по этой истории.
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
