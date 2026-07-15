// Единый набор SVG-иконок (24×24, stroke 1.8, скруглённые концы) — замена
// эмодзи-иконкам, чтобы весь интерфейс рисовался одним пером на любой ОС.
// <Icon name="mic" size={16} /> наследует currentColor.

const P = {
  // навигация / шелл
  "panel-left": <><rect x="3" y="4.5" width="18" height="15" rx="2" /><path d="M9.5 4.5v15" /></>,
  menu: <path d="M4 7h16M4 12h16M4 17h16" />,
  sliders: <><path d="M4 8h4.6M12.4 8H20M4 16h8.6M16.4 16H20" /><circle cx="10.5" cy="8" r="1.9" /><circle cx="14.5" cy="16" r="1.9" /></>,
  x: <path d="M6.2 6.2l11.6 11.6M17.8 6.2L6.2 17.8" />,
  check: <path d="M5 13l4.5 4.5L19 7" />,
  plus: <path d="M12 5.5v13M5.5 12h13" />,
  minus: <path d="M5.5 12h13" />,
  "chevron-down": <path d="M6.5 9.5l5.5 5.5 5.5-5.5" />,
  "chevron-up": <path d="M6.5 14.5L12 9l5.5 5.5" />,
  "chevron-right": <path d="M9.5 6.5L15 12l-5.5 5.5" />,
  "chevron-left": <path d="M14.5 6.5L9 12l5.5 5.5" />,
  "arrow-up": <path d="M12 19V5.5M5.8 11.2L12 5l6.2 6.2" />,
  "arrow-down": <path d="M12 5v13.5M5.8 12.8L12 19l6.2-6.2" />,
  "arrow-left": <path d="M19 12H5.5M11.2 5.8L5 12l6.2 6.2" />,
  dots: <><circle cx="4.5" cy="12" r="2.3" fill="currentColor" stroke="none" /><circle cx="12" cy="12" r="2.3" fill="currentColor" stroke="none" /><circle cx="19.5" cy="12" r="2.3" fill="currentColor" stroke="none" /></>,

  // действия
  trash: <><path d="M4.5 7h15" /><path d="M9.5 7V5.5A1.5 1.5 0 0 1 11 4h2a1.5 1.5 0 0 1 1.5 1.5V7" /><path d="M6.5 7l.8 12a2 2 0 0 0 2 1.9h5.4a2 2 0 0 0 2-1.9l.8-12" /><path d="M10 11v5.5M14 11v5.5" /></>,
  pen: <path d="M17 3.5l3.5 3.5L8 19.5H4.5V16L17 3.5Z" />,
  refresh: <><path d="M19.5 12a7.5 7.5 0 1 1-2.2-5.3" /><path d="M19.8 3.8v4h-4" /></>,
  download: <><path d="M12 4v10.5M7.2 10.7L12 15.5l4.8-4.8" /><path d="M4.5 19.5h15" /></>,
  upload: <><path d="M12 15V4.5M7.2 8.8L12 4l4.8 4.8" /><path d="M4.5 19.5h15" /></>,
  folder: <path d="M3.5 7.5a2 2 0 0 1 2-2h3.6l2 2.2h7.4a2 2 0 0 1 2 2v8.3a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2V7.5Z" />,
  search: <><circle cx="11" cy="11" r="6.5" /><path d="M15.8 15.8L21 21" /></>,

  // медиа / голос
  mic: <><rect x="9" y="3" width="6" height="12" rx="3" /><path d="M5.5 11.5a6.5 6.5 0 0 0 13 0" /><path d="M12 18v3" /></>,
  square: <rect x="7" y="7" width="10" height="10" rx="1.5" />,
  play: <path d="M8 5.5l10.5 6.5L8 18.5V5.5Z" />,
  pause: <path d="M9 5.5v13M15 5.5v13" />,
  volume: <><path d="M4.5 9.8v4.4h3.2L13 18.5v-13L7.7 9.8H4.5Z" /><path d="M16.2 8.7a4.8 4.8 0 0 1 0 6.6" /></>,

  // статусы
  info: <><circle cx="12" cy="12" r="8.6" /><path d="M12 8v.01M12 11.5V16" /></>,
  alert: <><path d="M12 3.5L22 20H2L12 3.5Z" /><path d="M12 10v4M12 16.8v.01" /></>,
  help: <><circle cx="12" cy="12" r="8.6" /><path d="M9.4 9.3a2.6 2.6 0 1 1 3.7 2.4c-.8.4-1.1.9-1.1 1.8" /><path d="M12 16.6v.01" /></>,
  clock: <><circle cx="12" cy="12" r="8.6" /><path d="M12 7.5V12l3 2" /></>,

  // сущности мира
  d20: <><path d="M12 2.5l8.5 5v9l-8.5 5-8.5-5v-9l8.5-5Z" /><path d="M12 6.8L16.6 14H7.4L12 6.8Z" /><path d="M12 2.5v4.3M3.5 7.5L7.4 14M20.5 7.5L16.6 14M12 21.5l-4.6-7.5M12 21.5l4.6-7.5" /></>,
  globe: <><circle cx="12" cy="12" r="8.6" /><path d="M3.4 12h17.2" /><path d="M12 3.4c2.3 2.4 3.6 5.4 3.6 8.6s-1.3 6.2-3.6 8.6c-2.3-2.4-3.6-5.4-3.6-8.6s1.3-6.2 3.6-8.6Z" /></>,
  book: <><path d="M12 6.6C10.4 5.1 8 4.5 4.5 4.5V19c3.5 0 5.9.6 7.5 2 1.6-1.4 4-2 7.5-2V4.5c-3.5 0-5.9.6-7.5 2.1Z" /><path d="M12 6.6V21" /></>,
  user: <><circle cx="12" cy="8" r="3.8" /><path d="M4.5 20.5c0-3.5 3.4-5.8 7.5-5.8s7.5 2.3 7.5 5.8" /></>,
  users: <><circle cx="9" cy="8.5" r="3.4" /><path d="M2.8 19.5c0-3 2.8-5 6.2-5s6.2 2 6.2 5" /><path d="M16 5.6a3.4 3.4 0 0 1 0 5.9M17.4 14.9c2.3.6 3.8 2.3 3.8 4.6" /></>,
  sparkles: <><path d="M12 3.5l1.7 4.8 4.8 1.7-4.8 1.7L12 16.5l-1.7-4.8-4.8-1.7 4.8-1.7L12 3.5Z" /><path d="M19 15.5l.9 2.6 2.6.9-2.6.9-.9 2.6-.9-2.6-2.6-.9 2.6-.9.9-2.6Z" /></>,
  map: <><path d="M9 4.5L3.5 6.7v12.8L9 17.3l6 2.2 5.5-2.2V4.5L15 6.7 9 4.5Z" /><path d="M9 4.5v12.8M15 6.7v12.8" /></>,
  pin: <><path d="M12 21.5S5.5 15.8 5.5 11a6.5 6.5 0 1 1 13 0c0 4.8-6.5 10.5-6.5 10.5Z" /><circle cx="12" cy="11" r="2.4" /></>,
  target: <><circle cx="12" cy="12" r="8.6" /><circle cx="12" cy="12" r="4.6" /><circle cx="12" cy="12" r="1" fill="currentColor" stroke="none" /></>,
  shield: <path d="M12 3l7 2.6v5.1c0 4.6-2.9 7.9-7 9.8-4.1-1.9-7-5.2-7-9.8V5.6L12 3Z" />,
  walk: <><circle cx="13" cy="4.5" r="1.8" /><path d="M12.5 8l-3 4 2.5 2.5V20M9.5 12l-2.5 6M12.5 8l3.5 2.5 2.5 1M12 14.5l3 2 1 3.5" /></>,
  image: <><rect x="3.5" y="5" width="17" height="14" rx="2" /><circle cx="8.7" cy="10" r="1.6" /><path d="M20.5 14.8l-4.3-4.3L6.5 19" /></>,
  message: <path d="M4 6.5a2 2 0 0 1 2-2h12a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H9.5L4.5 21V6.5Z" />,
  scroll: <><path d="M6.5 3.5h11a2 2 0 0 1 2 2v13a2 2 0 0 1-2 2h-11a2 2 0 0 1-2-2v-13a2 2 0 0 1 2-2Z" /><path d="M8.5 8h7M8.5 12h7M8.5 16h4.5" /></>,
  swap: <><path d="M4 8.5h13.5M14 4.5l4 4-4 4" /><path d="M20 15.5H6.5M10 19.5l-4-4 4-4" /></>,
};

export default function Icon({ name, size = 16, className = "", strokeWidth = 1.8 }) {
  const body = P[name];
  if (!body) return null;
  return (
    <svg
      className={"ico" + (className ? " " + className : "")}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {body}
    </svg>
  );
}
