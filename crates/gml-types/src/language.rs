//! Language-tag helpers shared by settings, prompts and model adapters.

/// Default language for newly generated user-visible model text.
pub const DEFAULT_RESPONSE_LANGUAGE: &str = "ru";

/// Normalize the safe BCP-47 subset accepted from runtime settings.
///
/// Language tags are embedded into a model instruction, so this parser is
/// deliberately conservative: ASCII letters in the primary subtag, followed
/// by ASCII alphanumeric subtags separated by single hyphens. Lower-casing is
/// valid for BCP-47 matching and gives settings a stable serialized form.
pub fn normalize_language_tag(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 64 || !value.is_ascii() {
        return None;
    }

    let mut subtags = value.split('-');
    let primary = subtags.next()?;
    if !(2..=8).contains(&primary.len()) || !primary.bytes().all(|byte| byte.is_ascii_alphabetic())
    {
        return None;
    }
    if subtags.any(|subtag| {
        subtag.is_empty()
            || subtag.len() > 8
            || !subtag.bytes().all(|byte| byte.is_ascii_alphanumeric())
    }) {
        return None;
    }

    Some(value.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_common_language_tags() {
        assert_eq!(normalize_language_tag(" ru "), Some("ru".to_string()));
        assert_eq!(
            normalize_language_tag("ZH-Hans-CN"),
            Some("zh-hans-cn".to_string())
        );
        assert_eq!(normalize_language_tag("pt-BR"), Some("pt-br".to_string()));
    }

    #[test]
    fn rejects_values_that_are_not_safe_language_tags() {
        for value in [
            "",
            "r",
            "русский",
            "en_US",
            "en--us",
            "en\nignore-system",
            "en\"><system>",
        ] {
            assert_eq!(normalize_language_tag(value), None, "{value:?}");
        }
    }
}
