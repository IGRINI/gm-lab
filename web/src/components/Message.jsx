import { memo, useContext } from "react";
import MarkdownText, { MarkdownInline } from "./MarkdownText.jsx";
import Spoiler from "./Spoiler.jsx";
import Tooltip from "./Tooltip.jsx";
import ToolCard from "./ToolCard.jsx";
import ToolResultCard from "./ToolResultCard.jsx";
import DiceRoll from "./DiceRoll.jsx";
import { NpcRosterContext } from "../npcContext.js";
import { StatusLabelsContext } from "../statusContext.js";
import { nameColor } from "../nameColor.js";
import { fmtK } from "../util.js";

function ListBody({ items }) {
  const list = items && items.length ? items : ["—"];
  return (
    <ul>
      {list.map((t, i) => (
        <li key={i}><MarkdownInline>{t}</MarkdownInline></li>
      ))}
    </ul>
  );
}

function metaText(d) {
  const cached = d.cached ? ` · ${fmtK(d.cached)}↻ кэш` : "";
  return `⏱ ${d.secs}s · ${d.tps} tok/s · ${fmtK(d.in)}↑ ${fmtK(d.out)}↓ ток${cached}`;
}
function metaTitle(d) {
  return (
    `${d.label}\nввод: ${d.in} ток / ${d.prompt_secs}s` +
    (d.cached ? `\nиз кэша: ${d.cached} ток` : "") +
    `\nвывод: ${d.out} ток / ${d.eval_secs}s` +
    (d.load_secs > 0 ? `\nзагрузка модели: ${d.load_secs}s` : "")
  );
}
function metaTotalTitle(d) {
  return (
    d.calls.map((m) => `${m.label}: ${m.in}↑ ${m.out}↓  ${m.tps} tok/s  ${m.secs}s`).join("\n") +
    `\n— — —\n≈ системный промпт ГМ: ~${d.sys_estimate} ток (оценка, входит в «ввод»)`
  );
}

function namesFromIds(ids, roster) {
  const rows = Array.isArray(ids) ? ids : [];
  const byId = new Map((roster || []).map((n) => [n.id, n.name || n.id]));
  return rows.map((id) => byId.get(id) || id).filter(Boolean);
}

// Inline colored character name (used in pills/steps where there's no markdown).
function NameTag({ name, roster }) {
  return <b style={{ color: nameColor(name, roster) }}>{name}</b>;
}

function Message({ m }) {
  const roster = useContext(NpcRosterContext);
  const statusLabels = useContext(StatusLabelsContext);
  const presentNames = namesFromIds(m.present_npcs, roster);
  switch (m.type) {
    case "player":
      return (
        <div className="player">
          <div className="who">Вы</div>
          <MarkdownText>{m.text}</MarkdownText>
        </div>
      );

    case "narration":
      return (
        <div className="narration">
          <div className="who">Гейм-мастер</div>
          <MarkdownText>{m.text}</MarkdownText>
        </div>
      );

    case "gm_think":
      return (
        <div className="step think">
          <Spoiler label="🧠 ГМ думает"><MarkdownText>{m.text || "—"}</MarkdownText></Spoiler>
        </div>
      );

    case "npc": {
      const npcAccent = nameColor(m.name, roster);
      return (
        <div className="card" style={{ "--c": npcAccent }}>
          <div className="hd">
            <span className="dot" style={{ "--c": npcAccent }} />
            <b><MarkdownInline>{m.name}</MarkdownInline></b>
            <span className="tag">персонаж</span>
          </div>
          <div className="speech">
            {m.revealed ? (
              <>
                <span className="q">«</span>
                <span className="txt"><MarkdownInline>{m.speech}</MarkdownInline></span>
                <span className="q">»</span>
              </>
            ) : (
              <span className="typing">печатает…</span>
            )}
          </div>
          {m.hidden != null && (
            <Spoiler label="🧠 Скрытые мысли (игрок не видит)"><MarkdownText>{m.hidden}</MarkdownText></Spoiler>
          )}
          {m.action && <div className="action">— <MarkdownInline>{m.action}</MarkdownInline></div>}
          {m.claims != null && (
            <Spoiler label="📌 Опора ответа">
              <ListBody items={m.claims} />
            </Spoiler>
          )}
        </div>
      );
    }

    case "tool":
      return <ToolCard name={m.name} args={m.args} result={m.result} resultLive={m.resultLive} rollId={m.id} />;

    case "tool_result":
      return <ToolResultCard name={m.name} payload={m.payload} />;

    case "fact":
      return (
        <div className="step">
          <Spoiler label="📖 факт мира (ГМ запросил)"><MarkdownText>{m.text}</MarkdownText></Spoiler>
        </div>
      );

    case "dice_roll":
      return <DiceRoll roll={m.roll} animate={m.resultLive} rollId={m.id} />;

    case "dice":
      return <div className="step dice">🎲 <MarkdownInline>{m.text}</MarkdownInline></div>;

    case "scene_update":
      if (m.title || m.scene_id) {
        return (
          <div className="step">
          <div className="pill ok">Сцена: {m.title || m.scene_id}</div>
          <div className="spoiler-body" style={{ border: 0, padding: 0, color: "var(--spoiler-text)" }}>
              <MarkdownText>Сейчас в сцене: {presentNames.join(", ") || "нет именованных персонажей"}</MarkdownText>
          </div>
          </div>
        );
      }
      return (
        <div className="step">
          <div className="pill ok">
            Сцена: <NameTag name={m.name} roster={roster} /> теперь {m.present ? "в сцене" : "вне сцены"}
          </div>
          <div className="spoiler-body" style={{ border: 0, padding: 0, color: "var(--spoiler-text)" }}>
            <MarkdownText>Сейчас в сцене: {presentNames.join(", ") || "нет именованных персонажей"}</MarkdownText>
          </div>
        </div>
      );

    case "npc_whereabouts": {
      const w = m.whereabouts || {};
      const status = statusLabels[w.status] || w.status || "неизвестно";
      const place = w.location_name || w.location_id || "место не установлено";
      return (
        <div className="step">
          <div className="pill ok">Местонахождение: <NameTag name={m.name} roster={roster} /> — {status}</div>
          <div className="spoiler-body" style={{ border: 0, padding: 0, color: "var(--spoiler-text)" }}>
            <MarkdownText>
              {`**Где искать:** ${place}${w.details ? `\n\n${w.details}` : ""}`}
            </MarkdownText>
          </div>
        </div>
      );
    }

    case "command":
      return (
        <div className="step">
          <div className="pill ok"><MarkdownInline>{m.text}</MarkdownInline></div>
        </div>
      );

    case "reject":
      return (
        <div>
          <div className="pill redo">✗ ГМ вернул действие <NameTag name={m.name} roster={roster} /> на переделку</div>
          <div className="reason">Замечание ГМ: <MarkdownInline>{m.reason}</MarkdownInline></div>
        </div>
      );

    case "error":
      return <div className="err">⚠ {m.agent}: <MarkdownInline>{m.text}</MarkdownInline></div>;

    case "meta":
      return (
        <Tooltip as="div" className="meta" content={metaTitle(m.data)}>
          {metaText(m.data)}
        </Tooltip>
      );

    case "meta_total": {
      const d = m.data;
      const cached = d.cached ? ` · ${fmtK(d.cached)}↻ кэш` : "";
      return (
        <Tooltip as="div" className="meta-total" content={metaTotalTitle(d)}>
          <b>Σ за ход: </b>
          {`⏱ ${d.secs}s · `}
          <span className="tok">{fmtK(d.tokens)} токенов</span>
          {` (${fmtK(d.in)}↑ ввод / ${fmtK(d.out)}↓ вывод)${cached} · ${d.calls.length} вызовов`}
        </Tooltip>
      );
    }

    default:
      return null;
  }
}

export default memo(Message);
