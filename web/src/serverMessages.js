const MESSAGE_NAMESPACE = "server:messages";

const CODE_ALIASES = Object.freeze({
  architect_world_failed: "architect_world_save_failed",
  architect_story_failed: "architect_story_save_failed",
  architect_character_failed: "architect_character_save_failed",
  world_architect_failed: "architect_world_save_failed",
  story_architect_failed: "architect_story_save_failed",
  character_architect_failed: "architect_character_save_failed",
});

const KNOWN_CODES = new Set([
  "generic",
  "model_call_failed",
  "architect_load_failed",
  "architect_turn_failed",
  "architect_draft_save_failed",
  "architect_history_load_failed",
  "architect_history_save_failed",
  "architect_world_save_failed",
  "architect_story_save_failed",
  "architect_character_save_failed",
  "architect_not_running",
  "world_lore_required",
  "world_version_drift",
  "protagonist_required",
  "story_pc_override",
  "character_world_mismatch",
  "character_story_mismatch",
  "turn_not_running",
  "empty_text",
  "api_key_required",
  "tts_unavailable",
]);

const LEGACY_MESSAGES = [
  ["model_call_failed", /(?:ошибка вызова модели|model (?:call|request) failed|failed to call (?:the )?model)/i],
  ["architect_world_save_failed", /(?:не удалось сохранить черновик мира|failed to save (?:the )?world draft)/i],
  ["architect_story_save_failed", /(?:не удалось сохранить черновик (?:истории|сюжета)|failed to save (?:the )?(?:story|plot) draft)/i],
  ["architect_character_save_failed", /(?:не удалось сохранить черновик персонажа|failed to save (?:the )?character draft)/i],
  ["architect_draft_save_failed", /не удалось сохранить черновик|failed to save (?:the )?(?:architect )?draft/i],
  ["architect_history_load_failed", /не удалось загрузить переписку архитектора|failed to load (?:the )?architect (?:chat|conversation|history)/i],
  ["architect_history_save_failed", /(?:не удалось сохранить переписку архитектора|ход выполнен, но переписка не сохранилась|failed to save (?:the )?architect (?:chat|conversation|history))/i],
  ["architect_world_save_failed", /не удалось сохранить мир|failed to save (?:the )?world/i],
  ["architect_story_save_failed", /не удалось сохранить (?:историю|сюжет)|failed to save (?:the )?(?:story|plot)/i],
  ["architect_character_save_failed", /не удалось сохранить персонажа|failed to save (?:the )?character/i],
  ["architect_not_running", /(?:ход архитектора уже не выполняется|architect (?:turn )?is not running)/i],
  ["world_lore_required", /(?:сначала создайте лор мира|world lore (?:is )?required)/i],
  ["world_version_drift", /(?:история создавалась под версию мира|story was (?:authored|created) for world version)/i],
  ["protagonist_required", /(?:нет протагониста|нужен персонаж|protagonist (?:is )?required|character (?:is )?required)/i],
  ["story_pc_override", /(?:история написана под своего героя|story (?:has|was written for) (?:its )?own (?:hero|protagonist))/i],
  ["character_world_mismatch", /(?:персонаж создавался под другой мир|character was (?:authored|created) for (?:a )?different world)/i],
  ["character_story_mismatch", /(?:персонаж создавался под другую историю|character was (?:authored|created) for (?:a )?different story)/i],
  ["turn_not_running", /(?:ход уже не выполняется|turn (?:is )?no longer running)/i],
  ["empty_text", /(?:пуст(?:ой текст|ое аудио)|empty (?:text|audio))/i],
  ["api_key_required", /(?:сначала (?:сохрани|укажи).*api[- ]?ключ|api[- ]?ключ.*(?:нужен|обязателен)|api key (?:is )?required)/i],
  ["tts_unavailable", /(?:tts-сервис недоступен|tts (?:service )?(?:is )?unavailable)/i],
  ["architect_turn_failed", /(?:архитектор недоступен|architect (?:is )?unavailable|architect turn failed)/i],
];

const RESERVED_PARAM_KEYS = new Set([
  "ok",
  "code",
  "params",
  "detail",
  "error",
  "message",
  "data",
  "serverMessage",
  "server_message",
]);

function isObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function cleanCode(value) {
  const code = String(value || "").trim().toLowerCase();
  const aliased = CODE_ALIASES[code] || code;
  if (!/^architect_.*_failed$/.test(aliased) || KNOWN_CODES.has(aliased)) return aliased;
  if (/(?:history|chat|conversation).*load/.test(aliased)) return "architect_history_load_failed";
  if (/(?:history|chat|conversation).*save/.test(aliased)) return "architect_history_save_failed";
  if (aliased.includes("draft")) return "architect_draft_save_failed";
  if (aliased.includes("world")) return "architect_world_save_failed";
  if (aliased.includes("story")) return "architect_story_save_failed";
  if (aliased.includes("character")) return "architect_character_save_failed";
  if (aliased.includes("load")) return "architect_load_failed";
  return "architect_turn_failed";
}

function structuredPayload(value) {
  if (!isObject(value)) return null;
  if (isObject(value.serverMessage)) return structuredPayload(value.serverMessage);
  if (isObject(value.server_message)) return structuredPayload(value.server_message);
  if (cleanCode(value.code)) return value;
  if (isObject(value.error)) return structuredPayload(value.error);
  if (isObject(value.data)) return structuredPayload(value.data);
  return null;
}

function rawMessage(value) {
  if (typeof value === "string") return value.trim();
  if (!isObject(value)) return "";
  if (value instanceof ServerMessageError) return rawMessage(value.payload);
  for (const key of ["detail", "error", "message", "data", "serverMessage", "server_message"]) {
    const nested = rawMessage(value[key]);
    if (nested) return nested;
  }
  return "";
}

function legacyCode(value) {
  const raw = rawMessage(value);
  const exactCode = cleanCode(raw);
  if (KNOWN_CODES.has(exactCode)) return exactCode;
  return LEGACY_MESSAGES.find(([, pattern]) => pattern.test(raw))?.[0] || "";
}

function payloadParams(payload, extraParams) {
  const params = isObject(payload?.params) ? { ...payload.params } : {};
  if (isObject(payload)) {
    for (const [key, value] of Object.entries(payload)) {
      if (!RESERVED_PARAM_KEYS.has(key) && ["string", "number", "boolean"].includes(typeof value)) {
        params[key] = value;
      }
    }
  }
  return { ...params, ...(isObject(extraParams) ? extraParams : {}) };
}

/**
 * Wrap a server payload for a Promise catch without copying its raw detail into
 * Error.message. The detail remains available explicitly through
 * serverMessageDetail() for developer-only surfaces and diagnostics.
 */
export class ServerMessageError extends Error {
  constructor(payload) {
    super("Server request failed");
    this.name = "ServerMessageError";
    this.payload = payload;
    const code = cleanCode(structuredPayload(payload)?.code);
    if (code) this.code = code;
  }
}

export function createServerMessageError(payload) {
  return payload instanceof ServerMessageError ? payload : new ServerMessageError(payload);
}

/** Return unlocalized server detail only when a developer-facing view requests it. */
export function serverMessageDetail(value) {
  return rawMessage(value);
}

/**
 * Resolve a structured `{code, params}` payload or a legacy server string to a
 * localized, user-safe message. Unknown raw details are deliberately hidden.
 */
export function localizeServerMessage(
  value,
  t,
  { fallbackCode = "generic", fallbackText = "", params } = {}
) {
  const source = value instanceof ServerMessageError ? value.payload : value;
  const payload = structuredPayload(source);
  const structuredCode = cleanCode(payload?.code || (isObject(source) ? source.code : ""));
  const legacy = legacyCode(source);
  const recognized = KNOWN_CODES.has(structuredCode) || Boolean(legacy);
  if (!recognized && fallbackText) return String(fallbackText);
  const code = KNOWN_CODES.has(structuredCode)
    ? structuredCode
    : legacy || (KNOWN_CODES.has(cleanCode(fallbackCode)) ? cleanCode(fallbackCode) : "generic");
  return t(`${MESSAGE_NAMESPACE}.${code}`, payloadParams(payload, params));
}
