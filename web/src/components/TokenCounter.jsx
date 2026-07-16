import { useEffect, useMemo, useState } from "react";
import Modal from "./Modal.jsx";
import Tooltip, { TipContent } from "./Tooltip.jsx";
import { api } from "../api.js";
import { useTranslation } from "react-i18next";

export default function TokenCounter({ models = [], currentModel = "", onClose }) {
  const { t } = useTranslation("game");
  const modelOptions = useMemo(() => {
    const list = (models || []).map((m) => m.id || m.slug).filter(Boolean);
    if (currentModel && !list.includes(currentModel)) list.unshift(currentModel);
    return list;
  }, [models, currentModel]);

  const [model, setModel] = useState(currentModel || modelOptions[0] || "");
  const [text, setText] = useState("");
  const [result, setResult] = useState(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  const [keySaved, setKeySaved] = useState(false);
  const [keyHint, setKeyHint] = useState("");
  const [keyInput, setKeyInput] = useState("");
  const [keyBusy, setKeyBusy] = useState(false);

  useEffect(() => {
    api
      .openaiKeyStatus()
      .then((d) => {
        if (d && d.ok) {
          setKeySaved(!!d.saved);
          setKeyHint(d.hint || "");
        }
      })
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!model && modelOptions.length) setModel(modelOptions[0]);
  }, [modelOptions, model]);

  const saveKey = async () => {
    const k = keyInput.trim();
    if (!k) return;
    setKeyBusy(true);
    try {
      const d = await api.saveOpenaiKey(k);
      if (d.ok) {
        setKeySaved(!!d.saved);
        setKeyHint(d.hint || "");
        setKeyInput("");
      }
    } finally {
      setKeyBusy(false);
    }
  };

  const deleteKey = async () => {
    setKeyBusy(true);
    try {
      const d = await api.deleteOpenaiKey();
      if (d.ok) {
        setKeySaved(false);
        setKeyHint("");
      }
    } finally {
      setKeyBusy(false);
    }
  };

  const count = async () => {
    setBusy(true);
    setError("");
    try {
      const d = await api.tokenize(text, model);
      if (!d.ok) throw new Error(d.error || t("tokenCounter.countFailed"));
      setResult(d);
    } catch (e) {
      setError(e.message || String(e));
      setResult(null);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title={t("tokenCounter.title")} subtitle="OpenAI · /v1/responses/input_tokens" wide depth={1} onClose={onClose}>
      <div className="tok-tool">
        <div className="tok-keyrow">
          <div className="tok-keyhead">
            <span>{t("tokenCounter.apiKey")}</span>
            {keySaved ? (
              <b className="tok-key-ok">{t("tokenCounter.keySaved")} · {keyHint}</b>
            ) : (
              <em className="tok-key-no">{t("tokenCounter.keyNotSaved")}</em>
            )}
          </div>
          <div className="tok-keyinputs">
            <input
              type="password"
              placeholder={keySaved ? t("tokenCounter.replaceKeyPlaceholder") : "sk-…"}
              value={keyInput}
              autoComplete="off"
              onChange={(e) => setKeyInput(e.target.value)}
            />
            <button type="button" className="btn" disabled={keyBusy || !keyInput.trim()} onClick={saveKey}>
              {t("actions.save")}
            </button>
            {keySaved && (
              <button type="button" className="btn" disabled={keyBusy} onClick={deleteKey}>
                {t("actions.delete")}
              </button>
            )}
          </div>
          <small className="tok-hint">
            {t("tokenCounter.keyHint")}
          </small>
        </div>

        <textarea
          className="tok-text"
          rows={6}
          placeholder={t("tokenCounter.textPlaceholder")}
          value={text}
          onChange={(e) => setText(e.target.value)}
        />

        <div className="tok-controls">
          <Tooltip
            className="tooltip-wrap"
            tipClassName="ui-tip-wrap"
            focusable={false}
            content={
              <TipContent
                title={t("tokenCounter.modelTitle")}
                note={t("tokenCounter.modelNote")}
              />
            }
          >
            <select value={model} onChange={(e) => setModel(e.target.value)} aria-label={t("tokenCounter.modelAria")}>
              {modelOptions.length ? (
                modelOptions.map((m) => (
                  <option key={m} value={m}>
                    {m}
                  </option>
                ))
              ) : (
                <option value="">{t("tokenCounter.modelsNotLoaded")}</option>
              )}
            </select>
          </Tooltip>
          <button
            type="button"
            className="btn primary"
            disabled={busy || !text.trim() || !keySaved}
            onClick={count}
          >
            {busy ? t("tokenCounter.counting") : t("tokenCounter.count")}
          </button>
        </div>

        {error && <div className="err">{error}</div>}

        {result && (
          <div className="tok-result">
            <div className="tok-stats">
              <span className="tok-count">
                <b>{result.count ?? "—"}</b> {t("tokenCounter.inputTokens", { count: result.count ?? 0 })}
              </span>
              <span>{t("tokenCounter.characters", { count: result.chars })}</span>
              {result.model ? <span className="tok-enc">{result.model}</span> : null}
            </div>
            <small className="tok-hint">
              {t("tokenCounter.endpointPrefix")} <code>/v1/responses/input_tokens</code>{t("tokenCounter.endpointSuffix")}
            </small>
          </div>
        )}
      </div>
    </Modal>
  );
}
