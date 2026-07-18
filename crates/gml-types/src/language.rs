//! Language-tag helpers shared by settings, prompts and model adapters.

use serde::{Deserialize, Serialize};

/// Default language for newly generated user-visible model text.
pub const DEFAULT_RESPONSE_LANGUAGE: &str = "en";

/// Static content bundles currently shipped with the application.
///
/// `Default` intentionally remains Russian because persisted worlds created
/// before `content_locale` was added deserialize missing values through it.
/// New worlds select their locale from [`DEFAULT_RESPONSE_LANGUAGE`] instead.
/// Any non-Russian response tag uses the English bundle as a neutral source
/// for the model, whose final response-language instruction can still request
/// another language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentLocale {
    #[serde(rename = "en")]
    English,
    #[default]
    #[serde(rename = "ru")]
    Russian,
}

impl ContentLocale {
    pub fn from_language_tag(value: &str) -> Self {
        let normalized =
            normalize_language_tag(value).unwrap_or_else(|| DEFAULT_RESPONSE_LANGUAGE.to_string());
        if normalized.split('-').next() == Some("ru") {
            Self::Russian
        } else {
            Self::English
        }
    }

    pub const fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Russian => "ru",
        }
    }

    pub const fn is_russian(self) -> bool {
        matches!(self, Self::Russian)
    }
}

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

    #[test]
    fn maps_response_tags_to_static_content_bundles() {
        assert_eq!(
            ContentLocale::from_language_tag("ru-RU"),
            ContentLocale::Russian
        );
        assert_eq!(
            ContentLocale::from_language_tag("en-US"),
            ContentLocale::English
        );
        assert_eq!(
            ContentLocale::from_language_tag("de"),
            ContentLocale::English
        );
        assert_eq!(
            ContentLocale::from_language_tag("invalid_tag"),
            ContentLocale::English
        );
    }

    #[test]
    fn legacy_missing_content_locale_still_defaults_to_russian() {
        assert_eq!(ContentLocale::default(), ContentLocale::Russian);
    }
}
