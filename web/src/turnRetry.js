const MODEL_ERROR_PREFIX = "Ошибка вызова модели:";
const ZERO_USAGE_FIELDS = ["in", "out", "cached", "tokens", "peak_context"];

export function isTerminalTurnError(message) {
  if (message?.type !== "error" || message?.agent !== "ГМ") return false;
  const text = typeof message.text === "string" ? message.text.trim() : "";
  return text.startsWith(MODEL_ERROR_PREFIX);
}

function isEmptyFailedUsage(message) {
  if (message?.type !== "meta_total") return false;
  const data = message.data;
  if (!data || typeof data !== "object" || !Array.isArray(data.calls) || data.calls.length !== 0) {
    return false;
  }
  return ZERO_USAGE_FIELDS.every(
    (field) => typeof data[field] === "number" && Number.isFinite(data[field]) && data[field] === 0
  );
}

// Legacy backends persisted a model transport failure after they had already
// opened the turn. Only this exact, mutation-free tail can be resumed safely.
export function historicalFailedTurn(messages, chatId) {
  if (!chatId || !Array.isArray(messages) || messages.length < 3) return null;

  const [player, modelError, usage] = messages.slice(-3);
  const text = typeof player?.text === "string" ? player.text.trim() : "";
  if (player?.type !== "player" || !text) return null;
  if (!isTerminalTurnError(modelError) || !isEmptyFailedUsage(usage)) return null;

  return {
    chatId,
    text,
    requestId: "",
    errorId: modelError.id,
    legacyResume: true,
  };
}
