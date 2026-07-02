import { useMemo, useState } from "react";
import Modal from "./Modal.jsx";

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function worldTitle(world) {
  return textValue(world?.title) || textValue(world?.world_lore?.name) || "Без названия";
}

// A form for creating a story bound to a saved world (docs/MODS_PACKAGES_TZ.md
// Phase 4). Procedural stories carry just a title/brief; authored stories add a
// player character, hidden truth (GM secret) and an explicit starting scene.
// The world is fixed to `world` — the binding the backend validates as a hard
// reference (a missing world is surfaced as an error, never swallowed).
export default function CreateStoryModal({ world, busy, onClose, onCreate }) {
  const [kind, setKind] = useState("procedural");
  const [title, setTitle] = useState("");
  const [description, setDescription] = useState("");
  const [storyBrief, setStoryBrief] = useState("");
  const [publicIntro, setPublicIntro] = useState("");
  const [hiddenTruth, setHiddenTruth] = useState("");
  const [pcName, setPcName] = useState("");
  const [pcRole, setPcRole] = useState("");
  const [sceneTitle, setSceneTitle] = useState("");
  const [sceneDescription, setSceneDescription] = useState("");
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState("");

  const worldId = textValue(world?.id);
  const isAuthored = kind === "authored";
  const locked = busy || submitting || !worldId;
  const canSubmit = useMemo(() => {
    if (locked || !textValue(title)) return false;
    if (isAuthored && !textValue(sceneTitle)) return false;
    return true;
  }, [locked, title, isAuthored, sceneTitle]);

  const submit = async (event) => {
    event.preventDefault();
    if (!canSubmit) return;
    setError("");

    const plot = {};
    if (textValue(storyBrief)) plot.story_brief = textValue(storyBrief);
    if (textValue(publicIntro)) plot.public_intro = textValue(publicIntro);
    if (isAuthored) {
      if (textValue(hiddenTruth)) plot.hidden_truth = textValue(hiddenTruth);
      if (textValue(pcName) || textValue(pcRole)) {
        plot.player_character = {};
        if (textValue(pcName)) plot.player_character.name = textValue(pcName);
        if (textValue(pcRole)) plot.player_character.class_role = textValue(pcRole);
      }
      const scene = { title: textValue(sceneTitle) };
      if (textValue(sceneDescription)) scene.description = textValue(sceneDescription);
      plot.scene = scene;
    }

    const body = {
      kind,
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
        <div className="story-form-kind" role="radiogroup" aria-label="Тип истории">
          <button
            type="button"
            className={"story-kind-btn" + (kind === "procedural" ? " active" : "")}
            onClick={() => setKind("procedural")}
            disabled={locked}
            aria-pressed={kind === "procedural"}
          >
            <b>Процедурная</b>
            <span>Живой мир генерируется из библии мира. Минимум ручной настройки.</span>
          </button>
          <button
            type="button"
            className={"story-kind-btn" + (kind === "authored" ? " active" : "")}
            onClick={() => setKind("authored")}
            disabled={locked}
            aria-pressed={kind === "authored"}
          >
            <b>Авторская</b>
            <span>Заданная завязка: персонаж, секрет GM и стартовая сцена поверх мира.</span>
          </button>
        </div>

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

        {isAuthored && (
          <>
            <label className="world-field">
              <span>Публичное вступление (необязательно)</span>
              <textarea
                value={publicIntro}
                onChange={(event) => setPublicIntro(event.target.value)}
                placeholder="Что игрок видит и знает в начале."
                rows={2}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Скрытая правда (секрет GM, необязательно)</span>
              <textarea
                value={hiddenTruth}
                onChange={(event) => setHiddenTruth(event.target.value)}
                placeholder="То, что знает только GM."
                rows={2}
                disabled={locked}
              />
            </label>

            <div className="world-field-grid">
              <label className="world-field">
                <span>Имя персонажа (необязательно)</span>
                <input
                  value={pcName}
                  onChange={(event) => setPcName(event.target.value)}
                  placeholder="Например: Мира"
                  disabled={locked}
                />
              </label>
              <label className="world-field">
                <span>Роль персонажа (необязательно)</span>
                <input
                  value={pcRole}
                  onChange={(event) => setPcRole(event.target.value)}
                  placeholder="Например: странствующий писец"
                  disabled={locked}
                />
              </label>
            </div>

            <label className="world-field">
              <span>Стартовая сцена — название</span>
              <input
                value={sceneTitle}
                onChange={(event) => setSceneTitle(event.target.value)}
                placeholder="Например: Ворота деревни"
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Стартовая сцена — описание (необязательно)</span>
              <textarea
                value={sceneDescription}
                onChange={(event) => setSceneDescription(event.target.value)}
                placeholder="Где находится игрок в начале истории."
                rows={2}
                disabled={locked}
              />
            </label>
          </>
        )}

        {error && <div className="chat-sidebar-error inline">{error}</div>}

        <div className="story-form-actions">
          <button type="button" className="btn" onClick={onClose} disabled={locked}>
            Отмена
          </button>
          <button type="submit" className="btn primary" disabled={!canSubmit}>
            {submitting ? "Создаю…" : "Создать историю"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
