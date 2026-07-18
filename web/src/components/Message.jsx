import Icon from "./Icon.jsx";
import { memo, useCallback, useContext, useState } from "react";
import MarkdownText, { MarkdownInline } from "./MarkdownText.jsx";
import Spoiler from "./Spoiler.jsx";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import ToolCard from "./ToolCard.jsx";
import ToolResultCard from "./ToolResultCard.jsx";
import DiceRoll from "./DiceRoll.jsx";
import { ZoomableImage } from "./ImagePreview.jsx";
import NpcTooltip from "./NpcTooltip.jsx";
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
import { useTranslation } from "react-i18next";
import { localizeServerMessage } from "../serverMessages.js";

// Resolve an NPC's voice from the roster (by id, falling back to the shown name),
// since historical messages may lack npc_id.
function npcVoice(roster, npc_id, name) {
  const npc = (roster || []).find((n) => (npc_id && n.id === npc_id) || n.name === name);
  return genderVoice(npc?.pronouns ?? npc?.gender);
}

function npcFromRoster(roster, npc_id, name) {
  return (roster || []).find((n) => (npc_id && n.id === npc_id) || n.name === name) || null;
}

// Speaker button shown top-right of GM narration and NPC cards. Click streams +
// plays the sequence (audio starts ~0.4s in and continues as it generates); click
// again stops. One message plays at a time. `segments` is a list of {text, body};
// NPC cards prefer ordered visible beats and fall back to older speech/action rows.
function TtsButton({ msgKey, segments }) {
  const { t } = useTranslation("game");
  const st = useTtsState(msgKey);
  if (!(segments || []).some((s) => s && s.text && s.text.trim())) return null;
  const status = st.status;

  // While a clip plays, expose pause/resume + stop (pause appears only then).
  if (status === "playing" || status === "paused") {
    return (
      <span className="tts-ctl">
        {status === "playing" ? (
          <Tooltip className="tooltip-wrap" tipClassName="ui-tip-wrap" focusable={false}
            content={<TipContent title={t("tts.pause")} note={t("tts.pauseNote")} />}>
            <button type="button" className="tts-btn is-playing" onClick={() => ttsPause(msgKey)}
              aria-label={t("tts.pause")}><Icon name="pause" size={14} /></button>
          </Tooltip>
        ) : (
          <Tooltip className="tooltip-wrap" tipClassName="ui-tip-wrap" focusable={false}
            content={<TipContent title={t("tts.resume")} note={t("tts.resumeNote")} />}>
            <button type="button" className="tts-btn is-playing" onClick={() => ttsResume(msgKey)}
              aria-label={t("tts.resume")}><Icon name="play" size={14} /></button>
          </Tooltip>
        )}
        <Tooltip className="tooltip-wrap" tipClassName="ui-tip-wrap" focusable={false}
          content={<TipContent title={t("tts.stop")} note={t("tts.stopNote")} />}>
          <button type="button" className="tts-btn" onClick={() => ttsStop(msgKey)}
            aria-label={t("tts.stop")}><Icon name="square" size={13} /></button>
        </Tooltip>
      </span>
    );
  }

  const icon = status === "error" ? <Icon name="alert" size={14} /> : <Icon name="volume" size={14} />;
  const title = status === "error" ? t("tts.errorRetry") : t("tts.play");
  return (
    <span className="tts-ctl">
      <Tooltip
        className="tooltip-wrap"
        tipClassName="ui-tip-wrap"
        focusable={false}
        content={<TipContent title={title} note={t("tts.playNote")} />}
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

function metaText(d, t) {
  const cached = d.cached ? ` · ${t("message.meta.cached", { count: fmtK(d.cached) })}` : "";
  return t("message.meta.line", {
    seconds: d.secs,
    tps: d.tps,
    input: fmtK(d.in),
    output: fmtK(d.out),
    cached,
  });
}
const META_LABEL_KEYS = new Map([
  ["GM — narration", "narration"],
  ["GM — decision", "decision"],
  ["GM — prelude", "prelude"],
  ["ГМ — нарратив", "narration"],
  ["ГМ — решение", "decision"],
  ["ГМ — прелюдия", "prelude"],
  ["scene sync", "sceneSync"],
]);
function metaLabel(d, t) {
  const knownKey = META_LABEL_KEYS.get(String(d?.label || "").trim());
  if (knownKey) return t(`message.meta.labels.${knownKey}`);
  if (d?.scope === "gm") return t("message.meta.labels.gm");
  if (d?.scope === "other") return t("message.meta.labels.background");
  return String(d?.label || t("message.meta.labels.model"));
}
function metaTitle(d, t) {
  return (
    `${metaLabel(d, t)}\n${t("message.meta.input", { count: d.in, seconds: d.prompt_secs })}` +
    (d.cached ? `\n${t("message.meta.fromCache", { count: d.cached })}` : "") +
    `\n${t("message.meta.output", { count: d.out, seconds: d.eval_secs })}` +
    (d.load_secs > 0 ? `\n${t("message.meta.modelLoad", { seconds: d.load_secs })}` : "")
  );
}
function metaTotalTitle(d, t) {
  return (
    d.calls.map((m) => `${metaLabel(m, t)}: ${m.in}↑ ${m.out}↓  ${m.tps} tok/s  ${m.secs}s`).join("\n") +
    `\n— — —\n${t("message.meta.systemPrompt", { count: d.sys_estimate })}`
  );
}

function namesFromIds(ids, roster) {
  const rows = Array.isArray(ids) ? ids : [];
  const byId = new Map((roster || []).map((n) => [n.id, n.name || n.id]));
  return rows.map((id) => byId.get(id) || id).filter(Boolean);
}

function sceneSnapshotFromMessage(message, liveScene) {
  const stored = message?.scene && typeof message.scene === "object"
    ? message.scene
    : {
        scene_id: message?.scene_id,
        location_id: message?.location_id,
        title: message?.title,
        description: message?.description,
        image_url: message?.image_url,
        present_npcs: message?.present_npcs || [],
      };
  const current = liveScene && typeof liveScene === "object" ? liveScene : null;
  const storedIdentity = stored.location_id || stored.scene_id || "";
  const currentIdentity = current?.location_id || current?.scene_id || "";
  const matchesCurrent = Boolean(
    current
      && ((storedIdentity && currentIdentity && storedIdentity === currentIdentity)
        || (!storedIdentity && stored.title && stored.title === current.title))
  );
  const fallback = matchesCurrent ? current : {};
  return {
    ...fallback,
    ...stored,
    image_url: stored.image_url || fallback.image_url || "",
    present_npcs: stored.present_npcs || fallback.present_npcs || [],
  };
}

// Inline colored character name (used in pills/steps where there's no markdown).
function NameTag({ name, roster }) {
  const npc = npcFromRoster(roster, null, name);
  return (
    <NpcTooltip npc={npc} label={name}>
      <b style={{ color: nameColor(name, roster) }}>{name}</b>
    </NpcTooltip>
  );
}

function agentLabel(agent, t) {
  const value = String(agent || "").trim();
  return ["gm", "GM", "ГМ", "Гейм-мастер"].includes(value)
    ? t("message.gameMaster")
    : value;
}

function PlayerMessage({ m, onEditFrom, onBranchFrom, historyBusy }) {
  const { t } = useTranslation("game");
  const [mode, setMode] = useState("");
  const [draft, setDraft] = useState(m.text || "");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");
  const rewindable = m.rewindable === true && Number.isInteger(m.turn) && m.turn > 0;
  const canEdit = rewindable && typeof onEditFrom === "function";
  const canBranch = rewindable && typeof onBranchFrom === "function";

  const begin = useCallback((nextMode) => {
    setDraft(m.text || "");
    setError("");
    setMode(nextMode);
  }, [m.text]);

  const cancel = useCallback(() => {
    if (submitting) return;
    setMode("");
    setError("");
  }, [submitting]);

  const submit = useCallback(async () => {
    const text = String(draft || "").trim();
    const action = mode === "edit" ? onEditFrom : onBranchFrom;
    if (!text || typeof action !== "function" || submitting || historyBusy) return;
    setSubmitting(true);
    setError("");
    try {
      await action(m.turn, text);
      setMode("");
    } catch (reason) {
      setError(localizeServerMessage(reason, t, {
        fallbackText: t("message.history.changeFailed"),
      }));
    } finally {
      setSubmitting(false);
    }
  }, [draft, historyBusy, m.turn, mode, onBranchFrom, onEditFrom, submitting, t]);

  return (
    <div className="player-message">
      <div className="player">
        <MarkdownText>{m.text}</MarkdownText>
      </div>
      {(canEdit || canBranch) && !mode ? (
        <div className="player-message-actions" aria-label={t("message.history.actionsAria")}>
          {canEdit ? (
            <button
              type="button"
              onClick={() => begin("edit")}
              disabled={historyBusy}
              title={t("message.history.editTitle")}
              aria-label={t("message.history.editAria")}
            >
              <Icon name="pen" size={13} />
              <span>{t("message.history.edit")}</span>
            </button>
          ) : null}
          {canBranch ? (
            <button
              type="button"
              onClick={() => begin("branch")}
              disabled={historyBusy}
              title={t("message.history.branchTitle")}
              aria-label={t("message.history.branchAria")}
            >
              <Icon name="branch" size={14} />
              <span>{t("message.history.branch")}</span>
            </button>
          ) : null}
        </div>
      ) : null}
      {mode ? (
        <div className="player-message-editor">
          <textarea
            rows={3}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.ctrlKey || event.metaKey)) {
                event.preventDefault();
                void submit();
              }
            }}
            disabled={submitting || historyBusy}
            autoFocus
            aria-label={mode === "edit" ? t("message.history.editInputAria") : t("message.history.branchInputAria")}
          />
          <div className="player-message-editor-note">
            {mode === "edit"
              ? t("message.history.editNote")
              : t("message.history.branchNote")}
          </div>
          {error ? <div className="player-message-editor-error">{error}</div> : null}
          <div className="player-message-editor-actions">
            <button type="button" className="btn ghost" onClick={cancel} disabled={submitting}>
              {t("actions.cancel")}
            </button>
            <button
              type="button"
              className="btn primary"
              onClick={() => void submit()}
              disabled={!draft.trim() || submitting || historyBusy}
            >
              {submitting
                ? t("message.history.preparing")
                : mode === "edit"
                ? t("message.history.editContinue")
                : t("message.history.createBranch")}
            </button>
          </div>
        </div>
      ) : null}
    </div>
  );
}

function Message({
  m,
  scene,
  onOpenScene,
  onRetry,
  retryBusy = false,
  onEditFrom,
  onBranchFrom,
  historyBusy = false,
}) {
  const { t } = useTranslation("game");
  const roster = useContext(NpcRosterContext);
  const statusLabels = useContext(StatusLabelsContext);
  const vis = useContext(VisibilityContext);
  const presentNames = namesFromIds(m.present_npcs, roster);
  switch (m.type) {
    case "player":
      return (
        <PlayerMessage
          m={m}
          onEditFrom={onEditFrom}
          onBranchFrom={onBranchFrom}
          historyBusy={historyBusy}
        />
      );

    case "narration":
      return (
        <div className="narration has-tts">
          <TtsButton msgKey={`${m.sid}:narration`} segments={gmSegments(m.text)} />
          <div className="who">{t("message.gameMaster")}</div>
          <MarkdownText>{m.text}</MarkdownText>
        </div>
      );

    case "gm_think":
      if (!vis.gmThoughts) return null;
      return (
        <div className="step think">
          <Spoiler label={t("message.gmThinking")}><MarkdownText>{m.text || "—"}</MarkdownText></Spoiler>
        </div>
      );

    case "npc": {
      const npcAccent = nameColor(m.name, roster);
      const npc = npcFromRoster(roster, m.npc_id, m.name);
      const portraitUrl = npc?.portrait_url || "";
      return (
        <div className={`card npc-message has-tts${portraitUrl ? " has-portrait" : ""}`} style={{ "--c": npcAccent }}>
          {portraitUrl && (
            <ZoomableImage
              className="npc-message-portrait"
              src={portraitUrl}
              alt={m.name || ""}
              title={m.name || ""}
              loading="lazy"
            />
          )}
          <div className="npc-message-body">
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
                <span className="typing">{t("message.typing")}</span>
              )}
            </div>
            {vis.npcInternals && m.hidden != null && (
              <Spoiler label={t("message.hiddenThoughts")}><MarkdownText>{m.hidden}</MarkdownText></Spoiler>
            )}
            {vis.npcInternals && Array.isArray(m.beats) && m.beats.length > 0 && (
              <Spoiler label={t("message.visibleSteps")}>
                <ListBody items={m.beats.map((beat) => `${beat.kind}: ${beat.text}`)} />
              </Spoiler>
            )}
            {!m.response && m.action && <div className="action">— <MarkdownInline>{m.action}</MarkdownInline></div>}
            {vis.npcInternals && m.claims != null && (
              <Spoiler label={t("message.responseBasis")}>
                <ListBody items={m.claims} />
              </Spoiler>
            )}
          </div>
        </div>
      );
    }

    case "tool": {
      const mode = toolMode(m.name, vis, m);
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
      const mode = toolMode(m.name, vis, m);
      if (mode === "hidden") return null;
      if (mode === "player") {
        return <ToolCard name={m.name} result={m.payload} mode="player" />;
      }
      return <ToolResultCard name={m.name} payload={m.payload} showRaw={vis.toolCalls} />;
    }

    case "fact":
      if (!vis.memoryOps) return null;
      return (
        <div className="step">
          <Spoiler label={t("message.worldFact")}><MarkdownText>{m.text}</MarkdownText></Spoiler>
        </div>
      );

    case "dice_roll":
      return <DiceRoll roll={m.roll} animate={m.resultLive} rollId={m.id} />;

    case "dice":
      return <div className="step dice">🎲 <MarkdownInline>{m.text}</MarkdownInline></div>;

    case "scene_update": {
      if (m.title || m.scene_id) {
        const sceneSnapshot = sceneSnapshotFromMessage(m, scene);
        const sceneTitle = sceneSnapshot.title || sceneSnapshot.scene_id;
        const imageUrl = sceneSnapshot.image_url || "";
        const canOpen = typeof onOpenScene === "function";
        return (
          <div className="scene-update-card">
            {imageUrl ? (
              <ZoomableImage
                className="scene-update-art"
                src={imageUrl}
                alt={sceneTitle}
                title={sceneTitle}
                loading="lazy"
              />
            ) : (
              <div className="scene-update-art scene-update-art-placeholder" aria-hidden="true">
                <Icon name="image" size={24} />
              </div>
            )}
            <div className="scene-update-copy">
              <span className="scene-update-kicker">{t("message.sceneUpdate.prefix")}</span>
              {canOpen ? (
                <button
                  type="button"
                  className="scene-update-title"
                  onClick={() => onOpenScene(sceneSnapshot)}
                  aria-label={t("message.sceneUpdate.openLocation", { title: sceneTitle })}
                >
                  <span>{sceneTitle}</span>
                  <Icon name="chevron-right" size={16} />
                </button>
              ) : (
                <strong className="scene-update-title-static">{sceneTitle}</strong>
              )}
              {sceneSnapshot.description && (
                <div className="scene-update-description">{sceneSnapshot.description}</div>
              )}
              <div className="scene-update-present">
                <MarkdownText>{t("message.sceneUpdate.present", {
                  names: presentNames.join(", ") || t("scene.noNamedCharacters"),
                })}</MarkdownText>
              </div>
            </div>
          </div>
        );
      }
      return (
        <div className="step">
          <div className="pill ok">
            {t("message.sceneUpdate.prefix")} <NameTag name={m.name} roster={roster} /> {m.present ? t("message.sceneUpdate.inScene") : t("message.sceneUpdate.outsideScene")}
          </div>
          <div className="step-note">
            <MarkdownText>{t("message.sceneUpdate.present", {
              names: presentNames.join(", ") || t("scene.noNamedCharacters"),
            })}</MarkdownText>
          </div>
        </div>
      );
    }

    case "npc_whereabouts": {
      const w = m.whereabouts || {};
      const status = ["present", "known", "likely", "rumored", "unknown", "left_scene"].includes(w.status)
        ? t(`scene.statuses.${w.status}`)
        : statusLabels[w.status] || w.status || t("scene.unknown");
      const place = w.location_name || w.location_id || t("scene.placeUnknown");
      return (
        <div className="step">
          <div className="pill ok">{t("message.whereabouts.title")} <NameTag name={m.name} roster={roster} /> — {status}</div>
          <div className="step-note">
            <MarkdownText>
              {`**${t("message.whereabouts.whereToFind")}:** ${place}${w.details ? `\n\n${w.details}` : ""}`}
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
          <div className="pill redo">{t("message.reject.prefix")} <NameTag name={m.name} roster={roster} /> {t("message.reject.suffix")}</div>
          <div className="reason">{t("message.reject.note")} <MarkdownInline>{m.reason}</MarkdownInline></div>
        </div>
      );

    case "error": {
      const errorText = localizeServerMessage(m.text, t, {
        fallbackCode: "generic",
      });
      return (
        <div className={"err" + (onRetry ? " has-retry" : "")}>
          <div className="turn-error-text">
            ⚠ {agentLabel(m.agent, t)}: <MarkdownInline>{errorText}</MarkdownInline>
          </div>
          {onRetry && (
            <button
              type="button"
              className="turn-retry"
              onClick={onRetry}
              disabled={retryBusy}
            >
              <Icon name="refresh" size={14} />
              {retryBusy ? t("message.retrying") : t("actions.retry")}
            </button>
          )}
        </div>
      );
    }

    case "meta":
      if (!vis.messageTokens) return null;
      return (
        <Tooltip as="div" className="meta" content={metaTitle(m.data, t)}>
          {metaText(m.data, t)}
        </Tooltip>
      );

    case "meta_total": {
      if (!vis.messageTokens) return null;
      const d = m.data;
      const cached = d.cached ? ` · ${t("message.meta.cached", { count: fmtK(d.cached) })}` : "";
      return (
        <Tooltip as="div" className="meta-total" content={metaTotalTitle(d, t)}>
          <b>{t("message.meta.turnTotal")} </b>
          {`⏱ ${d.secs}s · `}
          <span className="tok">{t("message.meta.tokenCount", { count: fmtK(d.tokens) })}</span>
          {t("message.meta.totalDetails", {
            input: fmtK(d.in),
            output: fmtK(d.out),
            cached,
            calls: d.calls.length,
          })}
        </Tooltip>
      );
    }

    default:
      return null;
  }
}

export default memo(Message);
