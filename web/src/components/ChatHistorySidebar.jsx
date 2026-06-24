import { useEffect, useMemo, useRef, useState } from "react";
import Tooltip, { TipContent } from "./Tooltip.jsx";

const DATE_FORMATTER = new Intl.DateTimeFormat("ru-RU", {
  day: "2-digit",
  month: "short",
  hour: "2-digit",
  minute: "2-digit",
});

function chatTitle(chat) {
  return chat?.title?.trim() || "Новый чат";
}

function chatPreview(chat) {
  return chat?.preview?.trim() || "Пустой чат";
}

function chatDate(chat) {
  const raw = chat?.updated_at || chat?.created_at;
  if (!raw) return "";
  const date = new Date(raw);
  if (Number.isNaN(date.getTime())) return "";
  return DATE_FORMATTER.format(date).replace(".", "");
}

function turnCount(chat) {
  const count = Number(chat?.turn_count || 0);
  return `${count} ходов`;
}

function sameChatId(a, b) {
  return a != null && b != null && String(a) === String(b);
}

function storyDescription(story) {
  return story?.story_brief?.trim?.() || story?.description?.trim?.() || "";
}

const WORLD_PRESETS = [
  {
    id: "machine",
    label: "Машинный постапокалипсис",
    description: "Руины, автономные узлы, вода, энергия, дроны и выжившие общины.",
    values: {
      title: "Пепельный Узел",
      genre: "postapocalyptic machine world",
      tone: "bleak",
      scale: "outpost",
      storyBrief: "Ты приходишь к форпосту у старого машинного узла, где вода, энергия и доступ к закрытым дверям важнее любых монет.",
      publicIntro: "Выжившие спорят за право пользоваться старым узлом. Машины вокруг не молчат окончательно: одни служат, другие охраняют забытые протоколы.",
    },
  },
  {
    id: "isekai",
    label: "Фентезийный иссекай",
    description: "Клятвы, духи мест, цена магии, призванные чужаки и местные долги.",
    values: {
      title: "Порог Второго Неба",
      genre: "fantasy isekai",
      tone: "tense hopeful",
      scale: "village",
      storyBrief: "Ты оказываешься в чужом мире у деревни, где имя, клятва и долг перед духами значат больше, чем сила оружия.",
      publicIntro: "Местные ждут знака от святилища, боятся чужаков и шепчутся, что старый договор с духами снова нарушен.",
    },
  },
  {
    id: "frontier",
    label: "Пограничье",
    description: "Дороги, фракции, слухи, старые места и поселения с реальной функцией.",
    values: {
      title: "Край Старых Дорог",
      genre: "frontier fantasy",
      tone: "tense",
      scale: "town",
      storyBrief: "Ты прибываешь в пограничный городок, где дороги важнее стен, а каждый слух может открыть путь к чужому долгу.",
      publicIntro: "Поселение живёт торговлей, старыми договорами и страхом перед местами, которые снова начали просыпаться.",
    },
  },
];

const DEFAULT_WORLD_DRAFT = {
  title: "",
  seed: "",
  genre: "fantasy",
  tone: "tense",
  scale: "village",
  storyBrief: "",
  publicIntro: "",
};

function cleanWorldDraft(draft) {
  return {
    title: draft.title.trim(),
    seed: draft.seed.trim(),
    genre: draft.genre.trim(),
    tone: draft.tone.trim(),
    scale: draft.scale.trim(),
    storyBrief: draft.storyBrief.trim(),
    publicIntro: draft.publicIntro.trim(),
  };
}

export default function ChatHistorySidebar({
  chats,
  activeChatId,
  open,
  busy,
  loading,
  error,
  stories,
  selectedStoryId,
  storiesLoading,
  storiesError,
  onSelectStory,
  onClose,
  onCreate,
  onCreateWorld,
  onActivate,
  onDelete,
}) {
  const closeRef = useRef(null);
  const createRef = useRef(null);
  const [confirmId, setConfirmId] = useState(null);
  const [deleting, setDeleting] = useState(false);
  const [tab, setTab] = useState("chats");
  const [worldDraft, setWorldDraft] = useState(DEFAULT_WORLD_DRAFT);
  const sortedChats = useMemo(() => (Array.isArray(chats) ? chats : []), [chats]);
  const storyOptions = useMemo(() => (Array.isArray(stories) ? stories : []), [stories]);
  const selectedStory = storyOptions.find((story) => sameChatId(story.id, selectedStoryId)) || null;
  const hasStories = storyOptions.length > 0;
  const locked = busy || loading;
  const storyLocked = locked || storiesLoading || Boolean(storiesError) || !hasStories;
  const createLocked = locked || storyLocked || !selectedStoryId;
  const confirmChat = confirmId
    ? sortedChats.find((chat) => sameChatId(chat.id, confirmId)) || null
    : null;
  const worldPayload = cleanWorldDraft(worldDraft);
  const worldCreateLocked =
    locked || !worldPayload.title || !worldPayload.genre || !worldPayload.tone || !worldPayload.scale;

  const updateWorldDraft = (field, value) => {
    setWorldDraft((current) => ({ ...current, [field]: value }));
  };

  const applyPreset = (preset) => {
    setWorldDraft((current) => ({
      ...current,
      ...preset.values,
      seed: current.seed,
    }));
  };

  const submitWorld = (event) => {
    event.preventDefault();
    if (worldCreateLocked) return;
    onCreateWorld?.(worldPayload);
  };

  const cancelDelete = () => {
    if (!deleting) setConfirmId(null);
  };
  const confirmDelete = async () => {
    if (!confirmChat || deleting) return;
    setDeleting(true);
    try {
      await onDelete?.(confirmChat.id);
      setConfirmId(null);
    } finally {
      setDeleting(false);
    }
  };

  // Confirm dialog owns Escape while open (capture phase, so it beats the sidebar handler).
  useEffect(() => {
    if (!confirmId || typeof document === "undefined") return undefined;
    const onKey = (event) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopPropagation();
      if (!deleting) setConfirmId(null);
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [confirmId, deleting]);

  useEffect(() => {
    if (!open || typeof document === "undefined") return undefined;
    // Only the mobile drawer needs focus-trapping + Esc-to-close. On desktop the
    // sidebar is a docked, collapsible column: stealing focus on load or collapsing
    // it on Escape would be surprising.
    if (typeof window !== "undefined" && !window.matchMedia("(max-width: 700px)").matches) {
      return undefined;
    }

    const previousFocus = document.activeElement;
    const onKeyDown = (event) => {
      if (event.key !== "Escape") return;
      if (confirmId) return; // the confirm dialog handles Escape first
      event.preventDefault();
      onClose();
    };

    document.addEventListener("keydown", onKeyDown);
    const raf = window.requestAnimationFrame(() => {
      const target = closeRef.current || createRef.current;
      target?.focus({ preventScroll: true });
    });

    return () => {
      document.removeEventListener("keydown", onKeyDown);
      window.cancelAnimationFrame(raf);
      if (previousFocus && typeof previousFocus.focus === "function") {
        previousFocus.focus({ preventScroll: true });
      }
    };
  }, [open, onClose]);

  return (
    <>
      <button
        type="button"
        className={"chat-sidebar-backdrop" + (open ? " is-open" : "")}
        onMouseDown={onClose}
        onClick={onClose}
        tabIndex={open ? 0 : -1}
        aria-label="Закрыть список чатов"
        aria-hidden={!open}
      />
      <aside
        id="chat-history-sidebar"
        className={"chat-sidebar" + (open ? " is-open" : "")}
        aria-label="Список чатов"
      >
        <div className="chat-sidebar-head">
          <div>
            <span>{tab === "world" ? "Менеджер" : "История"}</span>
            <h2>{tab === "world" ? "Мир и история" : "Чаты"}</h2>
          </div>
          <button
            ref={closeRef}
            type="button"
            className="icon-btn chat-sidebar-close"
            onClick={onClose}
            aria-label="Закрыть список чатов"
          >
            x
          </button>
        </div>

        <div className="chat-sidebar-tabs" role="tablist" aria-label="Режим панели чатов">
          <button
            type="button"
            className={"chat-sidebar-tab" + (tab === "chats" ? " active" : "")}
            onClick={() => setTab("chats")}
            role="tab"
            aria-selected={tab === "chats"}
          >
            Чаты
          </button>
          <button
            type="button"
            className={"chat-sidebar-tab" + (tab === "world" ? " active" : "")}
            onClick={() => setTab("world")}
            role="tab"
            aria-selected={tab === "world"}
          >
            Создать мир
          </button>
        </div>

        {tab === "chats" ? (
          <div className="chat-sidebar-actions">
            <div className="story-picker">
              <label htmlFor="new-chat-story">История</label>
              {hasStories && (
                <select
                  id="new-chat-story"
                  value={selectedStoryId || ""}
                  onChange={(event) => onSelectStory(event.target.value)}
                  disabled={storyLocked}
                >
                  {storyOptions.map((story) => (
                    <option key={story.id} value={story.id}>
                      {story.title}
                    </option>
                  ))}
                </select>
              )}
              {selectedStory && storyDescription(selectedStory) && (
                <Tooltip
                  as="p"
                  tipClassName="ui-tip-wrap"
                  content={
                    <TipContent
                      title={selectedStory.title || "История"}
                      subtitle="Короткое описание стартовой истории."
                      note={storyDescription(selectedStory)}
                    />
                  }
                >
                  {storyDescription(selectedStory)}
                </Tooltip>
              )}
              {storiesLoading && <span className="chat-sidebar-status">Загружаю истории...</span>}
              {storiesError && <span className="chat-sidebar-error inline">{storiesError}</span>}
              {!storiesLoading && !storiesError && !hasStories && (
                <span className="chat-sidebar-empty inline">Нет доступных историй.</span>
              )}
            </div>
            <button
              ref={createRef}
              type="button"
              className="btn primary chat-new"
              onClick={onCreate}
              disabled={createLocked}
            >
              + Новый чат
            </button>
            {loading && <span className="chat-sidebar-status">Обновляю...</span>}
          </div>
        ) : (
          <form className="world-manager" onSubmit={submitWorld}>
            <div className="world-manager-presets" aria-label="Быстрые пресеты мира">
              {WORLD_PRESETS.map((preset) => (
                <button
                  key={preset.id}
                  type="button"
                  className="world-preset"
                  onClick={() => applyPreset(preset)}
                  disabled={locked}
                >
                  <b>{preset.label}</b>
                  <span>{preset.description}</span>
                </button>
              ))}
            </div>

            <label className="world-field">
              <span>Название истории</span>
              <input
                value={worldDraft.title}
                onChange={(event) => updateWorldDraft("title", event.target.value)}
                placeholder="Например: Пепельный Узел"
                disabled={locked}
              />
            </label>

            <div className="world-field-grid">
              <label className="world-field">
                <span>Жанр</span>
                <input
                  value={worldDraft.genre}
                  onChange={(event) => updateWorldDraft("genre", event.target.value)}
                  placeholder="fantasy isekai"
                  disabled={locked}
                />
              </label>
              <label className="world-field">
                <span>Тон</span>
                <input
                  value={worldDraft.tone}
                  onChange={(event) => updateWorldDraft("tone", event.target.value)}
                  placeholder="tense"
                  disabled={locked}
                />
              </label>
            </div>

            <div className="world-field-grid">
              <label className="world-field">
                <span>Масштаб</span>
                <select
                  value={worldDraft.scale}
                  onChange={(event) => updateWorldDraft("scale", event.target.value)}
                  disabled={locked}
                >
                  <option value="village">Деревня</option>
                  <option value="town">Городок</option>
                  <option value="city">Город</option>
                  <option value="outpost">Форпост</option>
                  <option value="region">Регион</option>
                </select>
              </label>
              <label className="world-field">
                <span>Seed</span>
                <input
                  value={worldDraft.seed}
                  onChange={(event) => updateWorldDraft("seed", event.target.value)}
                  placeholder="пусто = случайный"
                  disabled={locked}
                />
              </label>
            </div>

            <label className="world-field">
              <span>Бриф для игрока</span>
              <textarea
                value={worldDraft.storyBrief}
                onChange={(event) => updateWorldDraft("storyBrief", event.target.value)}
                placeholder="Коротко: где игрок, что произошло и почему ему есть дело."
                rows={4}
                disabled={locked}
              />
            </label>

            <label className="world-field">
              <span>Публичная завязка мира</span>
              <textarea
                value={worldDraft.publicIntro}
                onChange={(event) => updateWorldDraft("publicIntro", event.target.value)}
                placeholder="Что известно о мире без скрытых секретов GM."
                rows={4}
                disabled={locked}
              />
            </label>

            <button type="submit" className="btn primary chat-new" disabled={worldCreateLocked}>
              Создать мир и чат
            </button>
            <p className="world-manager-note">
              Жанр, тон и масштаб станут рамками канона для GM и генератора локаций.
            </p>
          </form>
        )}

        {error && <div className="chat-sidebar-error">{error}</div>}

        <nav className="chat-list" aria-label="Предыдущие чаты">
          {sortedChats.length === 0 && !loading ? (
            <div className="chat-sidebar-empty">Сохраненных чатов пока нет.</div>
          ) : (
            sortedChats.map((chat) => {
              const active = chat.active || sameChatId(chat.id, activeChatId);
              return (
                <div
                  key={chat.id}
                  className={"chat-history-item" + (active ? " active" : "")}
                >
                  <button
                    type="button"
                    className={"chat-history-row" + (active ? " active" : "")}
                    onClick={() => {
                      if (!active) onActivate(chat.id);
                    }}
                    disabled={locked}
                    aria-current={active ? "page" : undefined}
                  >
                    <span className="chat-row-head">
                      <span className="chat-row-title">{chatTitle(chat)}</span>
                      <span className="chat-row-date">{chatDate(chat)}</span>
                    </span>
                    <span className="chat-row-preview">{chatPreview(chat)}</span>
                    <span className="chat-row-meta">
                      <span>{turnCount(chat)}</span>
                      {active && <span>активный</span>}
                    </span>
                  </button>
                  <Tooltip
                    className="tooltip-wrap"
                    tipClassName="ui-tip-wrap"
                    focusable={false}
                    content={
                      <TipContent
                        title="Удалить чат"
                        subtitle={chatTitle(chat)}
                        note="Перед удалением появится подтверждение."
                      />
                    }
                  >
                    <button
                      type="button"
                      className="chat-row-delete"
                      aria-label={`Удалить чат: ${chatTitle(chat)}`}
                      onClick={() => setConfirmId(chat.id)}
                      disabled={locked}
                    >
                      <span aria-hidden="true">🗑</span>
                    </button>
                  </Tooltip>
                </div>
              );
            })
          )}
        </nav>
      </aside>

      {confirmChat && (
        <div
          className="confirm-backdrop"
          role="presentation"
          onMouseDown={cancelDelete}
        >
          <div
            className="confirm-card"
            role="alertdialog"
            aria-modal="true"
            aria-labelledby="confirm-delete-title"
            aria-describedby="confirm-delete-note"
            onMouseDown={(event) => event.stopPropagation()}
          >
            <div className="confirm-icon" aria-hidden="true">🗑</div>
            <h3 id="confirm-delete-title">Удалить чат?</h3>
            <p className="confirm-name">«{chatTitle(confirmChat)}»</p>
            <p id="confirm-delete-note" className="confirm-note">
              Чат и все его данные удалятся из базы безвозвратно — история, персонажи,
              мир и связанные эмбеддинги. Это действие нельзя отменить.
            </p>
            <div className="confirm-actions">
              <button type="button" className="btn" onClick={cancelDelete} disabled={deleting}>
                Отмена
              </button>
              <button
                type="button"
                className="btn confirm-danger"
                onClick={confirmDelete}
                disabled={deleting}
                autoFocus
              >
                {deleting ? "Удаляю…" : "Удалить"}
              </button>
            </div>
          </div>
        </div>
      )}
    </>
  );
}
