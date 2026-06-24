import { useMemo, useState } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";

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

export default function WorldArchitectPanel({ locked, onCreateWorld, onArchitectTurn, className = "" }) {
  const [worldDraft, setWorldDraft] = useState(DEFAULT_WORLD_DRAFT);
  const [messages, setMessages] = useState([
    {
      role: "assistant",
      content: "Опиши мир, который хочешь получить: жанр, настроение, масштаб, что точно должно быть и чего нельзя добавлять.",
    },
  ]);
  const [input, setInput] = useState("");
  const [architectBusy, setArchitectBusy] = useState(false);
  const [architectError, setArchitectError] = useState("");
  const worldPayload = useMemo(() => cleanWorldDraft(worldDraft), [worldDraft]);
  const loreRows = useMemo(() => lorePreviewRows(worldPayload.worldLore), [worldPayload.worldLore]);
  const worldCreateLocked =
    locked ||
    !worldPayload.title ||
    !worldPayload.genre ||
    !worldPayload.tone ||
    !worldPayload.scale ||
    !worldPayload.worldLore;
  const architectLocked = locked || architectBusy;

  const updateWorldDraft = (field, value) => {
    setWorldDraft((current) => ({ ...current, [field]: value }));
  };

  const applyPreset = (preset) => {
    setWorldDraft((current) => applyPresetValues(current, preset));
  };

  const submitWorld = (event) => {
    event.preventDefault();
    if (worldCreateLocked) return;
    onCreateWorld?.(worldPayload);
  };

  const sendArchitectMessage = async (event) => {
    event.preventDefault();
    const text = input.trim();
    if (!text || architectLocked) return;
    const history = messages;
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
      });
      if (!data?.ok) throw new Error(data?.error || "Архитектор не ответил");
      const reply = textValue(data.reply) || "Черновик мира обновлён.";
      if (data.draft && typeof data.draft === "object") {
        setWorldDraft((current) => mergeArchitectDraft(current, data.draft));
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
    <form className={`world-manager${className ? ` ${className}` : ""}`} onSubmit={submitWorld}>
      <div className="world-manager-left">
        <section className="world-architect" aria-label="Чат с архитектором мира">
          <div className="world-architect-head">
            <div>
              <span>архитектор</span>
              <b>Собрать лор мира</b>
            </div>
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

      <div className="world-manager-right">
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

        {loreRows.length > 0 && (
          <section className="world-lore-preview" aria-label="Черновик библии мира">
            <div className="world-lore-preview-head">
              <span>библия мира</span>
              <b>{textValue(worldPayload.worldLore?.name) || worldPayload.title}</b>
            </div>
            <div className="world-lore-preview-rows">
              {loreRows.slice(0, 9).map(([label, value]) => (
                <div key={label} className="world-lore-preview-row">
                  <span>{label}</span>
                  <p>{value}</p>
                </div>
              ))}
            </div>
          </section>
        )}

        <button type="submit" className="btn primary chat-new" disabled={worldCreateLocked}>
          Создать мир и чат
        </button>
        <p className="world-manager-note">
          Создание доступно только после черновика архитектора: библия мира станет рамками канона для GM и генератора локаций.
        </p>
      </div>
    </form>
  );
}
