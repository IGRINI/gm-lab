import { useContext, useEffect, useMemo, useState } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import Modal from "./Modal.jsx";
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
      scale: "outpost",
      storyBrief: "Ты приходишь к форпосту у старого машинного узла, где вода, энергия и доступ к закрытым дверям важнее любых монет.",
      publicIntro: "Выжившие спорят за право пользоваться старым узлом. Машины вокруг не молчат окончательно: одни служат, другие охраняют забытые протоколы.",
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
      scale: "village",
      storyBrief: "Ты оказываешься в чужом мире у деревни, где имя, клятва и долг перед духами значат больше, чем сила оружия.",
      publicIntro: "Местные ждут знака от святилища, боятся чужаков и шепчутся, что старый договор с духами снова нарушен.",
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
      scale: "town",
      storyBrief: "Ты прибываешь в пограничный городок, где дороги важнее стен, а каждый слух может открыть путь к чужому долгу.",
      publicIntro: "Поселение живёт торговлей, старыми договорами и страхом перед местами, которые снова начали просыпаться.",
    },
  },
];

const DEFAULT_WORLD_DRAFT = {
  title: "",
  seed: "",
  genre: "fantasy",
  tone: "tense",
  scale: "village",
  storyBrief: "",
  publicIntro: "",
  worldLore: null,
};

const LORE_PREVIEW_FIELDS = [
  ["dogmas", "Догматы"],
  ["world_laws", "Законы мира"],
  ["regions", "Регионы"],
  ["power_centers", "Власть"],
  ["religions", "Вера"],
  ["gods", "Боги/силы"],
  ["cultures", "Культуры"],
  ["history", "История"],
  ["economy", "Экономика"],
  ["daily_life", "Быт"],
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
    seed: textValue(draft.seed),
    genre: textValue(draft.genre),
    tone: textValue(draft.tone),
    scale: textValue(draft.scale),
    storyBrief: textValue(draft.storyBrief),
    publicIntro: textValue(draft.publicIntro),
    worldLore: draft.worldLore && typeof draft.worldLore === "object" ? draft.worldLore : null,
  };
}

function mergeArchitectDraft(current, draft) {
  if (!draft || typeof draft !== "object") return current;
  const lore = draft.world_lore && typeof draft.world_lore === "object" ? draft.world_lore : null;
  return {
    ...current,
    title: textValue(draft.title) || current.title,
    genre: textValue(draft.genre) || current.genre,
    tone: textValue(draft.tone) || current.tone,
    scale: textValue(draft.scale) || current.scale,
    storyBrief: textValue(draft.story_brief) || current.storyBrief,
    publicIntro: textValue(draft.public_intro) || current.publicIntro,
    worldLore: lore ? normalizeWorldLore(lore, draft) : current.worldLore,
  };
}

function normalizeWorldLore(lore, draft) {
  const next = { ...lore };
  if (!textValue(next.name)) next.name = textValue(draft.title);
  if (!textValue(next.genre)) next.genre = textValue(draft.genre);
  if (!textValue(next.tone)) next.tone = textValue(draft.tone);
  if (!textValue(next.scale)) next.scale = textValue(draft.scale);
  if (!textValue(next.public_premise)) next.public_premise = textValue(draft.public_intro);
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
    seed: current.seed,
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
  return Array.isArray(lore?.[field]) ? lore[field].join("\n") : "";
}

// Build the final world_lore object on submit: clean list fields (trim + drop
// empties), keep a non-empty hidden premise, and backfill name/genre/tone/scale/
// public premise from the top-level draft so a fully manual world is still valid.
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
  const hidden = textValue(lore.hidden_premise);
  if (hidden) lore.hidden_premise = hidden;
  else delete lore.hidden_premise;
  if (!textValue(lore.public_premise) && textValue(payload.publicIntro)) {
    lore.public_premise = textValue(payload.publicIntro);
  }
  if (!textValue(lore.name)) lore.name = textValue(payload.title);
  if (!textValue(lore.genre)) lore.genre = textValue(payload.genre);
  if (!textValue(lore.tone)) lore.tone = textValue(payload.tone);
  if (!textValue(lore.scale)) lore.scale = textValue(payload.scale);
  return lore;
}

export default function WorldArchitectPanel({ locked, onCreateWorld, onArchitectTurn, className = "" }) {
  const [architectCache, setArchitectCache] = useState(() => {
    const id = createArchitectCacheId();
    return { sessionId: id, threadId: id };
  });
  const [worldDraft, setWorldDraft] = useState(DEFAULT_WORLD_DRAFT);
  const [messages, setMessages] = useState([
    {
      role: "assistant",
      content:
        "Можешь просто описать мир в свободной форме — или ответь прямо на вопросы:\n\n1. Жанр и сеттинг? (тёмное фэнтези, киберпанк, иссекай…)\n2. Настроение и тон? (мрачный, героический, ироничный)\n3. Масштаб старта? (деревня, городок, регион)\n4. Что в мире точно должно быть?\n5. Чего быть не должно ни в коем случае?\n6. Главный скрытый конфликт или тайна мира?\n\nОтвечай как удобно — одним абзацем или по пунктам. Поля справа можно заполнить и вручную.",
    },
  ]);
  const [modelMessages, setModelMessages] = useState([]);
  const [input, setInput] = useState("");
  const [architectBusy, setArchitectBusy] = useState(false);
  const [architectError, setArchitectError] = useState("");
  const [bibleOpen, setBibleOpen] = useState(false);
  const [architectUsage, setArchitectUsage] = useState(EMPTY_ARCHITECT_USAGE);
  const [architectDebug, setArchitectDebug] = useState(null);
  const [debugOpen, setDebugOpen] = useState(false);
  const vis = useContext(VisibilityContext);
  const worldPayload = useMemo(() => cleanWorldDraft(worldDraft), [worldDraft]);
  const loreFilled = useMemo(() => loreHasContent(worldPayload.worldLore), [worldPayload.worldLore]);
  // Creatable manually too: the basics plus either a public premise or any lore.
  const loreReady = !!textValue(worldPayload.publicIntro) || loreFilled;
  const worldCreateLocked =
    locked ||
    !worldPayload.title ||
    !worldPayload.genre ||
    !worldPayload.tone ||
    !worldPayload.scale ||
    !loreReady;
  const architectLocked = locked || architectBusy;

  // Reveal the bible editor the first time real lore appears (architect draft or
  // manual entry); the user can still collapse it afterwards.
  useEffect(() => {
    if (loreFilled) setBibleOpen(true);
  }, [loreFilled]);

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
    setInput("");
    setArchitectError("");
    setArchitectBusy(true);
    setMessages((current) => [...current, userMessage]);
    try {
      const data = await onArchitectTurn?.({
        message: text,
        history,
        draft: worldPayload,
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
      setMessages((current) => [...current, { role: "assistant", content: reply }]);
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
              Соберите лор с архитектором или заполните вручную — затем создайте мир и первый чат.
            </p>
          </div>
        </div>
        <span className={`world-studio-chip${worldCreateLocked ? "" : " ready"}`}>
          {worldCreateLocked ? "черновик не готов" : "готово к созданию"}
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
            {messages.map((message, index) => (
              <div key={`${message.role}-${index}`} className={`world-architect-msg ${message.role}`}>
                {message.content}
              </div>
            ))}
            {architectBusy && <div className="world-architect-msg assistant">Думаю над черновиком...</div>}
          </div>
          <div className="world-architect-input-row">
            <textarea
              value={input}
              onChange={(event) => setInput(event.target.value)}
              placeholder="Например: хочу тёмный иссекай про клятвы, богов-должников и живые дороги..."
              rows={3}
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
              <span className="world-inspector-label">Быстрый старт</span>
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
              <span>Название истории</span>
              <input
                value={worldDraft.title}
                onChange={(event) => updateWorldDraft("title", event.target.value)}
                placeholder="Например: Пепельный Узел"
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

            <div className="world-field-grid">
              <label className="world-field">
                <span>Масштаб</span>
                <select
                  value={worldDraft.scale}
                  onChange={(event) => updateWorldDraft("scale", event.target.value)}
                  disabled={locked}
                >
                  <option value="village">Деревня</option>
                  <option value="town">Городок</option>
                  <option value="city">Город</option>
                  <option value="outpost">Форпост</option>
                  <option value="region">Регион</option>
                </select>
              </label>
              <label className="world-field">
                <span>Seed</span>
                <input
                  value={worldDraft.seed}
                  onChange={(event) => updateWorldDraft("seed", event.target.value)}
                  placeholder="пусто = случайный"
                  disabled={locked}
                />
              </label>
            </div>

            <label className="world-field">
              <span>Бриф для игрока</span>
              <textarea
                value={worldDraft.storyBrief}
                onChange={(event) => updateWorldDraft("storyBrief", event.target.value)}
                placeholder="Коротко: где игрок, что произошло и почему ему есть дело."
                rows={4}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Публичная завязка мира</span>
              <textarea
                value={worldDraft.publicIntro}
                onChange={(event) => updateWorldDraft("publicIntro", event.target.value)}
                placeholder="Что известно о мире без скрытых секретов GM."
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
              Создать мир и чат
            </button>
            <p className="world-manager-note">
              Нужны название, жанр, тон, масштаб и публичная завязка (или библия мира). Лор станет рамками канона для GM и генератора локаций.
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
