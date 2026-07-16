import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import Modal from "./Modal.jsx";
import WizCard, {
  storyDescription,
  storyTip,
  storyTitle,
  worldMeta,
  worldPreview,
  worldTip,
  worldTitle,
} from "./WizCard.jsx";

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
// wizard's shared WizCard (incl. its hover tips) so the pick reads like the
// New-Game steps.

function idOf(value) {
  return value == null ? "" : String(value).trim();
}

function sameId(a, b) {
  return a != null && b != null && String(a) === String(b);
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
  const { t } = useTranslation("library");
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
        {t("actions.cancel")}
      </button>
      <div className="wiz-foot-right">
        <button type="button" className="btn primary" onClick={confirm} disabled={!canConfirm}>
          {t("basePicker.toStudio")}
        </button>
      </div>
    </div>
  );

  return (
    <Modal
      title={t(isStory ? "basePicker.newStory" : "basePicker.newCharacter")}
      subtitle={t(isStory ? "basePicker.chooseWorldBase" : "basePicker.chooseBase")}
      onClose={locked ? () => {} : onClose}
      className="wiz-modal"
      footer={footer}
    >
      <div className="wiz">
        <div className="wiz-panel">
          <p className="wiz-lead">
            {isStory
              ? t("basePicker.storyLead")
              : t("basePicker.characterLead")}
          </p>
          <div className="wiz-grid">
            {!isStory && (
              <WizCard
                selected={!worldId && !storyId}
                disabled={locked}
                onClick={pickStandalone}
                kicker={t("entities.baseLower")}
                title={t("basePicker.noBase")}
                desc={t("basePicker.noBaseDescription")}
              />
            )}
            {worldList.map((w) => (
              <WizCard
                key={`w-${w.id}`}
                selected={sameId(worldId, w.id)}
                disabled={locked}
                onClick={() => pickWorld(idOf(w.id))}
                kicker={t("entities.worldLower")}
                title={worldTitle(w, t)}
                meta={worldMeta(w)}
                desc={worldPreview(w)}
                tip={worldTip(w, t)}
              />
            ))}
            {!isStory &&
              builtinStories.map((s) => (
                <WizCard
                  key={`b-${s.id}`}
                  selected={builtinPicked && sameId(storyId, s.id)}
                  disabled={locked}
                  onClick={() => pickBuiltin(s.id)}
                  kicker={t("entities.storyLower")}
                  badge={t("badges.builtinClassic")}
                  title={storyTitle(s, t)}
                  desc={storyDescription(s)}
                  tip={storyTip(s, t, { kicker: t("badges.builtinClassic") })}
                />
              ))}
          </div>
          {worldList.length === 0 && (
            <div className="wiz-empty-cta">
              <p className="wiz-empty">
                {isStory
                  ? t("basePicker.noWorldsForStory")
                  : t(
                      builtinStories.length > 0
                        ? "basePicker.noWorldsCharacterWithBuiltin"
                        : "basePicker.noWorldsCharacter"
                    )}
              </p>
              {onCreateWorld && (
                <button type="button" className="btn" onClick={onCreateWorld} disabled={locked}>
                  {t("actions.createWorld")}
                </button>
              )}
            </div>
          )}
        </div>

        {!isStory && worldId && (
          <div className="wiz-panel">
            <p className="wiz-lead">
              {t("basePicker.optionalStory", {
                world: worldTitle(worldList.find((w) => sameId(w.id, worldId)), t),
              })}
            </p>
            <div className="wiz-grid">
              <WizCard
                selected={!storyId}
                disabled={locked}
                onClick={() => setStoryId("")}
                kicker={t("entities.storyLower")}
                title={t("basePicker.noStory")}
                desc={t("basePicker.noStoryDescription")}
              />
              {worldStories.map((s) => (
                <WizCard
                  key={`s-${s.id}`}
                  selected={sameId(storyId, s.id)}
                  disabled={locked}
                  onClick={() => setStoryId(idOf(s.id))}
                  kicker={t("entities.storyLower")}
                  title={storyTitle(s, t)}
                  desc={storyDescription(s)}
                  tip={storyTip(s, t)}
                />
              ))}
            </div>
            {worldStories.length === 0 && (
              <p className="wiz-empty">{t("basePicker.noAuthoredStories")}</p>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}
