import { useMemo, useState } from "react";
import Modal from "./Modal.jsx";

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function worldTitle(world) {
  return textValue(world?.title) || textValue(world?.world_lore?.name) || "Без названия";
}

// Create a PROCEDURAL story bound to a saved world (docs/MODS_PACKAGES_TZ.md
// Phase 4). Procedural stories carry just a title/brief and generate their living
// world from the bible on launch. AUTHORED stories are NOT created here anymore
// (§С1.3): the sole authored path is the story architect, opened via the
// "✨ Открыть в архитекторе" button below (which closes this modal). The world is
// fixed to `world` — the binding the backend validates as a hard reference.
export default function CreateStoryModal({ world, busy, onClose, onCreate, onOpenArchitect }) {
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [storyBrief, setStoryBrief] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");

  const worldId = textValue(world?.id);
  const locked = busy || submitting || !worldId;
  const canSubmit = useMemo(() => !locked && !!textValue(title), [locked, title]);

  const submit = async (event) => {
    event.preventDefault();
    if (!canSubmit) return;
    setError("");

    const plot = {};
    if (textValue(storyBrief)) plot.story_brief = textValue(storyBrief);

    const body = {
      kind: "procedural",
      world_id: worldId,
      title: textValue(title),
    };
    if (textValue(description)) body.description = textValue(description);
    if (Object.keys(plot).length > 0) body.plot = plot;

    setSubmitting(true);
    try {
      const created = await onCreate(body);
      if (created) onClose();
    } catch (e) {
      setError(e?.message || "история не создана");
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <Modal
      title="Новая история"
      subtitle={`по миру «${worldTitle(world)}»`}
      onClose={locked ? () => {} : onClose}
    >
      <form className="story-form" onSubmit={submit}>
        <div className="story-architect-cta">
          <div className="story-architect-cta-text">
            <b>Авторская история</b>
            <span>
              Заданная завязка — персонаж, секрет GM и стартовая сцена поверх мира — собирается в
              архитекторе историй.
            </span>
          </div>
          <button
            type="button"
            className="btn primary"
            onClick={() => onOpenArchitect?.(worldId)}
            disabled={locked}
          >
            ✨ Открыть в архитекторе
          </button>
        </div>

        <div className="story-form-divider" role="separator" aria-label="или процедурная история">
          <span>или процедурная</span>
        </div>

        <p className="world-bible-hint">
          Процедурная история генерирует живой мир из библии мира при запуске — минимум ручной
          настройки.
        </p>

        <label className="world-field">
          <span>Название истории</span>
          <input
            value={title}
            onChange={(event) => setTitle(event.target.value)}
            placeholder="Например: Деревня у живой дороги"
            disabled={locked}
            autoFocus
          />
        </label>

        <label className="world-field">
          <span>Короткое описание (необязательно)</span>
          <input
            value={description}
            onChange={(event) => setDescription(event.target.value)}
            placeholder="Одна строка для списка историй."
            disabled={locked}
          />
        </label>

        <label className="world-field">
          <span>Завязка для игрока (story brief, необязательно)</span>
          <textarea
            value={storyBrief}
            onChange={(event) => setStoryBrief(event.target.value)}
            placeholder="С чего начинается история и что движет игроком."
            rows={2}
            disabled={locked}
          />
        </label>

        {error && <div className="chat-sidebar-error inline">{error}</div>}

        <div className="story-form-actions">
          <button type="button" className="btn" onClick={onClose} disabled={locked}>
            Отмена
          </button>
          <button type="submit" className="btn primary" disabled={!canSubmit}>
            {submitting ? "Создаю…" : "Создать процедурную"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
