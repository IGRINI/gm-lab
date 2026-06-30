import { useMemo, useState } from "react";
import ImageThumbnail from "./ImagePreview.jsx";

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function statusLabel(sidecarStatus) {
  const image = sidecarStatus?.components?.image || {};
  if (image.enabled === false) return "Image выключен";
  if (image.up) return "Image готов";
  if (image.runtime_ready) return "Image прогревается";
  if (sidecarStatus?.state === "failed") return "Image ошибка";
  return "Image загружается";
}

function isImageReady(sidecarStatus) {
  const image = sidecarStatus?.components?.image || {};
  return image.enabled !== false && image.up === true;
}

export default function ImageLabPanel({ locked, sidecarStatus, onGenerateImage }) {
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState(null);

  const imageReady = isImageReady(sidecarStatus);
  const promptText = textValue(prompt);
  const canGenerate = !!promptText && !busy && !locked && imageReady && typeof onGenerateImage === "function";
  const label = useMemo(() => statusLabel(sidecarStatus), [sidecarStatus]);

  const submit = async (event) => {
    event.preventDefault();
    if (!canGenerate) return;
    setBusy(true);
    setError("");
    try {
      const data = await onGenerateImage({
        prompt: promptText,
        model: "nvfp4",
        width: 1024,
        height: 1024,
      });
      if (!data?.ok) throw new Error(data?.error || "картинка не сгенерирована");
      const image = Array.isArray(data.images) ? data.images.find((item) => textValue(item?.url)) : null;
      const url = textValue(image?.url);
      if (!url) throw new Error("sidecar не вернул URL картинки");
      setResult({
        url,
        seed: data.seed,
        elapsed: Number(data.elapsed_seconds) || 0,
        bytes: Number(image?.bytes) || 0,
      });
    } catch (err) {
      setError(err?.message || "не удалось сгенерировать картинку");
    } finally {
      setBusy(false);
    }
  };

  return (
    <form className="image-lab" onSubmit={submit}>
      <header className="image-lab-head">
        <div className="image-lab-id">
          <span className="image-lab-emblem" aria-hidden="true">✦</span>
          <div className="image-lab-title">
            <span className="image-lab-kicker">developer image lab</span>
            <b>Генерация картинки</b>
          </div>
        </div>
        <span className={`image-lab-chip${imageReady ? " ready" : ""}`}>{label}</span>
      </header>

      <section className="image-lab-body">
        <label className="image-lab-prompt">
          <span>Prompt</span>
          <textarea
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            placeholder="English prompt..."
            rows={7}
            disabled={locked || busy}
          />
        </label>

        <div className="image-lab-actions">
          <button type="submit" className="btn primary" disabled={!canGenerate}>
            {busy ? "Генерирую..." : "Сгенерировать"}
          </button>
          {result?.seed != null && <span className="image-lab-meta">seed {result.seed}</span>}
          {result?.elapsed > 0 && <span className="image-lab-meta">{result.elapsed.toFixed(1)} с</span>}
        </div>

        {error && <div className="image-lab-error">{error}</div>}

        <div className="image-lab-output" aria-live="polite">
          {result?.url ? (
            <ImageThumbnail
              src={result.url}
              alt="Generated image"
              caption="Generated image"
              className="image-lab-thumb"
            />
          ) : (
            <div className="image-lab-empty">Картинка появится здесь</div>
          )}
        </div>
      </section>
    </form>
  );
}
