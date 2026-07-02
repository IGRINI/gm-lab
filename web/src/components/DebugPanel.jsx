import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../api.js";
import MarkdownText from "./MarkdownText.jsx";
import Modal from "./Modal.jsx";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import { nameColor } from "../nameColor.js";

const actorColor = (actor, npcs) => {
  const a = String(actor || "").trim().toLowerCase();
  if (a === "player" || a === "игрок") return "var(--player)";
  if (a === "gm") return "var(--gm)";
  return nameColor(actor, npcs);
};

const FACT_KINDS = [
  { value: "public", label: "Публичный" },
  { value: "truth", label: "Скрытая правда" },
  { value: "rumor", label: "Слух" },
];
const factKindLabel = (kind) =>
  (FACT_KINDS.find((k) => k.value === kind) || {}).label || kind || "факт";

// state-record vocab mirrors world.py STATE_RECORD_KINDS / STATE_RECORD_SCOPES
const SR_KINDS = ["fact", "rumor", "npc_memory", "relationship", "goal"];
const SR_SCOPES = ["public", "gm", "owner", "subject", "participants"];

// --- tiny inline help bubble (ⓘ) -------------------------------------------
function Info({ children }) {
  return (
    <Tooltip content={children} className="dbg-info" tipClassName="dbg-tip">
      ⓘ
    </Tooltip>
  );
}

function ActionTip({ title, note, children }) {
  return (
    <Tooltip
      className="tooltip-wrap"
      tipClassName="ui-tip-wrap"
      focusable={false}
      content={<TipContent title={title} note={note} />}
    >
      {children}
    </Tooltip>
  );
}

// --- Управление бросками: следующий (одноразовый) + все (постоянный) ---
function RollsControls({ override, onRun }) {
  const [next, setNext] = useState("");
  const [all, setAll] = useState(override.all != null ? String(override.all) : "");
  return (
    <div className="dbg-form">
      <div className="dbg-block">
        <div className="dbg-block-head">
          <b>Следующий бросок</b>
          {override.next != null
            ? <span className="dbg-badge on">кубик = {override.next}</span>
            : <span className="dbg-badge">выкл</span>}
        </div>
        <p className="dbg-hint">Ближайший бросок выпадет этим значением кубика (модификаторы применятся), потом сбросится сам.</p>
        <div className="dbg-row">
          <input type="number" min="1" placeholder="напр. 20" value={next} onChange={(e) => setNext(e.target.value)} />
          <button type="button" className="btn primary" disabled={!next} onClick={() => { onRun({ next: Number(next) }); setNext(""); }}>Применить</button>
          <button type="button" className="btn" disabled={override.next == null} onClick={() => onRun({ next: null })}>Отменить</button>
        </div>
      </div>
      <div className="dbg-block">
        <div className="dbg-block-head">
          <b>Все броски</b>
          {override.all != null
            ? <span className="dbg-badge on">кубик = {override.all}</span>
            : <span className="dbg-badge">выкл</span>}
        </div>
        <p className="dbg-hint">Каждый бросок выпадает этим значением кубика, пока не выключишь.</p>
        <div className="dbg-row">
          <input type="number" min="1" placeholder="напр. 18" value={all} onChange={(e) => setAll(e.target.value)} />
          <button type="button" className="btn primary" disabled={!all} onClick={() => onRun({ all: Number(all) })}>Включить</button>
          <button type="button" className="btn" disabled={override.all == null} onClick={() => { onRun({ all: null }); setAll(""); }}>Выключить</button>
        </div>
      </div>
    </div>
  );
}

// --- Факты мира: добавить + список с удалением ---
function FactsManager({ facts, onAdd, onDelete }) {
  const [text, setText] = useState("");
  const [kind, setKind] = useState("public");
  return (
    <div className="dbg-form">
      <div className="dbg-block">
        <textarea className="dbg-textarea" rows={2} placeholder="Текст нового факта…" value={text} onChange={(e) => setText(e.target.value)} />
        <div className="dbg-row">
          <select value={kind} onChange={(e) => setKind(e.target.value)}>
            {FACT_KINDS.map((k) => <option key={k.value} value={k.value}>{k.label}</option>)}
          </select>
          <button type="button" className="btn primary" disabled={!text.trim()} onClick={() => { onAdd(text.trim(), kind); setText(""); }}>Добавить факт</button>
        </div>
      </div>
      <div className="dbg-fact-list">
        {facts.length ? facts.map((f) => (
          <div className={["dbg-fact", f.kind].join(" ")} key={f.id}>
            <span className={["dbg-fact-kind", f.kind].join(" ")}>{factKindLabel(f.kind)}</span>
            <span className="dbg-fact-text">{f.text}</span>
            <ActionTip title="Удалить факт" note="Факт будет убран из памяти мира.">
              <button type="button" className="icon-btn danger" onClick={() => onDelete(f.id)}>🗑</button>
            </ActionTip>
          </div>
        )) : <Empty>фактов пока нет</Empty>}
      </div>
    </div>
  );
}

// --- Записи состояния (durable state records) ---
function StateRecordsManager({ records, npcs, onApply }) {
  const [text, setText] = useState("");
  const [kind, setKind] = useState("fact");
  const [scope, setScope] = useState("public");
  const [entity, setEntity] = useState("");
  const add = () => {
    const record = { text: text.trim(), kind, scope };
    if (entity.trim()) record.entity_id = entity.trim();
    onApply({ add: [record] });
    setText(""); setEntity("");
  };
  return (
    <div className="dbg-form">
      <div className="dbg-block">
        <div className="dbg-block-head"><b>Новая запись</b><span className="dbg-badge">GM</span></div>
        <p className="dbg-hint">Долговременная память мира. Доходит до модели через query_world_state по выбранной видимости (scope). Кеш-префикс не трогает.</p>
        <textarea className="dbg-textarea" rows={2} placeholder="Текст записи состояния…" value={text} onChange={(e) => setText(e.target.value)} />
        <div className="dbg-row">
          <Tooltip
            className="tooltip-wrap"
            tipClassName="ui-tip-wrap"
            focusable={false}
            content={<TipContent title="Тип записи" note="Что именно сохраняется: факт, слух, память NPC, отношение или цель." />}
          >
            <select value={kind} onChange={(e) => setKind(e.target.value)} aria-label="Тип записи">
              {SR_KINDS.map((k) => <option key={k} value={k}>{k}</option>)}
            </select>
          </Tooltip>
          <Tooltip
            className="tooltip-wrap"
            tipClassName="ui-tip-wrap"
            focusable={false}
            content={<TipContent title="Видимость записи" note="Кому эта запись должна быть доступна при поиске памяти." />}
          >
            <select value={scope} onChange={(e) => setScope(e.target.value)} aria-label="Видимость записи">
              {SR_SCOPES.map((s) => <option key={s} value={s}>{s}</option>)}
            </select>
          </Tooltip>
          <input placeholder="entity_id (npc)" value={entity} list="dbg-npc-ids" onChange={(e) => setEntity(e.target.value)} />
          <button type="button" className="btn primary" disabled={!text.trim()} onClick={add}>Добавить</button>
        </div>
        <datalist id="dbg-npc-ids">{npcs.map((n) => <option key={n.id} value={n.id}>{n.name}</option>)}</datalist>
      </div>
      <div className="dbg-fact-list">
        {records.length ? records.map((r) => (
          <div className="dbg-fact" key={r.record_id || r.id}>
            <span className="dbg-fact-kind">{r.kind}/{r.scope}</span>
            <span className="dbg-fact-text">{r.text}{r.entity_id ? ` · ${r.entity_id}` : ""}</span>
            <ActionTip title="Удалить запись" note="Запись состояния больше не будет попадать в память модели.">
              <button type="button" className="icon-btn danger" onClick={() => onApply({ delete: [r.record_id || r.id] })}>🗑</button>
            </ActionTip>
          </div>
        )) : <Empty>записей нет</Empty>}
      </div>
    </div>
  );
}

// --- Слухи: добавить / подтвердить / удалить ---
function RumorsManager({ rumors, onAction }) {
  const [speaker, setSpeaker] = useState("");
  const [text, setText] = useState("");
  return (
    <div className="dbg-form">
      <div className="dbg-block">
        <div className="dbg-block-head"><b>Новый слух</b></div>
        <p className="dbg-hint">Доходит до ГМ через query_world_state (статус «неподтверждённое»). Кеш-префикс не трогает.</p>
        <textarea className="dbg-textarea" rows={2} placeholder="Текст слуха…" value={text} onChange={(e) => setText(e.target.value)} />
        <div className="dbg-row">
          <input placeholder="кто говорит (speaker)" value={speaker} onChange={(e) => setSpeaker(e.target.value)} />
          <button type="button" className="btn primary" disabled={!text.trim()} onClick={() => { onAction({ action: "add", speaker: speaker.trim(), text: text.trim() }); setText(""); setSpeaker(""); }}>Добавить</button>
        </div>
      </div>
      <div className="dbg-fact-list">
        {rumors.length ? rumors.map((r) => (
          <div className="dbg-fact" key={r.seq}>
            <span className={["dbg-fact-kind", r.confirmed ? "truth" : "rumor"].join(" ")}>{r.confirmed ? "подтв." : "слух"}</span>
            <span className="dbg-fact-text">{r.speaker ? `${r.speaker}: ` : ""}{r.text}</span>
            <button type="button" className="btn small" onClick={() => onAction({ action: "confirm", seq: r.seq, confirmed: !r.confirmed })}>{r.confirmed ? "снять" : "подтв."}</button>
            <ActionTip title="Удалить слух" note="Слух исчезнет из списка доступных мировых сведений.">
              <button type="button" className="icon-btn danger" onClick={() => onAction({ action: "delete", seq: r.seq })}>🗑</button>
            </ActionTip>
          </div>
        )) : <Empty>слухов нет</Empty>}
      </div>
    </div>
  );
}

// --- Список персонажей: клик -> правка ---
function NpcPicker({ npcs, onPick }) {
  if (!npcs.length) return <Empty>персонажей нет</Empty>;
  return (
    <div className="dbg-pick-list">
      {npcs.map((n) => (
        <button type="button" className="dbg-pick" key={n.id} onClick={() => onPick(n.id)}>
          <span className="dot" style={{ "--c": n.color || "var(--entity-unknown)" }} />
          <span className="dbg-pick-name" style={{ color: n.color || "var(--entity-unknown)" }}>{n.name}</span>
          <span className="dbg-pick-role">{n.role || "персонаж"}{n.present ? " · в сцене" : ""}</span>
          <span className="dbg-pick-go">✎</span>
        </button>
      ))}
    </div>
  );
}

function EditField({ label, children }) {
  return (
    <label className="dbg-edit-field">
      <span>{label}</span>
      {children}
    </label>
  );
}

// --- Правка сюжета и канона ---
function StoryEditor({ story, onSave }) {
  const [d, setD] = useState(() => ({
    title: story.title || "",
    story_brief: story.brief || story.story_brief || "",
    public_intro: story.public_intro || "",
    hidden_truth: story.hidden_truth || "",
    hidden_events: listText(story.hidden_events),
  }));
  const set = (patch) => setD((p) => ({ ...p, ...patch }));
  const introChanged = d.public_intro !== (story.public_intro || "");
  const save = () => {
    // hidden_truth/title/hidden_events never touch the cached prefix; public_intro does,
    // so only send it when it actually changed (avoids needless prefix re-caching).
    const body = {
      title: d.title,
      story_brief: d.story_brief,
      hidden_truth: d.hidden_truth,
      hidden_events: parseListField(d.hidden_events),
    };
    if (introChanged) body.public_intro = d.public_intro;
    onSave(body);
  };
  return (
    <div className="dbg-form">
      <EditField label="Название истории"><input value={d.title} onChange={(e) => set({ title: e.target.value })} /></EditField>
      <EditField label="Бриф для игрока">
        <textarea rows={4} value={d.story_brief} onChange={(e) => set({ story_brief: e.target.value })} />
      </EditField>
      <EditField label={<>Публичное интро <span className="dbg-warn">кеш-префикс</span></>}>
        <textarea rows={4} value={d.public_intro} onChange={(e) => set({ public_intro: e.target.value })} />
      </EditField>
      {introChanged && (
        <div className="dbg-danger-hint" role="alert">
          ⚠️ Интро лежит в кешируемом префиксе промпта. Сохранение пересоберёт префикс ОДИН раз
          (следующий ход дороже, дальше кеш снова тёплый). Остальные поля кеш не трогают.
        </div>
      )}
      <EditField label="Скрытая правда (секрет; в префикс не входит)"><textarea rows={4} className="dbg-secret" value={d.hidden_truth} onChange={(e) => set({ hidden_truth: e.target.value })} /></EditField>
      <EditField label="Скрытые события ГМ (по одному на строку)"><textarea rows={4} value={d.hidden_events} onChange={(e) => set({ hidden_events: e.target.value })} /></EditField>
      <div className="dbg-modal-actions">
        <button type="button" className="btn primary" onClick={save}>Сохранить</button>
      </div>
    </div>
  );
}

// scene_export uses item_id/exit_id; set_scene reads `id` — normalize so ids survive a round-trip.
function sceneItemsForEdit(items) {
  return asList(items).map((it) => ({
    id: it.item_id ?? it.id, name: it.name, location: it.location,
    visible: it.visible, portable: it.portable, owner: it.owner, details: it.details,
  }));
}
function sceneExitsForEdit(exits) {
  return asList(exits).map((ex) => ({
    id: ex.exit_id ?? ex.id, name: ex.name, destination: ex.destination,
    visible: ex.visible, blocked_by: ex.blocked_by,
  }));
}

// --- Правка сцены ---
function SceneEditor({ scene, npcs, onSave }) {
  const [d, setD] = useState(() => ({
    title: scene.title || "",
    location_id: scene.location_id || "",
    description: scene.description || "",
    tension: scene.tension || "",
    constraints: listText(scene.constraints),
    present: new Set(asList(scene.present_npcs)),
    items: prettyJson(sceneItemsForEdit(scene.items)),
    exits: prettyJson(sceneExitsForEdit(scene.exits)),
  }));
  const [editError, setEditError] = useState("");
  const set = (patch) => setD((p) => ({ ...p, ...patch }));
  const togglePresent = (id) => setD((p) => {
    const present = new Set(p.present);
    if (present.has(id)) present.delete(id); else present.add(id);
    return { ...p, present };
  });
  const save = () => {
    let patch;
    try {
      patch = {
        title: d.title,
        location_id: d.location_id,
        description: d.description,
        tension: d.tension,
        constraints: parseListField(d.constraints),
        present_npcs: Array.from(d.present),
        items: parseArrayField("Предметы", d.items),
        exits: parseArrayField("Выходы", d.exits),
      };
      setEditError("");
    } catch (e) {
      setEditError(e.message || String(e));
      return;
    }
    onSave(patch);
  };
  return (
    <div className="dbg-form">
      <div className="dbg-edit-grid">
        <EditField label="Название сцены"><input value={d.title} onChange={(e) => set({ title: e.target.value })} /></EditField>
        <EditField label="location_id"><input value={d.location_id} onChange={(e) => set({ location_id: e.target.value })} /></EditField>
      </div>
      <EditField label="Описание"><textarea rows={3} value={d.description} onChange={(e) => set({ description: e.target.value })} /></EditField>
      <EditField label="Напряжение"><textarea rows={2} value={d.tension} onChange={(e) => set({ tension: e.target.value })} /></EditField>
      <EditField label="Ограничения (по одному на строку)"><textarea rows={4} value={d.constraints} onChange={(e) => set({ constraints: e.target.value })} /></EditField>
      <div className="dbg-block">
        <div className="dbg-block-head"><b>Кто в сцене</b></div>
        <div className="dbg-check-grid">
          {npcs.length ? npcs.map((n) => (
            <label className="dbg-check" key={n.id}>
              <input type="checkbox" checked={d.present.has(n.id)} onChange={() => togglePresent(n.id)} />
              <span style={{ color: n.color || "var(--entity-unknown)" }}>{n.name}</span>
            </label>
          )) : <Empty>персонажей нет</Empty>}
        </div>
      </div>
      <div className="dbg-edit-grid">
        <EditField label={<>Предметы (JSON) <Info>Массив объектов: id, name, location, visible, portable, owner, details.</Info></>}>
          <textarea rows={6} value={d.items} onChange={(e) => set({ items: e.target.value })} />
        </EditField>
        <EditField label={<>Выходы (JSON) <Info>Массив объектов: id, name, destination, visible, blocked_by.</Info></>}>
          <textarea rows={6} value={d.exits} onChange={(e) => set({ exits: e.target.value })} />
        </EditField>
      </div>
      {editError && <div className="err">{editError}</div>}
      <div className="dbg-modal-actions">
        <button type="button" className="btn primary" onClick={save}>Сохранить</button>
      </div>
    </div>
  );
}

// --- Редактор карточки персонажа (полная карточка) ---
function NpcEditor({ npc, statusLabels, onSave }) {
  const mechanics = npc.mechanics || {};
  const [d, setD] = useState(() => ({
    name: npc.name || "", color: npc.color || "", role: npc.role || "", pronouns: npc.pronouns || "",
    public_label: npc.public_label || "", age: npc.age || "",
    physical_type: npc.physical_type || "", distinctive_features: npc.distinctive_features || "",
    life_status: npc.life_status || "alive", life_status_note: npc.life_status_note || "",
    condition: npc.condition || "",
    persona: npc.persona || "", personality: npc.personality || "", values: npc.values || "",
    habits: npc.habits || "", pressure_response: npc.pressure_response || "",
    boundaries: npc.boundaries || "", voice: npc.voice || "", goals: npc.goals || "",
    knowledge: npc.knowledge || "", secret: npc.secret || "",
    abilities: prettyJson(mechanics.abilities),
    skills: prettyJson(mechanics.skills),
    saving_throws: prettyJson(mechanics.saving_throws),
    passive_perception: mechanics.passive_perception != null ? String(mechanics.passive_perception) : "",
    ac: mechanics.ac != null ? String(mechanics.ac) : "",
    hp: prettyJson(mechanics.hp),
    speed: mechanics.speed || "",
    senses: mechanics.senses || "",
    languages: mechanics.languages || "",
    present: !!npc.present,
    wb_location: npc.whereabouts?.location_name || "",
    wb_status: npc.whereabouts?.status || "unknown",
    wb_details: npc.whereabouts?.details || "",
    reset_memory: false,
  }));
  const [editError, setEditError] = useState("");
  const set = (patch) => setD((p) => ({ ...p, ...patch }));
  const statusEntries = Object.entries(statusLabels || {});
  const secretChanged = d.secret !== (npc.secret || "");
  const presenceChanged = d.present !== !!npc.present;
  const save = () => {
    let fields;
    try {
      fields = {
        name: d.name, color: d.color, role: d.role, pronouns: d.pronouns,
        public_label: d.public_label, age: d.age,
        physical_type: d.physical_type, distinctive_features: d.distinctive_features,
        life_status: d.life_status, life_status_note: d.life_status_note,
        condition: d.condition, persona: d.persona, personality: d.personality,
        values: d.values, habits: d.habits, pressure_response: d.pressure_response,
        boundaries: d.boundaries, voice: d.voice, goals: d.goals, knowledge: d.knowledge,
        secret: d.secret, abilities: parseObjectField("Abilities", d.abilities),
        skills: parseObjectField("Skills", d.skills),
        saving_throws: parseObjectField("Saving throws", d.saving_throws),
        passive_perception: parseIntegerField("Passive Perception", d.passive_perception),
        ac: parseIntegerField("AC", d.ac), hp: parseObjectField("HP", d.hp),
        speed: d.speed, senses: d.senses, languages: d.languages,
      };
      setEditError("");
    } catch (e) {
      setEditError(e.message || String(e));
      return;
    }
    const body = {
      id: npc.id,
      fields,
      reset_memory: d.reset_memory,
    };
    // Only touch presence when the checkbox actually changed: a card-only edit
    // (persona/secret/voice/...) must never silently flip a hidden or non-hearing
    // NPC into visible+hearing. Whereabouts stay editable for an absent NPC.
    if (presenceChanged) body.present = d.present;
    if (!d.present) body.whereabouts = { location_name: d.wb_location, status: d.wb_status, details: d.wb_details };
    onSave(body);
  };
  return (
    <div className="dbg-form">
      <div className="dbg-edit-grid">
        <EditField label="Имя"><input value={d.name} onChange={(e) => set({ name: e.target.value })} /></EditField>
        <EditField label="Игрок видит"><input value={npc.player_label || npc.public_label || npc.name || ""} readOnly /></EditField>
        <EditField label="Цвет">
          <span className="dbg-color">
            <input type="color" value={/^#[0-9a-fA-F]{6}$/.test(d.color) ? d.color : "#908caa"} onChange={(e) => set({ color: e.target.value })} />
            <input value={d.color} placeholder="#e6c08a" onChange={(e) => set({ color: e.target.value })} />
          </span>
        </EditField>
        <EditField label="Роль"><input value={d.role} onChange={(e) => set({ role: e.target.value })} /></EditField>
        <EditField label="Род"><input value={d.pronouns} placeholder="M, F, N, PL, OTHER" onChange={(e) => set({ pronouns: e.target.value })} /></EditField>
        <EditField label="Публичный ярлык"><input value={d.public_label} placeholder="трактирщик" onChange={(e) => set({ public_label: e.target.value })} /></EditField>
        <EditField label="Известное имя"><input value={npc.known_name || ""} readOnly /></EditField>
        <EditField label="Возраст"><input value={d.age} onChange={(e) => set({ age: e.target.value })} /></EditField>
        <EditField label="Тип/размер/вид"><input value={d.physical_type} onChange={(e) => set({ physical_type: e.target.value })} /></EditField>
        <EditField label="Приметы"><input value={d.distinctive_features} onChange={(e) => set({ distinctive_features: e.target.value })} /></EditField>
        <EditField label="Состояние жизни"><input value={d.life_status} onChange={(e) => set({ life_status: e.target.value })} /></EditField>
        <EditField label="Заметка статуса"><input value={d.life_status_note} onChange={(e) => set({ life_status_note: e.target.value })} /></EditField>
        <EditField label="Текущее состояние"><input value={d.condition} onChange={(e) => set({ condition: e.target.value })} /></EditField>
      </div>

      <EditField label="Описание"><textarea rows={2} value={d.persona} onChange={(e) => set({ persona: e.target.value })} /></EditField>
      <EditField label="Характер"><textarea rows={2} value={d.personality} onChange={(e) => set({ personality: e.target.value })} /></EditField>
      <EditField label="Ценности"><textarea rows={2} value={d.values} onChange={(e) => set({ values: e.target.value })} /></EditField>
      <EditField label="Привычки"><textarea rows={2} value={d.habits} onChange={(e) => set({ habits: e.target.value })} /></EditField>
      <EditField label="Реакция на давление"><textarea rows={2} value={d.pressure_response} onChange={(e) => set({ pressure_response: e.target.value })} /></EditField>
      <EditField label="Границы"><textarea rows={2} value={d.boundaries} onChange={(e) => set({ boundaries: e.target.value })} /></EditField>
      <EditField label="Голос"><textarea rows={2} value={d.voice} onChange={(e) => set({ voice: e.target.value })} /></EditField>
      <EditField label="Мотивы"><textarea rows={2} value={d.goals} onChange={(e) => set({ goals: e.target.value })} /></EditField>
      <EditField label="Что знает"><textarea rows={2} value={d.knowledge} onChange={(e) => set({ knowledge: e.target.value })} /></EditField>
      <EditField label="Секрет (в контекст ГМ не попадает)"><textarea rows={2} className="dbg-secret" value={d.secret} onChange={(e) => set({ secret: e.target.value })} /></EditField>
      <div className="dbg-block">
        <div className="dbg-block-head"><b>Механика</b><span className="dbg-badge">GM only</span></div>
        <div className="dbg-edit-grid">
          <EditField label="Abilities JSON"><textarea rows={4} value={d.abilities} onChange={(e) => set({ abilities: e.target.value })} /></EditField>
          <EditField label="Skills JSON"><textarea rows={4} value={d.skills} onChange={(e) => set({ skills: e.target.value })} /></EditField>
          <EditField label="Saves JSON"><textarea rows={3} value={d.saving_throws} onChange={(e) => set({ saving_throws: e.target.value })} /></EditField>
          <EditField label="HP JSON"><textarea rows={3} value={d.hp} onChange={(e) => set({ hp: e.target.value })} /></EditField>
          <EditField label="Passive Perception"><input type="number" value={d.passive_perception} onChange={(e) => set({ passive_perception: e.target.value })} /></EditField>
          <EditField label="AC"><input type="number" value={d.ac} onChange={(e) => set({ ac: e.target.value })} /></EditField>
          <EditField label="Speed"><input value={d.speed} onChange={(e) => set({ speed: e.target.value })} /></EditField>
          <EditField label="Senses"><input value={d.senses} onChange={(e) => set({ senses: e.target.value })} /></EditField>
          <EditField label="Languages"><input value={d.languages} onChange={(e) => set({ languages: e.target.value })} /></EditField>
        </div>
      </div>
      {editError && <div className="err">{editError}</div>}
      {secretChanged && (
        <div className="dbg-danger-hint" role="alert">
          ⚠️ Смена секрета — опасная правка. Старая память NPC может конфликтовать с новой картой.
          Рекомендуется отметить «Сбросить память NPC» ниже (вручную, не автоматически).
        </div>
      )}

      <div className="dbg-block">
        <label className="dbg-check">
          <input type="checkbox" checked={d.present} onChange={(e) => set({ present: e.target.checked })} />
          <span>В текущей сцене</span>
        </label>
        {!d.present && (
          <div className="dbg-edit-grid">
            <EditField label="Где (место)"><input value={d.wb_location} onChange={(e) => set({ wb_location: e.target.value })} /></EditField>
            <EditField label="Статус">
              <select value={d.wb_status} onChange={(e) => set({ wb_status: e.target.value })}>
                {statusEntries.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
              </select>
            </EditField>
            <EditField label="Детали"><input value={d.wb_details} onChange={(e) => set({ wb_details: e.target.value })} /></EditField>
          </div>
        )}
      </div>

      <div className="dbg-danger-block">
        <label className="dbg-check dbg-danger">
          <input type="checkbox" checked={d.reset_memory} onChange={(e) => set({ reset_memory: e.target.checked })} />
          <span>🔥 Сбросить память NPC (история, компакт, тред) — необратимо</span>
        </label>
        {d.reset_memory && (
          <div className="dbg-danger-hint" role="alert">
            Будет удалена вся личная история этого NPC и поднят новый тред. Кеш и память только этого персонажа сгорят.
          </div>
        )}
      </div>

      <div className="dbg-modal-actions">
        <button type="button" className={"btn primary" + (d.reset_memory ? " danger" : "")} onClick={save}>
          {d.reset_memory ? "Сохранить и сбросить память" : "Сохранить"}
        </button>
      </div>
    </div>
  );
}

function PlayerEditor({ player, onSave }) {
  const [d, setD] = useState(() => ({
    name: player.name || "",
    pronouns: player.pronouns || "",
    class_role: player.class_role || "",
    level: player.level != null ? String(player.level) : "",
    background: player.background || "",
    age: player.age || "",
    physical_type: player.physical_type || "",
    distinctive_features: player.distinctive_features || "",
    life_status: player.life_status || "alive",
    life_status_note: player.life_status_note || "",
    condition: player.condition || "",
    personality: player.personality || "",
    values: player.values || "",
    gm_notes: player.gm_notes || "",
    abilities: prettyJson(player.abilities),
    skills: prettyJson(player.skills),
    saving_throws: prettyJson(player.saving_throws),
    passive_perception: player.passive_perception != null ? String(player.passive_perception) : "",
    ac: player.ac != null ? String(player.ac) : "",
    hp: prettyJson(player.hp),
    speed: player.speed || "",
    senses: player.senses || "",
    languages: player.languages || "",
    inventory: listText(player.inventory),
    equipment: listText(player.equipment),
    features: listText(player.features),
    spells: prettyJson(player.spells),
    spell_slots: prettyJson(player.spell_slots),
    spell_slots_max: prettyJson(player.spell_slots_max),
    concentration: player.concentration || "",
  }));
  const [editError, setEditError] = useState("");
  const set = (patch) => setD((p) => ({ ...p, ...patch }));
  const save = () => {
    let fields;
    try {
      fields = {
        name: d.name,
        pronouns: d.pronouns,
        class_role: d.class_role,
        level: parseIntegerField("Level", d.level),
        background: d.background,
        age: d.age,
        physical_type: d.physical_type,
        distinctive_features: d.distinctive_features,
        life_status: d.life_status,
        life_status_note: d.life_status_note,
        condition: d.condition,
        personality: d.personality,
        values: d.values,
        gm_notes: d.gm_notes,
        abilities: parseObjectField("Abilities", d.abilities),
        skills: parseObjectField("Skills", d.skills),
        saving_throws: parseObjectField("Saving throws", d.saving_throws),
        passive_perception: parseIntegerField("Passive Perception", d.passive_perception),
        ac: parseIntegerField("AC", d.ac),
        hp: parseObjectField("HP", d.hp),
        speed: d.speed,
        senses: d.senses,
        languages: d.languages,
        inventory: parseListField(d.inventory),
        equipment: parseListField(d.equipment),
        features: parseListField(d.features),
        spells: parseArrayField("Spells", d.spells),
        spell_slots: parseObjectField("Spell slots", d.spell_slots),
        spell_slots_max: parseObjectField("Spell slots max", d.spell_slots_max),
        concentration: d.concentration,
      };
      setEditError("");
    } catch (e) {
      setEditError(e.message || String(e));
      return;
    }
    onSave({ fields, reason: "debug edit" });
  };
  return (
    <div className="dbg-form">
      <div className="dbg-edit-grid">
        <EditField label="Имя"><input value={d.name} onChange={(e) => set({ name: e.target.value })} /></EditField>
        <EditField label="Род"><input value={d.pronouns} placeholder="M, F, N, PL, OTHER" onChange={(e) => set({ pronouns: e.target.value })} /></EditField>
        <EditField label="Класс/роль"><input value={d.class_role} onChange={(e) => set({ class_role: e.target.value })} /></EditField>
        <EditField label="Уровень"><input type="number" value={d.level} onChange={(e) => set({ level: e.target.value })} /></EditField>
        <EditField label="Предыстория"><input value={d.background} onChange={(e) => set({ background: e.target.value })} /></EditField>
        <EditField label="Возраст"><input value={d.age} onChange={(e) => set({ age: e.target.value })} /></EditField>
        <EditField label="Тип/вид"><input value={d.physical_type} onChange={(e) => set({ physical_type: e.target.value })} /></EditField>
        <EditField label="Приметы"><input value={d.distinctive_features} onChange={(e) => set({ distinctive_features: e.target.value })} /></EditField>
        <EditField label="Жизнь"><input value={d.life_status} onChange={(e) => set({ life_status: e.target.value })} /></EditField>
        <EditField label="Заметка статуса"><input value={d.life_status_note} onChange={(e) => set({ life_status_note: e.target.value })} /></EditField>
        <EditField label="Состояние"><input value={d.condition} onChange={(e) => set({ condition: e.target.value })} /></EditField>
      </div>
      <EditField label="Характер"><textarea rows={2} value={d.personality} onChange={(e) => set({ personality: e.target.value })} /></EditField>
      <EditField label="Ценности"><textarea rows={2} value={d.values} onChange={(e) => set({ values: e.target.value })} /></EditField>
      <EditField label="Заметки ГМ"><textarea rows={2} className="dbg-secret" value={d.gm_notes} onChange={(e) => set({ gm_notes: e.target.value })} /></EditField>
      <div className="dbg-block">
        <div className="dbg-block-head"><b>Механика игрока</b><span className="dbg-badge">sheet</span></div>
        <div className="dbg-edit-grid">
          <EditField label="Abilities JSON"><textarea rows={4} value={d.abilities} onChange={(e) => set({ abilities: e.target.value })} /></EditField>
          <EditField label="Skills JSON"><textarea rows={4} value={d.skills} onChange={(e) => set({ skills: e.target.value })} /></EditField>
          <EditField label="Saves JSON"><textarea rows={3} value={d.saving_throws} onChange={(e) => set({ saving_throws: e.target.value })} /></EditField>
          <EditField label="HP JSON"><textarea rows={3} value={d.hp} onChange={(e) => set({ hp: e.target.value })} /></EditField>
          <EditField label="Passive Perception"><input type="number" value={d.passive_perception} onChange={(e) => set({ passive_perception: e.target.value })} /></EditField>
          <EditField label="AC"><input type="number" value={d.ac} onChange={(e) => set({ ac: e.target.value })} /></EditField>
          <EditField label="Speed"><input value={d.speed} onChange={(e) => set({ speed: e.target.value })} /></EditField>
          <EditField label="Senses"><input value={d.senses} onChange={(e) => set({ senses: e.target.value })} /></EditField>
          <EditField label="Languages"><input value={d.languages} onChange={(e) => set({ languages: e.target.value })} /></EditField>
        </div>
      </div>
      <div className="dbg-edit-grid">
        <EditField label="Инвентарь"><textarea rows={4} value={d.inventory} onChange={(e) => set({ inventory: e.target.value })} /></EditField>
        <EditField label="Снаряжение"><textarea rows={4} value={d.equipment} onChange={(e) => set({ equipment: e.target.value })} /></EditField>
        <EditField label="Особенности"><textarea rows={4} value={d.features} onChange={(e) => set({ features: e.target.value })} /></EditField>
      </div>
      <div className="dbg-block">
        <div className="dbg-block-head"><b>Заклинания</b><span className="dbg-badge">spells</span></div>
        <EditField label="Spells JSON"><textarea rows={5} value={d.spells} onChange={(e) => set({ spells: e.target.value })} placeholder='[{"name":"Огненный снаряд","level":1,"concentration":false,"ritual":false,"effect":"..."}]' /></EditField>
        <div className="dbg-edit-grid">
          <EditField label="Spell slots JSON"><textarea rows={3} value={d.spell_slots} onChange={(e) => set({ spell_slots: e.target.value })} placeholder='{"1": 3, "2": 1}' /></EditField>
          <EditField label="Spell slots max JSON"><textarea rows={3} value={d.spell_slots_max} onChange={(e) => set({ spell_slots_max: e.target.value })} placeholder='{"1": 4, "2": 2}' /></EditField>
          <EditField label="Концентрация"><input value={d.concentration} onChange={(e) => set({ concentration: e.target.value })} /></EditField>
        </div>
      </div>
      {editError && <div className="err">{editError}</div>}
      <div className="dbg-modal-actions">
        <button type="button" className="btn primary" onClick={save}>Сохранить</button>
      </div>
    </div>
  );
}

function asList(items) {
  return Array.isArray(items) ? items.filter((item) => item != null && item !== "") : [];
}

function listText(items) {
  return asList(items).join("\n");
}

// Фаза С (ITEMS_AND_SPELLS_TZ §С3): read-only spell rendering helpers, shared
// by PlayerCard and WorldDetailModal.
function spellLevelLabel(level) {
  const n = Number(level);
  return Number.isFinite(n) && n > 0 ? `ур. ${n}` : "заговор";
}

function spellLine(sp) {
  if (!sp || typeof sp !== "object") return "";
  const marks = [sp.concentration ? "конц." : "", sp.ritual ? "ритуал" : ""].filter(Boolean);
  const head = `${sp.name || "—"} (${[spellLevelLabel(sp.level), ...marks].join(", ")})`;
  const effect = String(sp.effect || "").trim();
  return effect ? `${head}: ${effect}` : head;
}

// "1-й: 3/4, 2-й: 1/2" from the flat remaining/max slot maps; "" when no levels.
function slotsLine(slots, max) {
  const levels = new Set();
  for (const m of [slots, max]) {
    for (const k of Object.keys(m || {})) {
      const n = parseInt(k, 10);
      if (Number.isInteger(n) && n > 0) levels.add(n);
    }
  }
  const slotNum = (v) => { const n = Number(v); return Number.isFinite(n) ? n : 0; };
  return [...levels].sort((a, b) => a - b).map((lvl) => {
    const cur = slotNum((slots || {})[lvl]);
    const capRaw = (max || {})[lvl];
    const cap = capRaw == null ? "?" : slotNum(capRaw);
    return `${lvl}-й: ${cur}/${cap}`;
  }).join(", ");
}

function parseListField(value) {
  return String(value || "")
    .split(/\r?\n/)
    .map((item) => item.trim())
    .filter(Boolean);
}

function parseArrayField(label, value) {
  const raw = String(value || "").trim();
  if (!raw) return [];
  const parsed = JSON.parse(raw);
  if (!Array.isArray(parsed)) throw new Error(`${label}: нужен JSON-массив`);
  return parsed;
}

function prettyJson(value) {
  if (!value || (typeof value === "object" && !Array.isArray(value) && !Object.keys(value).length)) return "";
  if (Array.isArray(value) && !value.length) return "";
  return JSON.stringify(value, null, 2);
}

function parseObjectField(label, value) {
  const raw = String(value || "").trim();
  if (!raw) return {};
  const parsed = JSON.parse(raw);
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error(`${label}: нужен JSON-объект`);
  }
  return parsed;
}

function parseIntegerField(label, value) {
  const raw = String(value || "").trim();
  if (!raw) return null;
  const parsed = Number(raw);
  if (!Number.isInteger(parsed)) throw new Error(`${label}: нужно целое число`);
  return parsed;
}

function Empty({ children = "пока пусто" }) {
  return <div className="debug-empty">{children}</div>;
}

function TextBlock({ children, secret = false }) {
  const text = String(children || "").trim();
  if (!text) return <Empty />;
  return (
    <div className={["debug-text", secret ? "secret" : ""].filter(Boolean).join(" ")}>
      <MarkdownText>{text}</MarkdownText>
    </div>
  );
}

function JsonBlock({ value }) {
  const cleaned = Object.fromEntries(
    Object.entries(value || {}).filter(([, item]) => {
      if (item == null || item === "") return false;
      if (Array.isArray(item)) return item.length > 0;
      if (typeof item === "object") return Object.keys(item).length > 0;
      return true;
    })
  );
  if (!Object.keys(cleaned).length) return <Empty />;
  return <pre className="debug-json">{JSON.stringify(cleaned, null, 2)}</pre>;
}

function DebugList({ items, secret = false }) {
  const list = asList(items);
  if (!list.length) return <Empty />;
  return (
    <ul className={["debug-list", secret ? "secret" : ""].filter(Boolean).join(" ")}>
      {list.map((item, idx) => (
        <li key={idx}><MarkdownText>{String(item)}</MarkdownText></li>
      ))}
    </ul>
  );
}

// generic scalar key/value grid (used for usage/cache blocks of unknown shape)
function KVGrid({ obj }) {
  const rows = Object.entries(obj || {}).filter(([, v]) => v != null && typeof v !== "object");
  if (!rows.length) return <Empty />;
  return (
    <div className="debug-grid">
      {rows.map(([k, v]) => (
        <div key={k}><span>{k}</span><b>{String(v)}</b></div>
      ))}
    </div>
  );
}

function SceneSummary({ scene }) {
  if (!scene) return <Empty />;
  return (
    <div className="debug-grid">
      <div><span>сцена</span><b>{scene.title || scene.scene_id || "—"}</b></div>
      <div><span>локация</span><b>{scene.location_id || "—"}</b></div>
      <div><span>в сцене</span><b>{asList(scene.present_npcs).join(", ") || "нет"}</b></div>
      <div><span>напряжение</span><b>{scene.tension || "—"}</b></div>
    </div>
  );
}

function Facts({ facts, rumors }) {
  const groups = useMemo(() => {
    const rows = asList(facts);
    return {
      truth: rows.filter((fact) => fact.kind === "truth"),
      public: rows.filter((fact) => fact.kind === "public"),
      other: rows.filter((fact) => fact.kind !== "truth" && fact.kind !== "public"),
    };
  }, [facts]);

  return (
    <div className="debug-stack">
      <h4>Скрытая правда</h4>
      {groups.truth.length ? groups.truth.map((fact) => (
        <TextBlock key={fact.id} secret>{fact.text}</TextBlock>
      )) : <Empty />}

      <h4>Публичные факты</h4>
      {groups.public.length ? (
        <DebugList items={groups.public.map((fact) => fact.text)} />
      ) : <Empty />}

      <h4>Слухи и неподтверждённое</h4>
      {groups.other.length || asList(rumors).length ? (
        <>
          <DebugList items={groups.other.map((fact) => fact.text)} />
          <DebugList items={asList(rumors).map((rumor) => `${rumor.speaker}: ${rumor.text}`)} />
        </>
      ) : <Empty />}
    </div>
  );
}

function NpcCard({ npc, statusLabels = {}, onEdit }) {
  const status = npc.whereabouts?.status || (npc.present ? "present" : "unknown");
  const mechanics = npc.mechanics || {};
  return (
    <details className="debug-npc" open={npc.present}>
      <summary>
        <span>
          <b style={{ color: npc.color || "var(--entity-unknown)" }}>{npc.name}</b>
          <em>{npc.role || "персонаж"}{npc.player_label && npc.player_label !== npc.name ? ` · игрок видит: ${npc.player_label}` : ""}{npc.present ? " · в сцене" : ""}</em>
        </span>
        <span className="debug-npc-head-right">
          <button
            type="button"
            className="btn small"
            onClick={(e) => { e.preventDefault(); e.stopPropagation(); onEdit?.(); }}
          >✎ править</button>
          <small>{npc.messages || 0} сообщ.</small>
        </span>
      </summary>
      <div className="debug-grid">
        <div><span>id</span><b>{npc.id}</b></div>
        <div><span>игрок видит</span><b>{npc.player_label || npc.public_label || "—"}</b></div>
        <div><span>известное имя</span><b>{npc.known_name || "—"}</b></div>
        <div><span>публичный ярлык</span><b>{npc.public_label || "—"}</b></div>
        <div><span>род</span><b>{npc.pronouns || "—"}</b></div>
        <div><span>возраст</span><b>{npc.age || "—"}</b></div>
        <div><span>тип/вид</span><b>{npc.physical_type || "—"}</b></div>
        <div><span>приметы</span><b>{npc.distinctive_features || "—"}</b></div>
        <div><span>жизнь</span><b>{npc.life_status || "—"}</b></div>
        <div><span>статус жизни</span><b>{npc.life_status_note || "—"}</b></div>
        <div><span>состояние</span><b>{npc.condition || "—"}</b></div>
        <div><span>где</span><b>{npc.whereabouts?.location_name || npc.whereabouts?.location_id || "—"}</b></div>
        <div><span>статус</span><b>{statusLabels[status] || status}</b></div>
      </div>

      <h4>Личность</h4>
      <TextBlock>{npc.persona}</TextBlock>
      <DebugList items={[
        npc.personality && `Характер: ${npc.personality}`,
        npc.values && `Ценности: ${npc.values}`,
        npc.habits && `Привычки: ${npc.habits}`,
        npc.pressure_response && `Под давлением: ${npc.pressure_response}`,
        npc.boundaries && `Границы: ${npc.boundaries}`,
        npc.voice && `Голос: ${npc.voice}`,
      ].filter(Boolean)} />

      <h4>Мотивы</h4>
      <TextBlock>{npc.goals}</TextBlock>

      <h4>Что знает</h4>
      <TextBlock>{npc.knowledge}</TextBlock>

      <h4>Секрет</h4>
      <TextBlock secret>{npc.secret}</TextBlock>

      <h4>Механика</h4>
      <div className="debug-grid">
        <div><span>passive perception</span><b>{mechanics.passive_perception ?? "—"}</b></div>
        <div><span>AC</span><b>{mechanics.ac ?? "—"}</b></div>
        <div><span>speed</span><b>{mechanics.speed || "—"}</b></div>
        <div><span>senses</span><b>{mechanics.senses || "—"}</b></div>
        <div><span>languages</span><b>{mechanics.languages || "—"}</b></div>
      </div>
      <JsonBlock value={{
        abilities: mechanics.abilities,
        skills: mechanics.skills,
        saving_throws: mechanics.saving_throws,
        hp: mechanics.hp,
      }} />

      <h4>Память NPC</h4>
      <TextBlock>{npc.summary || npc.history}</TextBlock>

      <h4>Коммиты ответа</h4>
      <DebugList items={npc.commitments} />
    </details>
  );
}

function PlayerCard({ player, onEdit }) {
  if (!player) return <Empty>карточка игрока не загружена</Empty>;
  return (
    <details className="debug-npc debug-player" open>
      <summary>
        <span>
          <b>{player.name || "Персонаж игрока"}</b>
          <em>{[player.class_role, player.level != null ? `ур. ${player.level}` : ""].filter(Boolean).join(" · ") || "лист персонажа"}</em>
        </span>
        <span className="debug-npc-head-right">
          <button
            type="button"
            className="btn small"
            onClick={(e) => { e.preventDefault(); e.stopPropagation(); onEdit?.(); }}
          >✎ править</button>
          <small>rev {player.card_revision || 0}</small>
        </span>
      </summary>
      <div className="debug-grid">
        <div><span>род</span><b>{player.pronouns || "—"}</b></div>
        <div><span>предыстория</span><b>{player.background || "—"}</b></div>
        <div><span>возраст</span><b>{player.age || "—"}</b></div>
        <div><span>тип/вид</span><b>{player.physical_type || "—"}</b></div>
        <div><span>приметы</span><b>{player.distinctive_features || "—"}</b></div>
        <div><span>жизнь</span><b>{player.life_status || "—"}</b></div>
        <div><span>статус</span><b>{player.life_status_note || "—"}</b></div>
        <div><span>состояние</span><b>{player.condition || "—"}</b></div>
      </div>

      <h4>Характер</h4>
      <DebugList items={[
        player.personality && `Характер: ${player.personality}`,
        player.values && `Ценности: ${player.values}`,
      ].filter(Boolean)} />

      <h4>Механика</h4>
      <div className="debug-grid">
        <div><span>passive perception</span><b>{player.passive_perception ?? "—"}</b></div>
        <div><span>AC</span><b>{player.ac ?? "—"}</b></div>
        <div><span>speed</span><b>{player.speed || "—"}</b></div>
        <div><span>senses</span><b>{player.senses || "—"}</b></div>
        <div><span>languages</span><b>{player.languages || "—"}</b></div>
      </div>
      <JsonBlock value={{
        abilities: player.abilities,
        skills: player.skills,
        saving_throws: player.saving_throws,
        hp: player.hp,
      }} />

      <h4>Инвентарь</h4>
      <DebugList items={player.inventory} />

      <h4>Снаряжение и особенности</h4>
      <DebugList items={[...asList(player.equipment), ...asList(player.features)]} />

      {(asList(player.spells).length > 0
        || slotsLine(player.spell_slots, player.spell_slots_max)
        || String(player.concentration || "").trim()) && (
        <>
          <h4>Заклинания</h4>
          <DebugList items={asList(player.spells).map(spellLine).filter(Boolean)} />
          {slotsLine(player.spell_slots, player.spell_slots_max) && (
            <div className="debug-grid"><div><span>слоты</span><b>{slotsLine(player.spell_slots, player.spell_slots_max)}</b></div></div>
          )}
          {String(player.concentration || "").trim() && (
            <div className="debug-grid"><div><span>концентрация</span><b>{player.concentration}</b></div></div>
          )}
        </>
      )}

      <h4>Заметки ГМ</h4>
      <TextBlock secret>{player.gm_notes}</TextBlock>
    </details>
  );
}

function Events({ events, npcs }) {
  const rows = asList(events).slice(-24).reverse();
  if (!rows.length) return <Empty />;
  return (
    <div className="debug-events">
      {rows.map((event) => (
        <div className="debug-event" key={`${event.seq}-${event.actor}-${event.kind}`}>
          <div>
            <b>#{event.seq}</b>
            <span>ход {event.turn} · <b style={{ color: actorColor(event.actor, npcs) }}>{event.actor}</b> · {event.kind}</span>
          </div>
          <p>{event.speech || event.action || "—"}</p>
          <small>видели: {asList(event.witnesses).join(", ") || "—"}</small>
        </div>
      ))}
    </div>
  );
}

// --- Рантайм/кеш: только просмотр; настройки модели меняются в шапке «Настройки» ---
function RuntimeView({ meta, runtime }) {
  const cache = runtime?.cache || {};
  const s = runtime?.settings || {};
  return (
    <div className="debug-stack">
      <h4>Кеш токенов <Info>Кешируемый префикс промпта = системные правила + публичное интро (по prompt_cache_key). Правки сцены/NPC/фактов/записей/слухов идут в дописываемый ХВОСТ хода и не ломают префикс. Префикс пересобирается только при правке публичного интро.</Info></h4>
      <div className="debug-grid">
        <div><span>prompt_cache_key</span><b className="dbg-mono">{cache.prompt_cache_key || "—"}</b></div>
        <div><span>thread_id</span><b className="dbg-mono">{cache.thread_id || "—"}</b></div>
        <div><span>store</span><b>{String(cache.store ?? false)}</b></div>
        <div><span>ходов</span><b>{meta?.turns ?? "—"}</b></div>
      </div>

      <h4>Токены последнего прогона <Info>cached_tokens &gt; 0 означает, что префикс переиспользован из кеша.</Info></h4>
      <KVGrid obj={meta?.run_usage} />

      <h4>Контекст</h4>
      <KVGrid obj={meta?.context_usage} />

      <h4>Настройки модели <Info>Только просмотр. Меняются в шапке → кнопка «Настройки». Применяются со следующего хода (читаются заново на каждый запрос).</Info></h4>
      <div className="debug-grid">
        <div><span>модель</span><b>{meta?.model || "—"}</b></div>
        <div><span>бэкенд</span><b>{meta?.backend || "—"}</b></div>
        <div><span>GM reasoning</span><b>{(s.gm_reasoning_effort || "—") + " / " + (s.gm_reasoning_summary || "—")}</b></div>
        <div><span>NPC reasoning</span><b>{(s.npc_reasoning_effort || "—") + " / " + (s.npc_reasoning_summary || "—")}</b></div>
        <div><span>Compact reasoning</span><b>{(s.compact_reasoning_effort || "—") + " / " + (s.compact_reasoning_summary || "—")}</b></div>
        <div><span>verbosity</span><b>{s.text_verbosity || "—"}</b></div>
        <div><span>tool_choice</span><b>{s.tool_choice || "—"}</b></div>
        <div><span>GM stream</span><b>{String(s.stream_gm_content !== false)}</b></div>
        <div><span>parallel tools</span><b>{String(!!s.parallel_tool_calls)}</b></div>
        <div><span>предлагать варианты</span><b>{String(!!s.gm_suggest_options)}</b></div>
        <div><span>tool-hop лимит</span><b>{s.max_tool_hops ? s.max_tool_hops : "без ограничения"}</b></div>
        <div><span>лимит токенов</span><b>{s.max_output_tokens || 0}</b></div>
      </div>
    </div>
  );
}

const TABS = [
  { id: "overview", label: "Обзор" },
  { id: "story", label: "Сюжет" },
  { id: "scene", label: "Сцена" },
  { id: "player", label: "Игрок" },
  { id: "npcs", label: "Персонажи" },
  { id: "facts", label: "Факты" },
  { id: "memory", label: "Память" },
  { id: "runtime", label: "Рантайм" },
];

export default function DebugPanel({ refreshKey = "", open = false, onOpenChange }) {
  const setOpen = useCallback(
    (next) => onOpenChange?.(typeof next === "function" ? next(open) : next),
    [onOpenChange, open]
  );
  const [data, setData] = useState(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [stack, setStack] = useState([]);
  const [tab, setTab] = useState("overview");

  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const payload = await api.debug();
      if (!payload.ok) throw new Error(payload.error || "debug не загружен");
      setData(payload);
    } catch (e) {
      setError(e.message || String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (open) load();
  }, [open, refreshKey, load]);

  // --- modal stack manager: ESC / backdrop close only the TOP layer ---
  const openModal = useCallback((m) => setStack([m]), []);
  const pushModal = useCallback((m) => setStack((s) => (s.length >= 2 ? s : [...s, m])), []);
  const closeTop = useCallback(() => setStack((s) => s.slice(0, -1)), []);

  useEffect(() => {
    if (!stack.length) return undefined;
    const onKey = (e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setStack((s) => s.slice(0, -1));
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [stack.length]);

  const apply = useCallback(async (promise) => {
    setError("");
    try {
      const payload = await promise;
      if (payload && payload.ok !== false) setData(payload);
      else setError(payload?.error || "не удалось применить");
    } catch (e) {
      setError(e.message || String(e));
    }
  }, []);

  const runRoll = useCallback((body) => apply(api.debugRoll(body)), [apply]);
  const runAddFact = useCallback((text, kind) => apply(api.addFact(text, kind)), [apply]);
  const runDeleteFact = useCallback((id) => apply(api.deleteFact(id)), [apply]);
  const runUpdatePlayer = useCallback((body) => { apply(api.updatePlayer(body)); closeTop(); }, [apply, closeTop]);
  const runUpdateNpc = useCallback((body) => { apply(api.updateNpc(body)); closeTop(); }, [apply, closeTop]);
  const runUpdateStory = useCallback((body) => { apply(api.updateStory(body)); closeTop(); }, [apply, closeTop]);
  const runUpdateScene = useCallback((patch) => { apply(api.updateScene(patch)); closeTop(); }, [apply, closeTop]);
  const runStateRecord = useCallback((body) => apply(api.stateRecord(body)), [apply]);
  const runRumor = useCallback((body) => apply(api.rumor(body)), [apply]);

  const override = data?.roll_override || {};
  const rollBadge = [
    override.next != null ? `след:${override.next}` : "",
    override.all != null ? `все:${override.all}` : "",
  ].filter(Boolean).join(" · ");

  const title = data?.scene?.title || "История";
  const npcs = asList(data?.npcs);

  return (
    <>
      <ActionTip
        title={open ? "Скрыть дебаг" : "Открыть дебаг"}
        note="Панель для просмотра и правки служебного состояния истории."
      >
        <button
          type="button"
          className={["debug-tab", open ? "open" : ""].filter(Boolean).join(" ")}
          onClick={() => setOpen((value) => !value)}
          aria-expanded={open}
          aria-controls="debug-drawer"
        >
          Дебаг
        </button>
      </ActionTip>

      <aside id="debug-drawer" className={["debug-drawer", open ? "open" : ""].filter(Boolean).join(" ")}>
        <div className="debug-head">
          <div>
            <span>дебаг истории</span>
            <h2>{title}</h2>
          </div>
          <button type="button" className="icon-btn" onClick={() => setOpen(false)} aria-label="Закрыть">
            x
          </button>
        </div>

        <div className="debug-actions">
          <button type="button" className="btn" onClick={load} disabled={loading}>
            {loading ? "Обновляю…" : "↻ Обновить"}
          </button>
          {data?.meta && <span>{data.meta.backend} · {data.meta.model} · ходов: {data.meta.turns}</span>}
        </div>

        {data && (
          <nav className="dbg-tabs" role="tablist" aria-label="Разделы дебага">
            {TABS.map((t) => (
              <button
                key={t.id}
                type="button"
                role="tab"
                aria-selected={tab === t.id}
                className={["dbg-tab-btn", tab === t.id ? "active" : ""].filter(Boolean).join(" ")}
                onClick={() => setTab(t.id)}
              >
                {t.label}
              </button>
            ))}
          </nav>
        )}

        {error && <div className="err">debug: {error}</div>}
        {!data && !error && <Empty>{loading ? "загружаю…" : "панель ещё не открывалась"}</Empty>}

        {data && (
          <div className="debug-body">
            {tab === "overview" && (
              <div className="dbg-tabpanel">
                <div className="dbg-controls">
                  <button type="button" className="btn" onClick={() => openModal({ type: "rolls" })}>
                    🎲 Броски{rollBadge ? ` · ${rollBadge}` : ""}
                  </button>
                  <button type="button" className="btn" onClick={() => setTab("story")}>🎯 Сюжет</button>
                  <button type="button" className="btn" onClick={() => setTab("scene")}>🎬 Сцена</button>
                  <button type="button" className="btn" onClick={() => setTab("runtime")}>⚙ Рантайм</button>
                </div>
                <SceneSummary scene={data.scene} />
                <div className="debug-grid">
                  <div><span>бэкенд · модель</span><b>{data.meta?.backend} · {data.meta?.model}</b></div>
                  <div><span>ходов</span><b>{data.meta?.turns ?? 0}</b></div>
                  <div><span>персонажей</span><b>{npcs.length}</b></div>
                  <div><span>время</span><b>{data.time?.current_date_label || "—"}</b></div>
                </div>
                <h4>Бриф игрока на старте</h4>
                <TextBlock>{data.story?.brief}</TextBlock>
              </div>
            )}

            {tab === "story" && (
              <div className="dbg-tabpanel">
                <div className="dbg-controls">
                  <button type="button" className="btn primary" onClick={() => openModal({ type: "story" })}>✎ Править сюжет и канон</button>
                </div>
                <h4>Цель ведения <Info>Это пояснение для тебя. Модели оно напрямую не передаётся — поведение ГМ задаёт системный промпт.</Info></h4>
                <TextBlock>{data.story?.objective}</TextBlock>
                <h4>Бриф для игрока <Info>Именно этот текст показывается в верхней карточке чата. Кеш-префикс ГМ не трогает.</Info></h4>
                <TextBlock>{data.story?.brief}</TextBlock>
                <h4>Публичное интро <Info>Лежит в кешируемом префиксе. Правка пересоберёт префикс один раз.</Info></h4>
                <TextBlock>{data.story?.public_intro}</TextBlock>
                <h4>Скрытая правда <Info>В префикс не входит. Доходит до ГМ через query_world_state (scope=gm).</Info></h4>
                <TextBlock secret>{data.story?.hidden_truth}</TextBlock>
                <h4>Скрытые события ГМ</h4>
                <DebugList items={data.story?.hidden_events} secret />
              </div>
            )}

            {tab === "scene" && (
              <div className="dbg-tabpanel">
                <div className="dbg-controls">
                  <button type="button" className="btn primary" onClick={() => openModal({ type: "scene" })}>✎ Править сцену</button>
                </div>
                <SceneSummary scene={data.scene} />
                <h4>Описание</h4>
                <TextBlock>{data.scene?.description}</TextBlock>
                <h4>Ограничения</h4>
                <DebugList items={data.scene?.constraints} />
                <h4>Предметы</h4>
                <DebugList items={asList(data.scene?.items).map((i) => i.name + (i.location ? ` · ${i.location}` : ""))} />
                <h4>Выходы</h4>
                <DebugList items={asList(data.scene?.exits).map((e) => `${e.name} → ${e.destination}`)} />
              </div>
            )}

            {tab === "player" && (
              <div className="dbg-tabpanel">
                <PlayerCard player={data.player_character} onEdit={() => openModal({ type: "playerEdit" })} />
              </div>
            )}

            {tab === "npcs" && (
              <div className="dbg-tabpanel">
                <div className="debug-npcs">
                  {npcs.map((npc) => (
                    <NpcCard
                      key={npc.id}
                      npc={npc}
                      statusLabels={data.status_labels || {}}
                      onEdit={() => openModal({ type: "npcEdit", id: npc.id })}
                    />
                  ))}
                </div>
              </div>
            )}

            {tab === "facts" && (
              <div className="dbg-tabpanel">
                <div className="dbg-controls">
                  <button type="button" className="btn" onClick={() => openModal({ type: "facts" })}>📖 Факты мира</button>
                  <button type="button" className="btn" onClick={() => openModal({ type: "stateRecords" })}>🧬 Записи состояния</button>
                  <button type="button" className="btn" onClick={() => openModal({ type: "rumors" })}>🗣 Слухи</button>
                </div>
                <Facts facts={data.facts} rumors={data.rumors} />
                <h4>Записи состояния (state records)</h4>
                <DebugList items={asList(data.state_records).map((r) => `[${r.kind}/${r.scope}] ${r.text}`)} />
              </div>
            )}

            {tab === "memory" && (
              <div className="dbg-tabpanel">
                <h4>Сводка ГМ</h4>
                <TextBlock>{data.memory?.gm_summary}</TextBlock>
                <h4>Загруженные тулы ГМ</h4>
                <DebugList items={data.memory?.loaded_gm_tools} />
                <h4>Последние события</h4>
                <Events events={data.memory?.events} npcs={npcs} />
              </div>
            )}

            {tab === "runtime" && (
              <div className="dbg-tabpanel">
                <RuntimeView meta={data.meta} runtime={data.runtime} />
              </div>
            )}
          </div>
        )}
      </aside>

      {stack.map((m, i) => {
        if (m.type === "rolls") {
          return (
            <Modal key="rolls" depth={i} title="Управление бросками" subtitle="отладка кубов" onClose={closeTop}>
              <RollsControls override={override} onRun={runRoll} />
            </Modal>
          );
        }
        if (m.type === "facts") {
          return (
            <Modal key="facts" depth={i} wide title="Факты мира" subtitle="добавить / удалить" onClose={closeTop}>
              <FactsManager facts={asList(data?.facts)} onAdd={runAddFact} onDelete={runDeleteFact} />
            </Modal>
          );
        }
        if (m.type === "stateRecords") {
          return (
            <Modal key="sr" depth={i} wide title="Записи состояния" subtitle="durable state records" onClose={closeTop}>
              <StateRecordsManager records={asList(data?.state_records)} npcs={npcs} onApply={runStateRecord} />
            </Modal>
          );
        }
        if (m.type === "rumors") {
          return (
            <Modal key="rumors" depth={i} wide title="Слухи" subtitle="добавить / подтвердить / удалить" onClose={closeTop}>
              <RumorsManager rumors={asList(data?.rumors)} onAction={runRumor} />
            </Modal>
          );
        }
        if (m.type === "story") {
          return (
            <Modal key="story" depth={i} wide title="Правка сюжета и канона" subtitle="интро · скрытая правда · скрытые события" onClose={closeTop}>
              <StoryEditor story={data?.story || {}} onSave={runUpdateStory} />
            </Modal>
          );
        }
        if (m.type === "scene") {
          return (
            <Modal key="scene" depth={i} wide title="Правка сцены" subtitle={data?.scene?.title || ""} onClose={closeTop}>
              <SceneEditor scene={data?.scene || {}} npcs={npcs} onSave={runUpdateScene} />
            </Modal>
          );
        }
        if (m.type === "playerEdit") {
          return (
            <Modal key="playerEdit" depth={i} wide title="Правка персонажа игрока" subtitle="лист персонажа" onClose={closeTop}>
              <PlayerEditor player={data?.player_character || {}} onSave={runUpdatePlayer} />
            </Modal>
          );
        }
        if (m.type === "npcs") {
          return (
            <Modal key="npcs" depth={i} title="Персонажи" subtitle="выбери для правки" onClose={closeTop}>
              <NpcPicker npcs={npcs} onPick={(id) => pushModal({ type: "npcEdit", id })} />
            </Modal>
          );
        }
        if (m.type === "npcEdit") {
          const npc = npcs.find((n) => n.id === m.id);
          if (!npc) return null;
          return (
            <Modal key={`npcEdit-${m.id}`} depth={i} wide title={<>Правка: <span style={{ color: npc.color || "var(--entity-unknown)" }}>{npc.name}</span></>} subtitle={`ID: ${npc.id}`} onClose={closeTop}>
              <NpcEditor npc={npc} statusLabels={data?.status_labels || {}} onSave={runUpdateNpc} />
            </Modal>
          );
        }
        return null;
      })}
    </>
  );
}
