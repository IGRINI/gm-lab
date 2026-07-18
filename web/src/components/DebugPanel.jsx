import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../api.js";
import MarkdownText from "./MarkdownText.jsx";
import Modal from "./Modal.jsx";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import { nameColor } from "../nameColor.js";
import { localizeServerMessage } from "../serverMessages.js";
import { localizeStatusLabel } from "../statusContext.js";
import {
  asObject,
  numText,
  mapFromRows,
  namedListFromRows,
  spellsFromRows,
  slotsFromRows,
  useMapRows,
  useStringRows,
  useSpellRows,
  useSlotRows,
  AbilitiesEditor,
  MapRowsEditor,
  NamedListEditor,
  PronounsSelect,
  SpellsEditor,
  SlotsEditor,
} from "./sheetEditors.jsx";

const actorColor = (actor, npcs) => {
  const a = String(actor || "").trim().toLowerCase();
  if (a === "player" || a === "игрок") return "var(--player)";
  if (a === "gm") return "var(--gm)";
  return nameColor(actor, npcs);
};

const FACT_KINDS = [
  "public",
  "truth",
  "rumor",
];
const factKindLabel = (t, kind) =>
  t(`debug.factKinds.${kind}`, { defaultValue: kind || t("debug.factKinds.fact") });

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
  const { t } = useTranslation("developer");
  const [next, setNext] = useState("");
  const [all, setAll] = useState(override.all != null ? String(override.all) : "");
  return (
    <div className="dbg-form">
      <div className="dbg-block">
        <div className="dbg-block-head">
          <b>{t("debug.rolls.next")}</b>
          {override.next != null
            ? <span className="dbg-badge on">{t("debug.rolls.dieValue", { value: override.next })}</span>
            : <span className="dbg-badge">{t("debug.common.off")}</span>}
        </div>
        <p className="dbg-hint">{t("debug.rolls.nextHint")}</p>
        <div className="dbg-row">
          <input type="number" min="1" placeholder={t("debug.rolls.example20")} value={next} onChange={(e) => setNext(e.target.value)} />
          <button type="button" className="btn primary" disabled={!next} onClick={() => { onRun({ next: Number(next) }); setNext(""); }}>{t("debug.common.apply")}</button>
          <button type="button" className="btn" disabled={override.next == null} onClick={() => onRun({ next: null })}>{t("debug.common.cancel")}</button>
        </div>
      </div>
      <div className="dbg-block">
        <div className="dbg-block-head">
          <b>{t("debug.rolls.all")}</b>
          {override.all != null
            ? <span className="dbg-badge on">{t("debug.rolls.dieValue", { value: override.all })}</span>
            : <span className="dbg-badge">{t("debug.common.off")}</span>}
        </div>
        <p className="dbg-hint">{t("debug.rolls.allHint")}</p>
        <div className="dbg-row">
          <input type="number" min="1" placeholder={t("debug.rolls.example18")} value={all} onChange={(e) => setAll(e.target.value)} />
          <button type="button" className="btn primary" disabled={!all} onClick={() => onRun({ all: Number(all) })}>{t("debug.common.enable")}</button>
          <button type="button" className="btn" disabled={override.all == null} onClick={() => { onRun({ all: null }); setAll(""); }}>{t("debug.common.disable")}</button>
        </div>
      </div>
    </div>
  );
}

// --- Факты мира: добавить + список с удалением ---
function FactsManager({ facts, onAdd, onDelete }) {
  const { t } = useTranslation("developer");
  const [text, setText] = useState("");
  const [kind, setKind] = useState("public");
  return (
    <div className="dbg-form">
      <div className="dbg-block">
        <textarea className="dbg-textarea" rows={2} placeholder={t("debug.facts.newPlaceholder")} value={text} onChange={(e) => setText(e.target.value)} />
        <div className="dbg-row">
          <select value={kind} onChange={(e) => setKind(e.target.value)}>
            {FACT_KINDS.map((k) => <option key={k} value={k}>{factKindLabel(t, k)}</option>)}
          </select>
          <button type="button" className="btn primary" disabled={!text.trim()} onClick={() => { onAdd(text.trim(), kind); setText(""); }}>{t("debug.facts.add")}</button>
        </div>
      </div>
      <div className="dbg-fact-list">
        {facts.length ? facts.map((f) => (
          <div className={["dbg-fact", f.kind].join(" ")} key={f.id}>
            <span className={["dbg-fact-kind", f.kind].join(" ")}>{factKindLabel(t, f.kind)}</span>
            <span className="dbg-fact-text">{f.text}</span>
            <ActionTip title={t("debug.facts.deleteTitle")} note={t("debug.facts.deleteNote")}>
              <button type="button" className="icon-btn danger" onClick={() => onDelete(f.id)}>🗑</button>
            </ActionTip>
          </div>
        )) : <Empty>{t("debug.facts.empty")}</Empty>}
      </div>
    </div>
  );
}

// --- Записи состояния (durable state records) ---
function StateRecordsManager({ records, npcs, onApply }) {
  const { t } = useTranslation("developer");
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
        <div className="dbg-block-head"><b>{t("debug.stateRecords.new")}</b><span className="dbg-badge">{t("debug.badges.gm")}</span></div>
        <p className="dbg-hint">{t("debug.stateRecords.hint")}</p>
        <textarea className="dbg-textarea" rows={2} placeholder={t("debug.stateRecords.placeholder")} value={text} onChange={(e) => setText(e.target.value)} />
        <div className="dbg-row">
          <Tooltip
            className="tooltip-wrap"
            tipClassName="ui-tip-wrap"
            focusable={false}
            content={<TipContent title={t("debug.stateRecords.kindTitle")} note={t("debug.stateRecords.kindNote")} />}
          >
            <select value={kind} onChange={(e) => setKind(e.target.value)} aria-label={t("debug.stateRecords.kindTitle")}>
              {SR_KINDS.map((k) => <option key={k} value={k}>{t(`worldState.types.${k}`, { defaultValue: k })}</option>)}
            </select>
          </Tooltip>
          <Tooltip
            className="tooltip-wrap"
            tipClassName="ui-tip-wrap"
            focusable={false}
            content={<TipContent title={t("debug.stateRecords.scopeTitle")} note={t("debug.stateRecords.scopeNote")} />}
          >
            <select value={scope} onChange={(e) => setScope(e.target.value)} aria-label={t("debug.stateRecords.scopeTitle")}>
              {SR_SCOPES.map((s) => <option key={s} value={s}>{t(`worldState.scopes.${s}`, { defaultValue: s })}</option>)}
            </select>
          </Tooltip>
          <input placeholder={t("debug.fields.entityIdNpc")} value={entity} list="dbg-npc-ids" onChange={(e) => setEntity(e.target.value)} />
          <button type="button" className="btn primary" disabled={!text.trim()} onClick={add}>{t("debug.common.add")}</button>
        </div>
        <datalist id="dbg-npc-ids">{npcs.map((n) => <option key={n.id} value={n.id}>{n.name}</option>)}</datalist>
      </div>
      <div className="dbg-fact-list">
        {records.length ? records.map((r) => (
          <div className="dbg-fact" key={r.record_id || r.id}>
            <span className="dbg-fact-kind">{r.kind}/{r.scope}</span>
            <span className="dbg-fact-text">{r.text}{r.entity_id ? ` · ${r.entity_id}` : ""}</span>
            <ActionTip title={t("debug.stateRecords.deleteTitle")} note={t("debug.stateRecords.deleteNote")}>
              <button type="button" className="icon-btn danger" onClick={() => onApply({ delete: [r.record_id || r.id] })}>🗑</button>
            </ActionTip>
          </div>
        )) : <Empty>{t("debug.stateRecords.empty")}</Empty>}
      </div>
    </div>
  );
}

// --- Слухи: добавить / подтвердить / удалить ---
function RumorsManager({ rumors, onAction }) {
  const { t } = useTranslation("developer");
  const [speaker, setSpeaker] = useState("");
  const [text, setText] = useState("");
  return (
    <div className="dbg-form">
      <div className="dbg-block">
        <div className="dbg-block-head"><b>{t("debug.rumors.new")}</b></div>
        <p className="dbg-hint">{t("debug.rumors.hint")}</p>
        <textarea className="dbg-textarea" rows={2} placeholder={t("debug.rumors.placeholder")} value={text} onChange={(e) => setText(e.target.value)} />
        <div className="dbg-row">
          <input placeholder={t("debug.rumors.speakerPlaceholder")} value={speaker} onChange={(e) => setSpeaker(e.target.value)} />
          <button type="button" className="btn primary" disabled={!text.trim()} onClick={() => { onAction({ action: "add", speaker: speaker.trim(), text: text.trim() }); setText(""); setSpeaker(""); }}>{t("debug.common.add")}</button>
        </div>
      </div>
      <div className="dbg-fact-list">
        {rumors.length ? rumors.map((r) => (
          <div className="dbg-fact" key={r.seq}>
            <span className={["dbg-fact-kind", r.confirmed ? "truth" : "rumor"].join(" ")}>{r.confirmed ? t("debug.rumors.confirmedShort") : t("debug.rumors.rumor")}</span>
            <span className="dbg-fact-text">{r.speaker ? `${r.speaker}: ` : ""}{r.text}</span>
            <button type="button" className="btn small" onClick={() => onAction({ action: "confirm", seq: r.seq, confirmed: !r.confirmed })}>{r.confirmed ? t("debug.rumors.unconfirm") : t("debug.rumors.confirmedShort")}</button>
            <ActionTip title={t("debug.rumors.deleteTitle")} note={t("debug.rumors.deleteNote")}>
              <button type="button" className="icon-btn danger" onClick={() => onAction({ action: "delete", seq: r.seq })}>🗑</button>
            </ActionTip>
          </div>
        )) : <Empty>{t("debug.rumors.empty")}</Empty>}
      </div>
    </div>
  );
}

// --- Список персонажей: клик -> правка ---
function NpcPicker({ npcs, onPick }) {
  const { t } = useTranslation("developer");
  if (!npcs.length) return <Empty>{t("debug.npcs.empty")}</Empty>;
  return (
    <div className="dbg-pick-list">
      {npcs.map((n) => (
        <button type="button" className="dbg-pick" key={n.id} onClick={() => onPick(n.id)}>
          <span className="dot" style={{ "--c": n.color || "var(--entity-unknown)" }} />
          <span className="dbg-pick-name" style={{ color: n.color || "var(--entity-unknown)" }}>{n.name}</span>
          <span className="dbg-pick-role">{n.role || t("references.character")}{n.present ? t("debug.npcs.inSceneSuffix") : ""}</span>
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

// Правка числового ключа в объекте характеристик/ХП: пустое значение удаляет
// ключ, не-базовые ключи объекта выживают (зеркалит updateAbility/updateHp
// студии архитектора).
function numKeyUpdater(setter) {
  return (key, text) => {
    const n = parseInt(text, 10);
    setter((current) => {
      const next = { ...current };
      if (Number.isFinite(n)) next[key] = n;
      else delete next[key];
      return next;
    });
  };
}

// --- Правка сюжета и канона ---
function StoryEditor({ story, onSave }) {
  const { t } = useTranslation("developer");
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
      <EditField label={t("debug.storyEditor.title")}><input value={d.title} onChange={(e) => set({ title: e.target.value })} /></EditField>
      <EditField label={t("debug.storyEditor.brief")}>
        <textarea rows={4} value={d.story_brief} onChange={(e) => set({ story_brief: e.target.value })} />
      </EditField>
      <EditField label={<>{t("debug.storyEditor.publicIntro")} <span className="dbg-warn">{t("debug.storyEditor.cachePrefix")}</span></>}>
        <textarea rows={4} value={d.public_intro} onChange={(e) => set({ public_intro: e.target.value })} />
      </EditField>
      {introChanged && (
        <div className="dbg-danger-hint" role="alert">
          ⚠️ {t("debug.storyEditor.cacheWarning")}
        </div>
      )}
      <EditField label={t("debug.storyEditor.hiddenTruth")}><textarea rows={4} className="dbg-secret" value={d.hidden_truth} onChange={(e) => set({ hidden_truth: e.target.value })} /></EditField>
      <EditField label={t("debug.storyEditor.hiddenEvents")}><textarea rows={4} value={d.hidden_events} onChange={(e) => set({ hidden_events: e.target.value })} /></EditField>
      <div className="dbg-modal-actions">
        <button type="button" className="btn primary" onClick={save}>{t("debug.common.save")}</button>
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
  const { t } = useTranslation("developer");
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
        items: parseArrayField(t("debug.sceneEditor.items"), d.items, t),
        exits: parseArrayField(t("debug.sceneEditor.exits"), d.exits, t),
      };
      setEditError("");
    } catch (e) {
      setEditError(debugErrorText(e, t));
      return;
    }
    onSave(patch);
  };
  return (
    <div className="dbg-form">
      <div className="dbg-edit-grid">
        <EditField label={t("debug.sceneEditor.title")}><input value={d.title} onChange={(e) => set({ title: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.locationId")}><input value={d.location_id} onChange={(e) => set({ location_id: e.target.value })} /></EditField>
      </div>
      <EditField label={t("debug.sceneEditor.description")}><textarea rows={3} value={d.description} onChange={(e) => set({ description: e.target.value })} /></EditField>
      <EditField label={t("debug.sceneEditor.tension")}><textarea rows={2} value={d.tension} onChange={(e) => set({ tension: e.target.value })} /></EditField>
      <EditField label={t("debug.sceneEditor.constraints")}><textarea rows={4} value={d.constraints} onChange={(e) => set({ constraints: e.target.value })} /></EditField>
      <div className="dbg-block">
        <div className="dbg-block-head"><b>{t("debug.sceneEditor.present")}</b></div>
        <div className="dbg-check-grid">
          {npcs.length ? npcs.map((n) => (
            <label className="dbg-check" key={n.id}>
              <input type="checkbox" checked={d.present.has(n.id)} onChange={() => togglePresent(n.id)} />
              <span style={{ color: n.color || "var(--entity-unknown)" }}>{n.name}</span>
            </label>
          )) : <Empty>{t("debug.npcs.empty")}</Empty>}
        </div>
      </div>
      <div className="dbg-edit-grid">
        <EditField label={<>{t("debug.sceneEditor.itemsJson")} <Info>{t("debug.sceneEditor.itemsHelp")}</Info></>}>
          <textarea rows={6} value={d.items} onChange={(e) => set({ items: e.target.value })} />
        </EditField>
        <EditField label={<>{t("debug.sceneEditor.exitsJson")} <Info>{t("debug.sceneEditor.exitsHelp")}</Info></>}>
          <textarea rows={6} value={d.exits} onChange={(e) => set({ exits: e.target.value })} />
        </EditField>
      </div>
      {editError && <div className="err">{editError}</div>}
      <div className="dbg-modal-actions">
        <button type="button" className="btn primary" onClick={save}>{t("debug.common.save")}</button>
      </div>
    </div>
  );
}

// --- Редактор карточки персонажа (полная карточка) ---
function NpcEditor({ npc, statusLabels, onSave }) {
  const { t } = useTranslation("developer");
  const mechanics = npc.mechanics || {};
  const [d, setD] = useState(() => ({
    name: npc.name || "", color: npc.color || "", role: npc.role || "", pronouns: npc.pronouns || "",
    public_label: npc.public_label || "", age: npc.age || "",
    physical_type: npc.physical_type || "", current_appearance: npc.current_appearance || "",
    distinctive_features: npc.distinctive_features || "",
    life_status: npc.life_status || "alive", life_status_note: npc.life_status_note || "",
    condition: npc.condition || "",
    persona: npc.persona || "", personality: npc.personality || "", values: npc.values || "",
    habits: npc.habits || "", pressure_response: npc.pressure_response || "",
    boundaries: npc.boundaries || "", voice: npc.voice || "", goals: npc.goals || "",
    knowledge: npc.knowledge || "", secret: npc.secret || "",
    passive_perception: mechanics.passive_perception != null ? String(mechanics.passive_perception) : "",
    ac: mechanics.ac != null ? String(mechanics.ac) : "",
    speed: mechanics.speed || "",
    senses: mechanics.senses || "",
    languages: mechanics.languages || "",
    present: !!npc.present,
    wb_location: npc.whereabouts?.location_name || "",
    wb_status: npc.whereabouts?.status || "unknown",
    wb_details: npc.whereabouts?.details || "",
    reset_memory: false,
  }));
  // Характеристики/ХП правятся по ключам (структурные редакторы студии);
  // навыки/спасброски — строковыми буферами, собираются на сохранении.
  const [abilities, setAbilities] = useState(() => ({ ...(asObject(mechanics.abilities) || {}) }));
  const [hp, setHp] = useState(() => ({ ...(asObject(mechanics.hp) || {}) }));
  const skillRows = useMapRows(mechanics.skills);
  const saveRows = useMapRows(mechanics.saving_throws);
  const [editError, setEditError] = useState("");
  const set = (patch) => setD((p) => ({ ...p, ...patch }));
  const updateAbility = numKeyUpdater(setAbilities);
  const updateHp = numKeyUpdater(setHp);
  const statusEntries = Object.keys(statusLabels || {}).map((key) => [
    key,
    localizeStatusLabel(t, key, statusLabels),
  ]);
  const secretChanged = d.secret !== (npc.secret || "");
  const presenceChanged = d.present !== !!npc.present;
  const save = () => {
    let fields;
    try {
      fields = {
        name: d.name, color: d.color, role: d.role, pronouns: d.pronouns,
        public_label: d.public_label, age: d.age,
        physical_type: d.physical_type, current_appearance: d.current_appearance,
        distinctive_features: d.distinctive_features,
        life_status: d.life_status, life_status_note: d.life_status_note,
        condition: d.condition, persona: d.persona, personality: d.personality,
        values: d.values, habits: d.habits, pressure_response: d.pressure_response,
        boundaries: d.boundaries, voice: d.voice, goals: d.goals, knowledge: d.knowledge,
        secret: d.secret, abilities: { ...abilities },
        skills: mapFromRows(skillRows.rows),
        saving_throws: mapFromRows(saveRows.rows),
        passive_perception: parseIntegerField(t("debug.fields.passivePerception"), d.passive_perception, t),
        ac: parseIntegerField(t("debug.fields.ac"), d.ac, t), hp: { ...hp },
        speed: d.speed, senses: d.senses, languages: d.languages,
      };
      setEditError("");
    } catch (e) {
      setEditError(debugErrorText(e, t));
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
        <EditField label={t("debug.fields.name")}><input value={d.name} onChange={(e) => set({ name: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.playerSees")}><input value={npc.player_label || npc.public_label || npc.name || ""} readOnly /></EditField>
        <EditField label={t("debug.fields.color")}>
          <span className="dbg-color">
            <input type="color" value={/^#[0-9a-fA-F]{6}$/.test(d.color) ? d.color : "#908caa"} onChange={(e) => set({ color: e.target.value })} />
            <input value={d.color} placeholder="#e6c08a" onChange={(e) => set({ color: e.target.value })} />
          </span>
        </EditField>
        <EditField label={t("debug.fields.role")}><input value={d.role} onChange={(e) => set({ role: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.pronouns")}><PronounsSelect value={d.pronouns} onChange={(value) => set({ pronouns: value })} /></EditField>
        <EditField label={t("debug.fields.publicLabel")}><input value={d.public_label} placeholder={t("debug.fields.publicLabelPlaceholder")} onChange={(e) => set({ public_label: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.knownName")}><input value={npc.known_name || ""} readOnly /></EditField>
        <EditField label={t("debug.fields.age")}><input value={d.age} onChange={(e) => set({ age: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.physicalTypeNpc")}><input value={d.physical_type} onChange={(e) => set({ physical_type: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.features")}><input value={d.distinctive_features} onChange={(e) => set({ distinctive_features: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.lifeStatus")}><input value={d.life_status} onChange={(e) => set({ life_status: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.statusNote")}><input value={d.life_status_note} onChange={(e) => set({ life_status_note: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.currentCondition")}><input value={d.condition} onChange={(e) => set({ condition: e.target.value })} /></EditField>
      </div>

      <EditField label={t("debug.fields.currentAppearance")}><textarea rows={2} value={d.current_appearance} onChange={(e) => set({ current_appearance: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.description")}><textarea rows={2} value={d.persona} onChange={(e) => set({ persona: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.personality")}><textarea rows={2} value={d.personality} onChange={(e) => set({ personality: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.values")}><textarea rows={2} value={d.values} onChange={(e) => set({ values: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.habits")}><textarea rows={2} value={d.habits} onChange={(e) => set({ habits: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.pressureResponse")}><textarea rows={2} value={d.pressure_response} onChange={(e) => set({ pressure_response: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.boundaries")}><textarea rows={2} value={d.boundaries} onChange={(e) => set({ boundaries: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.voice")}><textarea rows={2} value={d.voice} onChange={(e) => set({ voice: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.goals")}><textarea rows={2} value={d.goals} onChange={(e) => set({ goals: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.knowledge")}><textarea rows={2} value={d.knowledge} onChange={(e) => set({ knowledge: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.secretNoContext")}><textarea rows={2} className="dbg-secret" value={d.secret} onChange={(e) => set({ secret: e.target.value })} /></EditField>
      <div className="dbg-block-head"><b>{t("debug.fields.mechanics")}</b><span className="dbg-badge">{t("debug.badges.gmOnly")}</span></div>
      <AbilitiesEditor abilities={abilities} onChange={updateAbility} />
      <div className="dbg-edit-grid">
        <EditField label={t("debug.fields.ac")}><input type="number" value={d.ac} onChange={(e) => set({ ac: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.passivePerception")}><input type="number" value={d.passive_perception} onChange={(e) => set({ passive_perception: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.hpCurrent")}><input type="number" value={numText(hp.current)} onChange={(e) => updateHp("current", e.target.value)} /></EditField>
        <EditField label={t("debug.fields.hpMax")}><input type="number" value={numText(hp.max)} onChange={(e) => updateHp("max", e.target.value)} /></EditField>
        <EditField label={t("debug.fields.speed")}><input value={d.speed} onChange={(e) => set({ speed: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.languages")}><input value={d.languages} onChange={(e) => set({ languages: e.target.value })} /></EditField>
      </div>
      {/* Чувства часто длиннее строки («острое зрение, острый слух, идеальный
          нюх…») — полноширинная textarea вместо клетки в сетке. */}
      <EditField label={t("debug.fields.senses")}><textarea rows={2} value={d.senses} onChange={(e) => set({ senses: e.target.value })} /></EditField>
      <MapRowsEditor
        label={t("debug.fields.skills")}
        rows={skillRows.rows}
        onEdit={skillRows.edit}
        onAdd={skillRows.add}
        onRemove={skillRows.remove}
        keyPlaceholder={t("debug.fields.skill")}
      />
      <MapRowsEditor
        label={t("debug.fields.savingThrows")}
        rows={saveRows.rows}
        onEdit={saveRows.edit}
        onAdd={saveRows.add}
        onRemove={saveRows.remove}
        keyPlaceholder={t("debug.fields.savingThrow")}
      />
      {editError && <div className="err">{editError}</div>}
      {secretChanged && (
        <div className="dbg-danger-hint" role="alert">
          ⚠️ {t("debug.npcEditor.secretWarning")}
        </div>
      )}

      <div className="dbg-block">
        <label className="dbg-check">
          <input type="checkbox" checked={d.present} onChange={(e) => set({ present: e.target.checked })} />
          <span>{t("debug.npcEditor.inCurrentScene")}</span>
        </label>
        {!d.present && (
          <div className="dbg-edit-grid">
            <EditField label={t("debug.npcEditor.where")}><input value={d.wb_location} onChange={(e) => set({ wb_location: e.target.value })} /></EditField>
            <EditField label={t("debug.fields.status")}>
              <select value={d.wb_status} onChange={(e) => set({ wb_status: e.target.value })}>
                {statusEntries.map(([key, label]) => <option key={key} value={key}>{label}</option>)}
              </select>
            </EditField>
            <EditField label={t("debug.fields.details")}><input value={d.wb_details} onChange={(e) => set({ wb_details: e.target.value })} /></EditField>
          </div>
        )}
      </div>

      <div className="dbg-danger-block">
        <label className="dbg-check dbg-danger">
          <input type="checkbox" checked={d.reset_memory} onChange={(e) => set({ reset_memory: e.target.checked })} />
          <span>🔥 {t("debug.npcEditor.resetMemory")}</span>
        </label>
        {d.reset_memory && (
          <div className="dbg-danger-hint" role="alert">
            {t("debug.npcEditor.resetWarning")}
          </div>
        )}
      </div>

      <div className="dbg-modal-actions">
        <button type="button" className={"btn primary" + (d.reset_memory ? " danger" : "")} onClick={save}>
          {d.reset_memory ? t("debug.npcEditor.saveAndReset") : t("debug.common.save")}
        </button>
      </div>
    </div>
  );
}

function PlayerEditor({ player, onSave }) {
  const { t } = useTranslation("developer");
  const [d, setD] = useState(() => ({
    name: player.name || "",
    pronouns: player.pronouns || "",
    class_role: player.class_role || "",
    level: player.level != null ? String(player.level) : "",
    background: player.background || "",
    age: player.age || "",
    physical_type: player.physical_type || "",
    current_appearance: player.current_appearance || "",
    distinctive_features: player.distinctive_features || "",
    life_status: player.life_status || "alive",
    life_status_note: player.life_status_note || "",
    condition: player.condition || "",
    personality: player.personality || "",
    values: player.values || "",
    gm_notes: player.gm_notes || "",
    passive_perception: player.passive_perception != null ? String(player.passive_perception) : "",
    ac: player.ac != null ? String(player.ac) : "",
    speed: player.speed || "",
    senses: player.senses || "",
    languages: player.languages || "",
    concentration: player.concentration || "",
  }));
  // Структурные редакторы студии архитектора (sheetEditors): характеристики/ХП
  // правятся по ключам (не-базовые ключи объекта выживают), остальное —
  // строковыми буферами, которые собираются в payload на сохранении.
  const [abilities, setAbilities] = useState(() => ({ ...(asObject(player.abilities) || {}) }));
  const [hp, setHp] = useState(() => ({ ...(asObject(player.hp) || {}) }));
  const skillRows = useMapRows(player.skills);
  const saveRows = useMapRows(player.saving_throws);
  const invRows = useStringRows(player.inventory);
  const equipRows = useStringRows(player.equipment);
  const featRows = useStringRows(player.features);
  const spellRows = useSpellRows(player.spells);
  const slotRows = useSlotRows(player.spell_slots, player.spell_slots_max);
  const [editError, setEditError] = useState("");
  const set = (patch) => setD((p) => ({ ...p, ...patch }));
  const updateAbility = numKeyUpdater(setAbilities);
  const updateHp = numKeyUpdater(setHp);
  const save = () => {
    let fields;
    try {
      const slotMaps = slotsFromRows(slotRows.rows);
      fields = {
        name: d.name,
        pronouns: d.pronouns,
        class_role: d.class_role,
        level: parseIntegerField(t("debug.fields.level"), d.level, t),
        background: d.background,
        age: d.age,
        physical_type: d.physical_type,
        current_appearance: d.current_appearance,
        distinctive_features: d.distinctive_features,
        life_status: d.life_status,
        life_status_note: d.life_status_note,
        condition: d.condition,
        personality: d.personality,
        values: d.values,
        gm_notes: d.gm_notes,
        abilities: { ...abilities },
        skills: mapFromRows(skillRows.rows),
        saving_throws: mapFromRows(saveRows.rows),
        passive_perception: parseIntegerField(t("debug.fields.passivePerception"), d.passive_perception, t),
        ac: parseIntegerField(t("debug.fields.ac"), d.ac, t),
        hp: { ...hp },
        speed: d.speed,
        senses: d.senses,
        languages: d.languages,
        inventory: namedListFromRows(invRows.rows),
        equipment: namedListFromRows(equipRows.rows),
        features: namedListFromRows(featRows.rows),
        spells: spellsFromRows(spellRows.rows),
        spell_slots: slotMaps.slots,
        spell_slots_max: slotMaps.max,
        concentration: d.concentration,
      };
      setEditError("");
    } catch (e) {
      setEditError(debugErrorText(e, t));
      return;
    }
    onSave({ fields, reason: "debug edit" });
  };
  return (
    <div className="dbg-form">
      <div className="dbg-edit-grid">
        <EditField label={t("debug.fields.name")}><input value={d.name} onChange={(e) => set({ name: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.pronouns")}><PronounsSelect value={d.pronouns} onChange={(value) => set({ pronouns: value })} /></EditField>
        <EditField label={t("debug.fields.classRole")}><input value={d.class_role} onChange={(e) => set({ class_role: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.level")}><input type="number" value={d.level} onChange={(e) => set({ level: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.background")}><input value={d.background} onChange={(e) => set({ background: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.age")}><input value={d.age} onChange={(e) => set({ age: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.physicalType")}><input value={d.physical_type} onChange={(e) => set({ physical_type: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.features")}><input value={d.distinctive_features} onChange={(e) => set({ distinctive_features: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.life")}><input value={d.life_status} onChange={(e) => set({ life_status: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.statusNote")}><input value={d.life_status_note} onChange={(e) => set({ life_status_note: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.condition")}><input value={d.condition} onChange={(e) => set({ condition: e.target.value })} /></EditField>
      </div>
      <EditField label={t("debug.fields.currentAppearance")}><textarea rows={2} value={d.current_appearance} onChange={(e) => set({ current_appearance: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.personality")}><textarea rows={2} value={d.personality} onChange={(e) => set({ personality: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.values")}><textarea rows={2} value={d.values} onChange={(e) => set({ values: e.target.value })} /></EditField>
      <EditField label={t("debug.fields.gmNotes")}><textarea rows={2} className="dbg-secret" value={d.gm_notes} onChange={(e) => set({ gm_notes: e.target.value })} /></EditField>
      <div className="dbg-block-head"><b>{t("debug.playerEditor.mechanics")}</b><span className="dbg-badge">{t("debug.badges.sheet")}</span></div>
      <AbilitiesEditor abilities={abilities} onChange={updateAbility} />
      <div className="dbg-edit-grid">
        <EditField label={t("debug.fields.ac")}><input type="number" value={d.ac} onChange={(e) => set({ ac: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.passivePerception")}><input type="number" value={d.passive_perception} onChange={(e) => set({ passive_perception: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.hpCurrent")}><input type="number" value={numText(hp.current)} onChange={(e) => updateHp("current", e.target.value)} /></EditField>
        <EditField label={t("debug.fields.hpMax")}><input type="number" value={numText(hp.max)} onChange={(e) => updateHp("max", e.target.value)} /></EditField>
        <EditField label={t("debug.fields.speed")}><input value={d.speed} onChange={(e) => set({ speed: e.target.value })} /></EditField>
        <EditField label={t("debug.fields.languages")}><input value={d.languages} onChange={(e) => set({ languages: e.target.value })} /></EditField>
      </div>
      {/* Чувства часто длиннее строки («острое зрение, острый слух, идеальный
          нюх…») — полноширинная textarea вместо клетки в сетке. */}
      <EditField label={t("debug.fields.senses")}><textarea rows={2} value={d.senses} onChange={(e) => set({ senses: e.target.value })} /></EditField>
      <MapRowsEditor
        label={t("debug.fields.skills")}
        rows={skillRows.rows}
        onEdit={skillRows.edit}
        onAdd={skillRows.add}
        onRemove={skillRows.remove}
        keyPlaceholder={t("debug.fields.skill")}
      />
      <MapRowsEditor
        label={t("debug.fields.savingThrows")}
        rows={saveRows.rows}
        onEdit={saveRows.edit}
        onAdd={saveRows.add}
        onRemove={saveRows.remove}
        keyPlaceholder={t("debug.fields.savingThrow")}
      />
      <NamedListEditor
        label={t("debug.fields.inventory")}
        rows={invRows.rows}
        onEdit={invRows.edit}
        onAdd={invRows.add}
        onRemove={invRows.remove}
      />
      <NamedListEditor
        label={t("debug.fields.equipment")}
        rows={equipRows.rows}
        onEdit={equipRows.edit}
        onAdd={equipRows.add}
        onRemove={equipRows.remove}
      />
      <NamedListEditor
        label={t("debug.fields.specialFeatures")}
        rows={featRows.rows}
        onEdit={featRows.edit}
        onAdd={featRows.add}
        onRemove={featRows.remove}
        descPlaceholder={t("debug.fields.featureDescription")}
      />
      <SpellsEditor
        rows={spellRows.rows}
        openSet={spellRows.open}
        onToggle={spellRows.toggle}
        onEdit={spellRows.edit}
        onAdd={spellRows.add}
        onRemove={spellRows.remove}
      />
      <SlotsEditor
        rows={slotRows.rows}
        missing={slotRows.missing}
        onEdit={slotRows.edit}
        onAddLevel={slotRows.addLevel}
        onRemove={slotRows.remove}
      />
      <EditField label={t("debug.fields.concentration")}><input value={d.concentration} onChange={(e) => set({ concentration: e.target.value })} /></EditField>
      {editError && <div className="err">{editError}</div>}
      <div className="dbg-modal-actions">
        <button type="button" className="btn primary" onClick={save}>{t("debug.common.save")}</button>
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
function spellLevelLabel(level, t) {
  const n = Number(level);
  return Number.isFinite(n) && n > 0 ? t("debug.spells.levelShort", { value: n }) : t("debug.spells.cantrip");
}

function spellLine(sp, t) {
  if (!sp || typeof sp !== "object") return "";
  const marks = [sp.concentration ? t("debug.spells.concentrationShort") : "", sp.ritual ? t("debug.spells.ritual") : ""].filter(Boolean);
  const head = `${sp.name || "—"} (${[spellLevelLabel(sp.level, t), ...marks].join(", ")})`;
  const effect = String(sp.effect || "").trim();
  return effect ? `${head}: ${effect}` : head;
}

// "1-й: 3/4, 2-й: 1/2" from the flat remaining/max slot maps; "" when no levels.
function slotsLine(slots, max, t) {
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
    return t("debug.spells.slotLevel", { level: lvl, current: cur, max: cap });
  }).join(", ");
}

function parseListField(value) {
  return String(value || "")
    .split(/\r?\n/)
    .map((item) => item.trim())
    .filter(Boolean);
}

class DebugValidationError extends Error {
  constructor(localizedMessage) {
    super("Debug form validation failed");
    this.name = "DebugValidationError";
    this.localizedMessage = localizedMessage;
  }
}

function debugErrorText(error, t, fallbackKey = "debug.errors.invalidValue") {
  if (error instanceof DebugValidationError) return error.localizedMessage;
  return localizeServerMessage(error, t, { fallbackText: t(fallbackKey) });
}

function parseArrayField(label, value, t) {
  const raw = String(value || "").trim();
  if (!raw) return [];
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    throw new DebugValidationError(t("debug.errors.jsonArray", { label }));
  }
  if (!Array.isArray(parsed)) {
    throw new DebugValidationError(t("debug.errors.jsonArray", { label }));
  }
  return parsed;
}

function prettyJson(value) {
  if (!value || (typeof value === "object" && !Array.isArray(value) && !Object.keys(value).length)) return "";
  if (Array.isArray(value) && !value.length) return "";
  return JSON.stringify(value, null, 2);
}

function parseIntegerField(label, value, t) {
  const raw = String(value || "").trim();
  if (!raw) return null;
  const parsed = Number(raw);
  if (!Number.isInteger(parsed)) {
    throw new DebugValidationError(t("debug.errors.integer", { label }));
  }
  return parsed;
}

function Empty({ children }) {
  const { t } = useTranslation("developer");
  return <div className="debug-empty">{children ?? t("debug.common.empty")}</div>;
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
  const { t } = useTranslation("developer");
  if (!scene) return <Empty />;
  return (
    <div className="debug-grid">
      <div><span>{t("debug.sceneSummary.scene")}</span><b>{scene.title || scene.scene_id || "—"}</b></div>
      <div><span>{t("debug.sceneSummary.location")}</span><b>{scene.location_id || "—"}</b></div>
      <div><span>{t("debug.sceneSummary.present")}</span><b>{asList(scene.present_npcs).join(", ") || t("debug.common.none")}</b></div>
      <div><span>{t("debug.sceneSummary.tension")}</span><b>{scene.tension || "—"}</b></div>
    </div>
  );
}

function Facts({ facts, rumors }) {
  const { t } = useTranslation("developer");
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
      <h4>{t("debug.factsView.hiddenTruth")}</h4>
      {groups.truth.length ? groups.truth.map((fact) => (
        <TextBlock key={fact.id} secret>{fact.text}</TextBlock>
      )) : <Empty />}

      <h4>{t("debug.factsView.publicFacts")}</h4>
      {groups.public.length ? (
        <DebugList items={groups.public.map((fact) => fact.text)} />
      ) : <Empty />}

      <h4>{t("debug.factsView.rumors")}</h4>
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
  const { t } = useTranslation("developer");
  const status = npc.whereabouts?.status || (npc.present ? "present" : "unknown");
  const mechanics = npc.mechanics || {};
  return (
    <details className="debug-npc" open={npc.present}>
      <summary>
        <span>
          <b style={{ color: npc.color || "var(--entity-unknown)" }}>{npc.name}</b>
          <em>{npc.role || t("references.character")}{npc.player_label && npc.player_label !== npc.name ? t("debug.npcs.playerSeesSuffix", { value: npc.player_label }) : ""}{npc.present ? t("debug.npcs.inSceneSuffix") : ""}</em>
        </span>
        <span className="debug-npc-head-right">
          <button
            type="button"
            className="btn small"
            onClick={(e) => { e.preventDefault(); e.stopPropagation(); onEdit?.(); }}
          >✎ {t("debug.common.edit")}</button>
          <small>{t("debug.npcs.messages", { count: npc.messages || 0 })}</small>
        </span>
      </summary>
      <div className="debug-grid">
        <div><span>{t("debug.fields.id")}</span><b>{npc.id}</b></div>
        <div><span>{t("debug.fields.playerSees")}</span><b>{npc.player_label || npc.public_label || "—"}</b></div>
        <div><span>{t("debug.fields.knownName")}</span><b>{npc.known_name || "—"}</b></div>
        <div><span>{t("debug.fields.publicLabel")}</span><b>{npc.public_label || "—"}</b></div>
        <div><span>{t("debug.fields.pronouns")}</span><b>{npc.pronouns || "—"}</b></div>
        <div><span>{t("debug.fields.age")}</span><b>{npc.age || "—"}</b></div>
        <div><span>{t("debug.fields.physicalType")}</span><b>{npc.physical_type || "—"}</b></div>
        <div><span>{t("debug.fields.currentAppearance")}</span><b>{npc.current_appearance || "—"}</b></div>
        <div><span>{t("debug.fields.features")}</span><b>{npc.distinctive_features || "—"}</b></div>
        <div><span>{t("debug.fields.life")}</span><b>{npc.life_status || "—"}</b></div>
        <div><span>{t("debug.fields.lifeStatus")}</span><b>{npc.life_status_note || "—"}</b></div>
        <div><span>{t("debug.fields.condition")}</span><b>{npc.condition || "—"}</b></div>
        <div><span>{t("debug.fields.where")}</span><b>{npc.whereabouts?.location_name || npc.whereabouts?.location_id || "—"}</b></div>
        <div><span>{t("debug.fields.status")}</span><b>{localizeStatusLabel(t, status, statusLabels)}</b></div>
      </div>

      <h4>{t("debug.npcCard.personality")}</h4>
      <TextBlock>{npc.persona}</TextBlock>
      <DebugList items={[
        npc.personality && t("debug.npcCard.trait", { label: t("debug.fields.personality"), value: npc.personality }),
        npc.values && t("debug.npcCard.trait", { label: t("debug.fields.values"), value: npc.values }),
        npc.habits && t("debug.npcCard.trait", { label: t("debug.fields.habits"), value: npc.habits }),
        npc.pressure_response && t("debug.npcCard.trait", { label: t("debug.fields.underPressure"), value: npc.pressure_response }),
        npc.boundaries && t("debug.npcCard.trait", { label: t("debug.fields.boundaries"), value: npc.boundaries }),
        npc.voice && t("debug.npcCard.trait", { label: t("debug.fields.voice"), value: npc.voice }),
      ].filter(Boolean)} />

      <h4>{t("debug.fields.goals")}</h4>
      <TextBlock>{npc.goals}</TextBlock>

      <h4>{t("debug.fields.knowledge")}</h4>
      <TextBlock>{npc.knowledge}</TextBlock>

      <h4>{t("debug.npcCard.secret")}</h4>
      <TextBlock secret>{npc.secret}</TextBlock>

      <h4>{t("debug.fields.mechanics")}</h4>
      <div className="debug-grid">
        <div><span>{t("debug.fields.passivePerception")}</span><b>{mechanics.passive_perception ?? "—"}</b></div>
        <div><span>{t("debug.fields.ac")}</span><b>{mechanics.ac ?? "—"}</b></div>
        <div><span>{t("debug.fields.speed")}</span><b>{mechanics.speed || "—"}</b></div>
        <div><span>{t("debug.fields.senses")}</span><b>{mechanics.senses || "—"}</b></div>
        <div><span>{t("debug.fields.languages")}</span><b>{mechanics.languages || "—"}</b></div>
      </div>
      <JsonBlock value={{
        abilities: mechanics.abilities,
        skills: mechanics.skills,
        saving_throws: mechanics.saving_throws,
        hp: mechanics.hp,
      }} />

      <h4>{t("debug.npcCard.memory")}</h4>
      <TextBlock>{npc.summary || npc.history}</TextBlock>

      <h4>{t("debug.npcCard.commitments")}</h4>
      <DebugList items={npc.commitments} />
    </details>
  );
}

function PlayerCard({ player, onEdit }) {
  const { t } = useTranslation("developer");
  if (!player) return <Empty>{t("debug.playerCard.notLoaded")}</Empty>;
  const slotSummary = slotsLine(player.spell_slots, player.spell_slots_max, t);
  return (
    <details className="debug-npc debug-player" open>
      <summary>
        <span>
          <b>{player.name || t("debug.playerCard.defaultName")}</b>
          <em>{[player.class_role, player.level != null ? t("debug.spells.levelShort", { value: player.level }) : ""].filter(Boolean).join(" · ") || t("debug.playerCard.sheet")}</em>
        </span>
        <span className="debug-npc-head-right">
          <button
            type="button"
            className="btn small"
            onClick={(e) => { e.preventDefault(); e.stopPropagation(); onEdit?.(); }}
          >✎ {t("debug.common.edit")}</button>
          <small>{t("debug.playerCard.revision", { value: player.card_revision || 0 })}</small>
        </span>
      </summary>
      <div className="debug-grid">
        <div><span>{t("debug.fields.pronouns")}</span><b>{player.pronouns || "—"}</b></div>
        <div><span>{t("debug.fields.background")}</span><b>{player.background || "—"}</b></div>
        <div><span>{t("debug.fields.age")}</span><b>{player.age || "—"}</b></div>
        <div><span>{t("debug.fields.physicalType")}</span><b>{player.physical_type || "—"}</b></div>
        <div><span>{t("debug.fields.currentAppearance")}</span><b>{player.current_appearance || "—"}</b></div>
        <div><span>{t("debug.fields.features")}</span><b>{player.distinctive_features || "—"}</b></div>
        <div><span>{t("debug.fields.life")}</span><b>{player.life_status || "—"}</b></div>
        <div><span>{t("debug.fields.status")}</span><b>{player.life_status_note || "—"}</b></div>
        <div><span>{t("debug.fields.condition")}</span><b>{player.condition || "—"}</b></div>
      </div>

      <h4>{t("debug.fields.personality")}</h4>
      <DebugList items={[
        player.personality && t("debug.npcCard.trait", { label: t("debug.fields.personality"), value: player.personality }),
        player.values && t("debug.npcCard.trait", { label: t("debug.fields.values"), value: player.values }),
      ].filter(Boolean)} />

      <h4>{t("debug.fields.mechanics")}</h4>
      <div className="debug-grid">
        <div><span>{t("debug.fields.passivePerception")}</span><b>{player.passive_perception ?? "—"}</b></div>
        <div><span>{t("debug.fields.ac")}</span><b>{player.ac ?? "—"}</b></div>
        <div><span>{t("debug.fields.speed")}</span><b>{player.speed || "—"}</b></div>
        <div><span>{t("debug.fields.senses")}</span><b>{player.senses || "—"}</b></div>
        <div><span>{t("debug.fields.languages")}</span><b>{player.languages || "—"}</b></div>
      </div>
      <JsonBlock value={{
        abilities: player.abilities,
        skills: player.skills,
        saving_throws: player.saving_throws,
        hp: player.hp,
      }} />

      <h4>{t("debug.fields.inventory")}</h4>
      <DebugList items={player.inventory} />

      <h4>{t("debug.playerCard.equipmentFeatures")}</h4>
      <DebugList items={[...asList(player.equipment), ...asList(player.features)]} />

      {(asList(player.spells).length > 0
        || slotSummary
        || String(player.concentration || "").trim()) && (
        <>
          <h4>{t("debug.playerCard.spells")}</h4>
          <DebugList items={asList(player.spells).map((spell) => spellLine(spell, t)).filter(Boolean)} />
          {slotSummary && (
            <div className="debug-grid"><div><span>{t("debug.playerCard.slots")}</span><b>{slotSummary}</b></div></div>
          )}
          {String(player.concentration || "").trim() && (
            <div className="debug-grid"><div><span>{t("debug.fields.concentration")}</span><b>{player.concentration}</b></div></div>
          )}
        </>
      )}

      <h4>{t("debug.fields.gmNotes")}</h4>
      <TextBlock secret>{player.gm_notes}</TextBlock>
    </details>
  );
}

function Events({ events, npcs }) {
  const { t } = useTranslation("developer");
  const rows = asList(events).slice(-24).reverse();
  if (!rows.length) return <Empty />;
  return (
    <div className="debug-events">
      {rows.map((event) => (
        <div className="debug-event" key={`${event.seq}-${event.actor}-${event.kind}`}>
          <div>
            <b>#{event.seq}</b>
            <span>{t("debug.events.turn", { value: event.turn })} · <b style={{ color: actorColor(event.actor, npcs) }}>{event.actor}</b> · {event.kind}</span>
          </div>
          <p>{event.speech || event.action || "—"}</p>
          <small>{t("debug.events.witnesses", { value: asList(event.witnesses).join(", ") || "—" })}</small>
        </div>
      ))}
    </div>
  );
}

// --- Рантайм/кеш: только просмотр; настройки модели меняются в шапке «Настройки» ---
function RuntimeView({ meta, runtime }) {
  const { t } = useTranslation("developer");
  const cache = runtime?.cache || {};
  const s = runtime?.settings || {};
  return (
    <div className="debug-stack">
      <h4>{t("debug.runtime.cacheTitle")} <Info>{t("debug.runtime.cacheHelp")}</Info></h4>
      <div className="debug-grid">
        <div><span>{t("debug.runtime.promptCacheKey")}</span><b className="dbg-mono">{cache.prompt_cache_key || "—"}</b></div>
        <div><span>{t("debug.runtime.threadId")}</span><b className="dbg-mono">{cache.thread_id || "—"}</b></div>
        <div><span>{t("debug.runtime.store")}</span><b>{t(cache.store ? "debug.common.yes" : "debug.common.no")}</b></div>
        <div><span>{t("debug.fields.turns")}</span><b>{meta?.turns ?? "—"}</b></div>
      </div>

      <h4>{t("debug.runtime.lastRunTokens")} <Info>{t("debug.runtime.cachedTokensHelp")}</Info></h4>
      <KVGrid obj={meta?.run_usage} />

      <h4>{t("debug.runtime.context")}</h4>
      <KVGrid obj={meta?.context_usage} />

      <h4>{t("debug.runtime.modelSettings")} <Info>{t("debug.runtime.modelSettingsHelp")}</Info></h4>
      <div className="debug-grid">
        <div><span>{t("debug.fields.model")}</span><b>{meta?.model || "—"}</b></div>
        <div><span>{t("debug.fields.backend")}</span><b>{meta?.backend || "—"}</b></div>
        <div><span>{t("debug.runtime.gmReasoning")}</span><b>{(s.gm_reasoning_effort || "—") + " / " + (s.gm_reasoning_summary || "—")}</b></div>
        <div><span>{t("debug.runtime.npcReasoning")}</span><b>{(s.npc_reasoning_effort || "—") + " / " + (s.npc_reasoning_summary || "—")}</b></div>
        <div><span>{t("debug.runtime.compactReasoning")}</span><b>{(s.compact_reasoning_effort || "—") + " / " + (s.compact_reasoning_summary || "—")}</b></div>
        <div><span>{t("debug.runtime.verbosity")}</span><b>{s.text_verbosity || "—"}</b></div>
        <div><span>{t("debug.runtime.toolChoice")}</span><b>{s.tool_choice || "—"}</b></div>
        <div><span>{t("debug.runtime.gmStream")}</span><b>{String(s.stream_gm_content !== false)}</b></div>
        <div><span>{t("debug.runtime.parallelTools")}</span><b>{String(!!s.parallel_tool_calls)}</b></div>
        <div><span>{t("debug.runtime.suggestOptions")}</span><b>{String(!!s.gm_suggest_options)}</b></div>
        <div><span>{t("debug.runtime.toolHopLimit")}</span><b>{s.max_tool_hops ? s.max_tool_hops : t("debug.runtime.unlimited")}</b></div>
        <div><span>{t("debug.runtime.tokenLimit")}</span><b>{s.max_output_tokens || 0}</b></div>
      </div>
    </div>
  );
}

const TABS = ["overview", "story", "scene", "player", "npcs", "facts", "memory", "runtime"];

export default function DebugPanel({ refreshKey = "", open = false, onOpenChange }) {
  const { t } = useTranslation("developer");
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
      if (!payload.ok) {
        setError(localizeServerMessage(payload, t, { fallbackText: t("debug.errors.load") }));
        return;
      }
      setData(payload);
    } catch (e) {
      setError(localizeServerMessage(e, t, { fallbackText: t("debug.errors.load") }));
    } finally {
      setLoading(false);
    }
  }, [t]);

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
      else setError(localizeServerMessage(payload, t, { fallbackText: t("debug.errors.apply") }));
    } catch (e) {
      setError(localizeServerMessage(e, t, { fallbackText: t("debug.errors.apply") }));
    }
  }, [t]);

  const runRoll = useCallback((body) => apply(api.debugRoll(body)), [apply]);
  const runAddFact = useCallback((text, kind) => apply(api.addFact(text, kind)), [apply]);
  const runDeleteFact = useCallback((id) => apply(api.deleteFact(id)), [apply]);
  const runUpdatePlayer = useCallback((body) => { apply(api.updatePlayer(body)); closeTop(); }, [apply, closeTop]);
  const runUpdateNpc = useCallback((body) => { apply(api.updateNpc(body)); closeTop(); }, [apply, closeTop]);
  const runUpdateStory = useCallback((body) => { apply(api.debugUpdateStory(body)); closeTop(); }, [apply, closeTop]);
  const runUpdateScene = useCallback((patch) => { apply(api.updateScene(patch)); closeTop(); }, [apply, closeTop]);
  const runStateRecord = useCallback((body) => apply(api.stateRecord(body)), [apply]);
  const runRumor = useCallback((body) => apply(api.rumor(body)), [apply]);

  const override = data?.roll_override || {};
  const rollBadge = [
    override.next != null ? t("debug.rolls.nextShort", { value: override.next }) : "",
    override.all != null ? t("debug.rolls.allShort", { value: override.all }) : "",
  ].filter(Boolean).join(" · ");

  const title = data?.scene?.title || t("debug.panel.storyFallback");
  const npcs = asList(data?.npcs);

  return (
    <>
      <ActionTip
        title={open ? t("debug.panel.hide") : t("debug.panel.open")}
        note={t("debug.panel.tip")}
      >
        <button
          type="button"
          className={["debug-tab", open ? "open" : ""].filter(Boolean).join(" ")}
          onClick={() => setOpen((value) => !value)}
          aria-expanded={open}
          aria-controls="debug-drawer"
        >
          {t("debug.panel.button")}
        </button>
      </ActionTip>

      <aside id="debug-drawer" className={["debug-drawer", open ? "open" : ""].filter(Boolean).join(" ")}>
        <div className="debug-head">
          <div>
            <span>{t("debug.panel.heading")}</span>
            <h2>{title}</h2>
          </div>
          <button type="button" className="icon-btn" onClick={() => setOpen(false)} aria-label={t("debug.common.close")}>
            x
          </button>
        </div>

        <div className="debug-actions">
          <button type="button" className="btn" onClick={load} disabled={loading}>
            {loading ? t("debug.panel.refreshing") : t("debug.panel.refresh")}
          </button>
          {data?.meta && <span>{data.meta.backend} · {data.meta.model} · {t("debug.panel.turns", { count: data.meta.turns })}</span>}
        </div>

        {data && (
          <nav className="dbg-tabs" role="tablist" aria-label={t("debug.panel.sections")}>
            {TABS.map((tabId) => (
              <button
                key={tabId}
                type="button"
                role="tab"
                aria-selected={tab === tabId}
                className={["dbg-tab-btn", tab === tabId ? "active" : ""].filter(Boolean).join(" ")}
                onClick={() => setTab(tabId)}
              >
                {t(`debug.tabs.${tabId}`)}
              </button>
            ))}
          </nav>
        )}

        {error && <div className="err">{error}</div>}
        {!data && !error && <Empty>{loading ? t("debug.panel.loading") : t("debug.panel.notOpened")}</Empty>}

        {data && (
          <div className="debug-body">
            {tab === "overview" && (
              <div className="dbg-tabpanel">
                <div className="dbg-controls">
                  <button type="button" className="btn" onClick={() => openModal({ type: "rolls" })}>
                    🎲 {t("debug.tabs.rolls")}{rollBadge ? ` · ${rollBadge}` : ""}
                  </button>
                  <button type="button" className="btn" onClick={() => setTab("story")}>🎯 {t("debug.tabs.story")}</button>
                  <button type="button" className="btn" onClick={() => setTab("scene")}>🎬 {t("debug.tabs.scene")}</button>
                  <button type="button" className="btn" onClick={() => setTab("runtime")}>⚙ {t("debug.tabs.runtime")}</button>
                </div>
                <SceneSummary scene={data.scene} />
                <div className="debug-grid">
                  <div><span>{t("debug.panel.backendModel")}</span><b>{data.meta?.backend} · {data.meta?.model}</b></div>
                  <div><span>{t("debug.fields.turns")}</span><b>{data.meta?.turns ?? 0}</b></div>
                  <div><span>{t("debug.panel.characters")}</span><b>{npcs.length}</b></div>
                  <div><span>{t("debug.panel.time")}</span><b>{data.time?.current_date_label || "—"}</b></div>
                </div>
                <h4>{t("debug.panel.startBrief")}</h4>
                <TextBlock>{data.story?.brief}</TextBlock>
              </div>
            )}

            {tab === "story" && (
              <div className="dbg-tabpanel">
                <div className="dbg-controls">
                  <button type="button" className="btn primary" onClick={() => openModal({ type: "story" })}>✎ {t("debug.panel.editStory")}</button>
                </div>
                <h4>{t("debug.panel.objective")} <Info>{t("debug.panel.objectiveHelp")}</Info></h4>
                <TextBlock>{t("debug.panel.objectiveText")}</TextBlock>
                <h4>{t("debug.storyEditor.brief")} <Info>{t("debug.panel.briefHelp")}</Info></h4>
                <TextBlock>{data.story?.brief}</TextBlock>
                <h4>{t("debug.storyEditor.publicIntro")} <Info>{t("debug.panel.publicIntroHelp")}</Info></h4>
                <TextBlock>{data.story?.public_intro}</TextBlock>
                <h4>{t("debug.factsView.hiddenTruth")} <Info>{t("debug.panel.hiddenTruthHelp")}</Info></h4>
                <TextBlock secret>{data.story?.hidden_truth}</TextBlock>
                <h4>{t("debug.panel.hiddenEvents")}</h4>
                <DebugList items={data.story?.hidden_events} secret />
              </div>
            )}

            {tab === "scene" && (
              <div className="dbg-tabpanel">
                <div className="dbg-controls">
                  <button type="button" className="btn primary" onClick={() => openModal({ type: "scene" })}>✎ {t("debug.panel.editScene")}</button>
                </div>
                <SceneSummary scene={data.scene} />
                <h4>{t("debug.fields.description")}</h4>
                <TextBlock>{data.scene?.description}</TextBlock>
                <h4>{t("debug.sceneEditor.constraintsShort")}</h4>
                <DebugList items={data.scene?.constraints} />
                <h4>{t("debug.sceneEditor.items")}</h4>
                <DebugList items={asList(data.scene?.items).map((i) => i.name + (i.location ? ` · ${i.location}` : ""))} />
                <h4>{t("debug.sceneEditor.exits")}</h4>
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
                  <button type="button" className="btn" onClick={() => openModal({ type: "facts" })}>📖 {t("debug.panel.worldFacts")}</button>
                  <button type="button" className="btn" onClick={() => openModal({ type: "stateRecords" })}>🧬 {t("debug.panel.stateRecords")}</button>
                  <button type="button" className="btn" onClick={() => openModal({ type: "rumors" })}>🗣 {t("debug.tabs.rumors")}</button>
                </div>
                <Facts facts={data.facts} rumors={data.rumors} />
                <h4>{t("debug.panel.stateRecordsLong")}</h4>
                <DebugList items={asList(data.state_records).map((r) => `[${r.kind}/${r.scope}] ${r.text}`)} />
              </div>
            )}

            {tab === "memory" && (
              <div className="dbg-tabpanel">
                <h4>{t("debug.panel.gmSummary")}</h4>
                <TextBlock>{data.memory?.gm_summary}</TextBlock>
                <h4>{t("debug.panel.loadedTools")}</h4>
                <DebugList items={data.memory?.loaded_gm_tools} />
                <h4>{t("debug.panel.recentEvents")}</h4>
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
            <Modal key="rolls" depth={i} title={t("debug.modals.rollsTitle")} subtitle={t("debug.modals.rollsSubtitle")} onClose={closeTop}>
              <RollsControls override={override} onRun={runRoll} />
            </Modal>
          );
        }
        if (m.type === "facts") {
          return (
            <Modal key="facts" depth={i} wide title={t("debug.panel.worldFacts")} subtitle={t("debug.modals.addDelete")} onClose={closeTop}>
              <FactsManager facts={asList(data?.facts)} onAdd={runAddFact} onDelete={runDeleteFact} />
            </Modal>
          );
        }
        if (m.type === "stateRecords") {
          return (
            <Modal key="sr" depth={i} wide title={t("debug.panel.stateRecords")} subtitle={t("debug.modals.stateRecordsSubtitle")} onClose={closeTop}>
              <StateRecordsManager records={asList(data?.state_records)} npcs={npcs} onApply={runStateRecord} />
            </Modal>
          );
        }
        if (m.type === "rumors") {
          return (
            <Modal key="rumors" depth={i} wide title={t("debug.tabs.rumors")} subtitle={t("debug.modals.rumorsSubtitle")} onClose={closeTop}>
              <RumorsManager rumors={asList(data?.rumors)} onAction={runRumor} />
            </Modal>
          );
        }
        if (m.type === "story") {
          return (
            <Modal key="story" depth={i} wide title={t("debug.modals.storyTitle")} subtitle={t("debug.modals.storySubtitle")} onClose={closeTop}>
              <StoryEditor story={data?.story || {}} onSave={runUpdateStory} />
            </Modal>
          );
        }
        if (m.type === "scene") {
          return (
            <Modal key="scene" depth={i} wide title={t("debug.panel.editScene")} subtitle={data?.scene?.title || ""} onClose={closeTop}>
              <SceneEditor scene={data?.scene || {}} npcs={npcs} onSave={runUpdateScene} />
            </Modal>
          );
        }
        if (m.type === "playerEdit") {
          return (
            <Modal key="playerEdit" depth={i} wide title={t("debug.modals.playerTitle")} subtitle={t("debug.playerCard.sheet")} onClose={closeTop}>
              <PlayerEditor player={data?.player_character || {}} onSave={runUpdatePlayer} />
            </Modal>
          );
        }
        if (m.type === "npcs") {
          return (
            <Modal key="npcs" depth={i} title={t("debug.tabs.npcs")} subtitle={t("debug.modals.chooseToEdit")} onClose={closeTop}>
              <NpcPicker npcs={npcs} onPick={(id) => pushModal({ type: "npcEdit", id })} />
            </Modal>
          );
        }
        if (m.type === "npcEdit") {
          const npc = npcs.find((n) => n.id === m.id);
          if (!npc) return null;
          return (
            <Modal key={`npcEdit-${m.id}`} depth={i} wide title={<>{t("debug.modals.editPrefix")} <span style={{ color: npc.color || "var(--entity-unknown)" }}>{npc.name}</span></>} subtitle={`ID: ${npc.id}`} onClose={closeTop}>
              <NpcEditor npc={npc} statusLabels={data?.status_labels || {}} onSave={runUpdateNpc} />
            </Modal>
          );
        }
        return null;
      })}
    </>
  );
}
