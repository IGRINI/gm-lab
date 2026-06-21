// Developer-mode + granular UI visibility, owned entirely on the client.
//
// This is purely presentational (it never changes what the model sees), so it
// lives in localStorage with a tiny external store and applies in realtime —
// no backend round-trip. `developerMode` is OFF by default: a fresh player sees
// the clean view (no token counters, no tool-call internals, no GM/NPC
// reasoning, no world-memory ops, no history-debug drawer). Turning developer
// mode ON restores everything and unlocks per-aspect toggles.

import { createContext, useSyncExternalStore } from "react";

const STORAGE_KEY = "gmlab.devSettings";

// One entry per individually-togglable visibility aspect (shown in Settings →
// «Дебаг-вид» when developer mode is on). Each is "show this dev-only UI".
export const FLAG_META = [
  {
    key: "tokenCards",
    label: "Счётчики токенов у поля ввода",
    hint: "Таблички «ГМ контекст» и «За ран» рядом с полем ввода.",
  },
  {
    key: "messageTokens",
    label: "Токены под сообщениями",
    hint: "Строка «Σ за ход» и метрики вызовов модели в ленте.",
  },
  {
    key: "toolCalls",
    label: "Вызовы инструментов ГМ",
    hint: "Карточки вызова тулов и сырой JSON. Выкл — игроку видны только безопасные результаты (бросок, время, лист персонажа).",
  },
  {
    key: "gmThoughts",
    label: "Мысли ГМ и возвраты",
    hint: "Поток размышлений ГМ и пометки о возврате реплики персонажа на переделку.",
  },
  {
    key: "npcInternals",
    label: "Скрытое у персонажей",
    hint: "Скрытые мысли персонажа и «опора ответа». Речь и действие игроку видны всегда.",
  },
  {
    key: "memoryOps",
    label: "Память мира (поиск и запись)",
    hint: "Чтение/запись памяти мира, запросы карточек NPC, загрузка инструментов.",
  },
  {
    key: "historyDebug",
    label: "Вкладка «Дебаг истории»",
    hint: "Боковая панель отладки истории (правка сцены, NPC, фактов и т.п.).",
  },
];

const FLAG_KEYS = FLAG_META.map((f) => f.key);

function defaultState() {
  const flags = {};
  for (const key of FLAG_KEYS) flags[key] = true;
  return { developerMode: false, flags };
}

function load() {
  const base = defaultState();
  if (typeof window === "undefined") return base;
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY);
    if (!raw) return base;
    const saved = JSON.parse(raw);
    const flags = saved && typeof saved.flags === "object" ? saved.flags : {};
    return {
      developerMode: !!saved.developerMode,
      flags: { ...base.flags, ...flags },
    };
  } catch {
    return base;
  }
}

let state = load();
const listeners = new Set();

function persist() {
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(state));
  } catch {
    /* localStorage unavailable (private mode) — non-fatal */
  }
}

function setState(next) {
  state = next; // always a fresh object, so useSyncExternalStore sees the change
  persist();
  listeners.forEach((listener) => listener());
}

export function setDeveloperMode(on) {
  setState({ ...state, developerMode: !!on });
}

export function setFlag(key, on) {
  if (!FLAG_KEYS.includes(key)) return;
  setState({ ...state, flags: { ...state.flags, [key]: !!on } });
}

export function setAllFlags(on) {
  const flags = {};
  for (const key of FLAG_KEYS) flags[key] = !!on;
  setState({ ...state, flags });
}

function subscribe(listener) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot() {
  return state;
}

export function useDevSettings() {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

// Stable all-hidden object for the player view (also the context default).
const PLAYER_VISIBILITY = (() => {
  const v = {};
  for (const key of FLAG_KEYS) v[key] = false;
  return v;
})();

// Effective per-aspect visibility: when developer mode is off, everything is
// hidden (player view); when on, each aspect follows its own flag.
export function computeVisibility(dev) {
  if (!dev || !dev.developerMode) return PLAYER_VISIBILITY;
  const v = {};
  for (const key of FLAG_KEYS) v[key] = dev.flags[key] !== false;
  return v;
}

export const VisibilityContext = createContext(PLAYER_VISIBILITY);

// World-memory + GM-internal lookups: hidden from players entirely (gated by
// `memoryOps`), never reduced to a "safe result".
export const MEMORY_TOOLS = new Set([
  "get_world_fact",
  "query_world_state",
  "update_world_state",
  "get_npc_profile",
  "tool_search",
]);

// Tools whose RESULT is safe and meaningful for a plain player — shown compact
// when tool-call internals are hidden.
export const PLAYER_TOOLS = new Set([
  "roll_dice",
  "advance_time",
  "update_player_character",
]);

// How to render one tool message given the effective visibility:
//   'full'   — the developer card (request + raw JSON + result)
//   'result' — header + result only (memory tools in dev with calls hidden)
//   'player' — compact, player-friendly result (dice / time / sheet)
//   'hidden' — render nothing
export function toolMode(name, vis) {
  if (MEMORY_TOOLS.has(name)) {
    if (!vis.memoryOps) return "hidden";
    return vis.toolCalls ? "full" : "result";
  }
  if (vis.toolCalls) return "full";
  return PLAYER_TOOLS.has(name) ? "player" : "hidden";
}

// Whether a timeline message renders anything under the current visibility.
// Mirrors the null-returns in Message.jsx so hidden rows can be filtered out
// BEFORE the virtualized list — otherwise each hidden message leaves an empty
// padded `.row`, and several in a row add up to large vertical gaps.
export function isMessageVisible(m, vis) {
  switch (m.type) {
    case "gm_think":
    case "reject":
      return !!vis.gmThoughts;
    case "fact":
      return !!vis.memoryOps;
    case "meta":
    case "meta_total":
      return !!vis.messageTokens;
    case "tool": {
      const mode = toolMode(m.name, vis);
      if (mode === "hidden") return false;
      // Player-safe tools (dice/time/sheet) render nothing until their result lands.
      if (mode === "player" && m.result == null) return false;
      return true;
    }
    case "tool_result":
      return toolMode(m.name, vis) !== "hidden";
    default:
      return true;
  }
}
