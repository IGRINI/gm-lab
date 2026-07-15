import Icon from "./Icon.jsx";
import { memo, useContext } from "react";
import MarkdownText, { MarkdownInline } from "./MarkdownText.jsx";
import Spoiler from "./Spoiler.jsx";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import ToolCard from "./ToolCard.jsx";
import ToolResultCard from "./ToolResultCard.jsx";
import DiceRoll from "./DiceRoll.jsx";
import { NpcRosterContext } from "../npcContext.js";
import { StatusLabelsContext } from "../statusContext.js";
import { VisibilityContext, toolMode } from "../devSettings.js";
import { nameColor } from "../nameColor.js";
import { fmtK } from "../util.js";
import {
  ttsToggle,
  ttsPause,
  ttsResume,
  ttsStop,
  useTtsState,
  gmSegments,
  npcSegments,
  genderVoice,
} from "../ttsStore.js";

// Resolve an NPC's voice from the roster (by id, falling back to the shown name),
// since historical messages may lack npc_id.
function npcVoice(roster, npc_id, name) {
  const npc = (roster || []).find((n) => (npc_id && n.id === npc_id) || n.name === name);
  return genderVoice(npc?.pronouns ?? npc?.gender);
}

// Speaker button shown top-right of GM narration and NPC cards. Click streams +
// plays the sequence (audio starts ~0.4s in and continues as it generates); click
// again stops. One message plays at a time. `segments` is a list of {text, body};
// NPC cards prefer ordered visible beats and fall back to older speech/action rows.
function TtsButton({ msgKey, segments }) {
  const st = useTtsState(msgKey);
  if (!(segments || []).some((s) => s && s.text && s.text.trim())) return null;
  const status = st.status;

  // While a clip plays, expose pause/resume + stop (pause appears only then).
  if (status === "playing" || status === "paused") {
    return (
      <span className="tts-ctl">
        {status === "playing" ? (
          <Tooltip className="tooltip-wrap" tipClassName="ui-tip-wrap" focusable={false}
            content={<TipContent title="Пауза" note="Приостановить текущую озвучку." />}>
            <button type="button" className="tts-btn is-playing" onClick={() => ttsPause(msgKey)}
              aria-label="Пауза"><Icon name="pause" size={14} /></button>
          </Tooltip>
        ) : (
          <Tooltip className="tooltip-wrap" tipClassName="ui-tip-wrap" focusable={false}
            content={<TipContent title="Продолжить" note="Возобновить озвучку с места паузы." />}>
            <button type="button" className="tts-btn is-playing" onClick={() => ttsResume(msgKey)}
              aria-label="Продолжить"><Icon name="play" size={14} /></button>
          </Tooltip>
        )}
        <Tooltip className="tooltip-wrap" tipClassName="ui-tip-wrap" focusable={false}
          content={<TipContent title="Стоп" note="Остановить озвучку этого сообщения." />}>
          <button type="button" className="tts-btn" onClick={() => ttsStop(msgKey)}
            aria-label="Стоп"><Icon name="square" size={13} /></button>
        </Tooltip>
      </span>
    );
  }

  const icon = status === "error" ? <Icon name="alert" size={14} /> : <Icon name="volume" size={14} />;
  const title = status === "error" ? "Ошибка озвучки — повторить" : "Озвучить";
  return (
    <span className="tts-ctl">
      <Tooltip
        className="tooltip-wrap"
        tipClassName="ui-tip-wrap"
        focusable={false}
        content={<TipContent title={title} note="Сгенерировать и воспроизвести аудио для видимого текста." />}
      >
        <button
          type="button"
          className="tts-btn"
          onClick={() => ttsToggle(msgKey, segments)}
          aria-label={title}
        >
          {icon}
        </button>
      </Tooltip>
    </span>
  );
}

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
  const vis = useContext(VisibilityContext);
  const presentNames = namesFromIds(m.present_npcs, roster);
  switch (m.type) {
    case "player":
      return (
        <div className="player">
          <MarkdownText>{m.text}</MarkdownText>
        </div>
      );

    case "narration":
      return (
        <div className="narration has-tts">
          <TtsButton msgKey={`${m.sid}:narration`} segments={gmSegments(m.text)} />
          <div className="who">Гейм-мастер</div>
          <MarkdownText>{m.text}</MarkdownText>
        </div>
      );

    case "gm_think":
      if (!vis.gmThoughts) return null;
      return (
        <div className="step think">
          <Spoiler label="🧠 ГМ думает"><MarkdownText>{m.text || "—"}</MarkdownText></Spoiler>
        </div>
      );

    case "npc": {
      const npcAccent = nameColor(m.name, roster);
      return (
        <div className="card has-tts" style={{ "--c": npcAccent }}>
          <TtsButton
            msgKey={`${m.sid}:npc`}
            segments={npcSegments({
              name: m.name,
              response: m.response,
              beats: m.beats,
              speech: m.speech,
              action: m.action,
              voice: npcVoice(roster, m.npc_id, m.name),
            })}
          />
          <div className="hd">
            <span className="dot" style={{ "--c": npcAccent }} />
            <b><MarkdownInline>{m.name}</MarkdownInline></b>
          </div>
          <div className="speech">
            {m.revealed ? (
              <span className="txt"><MarkdownInline>{m.response || m.speech}</MarkdownInline></span>
            ) : (
              <span className="typing">печатает…</span>
            )}
          </div>
          {vis.npcInternals && m.hidden != null && (
            <Spoiler label="🧠 Скрытые мысли (игрок не видит)"><MarkdownText>{m.hidden}</MarkdownText></Spoiler>
          )}
          {vis.npcInternals && Array.isArray(m.beats) && m.beats.length > 0 && (
            <Spoiler label="Видимые шаги">
              <ListBody items={m.beats.map((beat) => `${beat.kind}: ${beat.text}`)} />
            </Spoiler>
          )}
          {!m.response && m.action && <div className="action">— <MarkdownInline>{m.action}</MarkdownInline></div>}
          {vis.npcInternals && m.claims != null && (
            <Spoiler label="📌 Опора ответа">
              <ListBody items={m.claims} />
            </Spoiler>
          )}
        </div>
      );
    }

    case "tool": {
      const mode = toolMode(m.name, vis);
      if (mode === "hidden") return null;
      return (
        <ToolCard
          name={m.name}
          args={m.args}
          result={m.result}
          resultLive={m.resultLive}
          rollId={m.id}
          mode={mode}
        />
      );
    }

    case "tool_result": {
      const mode = toolMode(m.name, vis);
      if (mode === "hidden") return null;
      return <ToolResultCard name={m.name} payload={m.payload} showRaw={vis.toolCalls} />;
    }

    case "fact":
      if (!vis.memoryOps) return null;
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
          <div className="step-note">
              <MarkdownText>{`Сейчас в сцене: ${presentNames.join(", ") || "нет именованных персонажей"}`}</MarkdownText>
          </div>
          </div>
        );
      }
      return (
        <div className="step">
          <div className="pill ok">
            Сцена: <NameTag name={m.name} roster={roster} /> теперь {m.present ? "в сцене" : "вне сцены"}
          </div>
          <div className="step-note">
            <MarkdownText>{`Сейчас в сцене: ${presentNames.join(", ") || "нет именованных персонажей"}`}</MarkdownText>
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
          <div className="step-note">
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
      if (!vis.gmThoughts) return null;
      return (
        <div className="step">
          <div className="pill redo">✗ ГМ вернул действие <NameTag name={m.name} roster={roster} /> на переделку</div>
          <div className="reason">Замечание ГМ: <MarkdownInline>{m.reason}</MarkdownInline></div>
        </div>
      );

    case "error":
      return <div className="err">⚠ {m.agent}: <MarkdownInline>{m.text}</MarkdownInline></div>;

    case "meta":
      if (!vis.messageTokens) return null;
      return (
        <Tooltip as="div" className="meta" content={metaTitle(m.data)}>
          {metaText(m.data)}
        </Tooltip>
      );

    case "meta_total": {
      if (!vis.messageTokens) return null;
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
