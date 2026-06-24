//! Deterministic dice — faithful port of the dice tool in world.py
//! (`_roll_data`, `roll`, `roll_for_outcome`, `roll_outcome_payload`,
//! `_grade_from_margin`, `_roll_kind`, `_target_label`, `_coerce_int`).
//!
//! All randomness routes through [`crate::rng::MersenneTwister`] so save/restore
//! determinism holds. Forced-die overrides mirror world.py exactly:
//! `forced_die_next` (one-shot, consumed by the roll that uses it) takes
//! precedence over `forced_die_all` (sticky). The `forced` flag never reaches
//! the model-facing compact payload (geometry whitelist lives in the orchestrator).

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::rng::MersenneTwister;

// r"\s*(\d*)d(\d+)\s*(k[hl]\s*\d+)?\s*([+-]\s*\d+)?\s*" applied to the
// lowercased notation via re.fullmatch (anchored at both ends).
static NOTATION_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*(\d*)d(\d+)\s*(k[hl]\s*\d+)?\s*([+-]\s*\d+)?\s*$").unwrap());

// Generic signed-int finder for _coerce_int's string branch.
static INT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"-?\d+").unwrap());

/// `_grade_from_margin(margin)` — exact ladder, must match dice_grades.json.
pub fn grade_from_margin(margin: i64) -> &'static str {
    if margin >= 15 {
        "overwhelming_success"
    } else if margin >= 10 {
        "critical_success"
    } else if margin >= 5 {
        "strong_success"
    } else if margin >= 0 {
        "success"
    } else if margin >= -2 {
        "near_miss"
    } else if margin >= -4 {
        "weak_failure"
    } else if margin >= -9 {
        "failure"
    } else if margin >= -14 {
        "major_failure"
    } else {
        "critical_failure"
    }
}

/// `_roll_kind(value)` — normalize and apply aliases.
pub fn roll_kind(value: &str) -> String {
    let raw = value.trim().to_lowercase().replace(['-', ' '], "_");
    match raw.as_str() {
        "ability_check" => "check".to_string(),
        "saving_throw" => "save".to_string(),
        "random" => "chance".to_string(),
        "opposed" => "contest".to_string(),
        _ => raw,
    }
}

/// `_target_label(target_kind, roll_kind)`.
pub fn target_label(target_kind: &str, roll_kind: &str) -> String {
    let raw = target_kind.trim();
    if !raw.is_empty() && raw.to_lowercase() != "none" {
        return raw.to_string();
    }
    match roll_kind {
        "attack" => "AC".to_string(),
        "check" | "save" => "DC".to_string(),
        "contest" => "opposed_total".to_string(),
        _ => "target".to_string(),
    }
}

/// `_coerce_int(value)` — accepts ints, integral floats, and the first signed
/// integer found in a string. Booleans and everything else -> None.
pub fn coerce_int(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(_) => None,
        Value::Null => None,
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else if let Some(f) = n.as_f64() {
                if f.fract() == 0.0 {
                    Some(f as i64)
                } else {
                    None
                }
            } else {
                None
            }
        }
        Value::String(s) => INT_RE.find(s).and_then(|m| m.as_str().parse::<i64>().ok()),
        _ => None,
    }
}

/// Result of `_roll_data` — the structured roll geometry plus `ok`/`detail`.
#[derive(Clone, Debug)]
pub struct RollData {
    pub ok: bool,
    pub notation: String,
    pub sides: i64,
    pub count: i64,
    pub keep: String,
    pub rolls: Vec<i64>,
    pub kept: Vec<i64>,
    pub modifier: i64,
    pub total: i64,
    pub natural: Option<i64>,
    pub forced: bool,
    pub detail: String,
}

impl RollData {
    fn invalid(notation: &str) -> Self {
        RollData {
            ok: false,
            notation: notation.to_string(),
            sides: 0,
            count: 0,
            keep: String::new(),
            rolls: Vec::new(),
            kept: Vec::new(),
            modifier: 0,
            total: 0,
            natural: None,
            forced: false,
            detail: format!("invalid notation '{notation}'"),
        }
    }
}

/// Python list repr for a `[..]` of ints: `[6, 5, 3]`.
fn list_repr(values: &[i64]) -> String {
    let inner = values
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

/// Python `f'{mod:+d}'` — explicit sign.
fn signed(value: i64) -> String {
    if value >= 0 {
        format!("+{value}")
    } else {
        format!("{value}")
    }
}

/// `_roll_data(notation)` — parse, apply forced overrides, keep-highest/lowest,
/// compute total + the exact detail string. `forced_die_next`/`forced_die_all`
/// are read (and `forced_die_next` consumed) through the supplied mutable refs.
pub fn roll_data(
    rng: &mut MersenneTwister,
    forced_die_next: &mut Option<i64>,
    forced_die_all: &Option<i64>,
    notation: &str,
) -> RollData {
    let raw = notation.to_string();
    let lowered = raw.to_lowercase();
    let caps = match NOTATION_RE.captures(&lowered) {
        Some(c) => c,
        None => return RollData::invalid(notation),
    };

    let count: i64 = {
        let g = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        if g.is_empty() {
            1
        } else {
            g.parse().unwrap_or(0)
        }
    };
    let sides: i64 = caps.get(2).unwrap().as_str().parse().unwrap_or(0);
    let keep_raw = caps
        .get(3)
        .map(|m| m.as_str())
        .unwrap_or("")
        .replace(' ', "");
    let mod_str = caps
        .get(4)
        .map(|m| m.as_str())
        .unwrap_or("0")
        .replace(' ', "");
    let modifier: i64 = if mod_str.is_empty() {
        0
    } else {
        mod_str.parse().unwrap_or(0)
    };

    if count <= 0 || sides <= 0 {
        return RollData::invalid(notation);
    }

    let forced = forced_die_next.or(*forced_die_all);
    let rolls: Vec<i64> = if let Some(f) = forced {
        let face = std::cmp::max(1, std::cmp::min(sides, f));
        vec![face; count as usize]
    } else {
        (0..count).map(|_| rng.randint(1, sides)).collect()
    };
    if forced_die_next.is_some() {
        *forced_die_next = None; // one-shot override consumed
    }

    let mut kept = rolls.clone();
    let mut keep_note = String::new();
    if !keep_raw.is_empty() {
        // keep_raw like "kh3" / "kl1"; suffix after the 2-char prefix.
        let keep_count: i64 = keep_raw[2..].parse().unwrap_or(0);
        if keep_count <= 0 || keep_count > count {
            return RollData::invalid(notation);
        }
        if keep_raw.starts_with("kh") {
            let mut sorted = rolls.clone();
            sorted.sort_by(|a, b| b.cmp(a)); // descending
            kept = sorted.into_iter().take(keep_count as usize).collect();
            keep_note = format!(" keep highest {keep_count}: {}", list_repr(&kept));
        } else {
            let mut sorted = rolls.clone();
            sorted.sort(); // ascending
            kept = sorted.into_iter().take(keep_count as usize).collect();
            keep_note = format!(" keep lowest {keep_count}: {}", list_repr(&kept));
        }
    }

    let total: i64 = kept.iter().sum::<i64>() + modifier;
    let natural = if sides == 20 && kept.len() == 1 {
        Some(kept[0])
    } else {
        None
    };
    let mod_part = if modifier != 0 {
        format!(" {}", signed(modifier))
    } else {
        String::new()
    };
    let detail = format!(
        "{notation} -> {}{keep_note}{mod_part} = {total}",
        list_repr(&rolls)
    );

    RollData {
        ok: true,
        notation: notation.to_string(),
        sides,
        count,
        keep: keep_raw,
        rolls,
        kept,
        modifier,
        total,
        natural,
        forced: forced.is_some(),
        detail,
    }
}

/// `roll_outcome_payload(...)` — full graded payload (dict-shaped Value with
/// the same key order as world.py).
pub fn roll_outcome_payload(
    rng: &mut MersenneTwister,
    forced_die_next: &mut Option<i64>,
    forced_die_all: &Option<i64>,
    notation: &str,
    target_number: Option<&Value>,
    target_kind: &str,
    roll_kind_raw: &str,
) -> Value {
    let data = roll_data(rng, forced_die_next, forced_die_all, notation);
    let total = data.total;
    let detail = data.detail.clone();
    if !data.ok {
        let mut out = Map::new();
        out.insert("ok".to_string(), json!(false));
        out.insert("notation".to_string(), json!(notation));
        out.insert("total".to_string(), json!(total));
        out.insert("grade".to_string(), json!("invalid"));
        out.insert("detail".to_string(), json!(detail));
        return Value::Object(out);
    }

    // geometry (appended at the end via **geometry in Python — preserve order).
    let geometry: Vec<(&str, Value)> = vec![
        ("sides", json!(data.sides)),
        ("count", json!(data.count)),
        ("keep", json!(data.keep)),
        ("rolls", json!(data.rolls)),
        ("kept", json!(data.kept)),
        ("modifier", json!(data.modifier)),
        ("forced", json!(data.forced)),
    ];

    let kind = roll_kind(roll_kind_raw);
    let target = target_number.and_then(coerce_int);
    let graded = matches!(kind.as_str(), "check" | "save" | "attack" | "contest");
    if !graded || target.is_none() {
        let mut out = Map::new();
        out.insert("ok".to_string(), json!(true));
        out.insert("notation".to_string(), json!(data.notation));
        out.insert(
            "roll_kind".to_string(),
            json!(if kind.is_empty() {
                "roll".to_string()
            } else {
                kind.clone()
            }),
        );
        out.insert("total".to_string(), json!(total));
        out.insert("grade".to_string(), json!("ungraded"));
        out.insert("natural".to_string(), to_opt_value(data.natural));
        out.insert(
            "detail".to_string(),
            json!(format!("{detail}: grade=ungraded")),
        );
        for (k, v) in geometry {
            out.insert(k.to_string(), v);
        }
        return Value::Object(out);
    }

    let target = target.unwrap();
    let margin = total - target;
    let mut grade = grade_from_margin(margin).to_string();
    let natural = data.natural;
    let natural_note = match natural {
        Some(n) => format!(", natural={n}"),
        None => String::new(),
    };
    if kind == "attack" && natural == Some(20) {
        grade = "critical_success".to_string();
    } else if kind == "attack" && natural == Some(1) {
        grade = "critical_failure".to_string();
    }

    let tlabel = target_label(target_kind, &kind);
    let mut out = Map::new();
    out.insert("ok".to_string(), json!(true));
    out.insert("notation".to_string(), json!(data.notation));
    out.insert("roll_kind".to_string(), json!(kind));
    out.insert("target_kind".to_string(), json!(tlabel.clone()));
    out.insert("target_number".to_string(), json!(target));
    out.insert("total".to_string(), json!(total));
    out.insert("grade".to_string(), json!(grade.clone()));
    out.insert("margin".to_string(), json!(margin));
    out.insert("natural".to_string(), to_opt_value(natural));
    out.insert(
        "detail".to_string(),
        json!(format!(
            "{detail} vs {tlabel} {target}: grade={grade}, margin={}{natural_note}",
            signed(margin)
        )),
    );
    for (k, v) in geometry {
        out.insert(k.to_string(), v);
    }
    Value::Object(out)
}

fn to_opt_value(v: Option<i64>) -> Value {
    match v {
        Some(n) => json!(n),
        None => Value::Null,
    }
}
