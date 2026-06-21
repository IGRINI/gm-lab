// Per-message TTS playback via Web Audio.
//
// A message voices a SEQUENCE of segments (NPC card = speech in the character
// voice, then the action in the GM voice). On a cache miss the backend streams
// raw PCM16 head-first, which we schedule chunk-by-chunk for low latency; on a
// cache hit it returns a complete Opus/Ogg clip which we decode whole. Decoded
// audio is cached per message key for instant replay. Only one message plays at
// a time.
//
// Message key: `${sid}:narration` (GM) or `${sid}:npc`.
// Segment: { text, body } where body = { role:"gm" } or { voice:"male"|"female" }.
import { useSyncExternalStore } from "react";

const cache = new Map(); // key -> { status, buffers?: AudioBuffer[], priming?: bool, error? }
const listeners = new Set();
const IDLE = { status: "idle" };
let ac = null;
let cur = null; // active playback controller: { key, sources:[], abort, cancelled, onEnd }
let autoQueue = []; // pending {key, segments} for sequential auto-play

// iOS/WebKit (Safari 16.4+) routes Web Audio to the EARPIECE while the page's
// audio session is "play-and-record" — which voice input (getUserMedia) switches
// it to. Setting the type explicitly keeps TTS on the main loudspeaker. No-op on
// browsers without the AudioSession API (Android Chrome, desktop, older iOS).
export function setAudioSessionType(type) {
  try {
    const s = typeof navigator !== "undefined" ? navigator.audioSession : null;
    if (s && s.type !== type) s.type = type;
  } catch {
    /* unsupported — ignore */
  }
}

function audioCtx() {
  if (!ac) {
    ac = new (window.AudioContext || window.webkitAudioContext)();
    setAudioSessionType("playback"); // force speaker, not earpiece, on iOS
  }
  return ac;
}
function emit() {
  for (const l of listeners) l();
}
function setEntry(key, patch) {
  cache.set(key, { ...(cache.get(key) || { status: "idle" }), ...patch });
  emit();
}
function getEntry(key) {
  return cache.get(key) || IDLE;
}

// ---- segment builders (shared by auto-generation and the speaker button) ----
export function gmSegments(text) {
  return [{ text, body: { role: "gm" } }];
}
export function genderVoice(gender) {
  const g = String(gender || "").toLowerCase();
  return /жен|^f|female/.test(g) ? "female" : "male";
}
export function npcSegments({ name, speech, action, voice }) {
  const segs = [];
  if (speech && speech.trim()) segs.push({ text: speech, body: { voice: voice || "male" } });
  if (action && action.trim())
    segs.push({ text: `${name || ""} ${action}`.trim(), body: { role: "gm" } });
  return segs;
}
function usable(segments) {
  return (segments || []).filter((s) => s && s.text && s.text.trim());
}

// Strip chat markup (entity refs + markdown) to spoken prose before synthesis.
export function stripMarkup(input) {
  let t = String(input ?? "");
  t = t.replace(
    /\[\[[a-z][a-z0-9_-]*:([^\]|\n]+)(?:\|([^\]\n]+))?\]\]/gi,
    (_, id, label) => (label || id).trim()
  );
  t = t.replace(/!\[([^\]]*)\]\([^)]+\)/g, "$1");
  t = t.replace(/\[([^\]]+)\]\([^)]+\)/g, "$1");
  t = t.replace(/\*\*([\s\S]+?)\*\*/g, "$1");
  t = t.replace(/__([\s\S]+?)__/g, "$1");
  t = t.replace(/~~([\s\S]+?)~~/g, "$1");
  t = t.replace(/(?<!\*)\*(?!\*)([^*\n]+?)\*(?!\*)/g, "$1");
  t = t.replace(/(?<!_)_(?!_)([^_\n]+?)_(?!_)/g, "$1");
  t = t.replace(/`([^`]+)`/g, "$1");
  t = t.replace(/^[ \t]*#{1,6}[ \t]+/gm, "");
  t = t.replace(/^[ \t]*[-*+][ \t]+/gm, "");
  t = t.replace(/^[ \t]*\d+\.[ \t]+/gm, "");
  t = t.replace(/^[ \t]*>[ \t]?/gm, "");
  t = t.replace(/[ \t]+/g, " ").replace(/\n{3,}/g, "\n\n");
  return t.trim();
}

async function postTts(seg, { stream, signal }) {
  const r = await fetch("/tts", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text: stripMarkup(seg.text), ...seg.body, stream: !!stream }),
    signal,
  });
  if (!r.ok) {
    let m = `TTS ${r.status}`;
    try {
      m = (await r.json()).error || m;
    } catch {
      /* non-JSON error */
    }
    throw new Error(m);
  }
  return r;
}

function pcmChunkToBuffer(bytes, sr) {
  const n = Math.floor(bytes.length / 2);
  // copy to an aligned ArrayBuffer so Int16Array is valid regardless of offset
  const ab = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + n * 2);
  const i16 = new Int16Array(ab);
  const f32 = new Float32Array(n);
  for (let i = 0; i < n; i++) f32[i] = i16[i] / 32768;
  const buf = audioCtx().createBuffer(1, n, sr);
  buf.copyToChannel(f32, 0);
  return { buf, f32 };
}

// Fetch one segment; schedule its audio on the running playhead (live for PCM,
// whole for a decoded clip). Returns a single AudioBuffer for replay caching.
async function playSegment(seg, ctrl, schedule, { stream }) {
  const r = await postTts(seg, { stream, signal: ctrl.abort.signal });
  const ct = r.headers.get("Content-Type") || "";
  if (!ct.includes("audio/pcm")) {
    const buf = await audioCtx().decodeAudioData(await r.arrayBuffer());
    if (!ctrl.cancelled) schedule(buf);
    return buf;
  }
  const sr = parseInt(r.headers.get("X-Sample-Rate") || "24000", 10);
  const reader = r.body.getReader();
  let leftover = new Uint8Array(0);
  const parts = [];
  for (;;) {
    const { value, done } = await reader.read();
    if (done || ctrl.cancelled) {
      if (ctrl.cancelled) try { await reader.cancel(); } catch { /* noop */ }
      break;
    }
    let bytes = value;
    if (leftover.length) {
      const m = new Uint8Array(leftover.length + value.length);
      m.set(leftover);
      m.set(value, leftover.length);
      bytes = m;
    }
    const whole = Math.floor(bytes.length / 2) * 2;
    leftover = bytes.slice(whole);
    if (whole === 0) continue;
    const { buf, f32 } = pcmChunkToBuffer(bytes.subarray(0, whole), sr);
    parts.push(f32);
    schedule(buf);
  }
  const total = parts.reduce((a, f) => a + f.length, 0);
  if (!total) return null;
  const all = new Float32Array(total);
  let off = 0;
  for (const f of parts) {
    all.set(f, off);
    off += f.length;
  }
  const full = audioCtx().createBuffer(1, total, sr);
  full.copyToChannel(all, 0);
  return full;
}

// Pre-generate (no playback) and cache decoded buffers — auto-generation path.
export function ttsPrime(key, segments) {
  const segs = usable(segments);
  if (!segs.length) return;
  const e = cache.get(key);
  if (e?.buffers || e?.priming) return;
  setEntry(key, { priming: true });
  Promise.all(
    segs.map(async (s) => {
      const r = await postTts(s, { stream: false });
      return audioCtx().decodeAudioData(await r.arrayBuffer());
    })
  )
    .then((bufs) => {
      const playing = cache.get(key)?.status === "playing";
      setEntry(key, { buffers: bufs, priming: false, status: playing ? "playing" : "ready" });
    })
    .catch(() => setEntry(key, { priming: false }));
}

function stopPlayback() {
  if (!cur) return;
  const ctrl = cur;
  cur = null;
  ctrl.cancelled = true;
  try {
    ctrl.abort.abort();
  } catch {
    /* noop */
  }
  for (const s of ctrl.sources) {
    try {
      s.stop();
    } catch {
      /* already stopped */
    }
  }
  if (ac && ac.state === "suspended") {
    try {
      ac.resume(); // don't leave the context stuck after stopping a paused clip
    } catch {
      /* noop */
    }
  }
  const e = cache.get(ctrl.key);
  if (e && (e.status === "playing" || e.status === "paused")) setEntry(ctrl.key, { status: "ready" });
}

// Pause/resume the currently playing message (suspends the whole Web Audio clock).
export function ttsPause(key) {
  if (!cur || cur.key !== key || cache.get(key)?.status !== "playing") return;
  try {
    audioCtx().suspend();
  } catch {
    /* noop */
  }
  setEntry(key, { status: "paused" });
}
export function ttsResume(key) {
  if (!cur || cur.key !== key || cache.get(key)?.status !== "paused") return;
  try {
    audioCtx().resume();
  } catch {
    /* noop */
  }
  setEntry(key, { status: "playing" });
}
export function ttsStop(key) {
  autoQueue = []; // user stop halts the auto-chain
  if (cur && cur.key === key) stopPlayback();
}

// Core playback of one message's segment sequence. Calls onEnd(reason) when the
// clip finishes naturally or errors — NOT when the user stops it.
async function _play(key, segments, onEnd) {
  const segs = usable(segments);
  if (!segs.length) {
    if (onEnd) onEnd("empty");
    return;
  }
  stopPlayback();
  const ctxx = audioCtx();
  setAudioSessionType("playback"); // re-assert speaker route (mic use may have flipped it)
  try {
    await ctxx.resume();
  } catch {
    /* gesture should have unlocked it */
  }
  const ctrl = { key, sources: [], abort: new AbortController(), cancelled: false };
  cur = ctrl;
  setEntry(key, { status: "playing", error: undefined });
  let playhead = ctxx.currentTime + 0.06;
  const schedule = (buf) => {
    if (ctrl.cancelled || !buf) return;
    const src = ctxx.createBufferSource();
    src.buffer = buf;
    src.connect(ctxx.destination);
    const at = Math.max(playhead, ctxx.currentTime);
    try {
      src.start(at);
    } catch {
      return;
    }
    playhead = at + buf.duration;
    ctrl.sources.push(src);
  };

  try {
    const cached = cache.get(key)?.buffers;
    if (cached && cached.length) {
      cached.forEach(schedule);
    } else {
      const collected = [];
      for (const seg of segs) {
        if (ctrl.cancelled) break;
        const buf = await playSegment(seg, ctrl, schedule, { stream: true });
        if (buf) collected.push(buf);
      }
      if (!ctrl.cancelled && collected.length) setEntry(key, { buffers: collected });
    }
  } catch (e) {
    if (!ctrl.cancelled) setEntry(key, { status: "error", error: String(e.message || e) });
    if (cur === ctrl) cur = null;
    if (onEnd) onEnd("error");
    return;
  }

  if (ctrl.cancelled) return;
  const finish = (reason) => {
    if (cur === ctrl && !ctrl.cancelled) {
      setEntry(key, { status: "ready" });
      cur = null;
      if (onEnd) onEnd(reason);
    }
  };
  const last = ctrl.sources[ctrl.sources.length - 1];
  if (last) last.onended = () => finish("ended");
  else finish("ended");
}

// Speaker button: play this message (streaming on a miss), or stop if playing/paused.
export async function ttsToggle(key, segments) {
  const cs = cache.get(key)?.status;
  if (cs === "playing" || cs === "paused") {
    ttsStop(key);
    return;
  }
  autoQueue = []; // a manual play overrides the auto-chain
  await _play(key, segments, null);
}

// ---- sequential auto-play: messages play one after another until done/stop ----
function _autoNext() {
  const item = autoQueue.shift();
  if (item) _play(item.key, item.segments, () => _autoNext());
}
export function ttsAutoEnqueue(key, segments) {
  if (!usable(segments).length) return;
  autoQueue.push({ key, segments });
  if (!cur) _autoNext(); // nothing playing -> start the chain
}
export function ttsAutoReset() {
  autoQueue = [];
  stopPlayback();
}
// Unlock the AudioContext inside a user gesture so later auto-play makes sound.
export function ttsUnlock() {
  try {
    setAudioSessionType("playback"); // keep auto-play on the loudspeaker after mic use
    audioCtx().resume();
  } catch {
    /* noop */
  }
}

export function useTtsState(key) {
  return useSyncExternalStore(
    (cb) => {
      listeners.add(cb);
      return () => listeners.delete(cb);
    },
    () => getEntry(key)
  );
}
