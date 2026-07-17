import Icon from "./Icon.jsx";
import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import ImageThumbnail from "./ImagePreview.jsx";

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function statusLabel(sidecarStatus, t) {
  const image = sidecarStatus?.components?.image || {};
  if (image.enabled === false) return t("imageLab.status.disabled");
  if (image.up) return t("imageLab.status.ready");
  if (image.runtime_ready) return t("imageLab.status.warming");
  if (sidecarStatus?.state === "failed") return t("imageLab.status.failed");
  return t("imageLab.status.loading");
}

function isImageReady(sidecarStatus) {
  const image = sidecarStatus?.components?.image || {};
  return image.enabled !== false && image.up === true;
}

export default function ImageLabPanel({ locked, sidecarStatus, onGenerateImage }) {
  const { t } = useTranslation("studio");
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState(null);

  const imageReady = isImageReady(sidecarStatus);
  const promptText = textValue(prompt);
  const canGenerate = !!promptText && !busy && !locked && imageReady && typeof onGenerateImage === "function";
  const label = useMemo(() => statusLabel(sidecarStatus, t), [sidecarStatus, t]);

  const submit = async (event) => {
    event.preventDefault();
    if (!canGenerate) return;
    setBusy(true);
    setError("");
    try {
      const data = await onGenerateImage({
        prompt: promptText,
        width: 1024,
        height: 1024,
      });
      if (!data?.ok) throw new Error(data?.error || t("imageLab.errors.notGenerated"));
      const image = Array.isArray(data.images) ? data.images.find((item) => textValue(item?.url)) : null;
      const url = textValue(image?.url);
      if (!url) throw new Error(t("imageLab.errors.missingUrl"));
      setResult({
        url,
        seed: data.seed,
        elapsed: Number(data.elapsed_seconds) || 0,
        bytes: Number(image?.bytes) || 0,
      });
    } catch (err) {
      setError(err?.message || t("imageLab.errors.failed"));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="image-lab" onSubmit={submit}>
      <header className="image-lab-head">
        <div className="image-lab-id">
          <span className="image-lab-emblem" aria-hidden="true"><Icon name="image" size={18} /></span>
          <div className="image-lab-title">
            <span className="image-lab-kicker">{t("imageLab.kicker")}</span>
            <b>{t("imageLab.title")}</b>
          </div>
        </div>
        <span className={`image-lab-chip${imageReady ? " ready" : ""}`}>{label}</span>
      </header>

      <section className="image-lab-body">
        <label className="image-lab-prompt">
          <span>{t("imageLab.promptLabel")}</span>
          <textarea
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            placeholder={t("imageLab.promptPlaceholder")}
            rows={7}
            disabled={locked || busy}
          />
        </label>

        <div className="image-lab-actions">
          <button type="submit" className="btn primary" disabled={!canGenerate}>
            {busy ? t("imageLab.generating") : t("imageLab.generate")}
          </button>
          {result?.seed != null && <span className="image-lab-meta">seed {result.seed}</span>}
          {result?.elapsed > 0 && (
            <span className="image-lab-meta">
              {t("imageLab.elapsed", { seconds: result.elapsed.toFixed(1) })}
            </span>
          )}
        </div>

        {error && <div className="image-lab-error">{error}</div>}

        <div className="image-lab-output" aria-live="polite">
          {result?.url ? (
            <ImageThumbnail
              src={result.url}
              alt={t("imageLab.generatedAlt")}
              caption={t("imageLab.generatedCaption")}
              className="image-lab-thumb"
            />
          ) : (
            <div className="image-lab-empty">{t("imageLab.empty")}</div>
          )}
        </div>
      </section>
    </form>
  );
}
