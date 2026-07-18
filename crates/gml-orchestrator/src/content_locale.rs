use gml_types::ContentLocale;

/// Select deterministic game text using the locale persisted in world canon.
///
/// This is intentionally independent from the interface locale and the current
/// global response-language setting: an existing campaign keeps the language it
/// was created with after a restart or settings change.
pub(crate) const fn text<'a>(locale: ContentLocale, russian: &'a str, english: &'a str) -> &'a str {
    match locale {
        ContentLocale::Russian => russian,
        ContentLocale::English => english,
    }
}
