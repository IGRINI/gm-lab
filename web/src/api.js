// Thin wrappers around the GM-Lab Rust backend (gml-server).

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

  sidecarStatus: () => getJSON("/sidecar/status"),

  transcript: () => getJSON("/transcript"),

  stories: () => getJSON("/stories"),

  // GET /stories/{id}/draft — the GM-scoped plot draft `{story}` plus the
  // architect conversation `{architect: {messages}}` (the chat lives in the
  // package's architect.json now). The PLAYER-facing /stories catalog omits
  // both (hidden_truth is GM-only). 404 unknown / 400 builtin|procedural.
  storyDraft: (storyId) => getJSON(`/stories/${encodeURIComponent(storyId)}/draft`),

  // GET /worlds/{id}/architect — the world-architect conversation
  // `{architect: {messages}}` for the panel's reopen restore.
  worldArchitect: (worldId) => getJSON(`/worlds/${encodeURIComponent(worldId)}/architect`),

  createStory: (body) => _post("/stories", body),

  // POST /stories/{id} — shallow-merge a patch (title/description/seed/meta) into
  // an existing world-bound authored story. 400 for builtin/procedural/unknown.
  updateStory: (storyId, body) => _post(`/stories/${encodeURIComponent(storyId)}`, body),

  deleteStory: (storyId) => _post(`/stories/${encodeURIComponent(storyId)}/delete`),

  // --- K1: character packages (§К1.5) ---
  // GET /characters -> {ok, characters:[{id,version,title,preview,created_at,
  // updated_at,payload, world_ref?, story_ref?}]} — the refs are the OPTIONAL
  // base packages the hero was authored for ({id, version}, may dangle).
  characters: () => getJSON("/characters"),
  // POST /characters {title, payload, world_id?, story_id?} -> {ok, character:{...}}
  // The optional ids pin base world_ref/story_ref provenance (400 on a dangling
  // id and on a procedural story — it has no authored plot to base a hero on).
  createCharacter: (body) => _post("/characters", body),
  // POST /characters/{id} (metadata patch, e.g. {title}) -> {ok, character:{...}}
  updateCharacter: (id, body) => _post(`/characters/${encodeURIComponent(id)}`, body),
  // POST /characters/{id}/draft {player_character:{...}} -> {ok, character:{...}}
  // Direct manual save of the edited sheet: snapshots it (full replace + version
  // bump), follows the title to the hero name, no architect chat. 400 non-object
  // player_character, 404 unknown id.
  saveCharacterDraft: (id, playerCharacter) =>
    _post(`/characters/${encodeURIComponent(id)}/draft`, {
      player_character: playerCharacter,
    }),
  // POST /characters/{id}/delete -> {ok, deleted:bool}
  deleteCharacter: (id) => _post(`/characters/${encodeURIComponent(id)}/delete`),
  // GET /characters/{id}/export -> {id}.gmchar.zip attachment (download URL)
  exportCharacterUrl: (id) => `/characters/${encodeURIComponent(id)}/export`,
  // POST /chats/{chatId}/save-character {character_id?} -> {ok, character:{id,version,title}}
  saveCharacterFromChat: (chatId, body) =>
    _post(`/chats/${encodeURIComponent(chatId)}/save-character`, body),
  // GET /characters/{id}/architect -> {architect:{messages}} for the panel reopen.
  characterArchitect: (id) =>
    getJSON(`/characters/${encodeURIComponent(id)}/architect`),
  // POST /stories/{id}/save-protagonist -> {ok, character} — a .gmchar from the
  // story draft's seed.player_character. 404/400 like the other story routes.
  saveProtagonist: (storyId) =>
    _post(`/stories/${encodeURIComponent(storyId)}/save-protagonist`),

  chats: () => getJSON("/chats"),

  worlds: () => getJSON("/worlds"),

  createChat: (body) => _post("/chats", body),

  createWorld: (body) => _post("/worlds", body),

  updateWorld: (worldId, body) => _post(`/worlds/${encodeURIComponent(worldId)}`, body),

  activateChat: (chatId) => _post(`/chats/${encodeURIComponent(chatId)}/activate`),

  deleteChat: (chatId) => _post(`/chats/${encodeURIComponent(chatId)}/delete`),

  deleteWorld: (worldId) => _post(`/worlds/${encodeURIComponent(worldId)}/delete`),

  generateImage: (body) => _post("/images/generate", body),

  // --- Phase 5: share UX (export / import / reveal library folder) ---
  // Open the library root in the OS file manager. Returns {ok, path} or {ok:false,error}.
  revealLibrary: () => getJSON("/library/reveal", { method: "POST" }),

  // Download URLs for the export endpoints (used to trigger a browser download
  // via an <a download> click; the response is a zip attachment).
  exportWorldUrl: (worldId) => `/worlds/${encodeURIComponent(worldId)}/export`,
  exportStoryUrl: (storyId, bake) =>
    `/stories/${encodeURIComponent(storyId)}/export${bake ? "?bake=1" : ""}`,

  // POST raw zip bytes from a picked .zip file. Returns {ok,kind,id} or
  // {ok:false,error} (collision 409, malformed/unknown 400). Throws on network error.
  // On an id collision the thrown Error carries `.status = 409` and
  // `.collision = true` so the caller can offer an overwrite confirm.
  async importPackage(file, overwrite) {
    const url = `/library/import${overwrite ? "?overwrite=1" : ""}`;
    const r = await fetch(url, {
      method: "POST",
      headers: { "Content-Type": "application/zip" },
      body: file,
    });
    let data = {};
    try {
      data = await r.json();
    } catch {
      /* fall through to the generic error below */
    }
    if (!r.ok || !data.ok) {
      const err = new Error(data.error || `импорт не выполнен (${r.status})`);
      err.status = r.status;
      // 409 is the backend's distinct id-collision-without-overwrite signal.
      err.collision = r.status === 409;
      throw err;
    }
    return data;
  },

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
  // Debug-panel story mutation (NOT the package patch above — that one is
  // `updateStory`; this key used to shadow it as a duplicate `updateStory`).
  debugUpdateStory: (body) => _post("/debug/story", body),
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

  // Fetch a package-export URL and trigger a robust blob download. On a non-OK
  // response the body is parsed as JSON and surfaced as a thrown Error (so a
  // failed export reports the backend message instead of navigating the SPA away).
  // The download filename comes from Content-Disposition when present, else the
  // supplied fallback. Throws on network/error so the caller can show it inline.
  async downloadExport(url, fallbackName) {
    const r = await fetch(url);
    if (!r.ok) {
      let data = {};
      try {
        data = await r.json();
      } catch {
        /* fall through to the generic error below */
      }
      throw new Error(data.error || `экспорт не выполнен (${r.status})`);
    }
    const blob = await r.blob();
    const name = filenameFromContentDisposition(r.headers.get("Content-Disposition")) || fallbackName;
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = name;
    a.rel = "noopener";
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(a.href);
  },
};

// Parse a download filename out of a Content-Disposition header. Handles both
// the RFC 5987 `filename*=UTF-8''…` form and a plain `filename="…"`. Returns ""
// when no usable filename is present.
function filenameFromContentDisposition(header) {
  if (!header) return "";
  const star = /filename\*=(?:UTF-8'')?([^;]+)/i.exec(header);
  if (star) {
    try {
      return decodeURIComponent(star[1].trim().replace(/^"|"$/g, ""));
    } catch {
      /* malformed encoding — fall through to the plain form */
    }
  }
  const plain = /filename="?([^";]+)"?/i.exec(header);
  return plain ? plain[1].trim() : "";
}

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

// Stream an architect agent turn (SSE) from `endpoint`. `onEvent` fires for every
// event:
//   architect_delta {channel:"thinking"|"content", text, sid} — per-hop deltas
//   architect_tool  {name, arguments, sid}                    — a tool call
//   architect_done  {…}                                       — final payload
//   architect_error {…}
// Returns when the stream ends. Throws on network error.
//
// The architect routes validate EAGERLY and answer a plain JSON error (e.g. a
// 400 for a dangling world_id/story_id on a character create) BEFORE any SSE
// stream starts. A JSON body carries no `data:` frames, so without this check
// the read loop would drain it silently and the turn would no-op with no error
// shown — surface it as a throw instead (the panels' catch renders it).
async function streamArchitectAt(endpoint, body, onEvent) {
  const resp = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body || {}),
  });
  if (!resp.ok) {
    let message = "";
    try {
      const data = await resp.json();
      message = typeof data?.error === "string" ? data.error : "";
    } catch {
      // non-JSON error body — fall through to the status line
    }
    throw new Error(message || `архитектор недоступен (HTTP ${resp.status})`);
  }
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

// Stream a WORLD-architect turn. Endpoint defaults to the world path so existing
// callers are unchanged; the story panel uses `streamStoryArchitect` below.
export function streamArchitect(body, onEvent) {
  return streamArchitectAt("/world-architect/chat", body, onEvent);
}

// Stream a STORY-architect turn (§С1.3). Same event vocabulary as the world one;
// the done payload additionally carries {story_id, story, stories}.
export function streamStoryArchitect(body, onEvent) {
  return streamArchitectAt("/story-architect/chat", body, onEvent);
}

// Stream a CHARACTER-architect turn. Same event vocabulary as the other two; the
// done payload additionally carries {character_id, character, characters}. Body:
// {message, character_id?, draft?, world_id?, story_id?} — create-on-first-turn
// when character_id is absent; the optional base ids ride ONLY with that create
// (they pin world_ref/story_ref and give the architect the base's public canon).
export function streamCharacterArchitect(body, onEvent) {
  return streamArchitectAt("/character-architect/chat", body, onEvent);
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
