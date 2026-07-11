import { useEffect, useMemo, useState } from "react";
import Modal from "./Modal.jsx";

// NewGameWizard — the ONLY path to start a game (§Мастер «Новая игра»). Three
// steps in the approved order: Мир → История → Персонаж, then a name + «Начать».
// It launches through the EXISTING chat-create endpoints (no backend change):
//   authored / builtin story → onLaunch({ storyId, characterId?, title })
//   procedural campaign       → onLaunch({ storyId:"procedural", worldId, characterId, title })
// State lives entirely here; the integrator just mounts it and reacts to onLaunch
// / onOpenStudio / onClose.

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

function characterTitle(character) {
  return textValue(character?.title) || "Персонаж";
}

function characterMeta(character) {
  const pc = character?.payload?.player_character || {};
  const parts = [];
  const role = textValue(pc.class_role);
  if (role) parts.push(role);
  if (pc.level != null && `${pc.level}`.trim() !== "") parts.push(`ур. ${pc.level}`);
  return parts.join(" · ") || textValue(character?.preview) || "персонаж";
}

// A self-contained "builtin bundle": a story that ships its own world (no
// world_ref). In step 1 these render as world cards labeled «встроенная классика»
// — picking one fixes BOTH the world and the single story it carries.
function isBuiltinBundle(story) {
  return (
    !!story &&
    idOf(story.id) !== "procedural" &&
    !story.procedural &&
    !idOf(story?.world_ref?.id)
  );
}

// Whether a story carries an authored protagonist the player can take as-is. The
// PLAYER-facing catalog does not expose the seed, so trust an explicit `has_pc`
// flag when the integrator supplies it, else derive: any non-procedural authored
// story (incl. self-contained builtins) ships a protagonist.
function storyHasProtagonist(story, procedural) {
  if (procedural || !story) return false;
  if (typeof story.has_pc === "boolean") return story.has_pc;
  const pc = story.player_character;
  if (pc && typeof pc === "object" && !Array.isArray(pc) && Object.keys(pc).length > 0) return true;
  return story.kind === "authored" || !idOf(story?.world_ref?.id);
}

// Resolve the initial step/selection from a preselect hint (Library «Играть»):
// a story jumps straight to Персонаж, a world to История, a lone character just
// pre-picks the hero and starts at Мир.
function deriveInitial(preselect, worlds, stories) {
  const pre = preselect || {};
  const preChar = idOf(pre.characterId) || null;

  const preStory = idOf(pre.storyId);
  if (preStory) {
    const story = (stories || []).find((s) => sameId(s.id, preStory));
    if (story) {
      const bound = idOf(story?.world_ref?.id);
      const worldChoice = bound
        ? { type: "world", id: bound }
        : { type: "builtin", id: idOf(story.id), storyId: idOf(story.id) };
      return { step: 3, worldChoice, storyId: idOf(story.id), characterChoice: preChar };
    }
  }

  const preWorld = idOf(pre.worldId);
  if (preWorld) {
    const world = (worlds || []).find((w) => sameId(w.id, preWorld));
    if (world) {
      return {
        step: 2,
        worldChoice: { type: "world", id: preWorld },
        storyId: "",
        characterChoice: preChar,
      };
    }
  }

  return { step: 1, worldChoice: null, storyId: "", characterChoice: preChar };
}

function WizCard({ selected, disabled, onClick, kicker, title, badge, meta, desc, add }) {
  const className =
    "wiz-card" + (add ? " wiz-card-add" : "") + (selected ? " is-selected" : "");
  return (
    <button
      type="button"
      className={className}
      onClick={onClick}
      disabled={disabled}
      aria-pressed={add ? undefined : selected}
    >
      {add ? (
        <>
          <span className="wiz-card-add-icon" aria-hidden="true">＋</span>
          <span className="wiz-card-add-label">{title}</span>
        </>
      ) : (
        <>
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
              ✓
            </span>
          )}
        </>
      )}
    </button>
  );
}

export default function NewGameWizard({
  worlds = [],
  stories = [],
  characters = [],
  preselect = null,
  onLaunch,
  onOpenStudio,
  onClose,
  busy = false,
}) {
  // Seed the selection once, from the preselect hint. The integrator mounts a
  // fresh wizard per open, so a one-shot initializer is enough (no re-sync).
  const [init] = useState(() => deriveInitial(preselect, worlds, stories));
  const [step, setStep] = useState(init.step);
  const [worldChoice, setWorldChoice] = useState(init.worldChoice);
  const [storyId, setStoryId] = useState(init.storyId);
  const [characterChoice, setCharacterChoice] = useState(init.characterChoice);
  const [title, setTitle] = useState("");
  const [titleDirty, setTitleDirty] = useState(false);

  const locked = !!busy;

  const libraryWorlds = useMemo(() => (Array.isArray(worlds) ? worlds : []), [worlds]);
  const builtinBundles = useMemo(
    () => (Array.isArray(stories) ? stories.filter(isBuiltinBundle) : []),
    [stories],
  );

  const selectedWorld = useMemo(
    () =>
      worldChoice?.type === "world"
        ? libraryWorlds.find((w) => sameId(w.id, worldChoice.id)) || null
        : null,
    [libraryWorlds, worldChoice],
  );

  const worldBoundStories = useMemo(
    () =>
      worldChoice?.type === "world"
        ? (stories || []).filter(
            (s) =>
              sameId(s?.world_ref?.id, worldChoice.id) &&
              idOf(s.id) !== "procedural" &&
              !s.procedural,
          )
        : [],
    [stories, worldChoice],
  );

  const selectedStoryRow = useMemo(
    () =>
      storyId && storyId !== "procedural"
        ? (stories || []).find((s) => sameId(s.id, storyId)) || null
        : null,
    [stories, storyId],
  );

  const isSyntheticProcedural = storyId === "procedural";
  const isProcedural = isSyntheticProcedural || selectedStoryRow?.kind === "procedural";
  const hasProtagonist = storyHasProtagonist(selectedStoryRow, isProcedural);

  const selectedCharacter = useMemo(
    () =>
      characterChoice && characterChoice !== "protagonist"
        ? (characters || []).find((c) => sameId(c.id, characterChoice)) || null
        : null,
    [characters, characterChoice],
  );

  // On entering Персонаж with nothing picked yet, default to the story's own
  // protagonist when it has one (procedural has none → stays empty → a package
  // is required to launch).
  useEffect(() => {
    if (step !== 3 || characterChoice != null) return;
    if (hasProtagonist) setCharacterChoice("protagonist");
  }, [step, characterChoice, hasProtagonist]);

  const storyNameForTitle = isSyntheticProcedural
    ? "Процедурная кампания"
    : storyTitle(selectedStoryRow);
  const characterNameForTitle =
    characterChoice === "protagonist"
      ? ""
      : selectedCharacter
        ? characterTitle(selectedCharacter)
        : "";
  const defaultTitle = useMemo(() => {
    const base = storyNameForTitle || "Новая игра";
    return characterNameForTitle ? `${base} — ${characterNameForTitle}` : base;
  }, [storyNameForTitle, characterNameForTitle]);

  // Название auto-fills «{история} — {персонаж}» until the player edits it.
  useEffect(() => {
    if (!titleDirty) setTitle(defaultTitle);
  }, [defaultTitle, titleDirty]);

  // ESC closes (unless a launch is in flight).
  useEffect(() => {
    if (typeof document === "undefined") return undefined;
    const onKey = (event) => {
      if (event.key !== "Escape" || locked) return;
      event.preventDefault();
      onClose?.();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [locked, onClose]);

  const characterOk =
    (characterChoice === "protagonist" && hasProtagonist) ||
    (typeof characterChoice === "string" &&
      characterChoice !== "protagonist" &&
      characterChoice.length > 0 &&
      !!selectedCharacter);
  const canAdvance = step === 1 ? !!worldChoice : step === 2 ? !!storyId : true;
  const canLaunch = !locked && step === 3 && !!storyId && characterOk;

  const chooseWorld = (choice) => {
    if (locked) return;
    setWorldChoice(choice);
    setStoryId(choice.type === "builtin" ? choice.storyId : "");
    // Drop the "story protagonist" pick — it belongs to the previous story.
    setCharacterChoice((prev) => (prev === "protagonist" ? null : prev));
  };

  const chooseStory = (id) => {
    if (locked) return;
    setStoryId(idOf(id) === "procedural" ? "procedural" : idOf(id));
    setCharacterChoice((prev) => (prev === "protagonist" ? null : prev));
  };

  const chooseCharacter = (choice) => {
    if (locked) return;
    setCharacterChoice(choice);
  };

  const goBack = () => {
    if (step > 1) setStep(step - 1);
  };
  const goNext = () => {
    if (canAdvance && step < 3) setStep(step + 1);
  };
  const gotoStep = (n) => {
    if (locked) return;
    if (n === 1) setStep(1);
    else if (n === 2 && worldChoice) setStep(2);
    else if (n === 3 && storyId) setStep(3);
  };

  const openStudio = (kind, ctx) => {
    onOpenStudio?.(kind, ctx);
    onClose?.();
  };

  const launch = () => {
    if (!canLaunch || !onLaunch) return;
    const resolvedCharacterId = characterChoice === "protagonist" ? "" : idOf(characterChoice);
    const body = { title: textValue(title) || defaultTitle };
    if (isSyntheticProcedural) {
      body.storyId = "procedural";
      body.worldId = idOf(worldChoice?.id);
    } else {
      body.storyId = idOf(storyId);
    }
    if (resolvedCharacterId) body.characterId = resolvedCharacterId;
    onLaunch(body);
  };

  const worldLabel = worldChoice
    ? worldChoice.type === "builtin"
      ? storyTitle((stories || []).find((s) => sameId(s.id, worldChoice.storyId)))
      : worldTitle(selectedWorld)
    : "";
  const storyLabel = storyId
    ? isSyntheticProcedural
      ? "Процедурная кампания"
      : storyTitle(selectedStoryRow)
    : "";
  const characterLabel =
    characterChoice === "protagonist"
      ? "Протагонист истории"
      : selectedCharacter
        ? characterTitle(selectedCharacter)
        : "";

  const steps = [
    { n: 1, name: "Мир", value: worldLabel, reachable: true },
    { n: 2, name: "История", value: storyLabel, reachable: !!worldChoice },
    { n: 3, name: "Персонаж", value: characterLabel, reachable: !!storyId },
  ];

  const bundleStory =
    worldChoice?.type === "builtin"
      ? (stories || []).find((s) => sameId(s.id, worldChoice.storyId)) || null
      : null;

  const footer = (
    <div className="wiz-foot">
      <button
        type="button"
        className="btn"
        onClick={step === 1 ? onClose : goBack}
        disabled={locked}
      >
        {step === 1 ? "Отмена" : "← Назад"}
      </button>
      <div className="wiz-foot-right">
        {step === 3 && (
          <label className="wiz-name">
            <span>Название</span>
            <input
              value={title}
              placeholder={defaultTitle}
              onChange={(event) => {
                setTitle(event.target.value);
                setTitleDirty(true);
              }}
              disabled={locked}
            />
          </label>
        )}
        {step < 3 ? (
          <button
            type="button"
            className="btn primary"
            onClick={goNext}
            disabled={locked || !canAdvance}
          >
            Далее →
          </button>
        ) : (
          <button type="button" className="btn primary" onClick={launch} disabled={!canLaunch}>
            {locked ? "Запуск…" : "Начать"}
          </button>
        )}
      </div>
    </div>
  );

  return (
    <Modal
      title="Новая игра"
      subtitle={`Шаг ${step} из 3`}
      onClose={onClose}
      className="wiz-modal"
      footer={footer}
    >
      <div className="wiz">
        <ol className="wiz-steps">
          {steps.map((s) => (
            <li
              key={s.n}
              className={
                "wiz-step" +
                (step === s.n ? " is-active" : "") +
                (s.value ? " is-done" : "")
              }
            >
              <button
                type="button"
                className="wiz-step-btn"
                onClick={() => gotoStep(s.n)}
                disabled={!s.reachable || locked}
              >
                <span className="wiz-step-n">{s.n}</span>
                <span className="wiz-step-text">
                  <span className="wiz-step-name">{s.name}</span>
                  {s.value && <span className="wiz-step-value">{s.value}</span>}
                </span>
              </button>
            </li>
          ))}
        </ol>

        {step === 1 && (
          <div className="wiz-panel">
            <p className="wiz-lead">Выберите мир из библиотеки или встроенную классику.</p>
            <div className="wiz-grid">
              {libraryWorlds.map((w) => (
                <WizCard
                  key={`w-${w.id}`}
                  selected={worldChoice?.type === "world" && sameId(worldChoice.id, w.id)}
                  disabled={locked}
                  onClick={() => chooseWorld({ type: "world", id: idOf(w.id) })}
                  kicker="мир"
                  title={worldTitle(w)}
                  meta={worldMeta(w)}
                  desc={worldPreview(w)}
                />
              ))}
              {builtinBundles.map((s) => (
                <WizCard
                  key={`b-${s.id}`}
                  selected={worldChoice?.type === "builtin" && sameId(worldChoice.storyId, s.id)}
                  disabled={locked}
                  onClick={() =>
                    chooseWorld({ type: "builtin", id: idOf(s.id), storyId: idOf(s.id) })
                  }
                  kicker="мир"
                  badge="встроенная классика"
                  title={storyTitle(s)}
                  desc={storyDescription(s)}
                />
              ))}
              <WizCard
                add
                title="Создать мир"
                disabled={locked}
                onClick={() => openStudio("world", null)}
              />
            </div>
            {libraryWorlds.length === 0 && builtinBundles.length === 0 && (
              <p className="wiz-empty">Миров пока нет — создайте новый в студии.</p>
            )}
          </div>
        )}

        {step === 2 &&
          (worldChoice?.type === "builtin" ? (
            <div className="wiz-panel">
              <p className="wiz-lead">Встроенная классика — история идёт в комплекте.</p>
              <div className="wiz-grid">
                <WizCard
                  selected
                  disabled={locked}
                  onClick={() => chooseStory(worldChoice.storyId)}
                  kicker="история"
                  title={storyTitle(bundleStory)}
                  desc={storyDescription(bundleStory)}
                />
              </div>
            </div>
          ) : (
            <div className="wiz-panel">
              <p className="wiz-lead">Истории мира «{worldTitle(selectedWorld)}».</p>
              <div className="wiz-grid">
                {worldBoundStories.map((s) => (
                  <WizCard
                    key={`s-${s.id}`}
                    selected={sameId(storyId, s.id)}
                    disabled={locked}
                    onClick={() => chooseStory(s.id)}
                    kicker="история"
                    meta={s.kind === "procedural" ? "процедурная" : undefined}
                    title={storyTitle(s)}
                    desc={storyDescription(s)}
                  />
                ))}
                <WizCard
                  selected={isSyntheticProcedural}
                  disabled={locked}
                  onClick={() => chooseStory("procedural")}
                  kicker="история"
                  badge="процедурная"
                  title="Процедурная кампания"
                  desc="Живой мир генерируется из библии мира при запуске."
                />
                <WizCard
                  add
                  title="Создать историю"
                  disabled={locked}
                  onClick={() => openStudio("story", { worldId: idOf(worldChoice?.id) })}
                />
              </div>
            </div>
          ))}

        {step === 3 && (
          <div className="wiz-panel">
            <p className="wiz-lead">
              {isProcedural
                ? "Процедурной кампании нужен персонаж из библиотеки."
                : "Выберите персонажа. По умолчанию — протагонист истории."}
            </p>
            <div className="wiz-grid">
              {hasProtagonist && (
                <WizCard
                  selected={characterChoice === "protagonist"}
                  disabled={locked}
                  onClick={() => chooseCharacter("protagonist")}
                  kicker="персонаж"
                  badge="из истории"
                  title="Протагонист истории"
                  desc="Готовый герой, заданный автором истории."
                />
              )}
              {(characters || []).map((c) => {
                const preview = textValue(c.preview);
                return (
                  <WizCard
                    key={`c-${c.id}`}
                    selected={sameId(characterChoice, c.id)}
                    disabled={locked}
                    onClick={() => chooseCharacter(idOf(c.id))}
                    kicker="персонаж"
                    title={characterTitle(c)}
                    meta={characterMeta(c)}
                    desc={preview && preview !== characterTitle(c) ? preview : ""}
                  />
                );
              })}
              <WizCard
                add
                title="Создать персонажа"
                disabled={locked}
                onClick={() => openStudio("character", null)}
              />
            </div>
            {isProcedural && (characters || []).length === 0 && (
              <p className="wiz-hint">
                Персонажей пока нет — создайте в студии или импортируйте .gmchar, чтобы начать
                процедурную кампанию.
              </p>
            )}
            {!characterOk && (characters || []).length > 0 && (
              <p className="wiz-hint">Выберите персонажа, чтобы начать.</p>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}
