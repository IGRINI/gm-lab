import Icon from "./Icon.jsx";
import { useMemo, useState } from "react";
import Modal from "./Modal.jsx";

// BasePickerModal — the explicit "на чём строим?" step before a creation studio
// opens. Two modes:
//   kind="story"     — a STORY is always built over a world: pick one (required).
//   kind="character" — a CHARACTER may be standalone, based on a world, based on
//                      a world + one of its AUTHORED stories (an optional second
//                      row appears once a world is picked), or based on a builtin
//                      self-contained story (a base card in the first row, like
//                      the wizard's «встроенная классика» — it fixes the story
//                      and carries no world ref of its own).
// Procedural stories never appear: they carry no plot to base a hero on (the
// wizard's «Создать персонажа» ctx applies the same gate).
// Purely presentational: onConfirm(kind, { worldId, storyId }) hands the choice
// back to the integrator (App), which opens the matching studio. Reuses the
// wizard's wiz-card/wiz-grid styles so the pick reads like the New-Game steps.

function textValue(value) {
  return typeof value === "string" ? value.trim() : "";
}

function idOf(value) {
  return value == null ? "" : String(value).trim();
}

function sameId(a, b) {
  return a != null && b != null && String(a) === String(b);
}

function worldTitle(world) {
  return textValue(world?.title) || textValue(world?.world_lore?.name) || "Без названия";
}

function worldMeta(world) {
  return [world?.genre, world?.tone].map((v) => textValue(v)).filter(Boolean).join(" · ");
}

function worldPreview(world) {
  return textValue(world?.preview) || textValue(world?.public_premise) || "";
}

function storyTitle(story) {
  return textValue(story?.title) || "Без названия";
}

function storyDescription(story) {
  return textValue(story?.story_brief) || textValue(story?.description) || "";
}

function Card({ selected, disabled, onClick, kicker, title, badge, meta, desc }) {
  return (
    <button
      type="button"
      className={"wiz-card" + (selected ? " is-selected" : "")}
      onClick={onClick}
      disabled={disabled}
      aria-pressed={selected}
    >
      {(kicker || badge) && (
        <span className="wiz-card-top">
          {kicker && <span className="wiz-card-kicker">{kicker}</span>}
          {badge && <span className="wiz-badge">{badge}</span>}
        </span>
      )}
      <span className="wiz-card-title">{title}</span>
      {meta && <span className="wiz-card-meta">{meta}</span>}
      {desc && <span className="wiz-card-desc">{desc}</span>}
      {selected && (
        <span className="wiz-card-check" aria-hidden="true">
          <Icon name="check" size={13} strokeWidth={2.4} />
        </span>
      )}
    </button>
  );
}

export default function BasePickerModal({
  kind, // "story" | "character"
  worlds = [],
  stories = [],
  busy = false,
  onConfirm,
  onCreateWorld, // empty-library escape hatch → world studio
  onClose,
}) {
  const isStory = kind === "story";
  // Character mode selection, as a (worldId, storyId) pair: ("", "") = «без
  // основы»; (id, "") = world; (id, sid) = world + its authored story;
  // ("", sid) = builtin self-contained story. Story mode uses worldId only.
  const [worldId, setWorldId] = useState("");
  const [storyId, setStoryId] = useState("");

  const worldList = useMemo(() => (Array.isArray(worlds) ? worlds : []), [worlds]);
  const storyList = useMemo(() => (Array.isArray(stories) ? stories : []), [stories]);

  const isRealStory = (s) =>
    idOf(s?.id) !== "procedural" && !s?.procedural && s?.kind !== "procedural";

  // Stories of the picked world (character mode's optional second row). The
  // synthetic "procedural" catalog entry and procedural stories carry no plot to
  // base a hero on — only real authored rows pass.
  const worldStories = useMemo(
    () => (worldId ? storyList.filter((s) => sameId(s?.world_ref?.id, worldId) && isRealStory(s)) : []),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [storyList, worldId],
  );

  // Builtin self-contained stories (no world_ref): pickable as a base of their
  // own in character mode — they carry a full world inside, so basing a hero on
  // one needs no separate world pick.
  const builtinStories = useMemo(
    () => storyList.filter((s) => !idOf(s?.world_ref?.id) && isRealStory(s)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [storyList],
  );

  const locked = !!busy;
  const canConfirm = !locked && (isStory ? !!worldId : true);
  const builtinPicked = !worldId && !!storyId;

  const pickStandalone = () => {
    if (locked) return;
    setWorldId("");
    setStoryId("");
  };

  const pickWorld = (id) => {
    // Re-clicking the already-selected world must NOT clear a chosen story.
    if (locked || (worldId && sameId(id, worldId))) return;
    setWorldId(id);
    setStoryId(""); // stories belong to the previous world (or a builtin pick)
  };

  const pickBuiltin = (id) => {
    if (locked) return;
    setWorldId("");
    setStoryId(idOf(id));
  };

  const confirm = () => {
    if (!canConfirm) return;
    onConfirm?.(kind, { worldId: idOf(worldId), storyId: idOf(storyId) });
  };

  const footer = (
    <div className="wiz-foot">
      <button type="button" className="btn" onClick={onClose} disabled={locked}>
        Отмена
      </button>
      <div className="wiz-foot-right">
        <button type="button" className="btn primary" onClick={confirm} disabled={!canConfirm}>
          В студию →
        </button>
      </div>
    </div>
  );

  return (
    <Modal
      title={isStory ? "Новая история" : "Новый персонаж"}
      subtitle={isStory ? "выберите мир-основу" : "выберите основу"}
      onClose={locked ? () => {} : onClose}
      className="wiz-modal"
      footer={footer}
    >
      <div className="wiz">
        <div className="wiz-panel">
          <p className="wiz-lead">
            {isStory
              ? "История строится над миром: его канон станет библией сюжета."
              : "Персонаж может быть сам по себе, под конкретный мир или под встроенную классику — тогда архитектор будет опираться на её публичный канон."}
          </p>
          <div className="wiz-grid">
            {!isStory && (
              <Card
                selected={!worldId && !storyId}
                disabled={locked}
                onClick={pickStandalone}
                kicker="основа"
                title="Без основы"
                desc="Переносимый герой, не привязанный к миру, — подойдёт для любой истории."
              />
            )}
            {worldList.map((w) => (
              <Card
                key={`w-${w.id}`}
                selected={sameId(worldId, w.id)}
                disabled={locked}
                onClick={() => pickWorld(idOf(w.id))}
                kicker="мир"
                title={worldTitle(w)}
                meta={worldMeta(w)}
                desc={worldPreview(w)}
              />
            ))}
            {!isStory &&
              builtinStories.map((s) => (
                <Card
                  key={`b-${s.id}`}
                  selected={builtinPicked && sameId(storyId, s.id)}
                  disabled={locked}
                  onClick={() => pickBuiltin(s.id)}
                  kicker="история"
                  badge="встроенная классика"
                  title={storyTitle(s)}
                  desc={storyDescription(s)}
                />
              ))}
          </div>
          {worldList.length === 0 && (
            <div className="wiz-empty-cta">
              <p className="wiz-empty">
                {isStory
                  ? "Миров пока нет — сначала создайте мир, история строится над ним."
                  : "Миров пока нет — персонажа можно создать без основы" +
                    (builtinStories.length > 0 ? " или на встроенной классике." : ".")}
              </p>
              {onCreateWorld && (
                <button type="button" className="btn" onClick={onCreateWorld} disabled={locked}>
                  + Создать мир
                </button>
              )}
            </div>
          )}
        </div>

        {!isStory && worldId && (
          <div className="wiz-panel">
            <p className="wiz-lead">
              Опционально: история мира «
              {worldTitle(worldList.find((w) => sameId(w.id, worldId)))}» — герой станет её
              протагонистом.
            </p>
            <div className="wiz-grid">
              <Card
                selected={!storyId}
                disabled={locked}
                onClick={() => setStoryId("")}
                kicker="история"
                title="Без истории"
                desc="Опора только на мир — герой подойдёт любой его истории."
              />
              {worldStories.map((s) => (
                <Card
                  key={`s-${s.id}`}
                  selected={sameId(storyId, s.id)}
                  disabled={locked}
                  onClick={() => setStoryId(idOf(s.id))}
                  kicker="история"
                  title={storyTitle(s)}
                  desc={storyDescription(s)}
                />
              ))}
            </div>
            {worldStories.length === 0 && (
              <p className="wiz-empty">У этого мира пока нет авторских историй.</p>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}
