import { useRef, useState, useEffect, useCallback, useContext } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import { fmtK } from "../util.js";
import { transcribeAudio } from "../api.js";
import { VisibilityContext } from "../devSettings.js";
import { setAudioSessionType } from "../ttsStore.js";

// Pick a MediaRecorder MIME the browser actually supports, preferring Opus.
function pickRecorderMime() {
  if (typeof MediaRecorder === "undefined") return "";
  const candidates = [
    "audio/webm;codecs=opus",
    "audio/webm",
    "audio/ogg;codecs=opus",
    "audio/mp4",
  ];
  for (const mime of candidates) {
    if (MediaRecorder.isTypeSupported?.(mime)) return mime;
  }
  return "";
}

const PLACEHOLDER =
  "Действие игрока…  или /new ледяной порт, пропал корабль, рядом Ива и Рун\nEnter — отправить · Shift+Enter — новая строка";
const PLACEHOLDER_COMPACT = "Действие игрока…  или /new <сцена>";

function useCompact() {
  const [compact, setCompact] = useState(
    () => typeof window !== "undefined" && window.matchMedia("(max-width:700px)").matches
  );
  useEffect(() => {
    if (typeof window === "undefined") return;
    const mq = window.matchMedia("(max-width:700px)");
    const on = (e) => setCompact(e.matches);
    mq.addEventListener?.("change", on);
    return () => mq.removeEventListener?.("change", on);
  }, []);
  return compact;
}

// True on touch devices (phones/tablets) whose primary pointer is coarse and
// where the on-screen keyboard's Enter is a plain newline — there is no
// Shift+Enter, so Enter must NOT submit. Desktops (fine pointer) keep
// Enter-to-send / Shift+Enter-for-newline.
function useSoftKeyboard() {
  const [soft, setSoft] = useState(
    () => typeof window !== "undefined" && window.matchMedia("(pointer: coarse)").matches
  );
  useEffect(() => {
    if (typeof window === "undefined") return;
    const mq = window.matchMedia("(pointer: coarse)");
    const on = (e) => setSoft(e.matches);
    mq.addEventListener?.("change", on);
    return () => mq.removeEventListener?.("change", on);
  }, []);
  return soft;
}

function usageTitle(run) {
  return [
    "Весь ран",
    `ходов: ${run.turns || 0}`,
    `вызовов модели: ${run.calls || 0}`,
    `ввод: ${run.in || 0} ток`,
    `вывод: ${run.out || 0} ток`,
    `кэш: ${run.cached || 0} ток`,
    `ГМ: ${run.gm_tokens || 0} ток / ${run.gm_calls || 0} выз.`,
    `персонажи: ${run.npc_tokens || 0} ток / ${run.npc_calls || 0} выз.`,
    `пик контекста: ${run.peak_context || 0} ток`,
    `время: ${run.secs || 0}s`,
  ].join("\n");
}

function RunUsage({ run }) {
  const data = run || {};
  return (
    <Tooltip as="aside" className="run-usage" content={usageTitle(data)}>
      <div className="run-usage-label">За ран</div>
      <div className="run-usage-main">
        <b>{fmtK(data.tokens || 0)}</b>
        <span>токенов</span>
      </div>
      <div className="run-usage-grid">
        <span>Кэш</span>
        <b>{fmtK(data.cached || 0)}</b>
        <span>Персонажи</span>
        <b>{fmtK(data.npc_tokens || 0)}</b>
      </div>
    </Tooltip>
  );
}

function percent(used, limit) {
  const u = Number(used || 0);
  const l = Number(limit || 0);
  if (!l || l <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round((u / l) * 100)));
}

function Meter({ value }) {
  return (
    <div className="context-detail-meter" aria-hidden="true">
      <span style={{ width: `${Math.max(0, Math.min(100, value || 0))}%` }} />
    </div>
  );
}

function DetailRow({ label, value, pct }) {
  return (
    <div className="context-detail-row">
      <span>{label}</span>
      <b>{value}</b>
      {pct != null ? <em>{pct}%</em> : null}
    </div>
  );
}

function ContextDetails({ context, modelWindow }) {
  const data = context || {};
  const gm = data.gm || {};
  const npcs = (data.npcs && data.npcs.length ? data.npcs : data.npc?.name ? [data.npc] : []);
  const gmContextPct = percent(gm.active, gm.limit);
  const gmCompactPct = percent(gm.history, gm.limit);
  const gmLimit = gm.limit || 0;

  return (
    <div className="context-tip-panel">
      <div className="context-tip-head">
        <b>Контекст модели</b>
        <span>
          компакт {gmLimit ? fmtK(gmLimit) : "?"}
          {modelWindow ? ` · окно ${fmtK(modelWindow)}` : ""}
        </span>
      </div>

      <div className="context-detail-card">
        <div className="context-detail-title">
          <b style={{ color: "var(--gm)" }}>ГМ</b>
          <span>{fmtK(gm.active || 0)} / {gmLimit ? fmtK(gmLimit) : "?"}</span>
        </div>
        <Meter value={gmContextPct} />
        <DetailRow label="активно" value={`${fmtK(gm.active || 0)} / ${gmLimit ? fmtK(gmLimit) : "?"}`} pct={gmContextPct} />
        <DetailRow label="история" value={`${fmtK(gm.history || 0)} / ${fmtK(gm.limit || 0)}`} pct={gmCompactPct} />
        <DetailRow label="до компакта" value={`${fmtK(gm.remaining || 0)} ток`} />
        <DetailRow label="сводка" value={`${fmtK(gm.summary || 0)} ток`} />
      </div>

      <div className="context-detail-section">
        <span>Сессии персонажей</span>
        <b>{npcs.filter((npc) => npc.has_session).length}/{npcs.length}</b>
      </div>

      {npcs.length ? (
        npcs.map((npc) => {
          const contextPct = percent(npc.active, npc.limit);
          const compactPct = percent(npc.history, npc.limit);
          const npcLimit = npc.limit || 0;
          return (
            <div className={"context-detail-card npc" + (npc.has_session ? "" : " inactive")} key={npc.id || npc.name}>
              <div className="context-detail-title">
                <b style={{ color: npc.color || "var(--entity-unknown)" }}>{npc.name || npc.id || "персонаж"}</b>
                <span>{npc.has_session ? "есть история" : "ещё не вызывался"}</span>
              </div>
              <Meter value={contextPct} />
              <DetailRow label="активно" value={`${fmtK(npc.active || 0)} / ${npcLimit ? fmtK(npcLimit) : "?"}`} pct={contextPct} />
              <Meter value={compactPct} />
              <DetailRow label="история" value={`${fmtK(npc.history || 0)} / ${fmtK(npc.limit || 0)}`} pct={compactPct} />
              <DetailRow label="до компакта" value={`${fmtK(npc.remaining || 0)} ток`} />
              {npc.summary ? <DetailRow label="сводка" value={`${fmtK(npc.summary)} ток`} /> : null}
            </div>
          );
        })
      ) : (
        <div className="context-detail-empty">Сессий персонажей пока нет.</div>
      )}

      <div className="context-detail-foot">мир/сцена: {fmtK(data.world || 0)} ток</div>
    </div>
  );
}

function ContextUsage({ context, modelWindow }) {
  const data = context || {};
  const gm = data.gm || {};
  const gmActive = gm.active || data.current || 0;
  const contextPct = percent(gmActive, gm.limit);
  const compactPct = percent(gm.history, gm.limit);
  return (
    <Tooltip
      as="aside"
      className="context-usage"
      tipClassName="context-tip"
      content={<ContextDetails context={data} modelWindow={modelWindow} />}
    >
      <div className="context-usage-label">ГМ контекст</div>
      <div className="context-usage-main">
        <b>{fmtK(gmActive)}</b>
        <span>токенов</span>
        {gm.limit ? <em>{contextPct}%</em> : null}
      </div>
      <div className="context-meter" aria-hidden="true">
        <span style={{ width: `${contextPct}%` }} />
      </div>
      <div className="context-usage-grid">
        <span>История</span>
        <b>{fmtK(gm.history || 0)} / {fmtK(gm.limit || 0)} · {compactPct}%</b>
        <span>До компакта</span>
        <b>{fmtK(gm.remaining || 0)}</b>
      </div>
    </Tooltip>
  );
}

function QuickReplies({ playerOptions, busy, onPick }) {
  const options = Array.isArray(playerOptions?.options) ? playerOptions.options : [];
  const question = playerOptions?.question || "Что ты делаешь дальше?";
  const [collapsed, setCollapsed] = useState(false);
  // Re-expand automatically whenever a fresh batch of suggestions arrives.
  const sig = options.map((o) => o.label).join("|") + "::" + question;
  useEffect(() => {
    setCollapsed(false);
  }, [sig]);
  if (!options.length) return null;
  return (
    <section
      className={"quick-replies" + (collapsed ? " collapsed" : "")}
      aria-label="Варианты действий"
    >
      <div className="quick-replies-head">
        <span className="quick-replies-q">{question}</span>
        <Tooltip
          className="tooltip-wrap"
          tipClassName="ui-tip-wrap"
          focusable={false}
          content={
            <TipContent
              title={collapsed ? "Показать варианты" : "Скрыть варианты"}
              note={collapsed ? "Вернёт быстрые действия под поле ввода." : "Спрячет подсказки, поле ввода останется доступным."}
            />
          }
        >
          <button
            type="button"
            className="quick-replies-toggle"
            onClick={() => setCollapsed((c) => !c)}
            aria-expanded={!collapsed}
            aria-label={collapsed ? "Развернуть варианты" : "Свернуть варианты"}
          >
            {collapsed ? "▴" : "▾"}
          </button>
        </Tooltip>
      </div>
      {collapsed ? null : (
        <div className="quick-replies-list">
          {options.map((option, index) => (
            <Tooltip
              key={`${option.label}:${index}`}
              className="tooltip-block"
              tipClassName="ui-tip-wrap"
              focusable={false}
              content={
                <TipContent
                  title={option.label}
                  subtitle="Быстрый ответ игрока"
                  rows={[["отправит", option.message]]}
                />
              }
            >
              <button
                type="button"
                className="quick-reply"
                disabled={busy}
                onClick={() => onPick(option.message)}
              >
                <span>{option.label}</span>
                <small>{option.message}</small>
              </button>
            </Tooltip>
          ))}
        </div>
      )}
    </section>
  );
}

export default function Composer({
  onSend,
  busy,
  status,
  playerOptions,
  runUsage,
  contextUsage,
  modelWindow,
}) {
  const [value, setValue] = useState("");
  const ref = useRef(null);
  const compact = useCompact();
  const softKeyboard = useSoftKeyboard();
  const vis = useContext(VisibilityContext);

  // Auto-grow: reset to content height; CSS max-height caps it and switches to
  // an inner scroll once the limit is reached.
  const resize = useCallback(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = el.scrollHeight + "px";
  }, []);

  useEffect(() => {
    resize();
  }, [value, resize]);

  // --- voice input (Codex OAuth speech-to-text) ---------------------------
  const [recording, setRecording] = useState(false);
  const [transcribing, setTranscribing] = useState(false);
  const [voiceError, setVoiceError] = useState("");
  const recRef = useRef(null); // MediaRecorder
  const streamRef = useRef(null); // MediaStream
  const chunksRef = useRef([]);
  const blobRef = useRef(null); // last recording, kept so a failed transcription can be retried
  const attemptRef = useRef(0); // bumped to invalidate stale / cancelled transcriptions
  const micApi =
    typeof navigator !== "undefined" &&
    !!navigator.mediaDevices?.getUserMedia &&
    typeof MediaRecorder !== "undefined";
  // On an insecure origin (LAN http) browsers hide the mic API entirely. Still
  // show the button so a tap can explain WHY (needs https), instead of the
  // button silently vanishing on phones/tablets.
  const insecureContext =
    typeof window !== "undefined" && window.isSecureContext === false;
  const micSupported = micApi || insecureContext;

  const stopStream = useCallback(() => {
    streamRef.current?.getTracks().forEach((track) => track.stop());
    streamRef.current = null;
    // Hand the iOS audio session back to playback so TTS returns to the
    // loudspeaker instead of staying on the earpiece after recording.
    setAudioSessionType("playback");
  }, []);

  useEffect(
    () => () => {
      try {
        if (recRef.current && recRef.current.state !== "inactive") recRef.current.stop();
      } catch {
        /* ignore */
      }
      stopStream();
    },
    [stopStream]
  );

  const insertTranscript = useCallback(
    (text) => {
      const clean = String(text || "").trim();
      if (!clean) return;
      setValue((prev) => (prev.trim() ? prev.replace(/\s*$/, "") + " " + clean : clean));
      requestAnimationFrame(() => {
        resize();
        ref.current?.focus();
      });
    },
    [resize]
  );

  const sendVoice = useCallback(
    async (blob) => {
      if (!blob || !blob.size) return;
      blobRef.current = blob;
      const token = ++attemptRef.current;
      setVoiceError("");
      setTranscribing(true);
      try {
        const text = await transcribeAudio(blob);
        if (token !== attemptRef.current) return; // cancelled or superseded
        setTranscribing(false);
        if (text.trim()) {
          blobRef.current = null;
          insertTranscript(text);
        } else {
          setVoiceError("Пустой ответ распознавания");
        }
      } catch (err) {
        if (token !== attemptRef.current) return;
        setTranscribing(false);
        setVoiceError(err?.message || "Ошибка распознавания");
      }
    },
    [insertTranscript]
  );

  const startRecording = useCallback(async () => {
    if (recording || transcribing) return;
    setVoiceError("");
    if (!micApi) {
      setVoiceError(
        insecureContext
          ? "Голосовой ввод работает только по HTTPS. Открой приложение по https://<IP> (запусти сервер с GM_HTTPS=1)."
          : "Этот браузер не поддерживает запись с микрофона."
      );
      return;
    }
    try {
      // Tell iOS this is a record session up front; stopStream() restores playback.
      setAudioSessionType("play-and-record");
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      streamRef.current = stream;
      const mime = pickRecorderMime();
      const rec = new MediaRecorder(stream, mime ? { mimeType: mime } : undefined);
      chunksRef.current = [];
      rec.ondataavailable = (e) => {
        if (e.data && e.data.size) chunksRef.current.push(e.data);
      };
      rec.onstop = () => {
        const type = rec.mimeType || mime || "audio/webm";
        const blob = new Blob(chunksRef.current, { type });
        chunksRef.current = [];
        stopStream();
        setRecording(false);
        sendVoice(blob);
      };
      recRef.current = rec;
      rec.start();
      setRecording(true);
    } catch {
      stopStream();
      setRecording(false);
      setVoiceError("Нет доступа к микрофону");
    }
  }, [recording, transcribing, sendVoice, stopStream, micApi, insecureContext]);

  const stopRecording = useCallback(() => {
    const rec = recRef.current;
    if (rec && rec.state !== "inactive") {
      try {
        rec.stop();
      } catch {
        stopStream();
        setRecording(false);
      }
    }
  }, [stopStream]);

  const toggleRecord = useCallback(() => {
    if (recording) stopRecording();
    else startRecording();
  }, [recording, startRecording, stopRecording]);

  const retryVoice = useCallback(() => {
    if (blobRef.current) sendVoice(blobRef.current);
  }, [sendVoice]);

  const cancelVoice = useCallback(() => {
    attemptRef.current++; // drop any in-flight transcription result
    try {
      if (recRef.current && recRef.current.state !== "inactive") recRef.current.stop();
    } catch {
      /* ignore */
    }
    chunksRef.current = [];
    blobRef.current = null;
    stopStream();
    setRecording(false);
    setTranscribing(false);
    setVoiceError("");
  }, [stopStream]);

  const submit = () => {
    const t = value.trim();
    if (!t || busy) return;
    onSend(t);
    setValue("");
    requestAnimationFrame(() => {
      resize();
      ref.current?.focus();
    });
  };

  const sendQuickReply = (message) => {
    const text = String(message || "").trim();
    if (!text || busy) return;
    onSend(text);
    setValue("");
    requestAnimationFrame(() => {
      resize();
      ref.current?.focus();
    });
  };

  const onKeyDown = (e) => {
    // On phones/tablets Enter is a plain newline (no Shift+Enter) — never submit;
    // sending is done via the send button. Desktop keeps Enter-to-send.
    if (
      e.key === "Enter" &&
      !e.shiftKey &&
      !e.nativeEvent.isComposing &&
      !softKeyboard
    ) {
      e.preventDefault();
      submit();
    }
  };

  const voiceState = recording
    ? "recording"
    : transcribing
    ? "transcribing"
    : voiceError
    ? "error"
    : "idle";
  const showVoice = voiceState !== "idle";
  const voiceLabel = recording
    ? "Идёт запись… нажми ■, чтобы остановить"
    : transcribing
    ? "Распознаю…"
    : voiceError;

  return (
    <footer>
      <div className={"footer-main" + (vis.tokenCards ? "" : " no-usage")}>
        {vis.tokenCards ? <ContextUsage context={contextUsage} modelWindow={modelWindow} /> : null}
        <div className="composer-zone">
          <QuickReplies playerOptions={playerOptions} busy={busy} onPick={sendQuickReply} />
          {showVoice ? (
            <div className={"voice-pending voice-" + voiceState} role="status">
              <span className="voice-ico" aria-hidden="true">
                {recording ? "●" : transcribing ? "⏳" : "⚠"}
              </span>
              <span className="voice-text">{voiceLabel}</span>
              {voiceState === "error" && blobRef.current ? (
                <button type="button" className="voice-retry" onClick={retryVoice}>
                  ↻ Повторить
                </button>
              ) : null}
              <Tooltip
                className="tooltip-wrap"
                tipClassName="ui-tip-wrap"
                focusable={false}
                content={
                  <TipContent
                    title={recording ? "Отменить запись" : "Отменить голосовое"}
                    note={recording ? "Остановит запись и не отправит аудио на распознавание." : "Уберёт текущую голосовую операцию."}
                  />
                }
              >
                <button
                  type="button"
                  className="voice-cancel"
                  onClick={cancelVoice}
                  aria-label={recording ? "Отменить запись" : "Отменить голосовое"}
                >
                  ✕
                </button>
              </Tooltip>
            </div>
          ) : null}
          <div className="composer">
            <div className="inp-wrap">
              <textarea
                id="inp"
                ref={ref}
                rows={1}
                value={value}
                placeholder={compact || softKeyboard ? PLACEHOLDER_COMPACT : PLACEHOLDER}
                autoComplete="off"
                onChange={(e) => setValue(e.target.value)}
                onKeyDown={onKeyDown}
              />
              <div className="composer-actions">
                {micSupported ? (
                  <Tooltip
                    className="tooltip-wrap"
                    tipClassName="ui-tip-wrap"
                    focusable={false}
                    content={
                      <TipContent
                        title={recording ? "Остановить запись" : "Голосовой ввод"}
                        note={recording ? "Закончит запись и отправит аудио на распознавание." : "Запишет речь с микрофона и вставит распознанный текст в поле ввода."}
                      />
                    }
                  >
                    <button
                      type="button"
                      className="mic-btn"
                      data-recording={recording ? "true" : "false"}
                      onClick={toggleRecord}
                      disabled={transcribing}
                      aria-label={recording ? "Остановить запись" : "Голосовой ввод"}
                    >
                      {recording ? "■" : "🎤"}
                    </button>
                  </Tooltip>
                ) : null}
                <Tooltip
                  className="tooltip-wrap"
                  tipClassName="ui-tip-wrap"
                  focusable={false}
                  content={
                    <TipContent
                      title="Отправить"
                      subtitle="Передать действие игрока ГМ."
                      note={softKeyboard ? "На телефоне отправка только этой кнопкой." : "На клавиатуре также работает Enter; Shift+Enter добавляет новую строку."}
                    />
                  }
                >
                  <button
                    id="send"
                    className="send-btn"
                    onClick={submit}
                    disabled={busy || !value.trim()}
                    aria-label="Отправить"
                  >
                    <span className="send-ico" aria-hidden="true">➤</span>
                  </button>
                </Tooltip>
              </div>
            </div>
          </div>
        </div>
        {vis.tokenCards ? <RunUsage run={runUsage} /> : null}
      </div>
      <div id="status">{status ? <><span className="pulse" />{status}</> : null}</div>
    </footer>
  );
}
