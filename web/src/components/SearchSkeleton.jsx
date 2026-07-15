export default function SearchSkeleton({ variant = "rows", count = 5 }) {
  const safeCount = Math.max(1, Math.min(12, Number(count) || 1));
  return (
    <div className={`search-skeleton search-skeleton--${variant}`} role="status" aria-label="Идёт поиск">
      {Array.from({ length: safeCount }, (_, index) => (
        <div className="search-skeleton-item" key={index} aria-hidden="true">
          <span className="search-skeleton-line search-skeleton-line--title" />
          <span className="search-skeleton-line search-skeleton-line--body" />
          {variant === "cards" && <span className="search-skeleton-line search-skeleton-line--short" />}
        </div>
      ))}
      <span className="visually-hidden">Идёт поиск…</span>
    </div>
  );
}
