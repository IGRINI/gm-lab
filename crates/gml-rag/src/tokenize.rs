//! Tokenizer and stopwords — faithful port of `rag.py::_tokens` / `_STOPWORDS`.

use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::HashSet;

/// Exact `_STOPWORDS` set from `rag.py`.
pub static STOPWORDS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "the", "and", "or", "of", "to", "in", "on", "at", "a", "an", "is", "are", "was", "were",
        "who", "what", "where", "when", "why", "how", "exactly", "about",
        "это", "или", "что", "кто", "где", "когда", "зачем", "как", "какой", "какая", "какие",
        "искать", "найти", "про", "при", "для", "над", "под", "без", "уже", "сейчас", "тут",
        "здесь", "там", "его", "её", "она", "они", "он", "мне", "тебе", "меня", "тебя",
    ]
    .into_iter()
    .collect()
});

/// Regex `[a-zA-Zа-яА-ЯёЁ0-9_«»\-]+` (applied to lowercased text).
static TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[a-zA-Zа-яА-ЯёЁ0-9_«»\-]+").expect("valid token regex"));

/// Chars stripped from each raw match's ends: `«`, `»`, `-`, `.`.
const STRIP_CHARS: &[char] = &['«', '»', '-', '.'];

/// EXACT port of `_tokens(text)`.
///
/// - lowercase the whole text (Python `.lower()`),
/// - find all regex matches,
/// - strip `«»-.` from each match's ends and lowercase again,
/// - keep when `len(word) >= 3` (Unicode code points) and not a stopword.
pub fn tokens(text: &str) -> Vec<String> {
    let lowered = text.to_lowercase();
    let mut words: Vec<String> = Vec::new();
    for m in TOKEN_RE.find_iter(&lowered) {
        let word: String = m.as_str().trim_matches(STRIP_CHARS).to_lowercase();
        if word.chars().count() >= 3 && !STOPWORDS.contains(word.as_str()) {
            words.push(word);
        }
    }
    words
}
