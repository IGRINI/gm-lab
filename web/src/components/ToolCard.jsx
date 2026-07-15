import Icon from "./Icon.jsx";
import { useContext } from "react";
import MarkdownText, { MarkdownInline } from "./MarkdownText.jsx";
import Spoiler from "./Spoiler.jsx";
import Tooltip from "./Tooltip.jsx";
import { ToolResultBody } from "./ToolResultCard.jsx";
import { DiceBody, gradeAccent } from "./DiceRoll.jsx";
import { NpcRosterContext } from "../npcContext.js";
import { StatusLabelsContext } from "../statusContext.js";

// Per-tool accent: references the centralized CSS palette tokens (styles.css :root)
// so the cards never carry raw hex that can drift from the theme.
const ACCENT = {
  ask_npc: "var(--player)",
  ask_npc_redo: "var(--md-del)",
  move_npc: "var(--brand-text)",
  set_npc_presence: "var(--brand-text)",
  set_npc_whereabouts: "var(--md-em)",
  set_scene: "var(--gm)",
  roll_dice: "var(--md-strong)",
  get_world_fact: "var(--md-link)",
  ask_player: "var(--player)",
  draft_world_bible: "var(--gm)",
  edit_world_bible: "var(--md-em)",
  query_world_state: "var(--md-link)",
  update_world_state: "var(--entity-note)",
  update_player_character: "var(--player)",
  advance_time: "var(--md-em)",
  get_npc_profile: "var(--brand-text)",
  tool_search: "var(--text-3)",
  _: "var(--entity-unknown)",
};

// World-state record namespaces (update_world_state / query_world_state items).
// Russian labels + tone keyed by the backend type enum — presentation only.
const WS_TYPE = {
  fact: { label: "факт", tone: "ok" },
  rumor: { label: "слух", tone: "warn" },
  npc_memory: { label: "память NPC", tone: "" },
  relationship: { label: "отношение", tone: "" },
  goal: { label: "цель", tone: "" },
  public_lookup: { label: "публичный факт", tone: "ok" },
};
const WS_OP = {
  add: { label: "добавить", tone: "ok" },
  update: { label: "изменить", tone: "warn" },
  delete: { label: "удалить", tone: "redo" },
};
const WS_SCOPE = {
  public: "публично",
  gm: "ГМ",
  npc: "только NPC",
  shared: "общее",
  player: "игрок",
};
const PROFILE_PRESET = {
  visible: "видимое",
  social: "социальное",
  mechanics: "механика",
  status: "состояние",
  identity: "личность",
};

// Status LABELS come from the backend via StatusLabelsContext (single source).
// Tone (badge style) and help (tooltip copy) are presentation-only and keyed by
// the same status enum — defined once here, not duplicated across the app.
const STATUS_TONE = { known: "ok", likely: "warn", rumored: "muted", unknown: "muted" };
const STATUS_HELP = {
  known: "Местонахождение установлено как рабочий факт текущей истории.",
  likely: "Это вероятное местонахождение, но не железное подтверждение.",
  rumored: "Это слух или непроверенная зацепка.",
  unknown: "Точное местонахождение не установлено.",
  present: "Персонаж находится в текущей сцене.",
};
const TOOL_HELP = {
  ask_npc: "ГМ спрашивает отдельного персонажа, что тот говорит или делает. Это нужно, чтобы ГМ не придумывал личную реакцию персонажа сам.",
  move_npc: "Обновляет присутствие персонажа в текущей сцене: вошёл, вышел, виден, слышит или нет.",
  set_npc_presence: "Обновляет присутствие персонажа в текущей сцене: вошёл, вышел, виден, слышит или нет.",
  set_npc_whereabouts: "Запоминает, где искать отсутствующего персонажа. Это не добавляет его в текущую сцену.",
  set_scene: "Заменяет текущую сцену, когда персонаж игрока реально пришёл в новое место.",
  roll_dice: "Бросок по D&D 5e для действия с неопределённым исходом.",
  get_world_fact: "Проверка памяти мира: факты, слухи, показания и уже установленные сведения.",
  ask_player: "ГМ предлагает игроку быстрые варианты действий. Кнопки появляются над полем ввода; свободный ввод по-прежнему доступен.",
  query_world_state: "Поиск по памяти мира в нужной области видимости (игрок / NPC / ГМ) перед записью или решением.",
  update_world_state: "Запись долговременной памяти мира: факты, слухи, память NPC, отношения и цели.",
  update_player_character: "Обновление листа персонажа игрока: HP, AC, навыки, инвентарь, состояние.",
  advance_time: "Сдвигает скрытые часы мира на прошедшие минуты этого хода.",
  get_npc_profile: "Запрос отдельных безопасных полей карточки NPC для броска, описания или социальной оценки.",
  tool_search: "ГМ догружает скрытый инструмент по ключевым словам.",
  draft_world_bible: "Архитектор мира создаёт или обновляет структурированную библию мира (черновик): жанр, тон, размер, население, публичная предпосылка и разделы лора.",
  edit_world_bible: "Архитектор точечно правит библию мира: меняет отдельные поля или добавляет/убирает/заменяет записи в разделах, не переписывая весь черновик.",
};

// Labels for edit_world_bible patch ops.
const OP_LABEL = { add: "Добавлено", remove: "Убрано", replace: "Заменено" };

// Tooltip body for a bible-section chip: header + the actual entries.
function bibleTip(label, items) {
  return (
    <div className="tc-bible-tip">
      <span className="tc-bible-tip-h">{label} · {items.length}</span>
      <ul>
        {items.map((item, i) => (
          <li key={i}>{item}</li>
        ))}
      </ul>
    </div>
  );
}

// Lore sections shown as count chips in the draft_world_bible card (dev view).
const BIBLE_SECTIONS = [
  ["dogmas", "догматы"],
  ["world_laws", "законы мира"],
  ["inhabitants", "народы"],
  ["creatures", "существа"],
  ["regions", "регионы"],
  ["power_centers", "власть"],
  ["religions", "вера"],
  ["gods", "боги"],
  ["cultures", "культуры"],
  ["history", "история"],
  ["economy", "экономика"],
  ["daily_life", "быт"],
  ["story_hooks", "зацепки"],
  ["hidden_secrets", "секреты"],
  ["location_rules", "правила локаций"],
  ["prohibited_elements", "запреты"],
];
const BIBLE_VISUAL_PROMPTS = [
  ["world_image_prompt_en", "Prompt изображения мира (EN)"],
  ["world_map_prompt_en", "Prompt карты мира (EN)"],
];
const BIBLE_SET_LABELS = Object.fromEntries([
  ["title", "Название"],
  ["genre", "Жанр"],
  ["tone", "Тон"],
  ["world_size", "Размер мира"],
  ["population", "Население"],
  ["public_premise", "Публичная предпосылка"],
  ["hidden_premise", "Скрытая предпосылка (GM)"],
  ...BIBLE_VISUAL_PROMPTS,
]);
const FIELD_HELP = {
  "Ситуация": "Коротко и нейтрально описывает, что персонаж видит/слышит и на что должен отреагировать.",
  "Правка ГМ": "Почему предыдущий ответ персонажа был отправлен на переделку.",
  "Где": "Позиция или место персонажа относительно текущей сцены.",
  "Занятие": "Что персонаж сейчас делает видимо для игрока.",
  "Настрой": "Видимый настрой/отношение персонажа в этой сцене.",
  "Почему": "Причина, по которой ГМ меняет состояние мира или вызывает инструмент.",
  "Источник": "Откуда взялась информация о местонахождении.",
  "Детали": "Дополнительная видимая или известная информация.",
  "В сцене": "Именованные персонажи, которые присутствуют в новой сцене.",
  "Выходы": "Видимые пути, куда игрок может пойти из текущей сцены.",
  "Предметы": "Видимые объекты сцены, с которыми потенциально можно взаимодействовать.",
  "Ограничения": "Физические или ситуационные рамки сцены.",
  "Напряжение": "Текущий тон давления в сцене.",
  "Зачем": "Какой неопределённый исход проверяет бросок.",
};

function toolHelp(name) {
  return TOOL_HELP[name] || "Служебный инструмент ГМ. Подробности видны в сыром JSON ниже.";
}

export function useNpcResolver() {
  const roster = useContext(NpcRosterContext);
  return (id) => {
    const n = (roster || []).find((x) => x.id === id);
    const name = n?.name || id || "персонаж";
    return { name, c: n?.color || "var(--entity-unknown)", role: n?.role || "", pronouns: n?.pronouns || "", id };
  };
}

export function NpcRef({ id }) {
  const resolve = useNpcResolver();
  const { name, c, role, pronouns } = resolve(id);
  return (
    <Tooltip
      className="tc-npc"
      tipClassName="tool-tip"
      content={[
        name,
        role ? `роль: ${role}` : "",
        pronouns ? `род: ${pronouns}` : "",
        id ? `id: ${id}` : "",
      ].filter(Boolean).join("\n")}
    >
      <span className="dot" style={{ "--c": c }} />
      <span style={{ color: c }}>{name}</span>
    </Tooltip>
  );
}

export function Field({ label, tip, children }) {
  const help = tip || FIELD_HELP[label] || "";
  return (
    <div className="tc-field">
      {help ? (
        <Tooltip className="tc-flabel has-tip" tipClassName="tool-tip" content={help}>
          {label}
        </Tooltip>
      ) : (
        <span className="tc-flabel">{label}</span>
      )}
      <div className="tc-fval">{children}</div>
    </div>
  );
}

export function Badge({ tone, tip, children }) {
  const hasTip = nonEmpty(tip);
  const className = "tc-badge" + (tone ? " " + tone : "") + (hasTip ? " has-tip" : "");
  if (!hasTip) return <span className={className}>{children}</span>;
  return (
    <Tooltip className={className} tipClassName="tool-tip" content={tip}>
      {children}
    </Tooltip>
  );
}

export function ActorRef({ id }) {
  if (id === "player") return <Badge tone="muted">игрок</Badge>;
  return <NpcRef id={id} />;
}

export function ParticipantChips({ ids }) {
  const list = Array.isArray(ids) ? ids.filter(nonEmpty) : [];
  if (!list.length) return null;
  return (
    <>
      {list.map((id) => (
        <span className="tc-arrow-to" key={id}>+ <ActorRef id={id} /></span>
      ))}
    </>
  );
}

// A bordered text block for the "free-text" arguments (situation, reason, …).
export function TextBlock({ tone, children }) {
  return (
    <div className={"tc-text" + (tone ? " " + tone : "")}>
      <MarkdownText>{children}</MarkdownText>
    </div>
  );
}

export function nonEmpty(v) {
  return v != null && String(v).trim() !== "";
}

function diceTarget(args) {
  if (!nonEmpty(args.target_number)) return "";
  const rawKind = nonEmpty(args.target_kind) && args.target_kind !== "none" ? args.target_kind : "";
  const kind = rawKind || (args.roll_kind === "attack" ? "AC" : "DC");
  return `${kind} ${args.target_number}`;
}

// Builds { icon, accent, title, body } for one tool call. NpcRef is a component,
// so it can appear in the returned JSX without violating the rules of hooks.
function toolView(name, args, statusLabels) {
  switch (name) {
    case "ask_npc": {
      const redo = nonEmpty(args.correction);
      return {
        icon: redo ? <Icon name="refresh" size={14} /> : <Icon name="message" size={14} />,
        accent: redo ? ACCENT.ask_npc_redo : ACCENT.ask_npc,
        title: (
          <>
            {redo ? "Возврат ответа — " : "Запрос к персонажу — "}
            <NpcRef id={args.npc_id} />
          </>
        ),
        body: (
          <>
            {nonEmpty(args.situation) && (
              <Field label="Ситуация">
                <TextBlock>{args.situation}</TextBlock>
              </Field>
            )}
            {redo && (
              <Field label="Правка ГМ">
                <TextBlock tone="redo">{args.correction}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "move_npc":
    case "set_npc_presence": {
      const present = args.present;
      return {
        icon: <Icon name="walk" size={14} />,
        accent: ACCENT.move_npc,
        title: (
          <>
            Присутствие в сцене — <NpcRef id={args.npc_id} />
          </>
        ),
        body: (
          <>
            <div className="tc-chips">
              {present === true && <Badge tone="ok" tip="Персонаж добавлен в текущую сцену. Теперь он может быть видимым участником сцены.">входит в сцену</Badge>}
              {present === false && <Badge tone="muted" tip="Персонаж убран из текущей сцены. После этого он не должен отвечать без нового появления.">покидает сцену</Badge>}
              {args.visible === true && <Badge tip="Игрок и сцена могут видеть персонажа.">виден</Badge>}
              {args.visible === false && <Badge tone="muted" tip="Персонаж присутствует неявно или вне видимости игрока.">скрыт</Badge>}
              {args.can_hear === true && <Badge tip="Персонаж находится в зоне слышимости текущей сцены.">слышит</Badge>}
              {args.can_hear === false && <Badge tone="muted" tip="Персонаж не слышит текущую сцену и не должен реагировать на разговор.">не слышит</Badge>}
            </div>
            {nonEmpty(args.location) && <Field label="Где"><MarkdownInline>{args.location}</MarkdownInline></Field>}
            {nonEmpty(args.activity) && <Field label="Занятие"><MarkdownInline>{args.activity}</MarkdownInline></Field>}
            {nonEmpty(args.attitude) && <Field label="Настрой"><MarkdownInline>{args.attitude}</MarkdownInline></Field>}
            {nonEmpty(args.reason) && (
              <Field label="Почему">
                <TextBlock>{args.reason}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "set_npc_whereabouts": {
      const place = args.location_name || args.location_id;
      return {
        icon: <Icon name="pin" size={14} />,
        accent: ACCENT.set_npc_whereabouts,
        title: (
          <>
            Местонахождение — <NpcRef id={args.npc_id} />
          </>
        ),
        body: (
          <>
            <div className="tc-chips">
              <Badge tone={STATUS_TONE[args.status] || "muted"} tip={STATUS_HELP[args.status] || "Статус уверенности по местонахождению персонажа."}>
                {statusLabels[args.status] || args.status || "неизвестно"}
              </Badge>
              {nonEmpty(place) && <Badge tip="Место, где персонажа можно искать по текущим сведениям.">{place}</Badge>}
            </div>
            {nonEmpty(args.source) && <Field label="Источник"><MarkdownInline>{args.source}</MarkdownInline></Field>}
            {nonEmpty(args.details) && (
              <Field label="Детали">
                <TextBlock>{args.details}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "set_scene": {
      const npcs = args.present_npcs || [];
      const exits = args.exits || [];
      const items = args.items || [];
      const constraints = args.constraints || [];
      return {
        icon: <Icon name="map" size={14} />,
        accent: ACCENT.set_scene,
        title: "Смена сцены",
        body: (
          <>
            {nonEmpty(args.title) && (
              <Tooltip className="tc-scene-title" tipClassName="tool-tip" content="Название новой текущей сцены.">
                {args.title}
              </Tooltip>
            )}
            {nonEmpty(args.description) && <TextBlock>{args.description}</TextBlock>}
            {npcs.length > 0 && (
              <Field label="В сцене">
                <div className="tc-chips">
                  {npcs.map((id) => <NpcRef key={id} id={id} />)}
                </div>
              </Field>
            )}
            {exits.length > 0 && (
              <Field label="Выходы">
                <div className="tc-list">
                  {exits.map((e, i) => (
                    <Tooltip
                      as="div"
                      className="tc-exit"
                      tipClassName="tool-tip"
                      content={[
                        e.id ? `id: ${e.id}` : "",
                        e.destination ? `куда ведёт: ${e.destination}` : "",
                        e.visible === false ? "сейчас не виден" : "видимый выход",
                        e.blocked_by ? `заблокирован: ${e.blocked_by}` : "",
                      ].filter(Boolean).join("\n")}
                      key={e.id || i}
                    >
                      <span>{e.name || e.id || "выход"}</span>
                      {nonEmpty(e.destination) && (
                        <>
                          <span className="arr">→</span>
                          <span>{e.destination}</span>
                        </>
                      )}
                      {nonEmpty(e.blocked_by) && <Badge tone="redo" tip="Почему этот выход сейчас нельзя использовать свободно.">{e.blocked_by}</Badge>}
                    </Tooltip>
                  ))}
                </div>
              </Field>
            )}
            {items.length > 0 && (
              <Field label="Предметы">
                <div className="tc-chips">
                  {items.map((it, i) => (
                    <Badge
                      key={it.id || i}
                      tip={[
                        it.id ? `id: ${it.id}` : "",
                        it.location ? `где: ${it.location}` : "",
                        it.owner ? `владелец: ${it.owner}` : "",
                        it.portable === true ? "можно взять" : it.portable === false ? "не переносится как обычный предмет" : "",
                        it.details || "",
                      ].filter(Boolean).join("\n")}
                    >
                      {it.name || it.id || "предмет"}
                    </Badge>
                  ))}
                </div>
              </Field>
            )}
            {constraints.length > 0 && (
              <Field label="Ограничения">
                <div className="tc-list">
                  {constraints.map((c, i) => (
                    <Tooltip
                      as="div"
                      className="tc-exit"
                      tipClassName="tool-tip"
                      content="Ограничение сцены: физическое, социальное или ситуационное правило, которое ГМ должен учитывать."
                      key={i}
                    >
                      · <MarkdownInline>{c}</MarkdownInline>
                    </Tooltip>
                  ))}
                </div>
              </Field>
            )}
            {nonEmpty(args.tension) && <Field label="Напряжение"><MarkdownInline>{args.tension}</MarkdownInline></Field>}
            {nonEmpty(args.reason) && (
              <Field label="Почему">
                <TextBlock>{args.reason}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "roll_dice": {
      const target = diceTarget(args);
      return {
        icon: <Icon name="d20" size={14} />,
        accent: ACCENT.roll_dice,
        title: "Бросок кубика",
        body: (
          <>
            <div className="tc-dice">
              <Tooltip className="tc-notation" tipClassName="tool-tip" content="Формула броска. Например, 1d20 или 2d20kh1 для преимущества.">
                {args.notation || "—"}
              </Tooltip>
              {nonEmpty(args.roll_kind) && (
                <Tooltip className="tc-badge" tipClassName="tool-tip" content="Тип броска, выбранный до результата.">
                  {args.roll_kind}
                </Tooltip>
              )}
              {nonEmpty(target) && (
                <Tooltip className="tc-badge warn" tipClassName="tool-tip" content="Целевое число зафиксировано до броска.">
                  {target}
                </Tooltip>
              )}
            </div>
            {nonEmpty(args.check_name) && (
              <Field label="Проверка">
                <MarkdownInline>{args.check_name}</MarkdownInline>
              </Field>
            )}
            {nonEmpty(args.reason) && (
              <Field label="Зачем">
                <TextBlock>{args.reason}</TextBlock>
              </Field>
            )}
          </>
        ),
      };
    }

    case "get_world_fact": {
      return {
        icon: <Icon name="book" size={14} />,
        accent: ACCENT.get_world_fact,
        title: "Запрос к памяти мира",
        body: (
          <Tooltip className="tc-query" tipClassName="tool-tip" content="Запрос к памяти мира. ГМ должен проверять факты, а не придумывать их из головы.">
            <MarkdownInline>{args.query || "—"}</MarkdownInline>
          </Tooltip>
        ),
      };
    }

    case "ask_player": {
      const options = Array.isArray(args.options) ? args.options : [];
      return {
        icon: <Icon name="target" size={14} />,
        accent: ACCENT.ask_player,
        title: "Варианты для игрока",
        body: (
          <>
            {nonEmpty(args.question) && (
              <Tooltip className="tc-ask-q" tipClassName="tool-tip" content="Вопрос-подсказка над кнопками быстрых ответов.">
                <MarkdownInline>{args.question}</MarkdownInline>
              </Tooltip>
            )}
            {options.length > 0 && (
              <div className="tc-options">
                {options.map((o, i) => (
                  <div className="tc-option" key={i}>
                    <span className="tc-option-label">{nonEmpty(o.label) ? o.label : `вариант ${i + 1}`}</span>
                    {nonEmpty(o.message) && (
                      <span className="tc-option-msg"><MarkdownInline>{o.message}</MarkdownInline></span>
                    )}
                  </div>
                ))}
              </div>
            )}
          </>
        ),
      };
    }

    case "update_world_state": {
      const items = Array.isArray(args.items) ? args.items : [];
      return {
        icon: <Icon name="sparkles" size={14} />,
        accent: ACCENT.update_world_state,
        title: "Запись в память мира",
        body: items.length ? (
          <div className="tc-ws-list">
            {items.map((it, i) => {
              const op = WS_OP[it.op || "add"] || WS_OP.add;
              const typ = WS_TYPE[it.type] || { label: it.type || "запись", tone: "" };
              return (
                <div className="tc-ws-item" key={i}>
                  <div className="tc-chips">
                    <Badge tone={op.tone}>{op.label}</Badge>
                    <Badge tone={typ.tone}>{typ.label}</Badge>
                    {nonEmpty(it.scope) && <Badge tone="muted">{WS_SCOPE[it.scope] || it.scope}</Badge>}
                    {nonEmpty(it.npc_id) && <NpcRef id={it.npc_id} />}
                    {nonEmpty(it.target) && (
                      <span className="tc-arrow-to">→ {it.target === "player" ? "игрок" : <NpcRef id={it.target} />}</span>
                    )}
                    <ParticipantChips ids={it.participants} />
                    {nonEmpty(it.importance) && <Badge tone="warn">{it.importance}</Badge>}
                  </div>
                  {nonEmpty(it.text) && <TextBlock>{it.text}</TextBlock>}
                  {nonEmpty(it.known_name) && <Field label="Известное имя"><MarkdownInline>{it.known_name}</MarkdownInline></Field>}
                </div>
              );
            })}
          </div>
        ) : (
          <div className="tc-text">нет записей</div>
        ),
      };
    }

    case "query_world_state": {
      return {
        icon: <Icon name="search" size={14} />,
        accent: ACCENT.query_world_state,
        title: "Поиск в памяти мира",
        body: (
          <>
            <div className="tc-chips">
              {nonEmpty(args.scope) && <Badge tone="muted" tip="Область видимости, в которой ищет ГМ.">{WS_SCOPE[args.scope] || args.scope}</Badge>}
              {nonEmpty(args.npc_id) && <NpcRef id={args.npc_id} />}
            </div>
            <Tooltip className="tc-query" tipClassName="tool-tip" content="Поисковый запрос ГМ к памяти мира.">
              <MarkdownInline>{args.query || "—"}</MarkdownInline>
            </Tooltip>
          </>
        ),
      };
    }

    case "update_player_character": {
      const fields = (args.fields && typeof args.fields === "object") ? args.fields : {};
      const keys = Object.keys(fields);
      return {
        icon: <Icon name="shield" size={14} />,
        accent: ACCENT.update_player_character,
        title: "Лист персонажа игрока",
        body: (
          <>
            {keys.length ? (
              keys.map((k) => (
                <Field key={k} label={k}>
                  {typeof fields[k] === "object"
                    ? <code>{JSON.stringify(fields[k])}</code>
                    : <MarkdownInline>{String(fields[k])}</MarkdownInline>}
                </Field>
              ))
            ) : (
              <div className="tc-text">нет изменений</div>
            )}
            {nonEmpty(args.reason) && <Field label="Почему"><TextBlock>{args.reason}</TextBlock></Field>}
          </>
        ),
      };
    }

    case "advance_time": {
      return {
        icon: <Icon name="clock" size={14} />,
        accent: ACCENT.advance_time,
        title: "Сдвиг времени",
        body: (
          <>
            <div className="tc-chips">
              <Badge tone="warn" tip="Сколько внутриигровых минут прошло за этот ход.">+{args.minutes ?? 0} мин</Badge>
            </div>
            {nonEmpty(args.reason) && <Field label="Почему"><TextBlock>{args.reason}</TextBlock></Field>}
          </>
        ),
      };
    }

    case "get_npc_profile": {
      const fields = Array.isArray(args.fields) ? args.fields : [];
      return {
        icon: <Icon name="user" size={14} />,
        accent: ACCENT.get_npc_profile,
        title: (
          <>
            Карточка персонажа — <NpcRef id={args.npc_id} />
          </>
        ),
        body: (
          <div className="tc-chips">
            <Badge tone="muted" tip="Группа полей карточки, которую запросил ГМ.">{PROFILE_PRESET[args.preset || "visible"] || args.preset}</Badge>
            {fields.map((f) => <Badge key={f}>{f}</Badge>)}
          </div>
        ),
      };
    }

    case "tool_search": {
      return {
        icon: <Icon name="sliders" size={14} />,
        accent: ACCENT.tool_search,
        title: "Поиск инструмента ГМ",
        body: (
          <Tooltip className="tc-query" tipClassName="tool-tip" content="Запрос ГМ на загрузку скрытого инструмента.">
            <MarkdownInline>{args.query || "—"}</MarkdownInline>
          </Tooltip>
        ),
      };
    }

    case "draft_world_bible": {
      const lore = args.world_lore && typeof args.world_lore === "object" ? args.world_lore : {};
      const sections = BIBLE_SECTIONS
        .map(([field, label]) => [
          label,
          Array.isArray(lore[field])
            ? lore[field].filter((item) => typeof item === "string" && item.trim())
            : [],
        ])
        .filter(([, items]) => items.length > 0);
      return {
        icon: <Icon name="scroll" size={14} />,
        accent: ACCENT.draft_world_bible,
        title: "Черновик мира",
        body: (
          <>
            {nonEmpty(args.title) && (
              <Tooltip className="tc-scene-title" tipClassName="tool-tip" content="Название мира в черновике.">
                {args.title}
              </Tooltip>
            )}
            <div className="tc-chips">
              {nonEmpty(args.genre) && <Badge tone="muted">{args.genre}</Badge>}
              {nonEmpty(args.tone) && <Badge tone="muted">{args.tone}</Badge>}
            </div>
            {nonEmpty(args.world_size) && <Field label="Размер мира"><TextBlock>{args.world_size}</TextBlock></Field>}
            {nonEmpty(args.population) && <Field label="Население"><TextBlock>{args.population}</TextBlock></Field>}
            {nonEmpty(args.public_premise) && (
              <Field label="Публичная предпосылка"><TextBlock>{args.public_premise}</TextBlock></Field>
            )}
            {nonEmpty(lore.hidden_premise) && (
              <Field label="Скрытая предпосылка (GM)"><TextBlock tone="redo">{lore.hidden_premise}</TextBlock></Field>
            )}
            {BIBLE_VISUAL_PROMPTS.map(([field, label]) =>
              nonEmpty(lore[field]) ? (
                <Field key={field} label={label}>
                  <TextBlock>{lore[field]}</TextBlock>
                </Field>
              ) : null
            )}
            {sections.length > 0 && (
              <Field label="Разделы лора">
                <div className="tc-chips">
                  {sections.map(([label, items]) => (
                    <Badge key={label} tip={bibleTip(label, items)}>
                      {label}: {items.length}
                    </Badge>
                  ))}
                </div>
              </Field>
            )}
          </>
        ),
      };
    }

    case "edit_world_bible": {
      const set = args.set && typeof args.set === "object" ? args.set : {};
      const setKeys = Object.keys(set).filter((k) => nonEmpty(set[k]));
      const ops = [
        ["add", args.add],
        ["remove", args.remove],
        ["replace", args.replace],
      ].map(([op, obj]) => [
        op,
        obj && typeof obj === "object"
          ? Object.entries(obj).filter(([, v]) => Array.isArray(v) && v.length)
          : [],
      ]);
      const empty = setKeys.length === 0 && ops.every(([, entries]) => entries.length === 0);
      return {
        icon: <Icon name="pen" size={14} />,
        accent: ACCENT.edit_world_bible,
        title: "Правка мира",
        body: (
          <>
            {setKeys.map((k) => (
              <Field key={`set-${k}`} label={BIBLE_SET_LABELS[k] || k}>
                <TextBlock>{String(set[k])}</TextBlock>
              </Field>
            ))}
            {ops.map(([op, entries]) =>
              entries.length === 0 ? null : (
                <Field key={op} label={OP_LABEL[op]}>
                  <div className="tc-chips">
                    {entries.map(([section, items]) => (
                      <Badge key={section} tip={bibleTip(section, items)}>
                        {section}: {items.length}
                      </Badge>
                    ))}
                  </div>
                </Field>
              )
            )}
            {empty && <div className="tc-text">нет изменений</div>}
          </>
        ),
      };
    }

    default: {
      const entries = Object.entries(args || {});
      return {
        icon: <Icon name="sliders" size={14} />,
        accent: ACCENT._,
        title: <>ГМ вызвал инструмент <code>{name}</code></>,
        body: entries.length ? (
          entries.map(([k, v]) => (
            <Field key={k} label={k}>
              {typeof v === "object" ? <code>{JSON.stringify(v)}</code> : <MarkdownInline>{String(v)}</MarkdownInline>}
            </Field>
          ))
        ) : (
          <div className="tc-text">нет аргументов</div>
        ),
      };
    }
  }
}

// Player-friendly minutes → "1 дн 2 ч 30 мин" (compact, no zero parts).
function prettyElapsed(minutes) {
  const m = Math.max(0, Math.round(Number(minutes) || 0));
  if (m === 0) return "меньше минуты";
  const days = Math.floor(m / 1440);
  const hours = Math.floor((m % 1440) / 60);
  const mins = m % 60;
  const parts = [];
  if (days) parts.push(days + " дн");
  if (hours) parts.push(hours + " ч");
  if (mins) parts.push(mins + " мин");
  return parts.join(" ");
}

// Compact, player-facing time advance (used when tool internals are hidden).
function PlayerTimeCard({ payload }) {
  const p = payload || {};
  const current = p.current && typeof p.current === "object" ? p.current : {};
  const now = [current.current_date_label, current.time_of_day].filter(nonEmpty).join(" · ");
  return (
    <div className="play-card time" style={{ "--tc": "var(--md-em)" }}>
      <span className="play-ico" aria-hidden="true"><Icon name="clock" size={16} /></span>
      <span className="play-main">
        <b>Прошло {prettyElapsed(p.elapsed_minutes)}</b>
        {nonEmpty(now) && <span className="play-sub">{now}</span>}
      </span>
    </div>
  );
}

// Compact, player-facing character-sheet update.
function PlayerSheetCard({ payload }) {
  const p = payload || {};
  const updated = Array.isArray(p.updated) ? p.updated : [];
  return (
    <div className="play-card sheet" style={{ "--tc": "var(--player)" }}>
      <span className="play-ico" aria-hidden="true"><Icon name="shield" size={16} /></span>
      <span className="play-main">
        <b>Лист персонажа обновлён</b>
        {updated.length > 0 && <span className="play-sub">{updated.join(", ")}</span>}
      </span>
    </div>
  );
}

// `result` is the tool's outcome payload (attached by the timeline once it arrives),
// rendered under the request inside the SAME card so call+result read as one unit.
// `mode` controls how much is shown:
//   'full'   — request + raw JSON + result (developer view)
//   'result' — header + result only (no request, no raw call JSON)
//   'player' — compact, player-facing result (dice / time / sheet)
export default function ToolCard({ name, args = {}, result, resultLive, rollId, mode = "full" }) {
  const statusLabels = useContext(StatusLabelsContext);
  const view = toolView(name, args || {}, statusLabels);
  const hasResult = result != null;
  const isDice = name === "roll_dice" && hasResult;
  const accent = isDice ? gradeAccent(result.grade) : view.accent;

  if (mode === "player") {
    if (!hasResult) return null;
    if (name === "roll_dice") {
      return (
        <div className="tool-card play-dice" style={{ "--tc": gradeAccent(result.grade) }}>
          <DiceBody roll={result} animate={resultLive} rollId={rollId} />
        </div>
      );
    }
    if (name === "advance_time") return <PlayerTimeCard payload={result} />;
    if (name === "update_player_character") return <PlayerSheetCard payload={result} />;
    return null;
  }

  // 'full'   — developer view: rich body + raw JSON spoilers + raw tool name.
  // 'detail' — player view: the same rich body, no dev-only JSON/raw-name noise.
  const showBody = mode === "full" || mode === "detail";
  const showRaw = mode === "full";
  return (
    <div className={"tool-card" + (hasResult ? " has-result" : "")} style={{ "--tc": accent }}>
      <div className="tc-hd">
        <Tooltip className="tc-ico" tipClassName="tool-tip" content={toolHelp(name)}>
          {view.icon}
        </Tooltip>
        <span className="tc-title">{view.title}</span>
        {showRaw && (
          <Tooltip className="tc-name" tipClassName="tool-tip" content={`Сырое имя инструмента модели: ${name}\n${toolHelp(name)}`}>
            {name}
          </Tooltip>
        )}
      </div>
      {showBody && <div className="tc-body">{view.body}</div>}
      {showRaw && (
        <Spoiler label="сырой вызов (JSON)">
          <MarkdownText>{"```json\n" + JSON.stringify(args, null, 2) + "\n```"}</MarkdownText>
        </Spoiler>
      )}
      {hasResult && (
        <div className="tc-result-sec">
          <div className="tc-result-divider">результат</div>
          {isDice ? (
            <DiceBody roll={result} animate={resultLive} rollId={rollId} />
          ) : (
            <>
              <div className="tc-body"><ToolResultBody name={name} payload={result} /></div>
              {showRaw && (
                <Spoiler label="сырой результат (JSON)">
                  <MarkdownText>{"```json\n" + JSON.stringify(result, null, 2) + "\n```"}</MarkdownText>
                </Spoiler>
              )}
            </>
          )}
        </div>
      )}
    </div>
  );
}
