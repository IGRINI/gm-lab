import Icon from "./Icon.jsx";

export default function SearchField({
  value = "",
  onChange,
  placeholder = "Поиск",
  ariaLabel = "Поиск",
  compact = false,
  loading = false,
  inputRef,
  autoFocus = false,
  onKeyDown,
}) {
  const clear = () => {
    onChange?.("");
    inputRef?.current?.focus({ preventScroll: true });
  };

  return (
    <div className={`search-field${compact ? " search-field--compact" : ""}${loading ? " is-loading" : ""}`}>
      <Icon name="search" size={compact ? 14 : 16} className="search-field-icon" />
      <input
        ref={inputRef}
        type="search"
        value={value}
        maxLength={160}
        autoComplete="off"
        autoCorrect="off"
        spellCheck={false}
        autoFocus={autoFocus}
        placeholder={placeholder}
        aria-label={ariaLabel}
        aria-busy={loading || undefined}
        onChange={(event) => onChange?.(event.target.value)}
        onKeyDown={onKeyDown}
      />
      {loading && <span className="search-field-progress" aria-hidden="true" />}
      {value && (
        <button type="button" className="search-field-clear" onClick={clear} aria-label="Очистить поиск">
          <Icon name="x" size={compact ? 13 : 14} />
        </button>
      )}
    </div>
  );
}
