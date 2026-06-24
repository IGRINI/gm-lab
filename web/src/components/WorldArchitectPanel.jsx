import { useContext, useEffect, useMemo, useRef, useState } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import Modal from "./Modal.jsx";
import ToolCard from "./ToolCard.jsx";
import { fmtK } from "../util.js";
import { VisibilityContext } from "../devSettings.js";

const EMPTY_ARCHITECT_USAGE = { in: 0, out: 0, cached: 0, tokens: 0, calls: 0 };

const WORLD_PRESETS = [
  {
    id: "machine",
    label: "Машинный постапокалипсис",
    description: "Руины, автономные узлы, вода, энергия, дроны и выжившие общины.",
    values: {
      title: "Пепельный Узел",
      genre: "postapocalyptic machine world",
      tone: "bleak",
      worldSize: "Один большой регион вокруг цепочки старых машинных узлов; за его пределами мир шире, но связь и дороги ненадежны.",
      population: "Сотни тысяч выживших, разбитых на общины, караванные союзы и технокульты.",
      publicPremise: "Люди живут вокруг старых узлов воды, энергии и ремонта. Машины не исчезли: одни служат, другие охраняют протоколы, которые никто уже не понимает.",
      worldLore: {
        world_laws: [
          "старые машины следуют протоколам, а не морали",
          "вода, энергия, детали и доступ к узлам важнее монет",
        ],
        inhabitants: ["выжившие общины", "технокульты", "караванщики", "автономные дроны"],
        regions: ["сеть узлов в пепельной зоне", "сухие трассы между поселениями", "закрытые машинные сектора"],
        power_centers: ["советы водных общин", "ремонтные братства", "узловые культы"],
        location_rules: ["каждая локация должна показывать цену воды, энергии, деталей, доступа или сигнала"],
        prohibited_elements: ["классическая магия без объяснения как сбой, культ или чужеродный артефакт"],
      },
    },
  },
  {
    id: "isekai",
    label: "Фентезийный иссекай",
    description: "Клятвы, духи мест, цена магии, призванные чужаки и местные долги.",
    values: {
      title: "Порог Второго Неба",
      genre: "fantasy isekai",
      tone: "tense hopeful",
      worldSize: "Континент с несколькими королевствами, духами дорог и местными святилищами; игра может начинаться где угодно внутри него.",
      population: "Десятки миллионов людей, духовных родов, призванных чужаков и малых народов.",
      publicPremise: "Имя, клятва и долг имеют силу закона и магии. Призванные чужаки появляются редко, но почти всегда становятся частью старого договора.",
      worldLore: {
        dogmas: ["имя и клятва имеют юридическую и мистическую силу", "духи мест помнят долги лучше людей"],
        world_laws: ["магия требует имени, цены или признанного права", "дальняя дорога меняет слухи и баланс сил"],
        inhabitants: ["родовые дома", "духи мест", "призванные чужаки", "гильдии рунников"],
        regions: ["Семь земель под Осколочной Луной", "живые дороги", "пороговые святилища"],
        religions: ["культ дорожных духов", "официальная вера клятв"],
        gods: ["Старшие Духи Порогов"],
        location_rules: ["каждая новая локация должна иметь связь с долгом, властью, дорогой или духом места"],
        prohibited_elements: ["бесплатное воскрешение", "магия без имени, цены или договора"],
      },
    },
  },
  {
    id: "frontier",
    label: "Пограничье",
    description: "Дороги, фракции, слухи, старые места и поселения с реальной функцией.",
    values: {
      title: "Край Старых Дорог",
      genre: "frontier fantasy",
      tone: "tense",
      worldSize: "Один обжитой континент с королевствами, пограничными трактами, старыми местами и дальними землями за картой.",
      population: "Несколько миллионов жителей: города-государства, деревни, кочевые семьи, дорожные ордена и старые народы.",
      publicPremise: "Дороги важнее стен: слухи, караваны, старые договоры и опасные места связывают поселения сильнее любой короны.",
      worldLore: {
        conflicts: ["корона пытается подчинить дороги", "старые места снова просыпаются", "пограничные поселения спорят за пошлины"],
        regions: ["королевские тракты", "старые леса", "пограничные города", "забытые святилища"],
        power_centers: ["дорожные ордена", "городские советы", "караванные дома", "пограничная стража"],
        economy: ["пошлины", "караваны", "долги за охрану", "право прохода"],
        location_rules: ["каждая дорога должна иметь характер, цену, риск и тех, кто о ней знает"],
        prohibited_elements: ["случайные данжи без связи с дорогами, долгами или старым правом"],
      },
    },
  },
];

const DEFAULT_WORLD_DRAFT = {
  title: "",
  genre: "fantasy",
  tone: "tense",
  worldSize: "",
  population: "",
  publicPremise: "",
  worldLore: null,
};

const DEFAULT_ARCHITECT_MESSAGES = [
  {
    role: "assistant",
    content:
      "Опиши мир свободно — или дай направление, а детали я соберу сам.\n\nЧто особенно полезно:\n\n1. Жанр и настроение.\n2. Насколько большой мир и сколько в нём жителей примерно.\n3. Кто его населяет: люди, расы, виды, культуры, фракции.\n4. Какие законы реальности работают: магия, технологии, боги, смерть, дороги.\n5. Что в мире точно должно быть.\n6. Чего нельзя добавлять без причины.\n7. Какие скрытые истины должен знать только GM.\n\nСтартовую сцену, роль игрока и квест сейчас не придумываем — это будет отдельный шаг истории.",
  },
];

const LORE_PREVIEW_FIELDS = [
  ["dogmas", "Догматы"],
  ["world_laws", "Законы мира"],
  ["inhabitants", "Народы/виды"],
  ["creatures", "Существа/угрозы"],
  ["power_sources", "Силы/магия/технологии"],
  ["technologies", "Материальная культура"],
  ["taboos", "Табу/законы"],
  ["conflicts", "Конфликты"],
  ["inspirations", "Референсы"],
  ["regions", "Регионы"],
  ["power_centers", "Власть"],
  ["religions", "Вера"],
  ["gods", "Боги/силы"],
  ["cultures", "Культуры"],
  ["history", "История"],
  ["economy", "Экономика"],
  ["daily_life", "Быт"],
  ["story_hooks", "Напряжения для будущих историй"],
  ["hidden_secrets", "Секреты GM"],
  ["location_rules", "Правила локаций"],
  ["prohibited_elements", "Нельзя без причины"],
];

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function createArchitectCacheId() {
  const cryptoApi = globalThis.crypto;
  if (cryptoApi && typeof cryptoApi.randomUUID === "function") {
    return `world-architect:${cryptoApi.randomUUID()}`;
  }
  const time = Date.now().toString(36);
  const random = Math.random().toString(36).slice(2, 12);
  return `world-architect:${time}-${random}`;
}

function normalizeModelMessage(value) {
  if (!value || typeof value !== "object") return null;
  const role = textValue(value.role);
  if (role !== "user" && role !== "assistant") return null;
  const content = textValue(value.content);
  if (!content) return null;
  return { role, content };
}

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

function worldDraftFromSaved(world) {
  if (!world || typeof world !== "object") return { ...DEFAULT_WORLD_DRAFT };
  const isDraft = textValue(world.status) === "draft";
  const savedTitle = textValue(world.title);
  const title = isDraft && savedTitle === "Новый мир" ? "" : savedTitle;
  return {
    title: title || textValue(world.world_lore?.name),
    genre: textValue(world.genre) || DEFAULT_WORLD_DRAFT.genre,
    tone: textValue(world.tone) || DEFAULT_WORLD_DRAFT.tone,
    worldSize: textValue(world.world_size),
    population: textValue(world.population),
    publicPremise: textValue(world.public_premise) || textValue(world.world_lore?.public_premise),
    worldLore: world.world_lore && typeof world.world_lore === "object" ? world.world_lore : null,
  };
}

function normalizeVisibleMessage(value) {
  if (!value || typeof value !== "object") return null;
  const role = textValue(value.role);
  if (role === "tool") {
    const name = textValue(value.name);
    if (!name) return null;
    return {
      role: "tool",
      name,
      args: value.args && typeof value.args === "object" ? value.args : {},
    };
  }
  if (role !== "user" && role !== "assistant") return null;
  const content = textValue(value.content);
  if (!content) return null;
  return { role, content };
}

function architectMessagesFromWorld(world) {
  const messages = Array.isArray(world?.architect_messages)
    ? world.architect_messages.map(normalizeVisibleMessage).filter(Boolean)
    : [];
  return messages.length > 0 ? messages : DEFAULT_ARCHITECT_MESSAGES;
}

function modelMessagesFromWorld(world) {
  return Array.isArray(world?.architect_model_history)
    ? world.architect_model_history.map(normalizeModelMessage).filter(Boolean)
    : [];
}

function architectCacheFromWorld(world) {
  const sessionId = textValue(world?.architect_cache_session_id);
  const threadId = textValue(world?.architect_cache_thread_id);
  if (sessionId || threadId) {
    const fallback = sessionId || threadId;
    return {
      sessionId: sessionId || fallback,
      threadId: threadId || fallback,
    };
  }
  const id = createArchitectCacheId();
  return { sessionId: id, threadId: id };
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

function lorePreviewRows(lore) {
  if (!lore || typeof lore !== "object") return [];
  const rows = [];
  if (textValue(lore.public_premise)) rows.push(["Публично", textValue(lore.public_premise)]);
  if (textValue(lore.hidden_premise)) rows.push(["Скрыто", textValue(lore.hidden_premise)]);
  for (const [field, label] of LORE_PREVIEW_FIELDS) {
    const values = loreArray(lore[field]);
    if (values.length > 0) rows.push([label, values.join("; ")]);
  }
  return rows;
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

export default function WorldArchitectPanel({ world, locked, onCreateWorld, onArchitectTurn, className = "" }) {
  const [architectCache, setArchitectCache] = useState(() => architectCacheFromWorld(world));
  const [worldDraft, setWorldDraft] = useState(() => worldDraftFromSaved(world));
  const [messages, setMessages] = useState(() => architectMessagesFromWorld(world));
  const [modelMessages, setModelMessages] = useState(() => modelMessagesFromWorld(world));
  const [input, setInput] = useState("");
  const [architectBusy, setArchitectBusy] = useState(false);
  const [architectError, setArchitectError] = useState("");
  const [bibleOpen, setBibleOpen] = useState(false);
  const [architectUsage, setArchitectUsage] = useState(EMPTY_ARCHITECT_USAGE);
  const [architectDebug, setArchitectDebug] = useState(null);
  const [debugOpen, setDebugOpen] = useState(false);
  const inputRef = useRef(null);
  const vis = useContext(VisibilityContext);
  const worldPayload = useMemo(() => cleanWorldDraft(worldDraft), [worldDraft]);
  const loreFilled = useMemo(() => loreHasContent(worldPayload.worldLore), [worldPayload.worldLore]);
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
  const architectLocked = locked || architectBusy;

  useEffect(() => {
    const nextDraft = worldDraftFromSaved(world);
    setArchitectCache(architectCacheFromWorld(world));
    setWorldDraft(nextDraft);
    setMessages(architectMessagesFromWorld(world));
    setModelMessages(modelMessagesFromWorld(world));
    setInput("");
    setArchitectError("");
    setArchitectUsage(EMPTY_ARCHITECT_USAGE);
    setArchitectDebug(null);
    setDebugOpen(false);
    setBibleOpen(loreHasContent(nextDraft.worldLore));
  }, [world?.id]);

  // Reveal the bible editor the first time real lore appears (architect draft or
  // manual entry); the user can still collapse it afterwards.
  useEffect(() => {
    if (loreFilled) setBibleOpen(true);
  }, [loreFilled]);

  // Auto-grow the architect input with its content (CSS max-height caps it at
  // ~15 lines and switches to an inner scroll). Resets to one line on send.
  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [input]);

  const updateWorldDraft = (field, value) => {
    setWorldDraft((current) => ({ ...current, [field]: value }));
  };

  const applyPreset = (preset) => {
    setWorldDraft((current) => applyPresetValues(current, preset));
  };

  const updateWorldLore = (field, value) => {
    setWorldDraft((current) => {
      const lore = current.worldLore && typeof current.worldLore === "object" ? { ...current.worldLore } : {};
      lore[field] = value;
      return { ...current, worldLore: lore };
    });
  };
  const updateLoreText = (field, text) => updateWorldLore(field, text);
  const updateLoreList = (field, text) => updateWorldLore(field, text.split("\n"));

  const submitWorld = (event) => {
    event.preventDefault();
    if (worldCreateLocked) return;
    onCreateWorld?.({ ...worldPayload, worldLore: finalizeWorldLore(worldPayload) });
  };

  const sendArchitectMessage = async (event) => {
    event.preventDefault();
    const text = input.trim();
    if (!text || architectLocked) return;
    const history = modelMessages;
    const userMessage = { role: "user", content: text };
    const visibleMessages = [...messages, userMessage];
    setInput("");
    setArchitectError("");
    setArchitectBusy(true);
    setMessages(visibleMessages);
    try {
      const data = await onArchitectTurn?.({
        message: text,
        history,
        draft: worldPayload,
        visible_messages: visibleMessages,
        cache_session_id: architectCache.sessionId,
        cache_thread_id: architectCache.threadId,
      });
      if (!data?.ok) throw new Error(data?.error || "Архитектор не ответил");
      const reply = textValue(data.reply) || "Черновик мира обновлён.";
      const usage = data.usage && typeof data.usage === "object" ? data.usage : null;
      if (usage) {
        setArchitectUsage((current) => ({
          in: current.in + (Number(usage.in) || 0),
          out: current.out + (Number(usage.out) || 0),
          cached: current.cached + (Number(usage.cached) || 0),
          tokens: current.tokens + (Number(usage.tokens) || 0),
          calls: current.calls + 1,
        }));
      }
      setArchitectDebug({
        request: data.request_messages ?? null,
        response: data.assistant_message ?? null,
        thinking: textValue(data.thinking),
        stats: data.stats ?? null,
        calls: Array.isArray(data.calls) ? data.calls : [],
        usage,
      });
      const nextSessionId = textValue(data.cache_session_id);
      const nextThreadId = textValue(data.cache_thread_id);
      if (nextSessionId || nextThreadId) {
        setArchitectCache((current) => ({
          sessionId: nextSessionId || current.sessionId,
          threadId: nextThreadId || current.threadId,
        }));
      }
      if (data.draft && typeof data.draft === "object") {
        setWorldDraft((current) => mergeArchitectDraft(current, data.draft));
      }
      const modelUserMessage = normalizeModelMessage(data.user_message);
      const modelAssistantMessage =
        normalizeModelMessage(data.assistant_history_message) || { role: "assistant", content: reply };
      if (modelUserMessage) {
        setModelMessages((current) => [...current, modelUserMessage, modelAssistantMessage]);
      }
      // Surface tool calls (e.g. draft_world_bible) inline in the chat, like the
      // main GM chat — rendered only in debug mode (vis.toolCalls).
      const toolEntries = (Array.isArray(data.calls) ? data.calls : [])
        .filter((call) => textValue(call?.name))
        .map((call) => ({
          role: "tool",
          name: textValue(call.name),
          args: call.arguments && typeof call.arguments === "object" ? call.arguments : {},
        }));
      setMessages((current) => [...current, ...toolEntries, { role: "assistant", content: reply }]);
    } catch (error) {
      const message = error?.message || "Не удалось вызвать архитектора";
      setArchitectError(message);
      setMessages((current) => [...current, { role: "assistant", content: `Не получилось обновить мир: ${message}` }]);
    } finally {
      setArchitectBusy(false);
    }
  };

  return (
    <form className={`world-studio${className ? ` ${className}` : ""}`} onSubmit={submitWorld}>
      <header className="world-studio-head">
        <div className="world-studio-id">
          <span className="world-studio-emblem" aria-hidden="true">✦</span>
          <div className="world-studio-title">
            <span className="world-studio-kicker">создание мира</span>
            <b>Студия миров</b>
            <p className="world-studio-sub">
              Соберите лор и правила мира с архитектором или заполните вручную — без старта игрового чата.
            </p>
          </div>
        </div>
        <span className={`world-studio-chip${worldCreateLocked ? "" : " ready"}`}>
          {worldCreateLocked ? "черновик не готов" : "готово к сохранению"}
        </span>
      </header>

      <div className="world-studio-body">
        <section className="world-studio-pane world-architect" aria-label="Чат с архитектором мира">
          <div className="world-architect-head">
            <div className="world-architect-head-id">
              <span>архитектор</span>
              <b>Собрать лор мира</b>
            </div>
            <div className="world-architect-tools">
              {vis.tokenCards && architectUsage.calls > 0 && (
                <Tooltip
                  tipClassName="ui-tip-wrap"
                  content={
                    <TipContent
                      title="Токены архитектора"
                      subtitle={`вызовов: ${architectUsage.calls}`}
                      rows={[
                        ["ввод", `${architectUsage.in}`],
                        ["вывод", `${architectUsage.out}`],
                        ["кэш", `${architectUsage.cached}`],
                        ["всего", `${architectUsage.tokens}`],
                      ]}
                    />
                  }
                >
                  <span className="world-architect-usage">
                    {fmtK(architectUsage.tokens)} ток · кэш {fmtK(architectUsage.cached)}
                  </span>
                </Tooltip>
              )}
              {vis.historyDebug && (
                <button
                  type="button"
                  className="world-architect-debug"
                  onClick={() => setDebugOpen(true)}
                  disabled={!architectDebug}
                >
                  debug
                </button>
              )}
              <Tooltip
                tipClassName="ui-tip-wrap"
                focusable={false}
                content={
                  <TipContent
                    title="Архитектор мира"
                    subtitle="Отдельный AI-контур до старта игры."
                    note="Он задаёт вопросы и собирает библию мира: законы, веру, историю, регионы, власти, секреты и правила генерации локаций."
                  />
                }
              >
                <span className="world-architect-help" aria-hidden="true">?</span>
              </Tooltip>
            </div>
          </div>
          <div className="world-architect-log" aria-live="polite">
            {messages.map((message, index) => {
              if (message.role === "tool") {
                if (!vis.toolCalls) return null;
                return (
                  <ToolCard
                    key={`tool-${index}`}
                    name={message.name}
                    args={message.args}
                    mode="full"
                  />
                );
              }
              return (
                <div key={`${message.role}-${index}`} className={`world-architect-msg ${message.role}`}>
                  {message.content}
                </div>
              );
            })}
            {architectBusy && <div className="world-architect-msg assistant">Думаю над черновиком...</div>}
          </div>
          <div className="world-architect-input-row">
            <textarea
              ref={inputRef}
              value={input}
              onChange={(event) => setInput(event.target.value)}
              placeholder="Например: хочу тёмный иссекай про клятвы, богов-должников и живые дороги..."
              rows={2}
              disabled={architectLocked}
            />
            <button type="button" className="btn" onClick={sendArchitectMessage} disabled={architectLocked || !input.trim()}>
              Спросить
            </button>
          </div>
          {architectError && <div className="chat-sidebar-error inline">{architectError}</div>}
        </section>

        <section
          className={`world-studio-pane world-inspector${loreReady ? " is-live" : ""}`}
          aria-label="Параметры мира"
        >
          <div className="world-inspector-head">
            <span className="world-inspector-kicker">параметры</span>
            <b>{textValue(worldPayload.worldLore?.name) || worldDraft.title || "Без названия"}</b>
          </div>

          <div className="world-inspector-body">
            <div className="world-inspector-section">
              <span className="world-inspector-label">Пресеты мира</span>
              <div className="world-manager-presets" aria-label="Быстрые пресеты мира">
                {WORLD_PRESETS.map((preset) => (
                  <button
                    key={preset.id}
                    type="button"
                    className="world-preset"
                    onClick={() => applyPreset(preset)}
                    disabled={locked}
                  >
                    <b>{preset.label}</b>
                    <span>{preset.description}</span>
                  </button>
                ))}
              </div>
            </div>

            <label className="world-field">
              <span>Название мира</span>
              <input
                value={worldDraft.title}
                onChange={(event) => updateWorldDraft("title", event.target.value)}
                placeholder="Например: Порог Второго Неба"
                disabled={locked}
              />
            </label>

            <div className="world-field-grid">
              <label className="world-field">
                <span>Жанр</span>
                <input
                  value={worldDraft.genre}
                  onChange={(event) => updateWorldDraft("genre", event.target.value)}
                  placeholder="fantasy isekai"
                  disabled={locked}
                />
              </label>
              <label className="world-field">
                <span>Тон</span>
                <input
                  value={worldDraft.tone}
                  onChange={(event) => updateWorldDraft("tone", event.target.value)}
                  placeholder="tense"
                  disabled={locked}
                />
              </label>
            </div>

            <label className="world-field">
              <span>Размер мира</span>
              <textarea
                value={worldDraft.worldSize}
                onChange={(event) => updateWorldDraft("worldSize", event.target.value)}
                placeholder="Например: один континент; школа внутри большого магического общества; сектор галактики с десятками планет."
                rows={3}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Население</span>
              <input
                value={worldDraft.population}
                onChange={(event) => updateWorldDraft("population", event.target.value)}
                placeholder="Например: десятки миллионов, 5 разумных видов, сотни культур."
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Публичное описание мира</span>
              <textarea
                value={worldDraft.publicPremise}
                onChange={(event) => updateWorldDraft("publicPremise", event.target.value)}
                placeholder="Что можно безопасно рассказать игроку о мире без стартового квеста и скрытых секретов GM."
                rows={4}
                disabled={locked}
              />
            </label>

            <div className="world-bible">
              <button
                type="button"
                className="world-bible-toggle"
                onClick={() => setBibleOpen((open) => !open)}
                aria-expanded={bibleOpen}
                disabled={locked}
              >
                <span className="world-bible-toggle-label">
                  <b>Библия мира</b>
                  <small>{loreFilled ? "заполнена — можно править" : "вручную или через архитектора"}</small>
                </span>
                <span className="world-bible-caret" aria-hidden="true">{bibleOpen ? "▾" : "▸"}</span>
              </button>
              {bibleOpen && (
                <div className="world-bible-fields">
                  <p className="world-bible-hint">
                    Заполни сам или дождись архитектора. Каждый пункт — с новой строки.
                  </p>
                  <label className="world-field">
                    <span>Скрытая предпосылка (секрет GM)</span>
                    <textarea
                      value={worldDraft.worldLore?.hidden_premise || ""}
                      onChange={(event) => updateLoreText("hidden_premise", event.target.value)}
                      placeholder="То, что знает только GM и чего не должен знать игрок."
                      rows={2}
                      disabled={locked}
                    />
                  </label>
                  {LORE_PREVIEW_FIELDS.map(([field, label]) => (
                    <label key={field} className="world-field">
                      <span>{label}</span>
                      <textarea
                        value={loreFieldText(worldDraft.worldLore, field)}
                        onChange={(event) => updateLoreList(field, event.target.value)}
                        placeholder="по пункту на строку"
                        rows={2}
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
              Сохранить мир
            </button>
            <p className="world-manager-note">
              Нужны название, жанр, тон, размер мира, население и публичное описание или библия мира. Сохранение не запускает чат.
            </p>
          </div>
        </section>
      </div>

      {debugOpen && architectDebug && (
        <Modal
          title="Debug · архитектор"
          subtitle="последний вызов модели"
          wide
          onClose={() => setDebugOpen(false)}
        >
          <div className="arch-debug">
            <section className="arch-debug-sec">
              <h4>Токены</h4>
              <div className="arch-debug-usage">
                <span>ввод <b>{architectDebug.usage?.in ?? "—"}</b></span>
                <span>вывод <b>{architectDebug.usage?.out ?? "—"}</b></span>
                <span>кэш <b>{architectDebug.usage?.cached ?? "—"}</b></span>
                <span>всего <b>{architectDebug.usage?.tokens ?? "—"}</b></span>
              </div>
            </section>
            {architectDebug.thinking && (
              <section className="arch-debug-sec">
                <h4>Рассуждение</h4>
                <pre>{architectDebug.thinking}</pre>
              </section>
            )}
            <section className="arch-debug-sec">
              <h4>Ответ модели</h4>
              <pre>{JSON.stringify(architectDebug.response, null, 2)}</pre>
            </section>
            {architectDebug.calls.length > 0 && (
              <section className="arch-debug-sec">
                <h4>Tool calls</h4>
                <pre>{JSON.stringify(architectDebug.calls, null, 2)}</pre>
              </section>
            )}
            <section className="arch-debug-sec">
              <h4>Запрос (messages)</h4>
              <pre>{JSON.stringify(architectDebug.request, null, 2)}</pre>
            </section>
            <section className="arch-debug-sec">
              <h4>Stats (raw _meta)</h4>
              <pre>{JSON.stringify(architectDebug.stats, null, 2)}</pre>
            </section>
          </div>
        </Modal>
      )}
    </form>
  );
}
