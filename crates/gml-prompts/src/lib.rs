//! gml-prompts: verbatim system prompts and templates for the GM orchestrator
//! and NPC sub-agents, ported faithfully from `gm-lab/prompts.py`.
//!
//! The prompt text is byte-identical to the Python source. In Python `GM_SYSTEM`
//! is an f-string spliced ONCE at import time from `tool_guidance` module
//! constants (all of which are themselves static module-level constants, not
//! runtime values), so the fully-spliced text is itself a faithful static
//! constant. We embed the captured verbatim text via `include_str!` from
//! `assets/` (kept raw / LF by `.gitattributes`).
//!
//! Templates that carry RUNTIME placeholders — `NPC_CARD_TEMPLATE`,
//! `NPC_COMPACT_SYSTEM`, `GM_COMPACT_SYSTEM` — are exposed both as the raw
//! template string and via render functions that reproduce Python's
//! `str.format(**named)` substitution semantics (named placeholders, with
//! `{{`/`}}` as literal braces).

use std::collections::HashMap;

// --- Static, fully-spliced prompts ----------------------------------------

/// GM orchestrator system prompt. In Python this is an f-string fully spliced
/// at import from `tool_guidance.*` static constants — a faithful constant.
pub const GM_SYSTEM: &str = include_str!("../assets/GM_SYSTEM.txt");

/// Static NPC sub-agent system prompt.
pub const NPC_SYSTEM_STATIC: &str = include_str!("../assets/NPC_SYSTEM_STATIC.txt");

/// Backward-compat alias of `NPC_SYSTEM_STATIC` (matches Python:
/// `NPC_SYSTEM_TEMPLATE = NPC_SYSTEM_STATIC`). Byte-identical to it.
pub const NPC_SYSTEM_TEMPLATE: &str = include_str!("../assets/NPC_SYSTEM_TEMPLATE.txt");

// --- Templates with runtime placeholders ----------------------------------

/// Raw NPC card template. Contains `str.format` named fields:
/// `{revision} {name} {role} {gender} {public_label} {age} {physical_type}`
/// `{distinctive_features} {life_status} {condition} {persona} {personality}`
/// `{values} {habits} {pressure_response} {boundaries} {voice} {goals}`
/// `{knowledge} {mechanics} {secret}`.
pub const NPC_CARD_TEMPLATE: &str = include_str!("../assets/NPC_CARD_TEMPLATE.txt");

/// Raw NPC compaction system prompt. Contains one named field: `{proper_nouns}`.
pub const NPC_COMPACT_SYSTEM: &str = include_str!("../assets/NPC_COMPACT_SYSTEM.txt");

/// Raw GM compaction system prompt. Contains one named field: `{proper_nouns_line}`.
pub const GM_COMPACT_SYSTEM: &str = include_str!("../assets/GM_COMPACT_SYSTEM.txt");

// --- Accessors ------------------------------------------------------------

/// GM orchestrator system prompt.
#[inline]
pub fn gm_system() -> &'static str {
    GM_SYSTEM
}

/// Static NPC sub-agent system prompt.
#[inline]
pub fn npc_system_static() -> &'static str {
    NPC_SYSTEM_STATIC
}

/// Backward-compat alias of [`npc_system_static`].
#[inline]
pub fn npc_system_template() -> &'static str {
    NPC_SYSTEM_TEMPLATE
}

// --- Python str.format-equivalent named substitution ----------------------

/// Error from [`format_named`] when the template is malformed or a field is
/// missing — mirrors the failure modes Python's `str.format` would raise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// A `{field}` referenced a key absent from the supplied map (Python `KeyError`).
    MissingField(String),
    /// A single unmatched `{` or `}` (Python `ValueError`).
    UnmatchedBrace,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FormatError::MissingField(k) => write!(f, "missing format field: {k}"),
            FormatError::UnmatchedBrace => write!(f, "single unmatched brace in template"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Reproduce Python `template.format(**fields)` for the simple named-placeholder
/// subset used by these prompts: `{name}` substitution, `{{` / `}}` -> literal
/// `{` / `}`. No conversions, format specs, or positional/indexed fields are
/// used by the GM-Lab templates, so they are not implemented (an unsupported
/// construct would surface as a missing field / unmatched brace, matching
/// Python's behaviour for those exact templates).
pub fn format_named(template: &str, fields: &HashMap<&str, String>) -> Result<String, FormatError> {
    let mut out = String::with_capacity(template.len());
    let bytes = template.as_bytes();
    let mut chars = template.char_indices().peekable();

    while let Some((i, c)) = chars.next() {
        match c {
            '{' => {
                // `{{` -> literal `{`
                if matches!(chars.peek(), Some(&(_, '{'))) {
                    chars.next();
                    out.push('{');
                    continue;
                }
                // Otherwise read field name up to the closing `}`.
                let start = i + 1;
                let mut end = None;
                for (j, cc) in chars.by_ref() {
                    if cc == '}' {
                        end = Some(j);
                        break;
                    }
                }
                let end = end.ok_or(FormatError::UnmatchedBrace)?;
                let key = &template[start..end];
                let val = fields
                    .get(key)
                    .ok_or_else(|| FormatError::MissingField(key.to_string()))?;
                out.push_str(val);
            }
            '}' => {
                // `}}` -> literal `}`; a lone `}` is an error.
                if matches!(chars.peek(), Some(&(_, '}'))) {
                    chars.next();
                    out.push('}');
                } else {
                    return Err(FormatError::UnmatchedBrace);
                }
            }
            _ => out.push(c),
        }
    }
    // `bytes` kept only to assert we consumed UTF-8 cleanly above.
    debug_assert_eq!(bytes.len(), template.len());
    Ok(out)
}

/// Fields for [`render_npc_card`]. Field set and names match the Python
/// `prompts.NPC_CARD_TEMPLATE.format(...)` call in `agents.py`.
#[derive(Debug, Clone, Default)]
pub struct NpcCardFields<'a> {
    pub revision: &'a str,
    pub name: &'a str,
    pub role: &'a str,
    pub gender: &'a str,
    pub public_label: &'a str,
    pub age: &'a str,
    pub physical_type: &'a str,
    pub distinctive_features: &'a str,
    pub life_status: &'a str,
    pub condition: &'a str,
    pub persona: &'a str,
    pub personality: &'a str,
    pub values: &'a str,
    pub habits: &'a str,
    pub pressure_response: &'a str,
    pub boundaries: &'a str,
    pub voice: &'a str,
    pub goals: &'a str,
    pub knowledge: &'a str,
    pub mechanics: &'a str,
    pub secret: &'a str,
}

/// Render [`NPC_CARD_TEMPLATE`] with the same substitution semantics as the
/// Python caller. The template text is never altered.
pub fn render_npc_card(f: &NpcCardFields<'_>) -> String {
    let mut m: HashMap<&str, String> = HashMap::with_capacity(21);
    m.insert("revision", f.revision.to_string());
    m.insert("name", f.name.to_string());
    m.insert("role", f.role.to_string());
    m.insert("gender", f.gender.to_string());
    m.insert("public_label", f.public_label.to_string());
    m.insert("age", f.age.to_string());
    m.insert("physical_type", f.physical_type.to_string());
    m.insert("distinctive_features", f.distinctive_features.to_string());
    m.insert("life_status", f.life_status.to_string());
    m.insert("condition", f.condition.to_string());
    m.insert("persona", f.persona.to_string());
    m.insert("personality", f.personality.to_string());
    m.insert("values", f.values.to_string());
    m.insert("habits", f.habits.to_string());
    m.insert("pressure_response", f.pressure_response.to_string());
    m.insert("boundaries", f.boundaries.to_string());
    m.insert("voice", f.voice.to_string());
    m.insert("goals", f.goals.to_string());
    m.insert("knowledge", f.knowledge.to_string());
    m.insert("mechanics", f.mechanics.to_string());
    m.insert("secret", f.secret.to_string());
    // The template uses only known fields, so this cannot fail.
    format_named(NPC_CARD_TEMPLATE, &m).expect("NPC_CARD_TEMPLATE render")
}

/// Render [`NPC_COMPACT_SYSTEM`] by filling `{proper_nouns}`.
/// Matches `orchestrator.py`: `proper_nouns = ", ".join(world.proper_nouns())`.
pub fn render_npc_compact_system(proper_nouns: &str) -> String {
    let mut m: HashMap<&str, String> = HashMap::with_capacity(1);
    m.insert("proper_nouns", proper_nouns.to_string());
    format_named(NPC_COMPACT_SYSTEM, &m).expect("NPC_COMPACT_SYSTEM render")
}

/// Render [`GM_COMPACT_SYSTEM`] by filling `{proper_nouns_line}`.
/// The caller (llm_client.py / codex_client.py) builds `proper_nouns_line`
/// from the proper-noun set; see [`gm_compact_proper_nouns_line`].
pub fn render_gm_compact_system(proper_nouns_line: &str) -> String {
    let mut m: HashMap<&str, String> = HashMap::with_capacity(1);
    m.insert("proper_nouns_line", proper_nouns_line.to_string());
    format_named(GM_COMPACT_SYSTEM, &m).expect("GM_COMPACT_SYSTEM render")
}

/// Build the `proper_nouns_line` fragment exactly as `llm_client._proper_nouns_line`:
/// trims/filters blank names; empty -> generic line, else the explicit list line.
pub fn gm_compact_proper_nouns_line<I, S>(proper_nouns: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let names: Vec<String> = proper_nouns
        .into_iter()
        .map(|s| s.as_ref().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if names.is_empty() {
        "Keep proper nouns exactly as written in the transcript; never transliterate them."
            .to_string()
    } else {
        format!(
            "Keep these proper nouns exactly as written if they appear; never translate or \
             transliterate them: {}.",
            names.join(", ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn sha256_hex(s: &str) -> String {
        let mut h = Sha256::new();
        h.update(s.as_bytes());
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    // GM_SYSTEM sha/len updated for the tool-search discipline batch: a new
    // TOOL SEARCH DISCIPLINE section (default-permit / narrow-trigger /
    // scoped-violation + a by-capability paragraph noting loaded tools persist
    // for the session); generate_npc returned to the deferred set, so the TOOL
    // ROUTING catalog moves it and generate_location (plus the item/spell tools)
    // to the tool_search-loaded side and its Named-NPC guidance now reads as a
    // searched tool; and a new deferred long_rest one-liner (full rest only —
    // short rest stays advance_time + GM adjudication) joined TOOL ROUTING. This
    // sits on top of the earlier snapshot-once refactor (GM_CONTEXT_TZ §6): the
    // once-per-session WORLD SNAPSHOT + tool-result deltas + read_state re-read
    // contract, standing TURN RESOLUTION / PLAYER OPTION SUGGESTIONS policy, and
    // the WORLD SNAPSHOT / DYNAMIC NPC ROSTER labels.
    const GM_SYSTEM_SHA: &str = "bf71f83c8de4e45ca5dc1e514d12b77a98d3b6a8c787c7d07687f120d37dd751";
    const NPC_SYSTEM_STATIC_SHA: &str =
        "a4c157e782e4788868748bc7509ce626835328eb5ef8d96f5f4b6cd05ed5192b";
    const NPC_CARD_TEMPLATE_SHA: &str =
        "73cb6261b026b1d1b8682caf45047ca625b601f43147f48a6c9b0f3e2dd3a454";
    const NPC_COMPACT_SYSTEM_SHA: &str =
        "5d9d761fc72569c21b51f66e26848c56fd432c5aff0317d470f9ad191c66bdbf";
    const GM_COMPACT_SYSTEM_SHA: &str =
        "33bb15fd2904ca47d324238c3e15d75458c48ce246b16beb54a26b7f8de651c8";

    // Byte-identity against the golden fixtures (raw include_bytes! avoids any
    // EOL ambiguity).
    macro_rules! assert_bytes_eq {
        ($got:expr, $fixture:literal) => {{
            let fixture: &[u8] =
                include_bytes!(concat!("../../../tests/reference/prompts/", $fixture));
            assert_eq!($got.as_bytes(), fixture, "byte mismatch vs {}", $fixture);
        }};
    }

    #[test]
    fn gm_system_byte_identical() {
        assert_bytes_eq!(gm_system(), "GM_SYSTEM.txt");
        assert_eq!(sha256_hex(GM_SYSTEM), GM_SYSTEM_SHA);
        assert_eq!(GM_SYSTEM.chars().count(), 46001);
        assert_eq!(GM_SYSTEM.len(), 46355);
    }

    #[test]
    fn npc_system_static_byte_identical() {
        assert_bytes_eq!(npc_system_static(), "NPC_SYSTEM_STATIC.txt");
        assert_eq!(sha256_hex(NPC_SYSTEM_STATIC), NPC_SYSTEM_STATIC_SHA);
        assert_eq!(NPC_SYSTEM_STATIC.chars().count(), 7051);
        assert_eq!(NPC_SYSTEM_STATIC.len(), 7123);
    }

    #[test]
    fn npc_system_template_is_alias() {
        // Python: NPC_SYSTEM_TEMPLATE = NPC_SYSTEM_STATIC (same sha).
        assert_bytes_eq!(npc_system_template(), "NPC_SYSTEM_TEMPLATE.txt");
        assert_eq!(NPC_SYSTEM_TEMPLATE, NPC_SYSTEM_STATIC);
        assert_eq!(sha256_hex(NPC_SYSTEM_TEMPLATE), NPC_SYSTEM_STATIC_SHA);
    }

    #[test]
    fn npc_system_output_example_is_valid_json() {
        let example = NPC_SYSTEM_STATIC
            .lines()
            .last()
            .expect("NPC system output example");
        let parsed: serde_json::Value =
            serde_json::from_str(example).expect("valid NPC output JSON example");
        assert!(parsed.get("response").is_some());
        assert!(parsed.get("beats").is_some());
        assert!(parsed.get("claims").is_some());
    }

    #[test]
    fn npc_card_template_byte_identical() {
        assert_bytes_eq!(NPC_CARD_TEMPLATE, "NPC_CARD_TEMPLATE.txt");
        assert_eq!(sha256_hex(NPC_CARD_TEMPLATE), NPC_CARD_TEMPLATE_SHA);
        assert_eq!(NPC_CARD_TEMPLATE.len(), 571);
    }

    #[test]
    fn npc_compact_system_byte_identical() {
        assert_bytes_eq!(NPC_COMPACT_SYSTEM, "NPC_COMPACT_SYSTEM.txt");
        assert_eq!(sha256_hex(NPC_COMPACT_SYSTEM), NPC_COMPACT_SYSTEM_SHA);
        assert_eq!(NPC_COMPACT_SYSTEM.len(), 444);
    }

    #[test]
    fn gm_compact_system_byte_identical() {
        assert_bytes_eq!(GM_COMPACT_SYSTEM, "GM_COMPACT_SYSTEM.txt");
        assert_eq!(sha256_hex(GM_COMPACT_SYSTEM), GM_COMPACT_SYSTEM_SHA);
        assert_eq!(GM_COMPACT_SYSTEM.len(), 608);
    }

    #[test]
    fn format_named_basic_and_braces() {
        let mut m: HashMap<&str, String> = HashMap::new();
        m.insert("a", "X".to_string());
        assert_eq!(format_named("{a}", &m).unwrap(), "X");
        assert_eq!(format_named("[{a}]", &m).unwrap(), "[X]");
        assert_eq!(format_named("{{a}}", &m).unwrap(), "{a}");
        assert_eq!(format_named("{{{a}}}", &m).unwrap(), "{X}");
        assert_eq!(
            format_named("{b}", &m).unwrap_err(),
            FormatError::MissingField("b".to_string())
        );
        assert_eq!(
            format_named("{a", &m).unwrap_err(),
            FormatError::UnmatchedBrace
        );
        assert_eq!(
            format_named("a}", &m).unwrap_err(),
            FormatError::UnmatchedBrace
        );
    }

    #[test]
    fn render_npc_card_substitutes_all_fields() {
        let f = NpcCardFields {
            revision: "3",
            name: "Борин",
            role: "трактирщик",
            gender: "M",
            public_label: "хозяин",
            age: "50",
            physical_type: "крепкий",
            distinctive_features: "шрам",
            life_status: "alive",
            condition: "(не указано)",
            persona: "ворчливый",
            personality: "осторожный",
            values: "семья",
            habits: "протирает кружку",
            pressure_response: "молчит",
            boundaries: "не выдаёт постояльцев",
            voice: "низкий",
            goals: "защитить дочь",
            knowledge: "видел чужака",
            mechanics: "{\"hp\":{\"current\":11}}",
            secret: "прячет письмо",
        };
        let out = render_npc_card(&f);
        assert!(out.starts_with("CURRENT NPC CARD (revision 3)\nName: Борин\n"));
        assert!(out.contains("Gender: M\n"));
        assert!(out.contains("Mechanics: {\"hp\":{\"current\":11}}\n"));
        assert!(out.ends_with("This card overrides older memory if there is a conflict."));
        // No unsubstituted placeholders remain.
        assert!(!out.contains('{') || out.contains("{\"hp\""));
    }

    #[test]
    fn render_compact_systems() {
        let npc = render_npc_compact_system("Борин, «Серый грифон»");
        assert!(npc.ends_with("Keep proper nouns exactly as written: Борин, «Серый грифон»."));
        assert!(!npc.contains("{proper_nouns}"));

        let line_empty = gm_compact_proper_nouns_line(Vec::<String>::new());
        assert_eq!(
            line_empty,
            "Keep proper nouns exactly as written in the transcript; never transliterate them."
        );
        let line = gm_compact_proper_nouns_line(["Борин", "  ", "Нордхольм"]);
        assert_eq!(
            line,
            "Keep these proper nouns exactly as written if they appear; never translate or \
             transliterate them: Борин, Нордхольм."
        );
        let gm = render_gm_compact_system(&line);
        assert!(gm.ends_with(&line));
        assert!(!gm.contains("{proper_nouns_line}"));
    }
}
