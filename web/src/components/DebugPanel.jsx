import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../api.js";
import MarkdownText from "./MarkdownText.jsx";
import Modal from "./Modal.jsx";
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
            <button type="button" className="icon-btn danger" title="Удалить факт" onClick={() => onDelete(f.id)}>🗑</button>
          </div>
        )) : <Empty>фактов пока нет</Empty>}
      </div>
    </div>
  );
}

// --- Список персонажей: клик -> правка (слой 2) ---
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

// --- Редактор карточки персонажа (полная карточка) ---
function NpcEditor({ npc, statusLabels, onSave }) {
  const [d, setD] = useState(() => ({
    name: npc.name || "", color: npc.color || "", role: npc.role || "", pronouns: npc.pronouns || "",
    persona: npc.persona || "", voice: npc.voice || "", goals: npc.goals || "",
    knowledge: npc.knowledge || "", secret: npc.secret || "",
    present: !!npc.present,
    wb_location: npc.whereabouts?.location_name || "",
    wb_status: npc.whereabouts?.status || "unknown",
    wb_details: npc.whereabouts?.details || "",
    reset_memory: false,
  }));
  const set = (patch) => setD((p) => ({ ...p, ...patch }));
  const statusEntries = Object.entries(statusLabels || {});
  const secretChanged = d.secret !== (npc.secret || "");
  const presenceChanged = d.present !== !!npc.present;
  const save = () => {
    const body = {
      id: npc.id,
      fields: {
        name: d.name, color: d.color, role: d.role, pronouns: d.pronouns,
        persona: d.persona, voice: d.voice, goals: d.goals, knowledge: d.knowledge, secret: d.secret,
      },
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
        <EditField label="Цвет">
          <span className="dbg-color">
            <input type="color" value={/^#[0-9a-fA-F]{6}$/.test(d.color) ? d.color : "#908caa"} onChange={(e) => set({ color: e.target.value })} />
            <input value={d.color} placeholder="#e6c08a" onChange={(e) => set({ color: e.target.value })} />
          </span>
        </EditField>
        <EditField label="Роль"><input value={d.role} onChange={(e) => set({ role: e.target.value })} /></EditField>
        <EditField label="Род"><input value={d.pronouns} placeholder="M, F, N, PL, OTHER" onChange={(e) => set({ pronouns: e.target.value })} /></EditField>
      </div>

      <EditField label="Описание"><textarea rows={2} value={d.persona} onChange={(e) => set({ persona: e.target.value })} /></EditField>
      <EditField label="Голос"><textarea rows={2} value={d.voice} onChange={(e) => set({ voice: e.target.value })} /></EditField>
      <EditField label="Мотивы"><textarea rows={2} value={d.goals} onChange={(e) => set({ goals: e.target.value })} /></EditField>
      <EditField label="Что знает"><textarea rows={2} value={d.knowledge} onChange={(e) => set({ knowledge: e.target.value })} /></EditField>
      <EditField label="Секрет (в контекст ГМ не попадает)"><textarea rows={2} className="dbg-secret" value={d.secret} onChange={(e) => set({ secret: e.target.value })} /></EditField>
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

function asList(items) {
  return Array.isArray(items) ? items.filter((item) => item != null && item !== "") : [];
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

function NpcCard({ npc, statusLabels = {} }) {
  const status = npc.whereabouts?.status || (npc.present ? "present" : "unknown");
  return (
    <details className="debug-npc" open={npc.present}>
      <summary>
        <span>
          <b style={{ color: npc.color || "var(--entity-unknown)" }}>{npc.name}</b>
          <em>{npc.role || "персонаж"}{npc.present ? " · в сцене" : ""}</em>
        </span>
        <small>{npc.messages || 0} сообщ.</small>
      </summary>
      <div className="debug-grid">
        <div><span>id</span><b>{npc.id}</b></div>
        <div><span>род</span><b>{npc.pronouns || "—"}</b></div>
        <div><span>где</span><b>{npc.whereabouts?.location_name || npc.whereabouts?.location_id || "—"}</b></div>
        <div><span>статус</span><b>{statusLabels[status] || status}</b></div>
      </div>

      <h4>Мотивы</h4>
      <TextBlock>{npc.goals}</TextBlock>

      <h4>Что знает</h4>
      <TextBlock>{npc.knowledge}</TextBlock>

      <h4>Секрет</h4>
      <TextBlock secret>{npc.secret}</TextBlock>

      <h4>Память NPC</h4>
      <TextBlock>{npc.summary || npc.history}</TextBlock>

      <h4>Коммиты ответа</h4>
      <DebugList items={npc.commitments} />
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

export default function DebugPanel({ refreshKey = "" }) {
  const [open, setOpen] = useState(false);
  const [data, setData] = useState(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [stack, setStack] = useState([]);

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
  const runUpdateNpc = useCallback((body) => { apply(api.updateNpc(body)); closeTop(); }, [apply, closeTop]);

  const override = data?.roll_override || {};
  const rollBadge = [
    override.next != null ? `след:${override.next}` : "",
    override.all != null ? `все:${override.all}` : "",
  ].filter(Boolean).join(" · ");

  const title = data?.scene?.title || "История";

  return (
    <>
      <button
        type="button"
        className={["debug-tab", open ? "open" : ""].filter(Boolean).join(" ")}
        onClick={() => setOpen((value) => !value)}
        aria-expanded={open}
        aria-controls="debug-drawer"
        title="Открыть дебаг истории"
      >
        Дебаг
      </button>

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
            {loading ? "Обновляю…" : "Обновить"}
          </button>
          {data?.meta && <span>{data.meta.backend} · {data.meta.model} · ходов: {data.meta.turns}</span>}
        </div>

        {error && <div className="err">debug: {error}</div>}
        {!data && !error && <Empty>{loading ? "загружаю…" : "панель ещё не открывалась"}</Empty>}

        {data && (
          <div className="debug-body">
            <details className="debug-section" open>
              <summary>⚙ Управление</summary>
              <div className="dbg-controls">
                <button type="button" className="btn" onClick={() => openModal({ type: "rolls" })}>
                  🎲 Броски{rollBadge ? ` · ${rollBadge}` : ""}
                </button>
                <button type="button" className="btn" onClick={() => openModal({ type: "facts" })}>📖 Факты мира</button>
                <button type="button" className="btn" onClick={() => openModal({ type: "npcs" })}>👤 Персонажи</button>
              </div>
            </details>

            <details className="debug-section" open>
              <summary>Цель и канон</summary>
              <h4>Цель ведения</h4>
              <TextBlock>{data.story?.objective}</TextBlock>
              <h4>Что игрок знает на старте</h4>
              <TextBlock>{data.story?.public_intro}</TextBlock>
              <h4>Что по факту произошло</h4>
              <TextBlock secret>{data.story?.hidden_truth}</TextBlock>
            </details>

            <details className="debug-section" open>
              <summary>Факты и слухи</summary>
              <Facts facts={data.facts} rumors={data.rumors} />
            </details>

            <details className="debug-section">
              <summary>Текущая сцена</summary>
              <SceneSummary scene={data.scene} />
              <h4>Описание</h4>
              <TextBlock>{data.scene?.description}</TextBlock>
              <h4>Ограничения</h4>
              <DebugList items={data.scene?.constraints || data.story?.constraints} />
              <h4>Скрытые события</h4>
              <DebugList items={data.story?.hidden_events} secret />
            </details>

            <details className="debug-section" open>
              <summary>Персонажи и секреты</summary>
              <div className="debug-npcs">
                {asList(data.npcs).map((npc) => <NpcCard key={npc.id} npc={npc} statusLabels={data.status_labels || {}} />)}
              </div>
            </details>

            <details className="debug-section">
              <summary>Память и события</summary>
              <h4>Сводка ГМ</h4>
              <TextBlock>{data.memory?.gm_summary}</TextBlock>
              <h4>Загруженные тулы ГМ</h4>
              <DebugList items={data.memory?.loaded_gm_tools} />
              <h4>Последние события</h4>
              <Events events={data.memory?.events} npcs={asList(data?.npcs)} />
            </details>
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
        if (m.type === "npcs") {
          return (
            <Modal key="npcs" depth={i} title="Персонажи" subtitle="выбери для правки" onClose={closeTop}>
              <NpcPicker npcs={asList(data?.npcs)} onPick={(id) => pushModal({ type: "npcEdit", id })} />
            </Modal>
          );
        }
        if (m.type === "npcEdit") {
          const npc = asList(data?.npcs).find((n) => n.id === m.id);
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
