// Thin wrappers around the GM-Lab Python backend.
// Same endpoints/semantics as the original index.html.

async function getJSON(url, opts) {
  const r = await fetch(url, opts);
  return r.json();
}

function _post(url, body) {
  return getJSON(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body || {}),
  });
}

export const api = {
  state: () => getJSON("/state"),

  debug: () => getJSON("/debug"),

  models: () => getJSON("/models"),

  settings: () => getJSON("/settings"),

  transcript: () => getJSON("/transcript"),

  stories: () => getJSON("/stories"),

  chats: () => getJSON("/chats"),

  createChat: (body) => _post("/chats", body),

  activateChat: (chatId) => _post(`/chats/${encodeURIComponent(chatId)}/activate`),

  deleteChat: (chatId) => _post(`/chats/${encodeURIComponent(chatId)}/delete`),

  setModel: (model) =>
    getJSON("/model", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model }),
    }),

  updateSettings: (settings) =>
    getJSON("/settings", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ settings }),
    }),

  command: (cmd, arg) =>
    getJSON("/cmd", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ cmd, arg }),
    }),

  codexLogin: () => getJSON("/codex/login", { method: "POST" }),
  codexLogout: () => getJSON("/codex/logout", { method: "POST" }),

  // --- dev token counter (local tiktoken) + optional OpenAI key storage ---
  tokenize: (text, model) => _post("/debug/tokenize", { text, model }),
  openaiKeyStatus: () => getJSON("/debug/openai_key"),
  saveOpenaiKey: (key) => _post("/debug/openai_key", { key }),
  deleteOpenaiKey: () => _post("/debug/openai_key/delete"),

  // --- debug-panel mutations: each returns the fresh /debug payload ---
  debugRoll: (body) => _post("/debug/roll", body),
  addFact: (text, kind) => _post("/debug/fact", { text, kind }),
  deleteFact: (id) => _post("/debug/fact_delete", { id }),
  updatePlayer: (body) => _post("/debug/player", body),
  updateNpc: (body) => _post("/debug/npc", body),
  updateStory: (body) => _post("/debug/story", body),
  updateScene: (patch) => _post("/debug/scene", { patch }),
  stateRecord: (body) => _post("/debug/state_record", body),
  rumor: (body) => _post("/debug/rumor", body),

  async export() {
    const r = await fetch("/export");
    const blob = await r.blob();
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = "gm-lab-export.json";
    a.click();
    URL.revokeObjectURL(a.href);
  },
};

// Send a recorded audio blob to the backend for speech-to-text (Codex OAuth).
// Resolves to the transcribed text; throws on failure so the caller can retry.
export async function transcribeAudio(blob) {
  const resp = await fetch("/transcribe", {
    method: "POST",
    headers: { "Content-Type": blob.type || "audio/webm" },
    body: blob,
  });
  let data = {};
  try {
    data = await resp.json();
  } catch {
    /* fall through to the generic error below */
  }
  if (!resp.ok || !data.ok) {
    throw new Error(data.error || `Ошибка распознавания (${resp.status})`);
  }
  return String(data.text || "");
}

// Stream a player turn. `onEvent` is called for every SSE event object.
// Returns when the stream ends. Throws on network error.
export async function streamTurn(text, onEvent) {
  const resp = await fetch("/turn", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ text }),
  });
  const reader = resp.body.getReader();
  const dec = new TextDecoder();
  let buf = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    let i;
    while ((i = buf.indexOf("\n\n")) >= 0) {
      const chunk = buf.slice(0, i);
      buf = buf.slice(i + 2);
      if (chunk.startsWith("data: ")) {
        const ev = JSON.parse(chunk.slice(6));
        if (ev.kind === "done") continue;
        onEvent(ev);
      }
    }
  }
}
