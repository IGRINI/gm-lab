//! Persisted runtime inference settings — faithful port of
//! `gm-lab/runtime_settings.py`.
//!
//! These are UI-controlled inference knobs (per-role reasoning effort/summary,
//! verbosity, tool_choice, streaming, parallel tools, TTS, max tool hops, max
//! output tokens) stored atomically in `gm_lab_settings.json`, with
//! validation/clamp/normalize logic, legacy migration, a thread-safe cache and
//! model-capability reconciliation.
//!
//! Fidelity notes:
//! - On-disk JSON matches Python `json.dumps(data, ensure_ascii=False,
//!   indent=2, sort_keys=True)` plus a trailing `"\n"`. We reproduce this with
//!   a [`BTreeMap`] (sorted keys) and a 2-space pretty printer.
//! - Atomic write: tempfile in the **same dir** + atomic rename
//!   (`tempfile::NamedTempFile::persist`), mirroring `mkstemp` + `os.replace`.
//! - Values are scalars only (string / bool / int), as in the Python settings
//!   dict; we model them as [`serde_json::Value`] to preserve the exact JSON
//!   shape and the Python truthiness/normalization rules.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value};

use crate::config::{self, Config};

/// `REASONING_EFFORTS = ("none","minimal","low","medium","high","xhigh")`.
pub const REASONING_EFFORTS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];
/// `REASONING_SUMMARIES = ("auto","concise","detailed","none")`.
pub const REASONING_SUMMARIES: [&str; 4] = ["auto", "concise", "detailed", "none"];
/// `TEXT_VERBOSITIES = ("default","low","medium","high")`.
pub const TEXT_VERBOSITIES: [&str; 4] = ["default", "low", "medium", "high"];
/// `TOOL_CHOICES = ("auto","required","none")`.
pub const TOOL_CHOICES: [&str; 3] = ["auto", "required", "none"];
/// `MAX_TOOL_HOPS_CAP = 100`.
pub const MAX_TOOL_HOPS_CAP: i64 = 100;

/// The reasoning role string keys, in `config.REASONING_ROLES` order.
fn reasoning_role_keys() -> [&'static str; 4] {
    [
        config::Role::Gm.as_str(),
        config::Role::Npc.as_str(),
        config::Role::Compact.as_str(),
        config::Role::Location.as_str(),
    ]
}

/// Settings as a JSON object. Insertion order is irrelevant for behavior; the
/// on-disk file is always written sorted. We use a [`BTreeMap`] internally so
/// that round-trips and serialization are deterministically key-sorted, exactly
/// like Python's `sort_keys=True`.
pub type SettingsMap = BTreeMap<String, Value>;

/// Persisted runtime settings with a thread-safe cache and atomic JSON storage.
///
/// One instance owns a settings path + cache, replacing the Python module
/// globals (`_SETTINGS_PATH`, `_LOCK`, `_CACHE`). All defaults are derived from
/// a [`Config`] (the Python `_DEFAULTS` is computed from `config.*` + env).
pub struct RuntimeSettings {
    path: PathBuf,
    defaults: SettingsMap,
    base_reasoning_summary: String,
    max_output_tokens_cap: i64,
    cache: Mutex<Option<SettingsMap>>,
}

impl RuntimeSettings {
    /// Build with the default settings-path resolution and a [`Config`] built
    /// from the environment.
    pub fn from_env() -> Self {
        let cfg = Config::from_env();
        Self::new(&cfg, default_settings_path())
    }

    /// Build from an explicit [`Config`] and settings path. The defaults dict
    /// is computed exactly as the Python `_DEFAULTS` block (per-role reasoning
    /// base from Codex base + env `GM_<ROLE>_REASONING_<KIND>`, etc.).
    pub fn new(cfg: &Config, path: PathBuf) -> Self {
        let (defaults, base_reasoning_summary) = compute_defaults(cfg);
        RuntimeSettings {
            path,
            defaults,
            base_reasoning_summary,
            max_output_tokens_cap: cfg.max_output_tokens_cap,
            cache: Mutex::new(None),
        }
    }

    /// `settings_path()` — the resolved JSON path.
    pub fn settings_path(&self) -> &Path {
        &self.path
    }

    /// `defaults()` — a copy of the computed defaults dict.
    pub fn defaults(&self) -> SettingsMap {
        self.defaults.clone()
    }

    /// `options()` — allowed-value lists for the UI plus the two caps.
    pub fn options(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(
            "reasoning_efforts".into(),
            Value::from(
                REASONING_EFFORTS
                    .iter()
                    .map(|s| Value::from(*s))
                    .collect::<Vec<_>>(),
            ),
        );
        m.insert(
            "reasoning_summaries".into(),
            Value::from(
                REASONING_SUMMARIES
                    .iter()
                    .map(|s| Value::from(*s))
                    .collect::<Vec<_>>(),
            ),
        );
        m.insert(
            "reasoning_roles".into(),
            Value::from(
                reasoning_role_keys()
                    .iter()
                    .map(|s| Value::from(*s))
                    .collect::<Vec<_>>(),
            ),
        );
        m.insert(
            "text_verbosities".into(),
            Value::from(
                TEXT_VERBOSITIES
                    .iter()
                    .map(|s| Value::from(*s))
                    .collect::<Vec<_>>(),
            ),
        );
        m.insert(
            "tool_choices".into(),
            Value::from(
                TOOL_CHOICES
                    .iter()
                    .map(|s| Value::from(*s))
                    .collect::<Vec<_>>(),
            ),
        );
        m.insert("max_tool_hops_max".into(), Value::from(MAX_TOOL_HOPS_CAP));
        m.insert(
            "max_output_tokens_max".into(),
            Value::from(self.max_output_tokens_cap),
        );
        m
    }

    /// `get()` — lazily loads from disk into the cache, returns a copy.
    pub fn get(&self) -> SettingsMap {
        let mut guard = self.cache.lock().expect("settings cache lock");
        if guard.is_none() {
            *guard = Some(self.load());
        }
        guard.as_ref().unwrap().clone()
    }

    /// `update(values)` — merge cleaned `values` over current, normalize,
    /// atomically save, refresh cache, return a copy. `None`/empty == `{}`.
    pub fn update(&self, values: Option<&Map<String, Value>>) -> SettingsMap {
        let mut guard = self.cache.lock().expect("settings cache lock");
        // current = get() (but we already hold the lock; replicate lazy load)
        let mut current = match guard.as_ref() {
            Some(c) => c.clone(),
            None => self.load(),
        };
        let empty = Map::new();
        let incoming = values.unwrap_or(&empty);
        let cleaned = self.clean(incoming, &current);
        for (k, v) in cleaned {
            current.insert(k, v);
        }
        let normalized = self.normalize_map(&current);
        self.save(&normalized);
        *guard = Some(normalized.clone());
        normalized
    }

    // --- model reconciliation ----------------------------------------------

    /// `reconcile_for_model(model)` — adjust per-role reasoning settings only
    /// when model metadata makes them invalid; persist via `update` if changed.
    pub fn reconcile_for_model(&self, model: Option<&Map<String, Value>>) -> SettingsMap {
        let Some(model) = model else {
            return self.get();
        };
        let settings = self.get();
        let supports = model.get("supports_reasoning_summaries");
        let mut supported = supported_reasoning_efforts(Some(model));

        let mut next: Map<String, Value> = Map::new();

        // `if supports is False: supported = []`
        let supports_is_false = matches!(supports, Some(Value::Bool(false)));
        if supports_is_false {
            supported = Vec::new();
        }

        // default_effort = level || effort || supported[0] || "none"
        let default_effort = {
            let lvl = string_of(model.get("default_reasoning_level")).to_lowercase();
            if !lvl.is_empty() {
                lvl
            } else {
                let eff = string_of(model.get("default_reasoning_effort")).to_lowercase();
                if !eff.is_empty() {
                    eff
                } else {
                    supported
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "none".to_string())
                }
            }
        };
        // default_summary = model.default_reasoning_summary || base_summary
        let mut default_summary = {
            let s = string_of(model.get("default_reasoning_summary")).to_lowercase();
            if s.is_empty() {
                self.base_reasoning_summary.clone()
            } else {
                s
            }
        };
        if !REASONING_SUMMARIES.contains(&default_summary.as_str()) {
            default_summary = "auto".to_string();
        }

        for role in reasoning_role_keys() {
            let effort_key = format!("{role}_reasoning_effort");
            let summary_key = format!("{role}_reasoning_summary");
            let effort = string_of(settings.get(&effort_key)).to_lowercase();

            if supports_is_false && effort != "none" {
                next.insert(effort_key.clone(), Value::from("none"));
                next.insert(summary_key.clone(), Value::from("none"));
            } else if !supported.is_empty() && !supported.contains(&effort) && effort != "none" {
                next.insert(effort_key.clone(), Value::from(default_effort.clone()));
                if default_effort == "none" {
                    next.insert(summary_key.clone(), Value::from("none"));
                } else if settings.get(&summary_key) == Some(&Value::from("none")) {
                    next.insert(summary_key.clone(), Value::from(default_summary.clone()));
                }
            } else if effort == "none" && settings.get(&summary_key) != Some(&Value::from("none")) {
                next.insert(summary_key.clone(), Value::from("none"));
            }
        }

        if !next.is_empty() {
            return self.update(Some(&next));
        }
        settings
    }

    // --- per-request accessors ---------------------------------------------

    /// `role_settings(role, settings?)` — `{effort, summary}` for a role,
    /// lowercased, defaulting to `"none"`; summary forced `"none"` if effort is
    /// `"none"` or if summary is not a known summary value.
    pub fn role_settings(&self, role: &str, settings: Option<&SettingsMap>) -> RoleSettings {
        let role = normalize_role(role);
        let owned;
        let values: &SettingsMap = match settings {
            Some(s) => s,
            None => {
                owned = self.get();
                &owned
            }
        };
        let mut effort = string_of(values.get(&format!("{role}_reasoning_effort"))).to_lowercase();
        if effort.is_empty() {
            effort = "none".to_string();
        }
        let mut summary =
            string_of(values.get(&format!("{role}_reasoning_summary"))).to_lowercase();
        if summary.is_empty() {
            summary = "none".to_string();
        }
        if effort == "none" {
            summary = "none".to_string();
        }
        if !REASONING_SUMMARIES.contains(&summary.as_str()) {
            summary = "none".to_string();
        }
        RoleSettings { effort, summary }
    }

    /// `reasoning_enabled(think, role)` — think truthy AND role effort != none.
    pub fn reasoning_enabled(&self, think: Option<bool>, role: &str) -> bool {
        think.unwrap_or(false) && self.role_settings(role, None).effort != "none"
    }

    /// `reasoning_for_request(think, role)` — the per-request reasoning payload,
    /// or `None`. Emits `{effort}` plus `summary` only when summary != none.
    pub fn reasoning_for_request(
        &self,
        think: Option<bool>,
        role: &str,
    ) -> Option<Map<String, Value>> {
        if !think.unwrap_or(false) {
            return None;
        }
        let values = self.role_settings(role, None);
        if values.effort == "none" {
            return None;
        }
        let mut out = Map::new();
        out.insert("effort".into(), Value::from(values.effort));
        if values.summary != "none" {
            out.insert("summary".into(), Value::from(values.summary));
        }
        Some(out)
    }

    /// `role_reasoning_enabled(role)` — role effort != none (ignores think).
    pub fn role_reasoning_enabled(&self, role: &str) -> bool {
        self.role_settings(role, None).effort != "none"
    }

    /// `tool_choice_for_request(has_tools)`.
    pub fn tool_choice_for_request(&self, has_tools: bool) -> String {
        if !has_tools {
            return "none".to_string();
        }
        let choice = string_of(self.get().get("tool_choice")).to_lowercase();
        if TOOL_CHOICES.contains(&choice.as_str()) {
            choice
        } else {
            "auto".to_string()
        }
    }

    /// `parallel_tool_calls_for_request(has_tools)`.
    pub fn parallel_tool_calls_for_request(&self, has_tools: bool) -> bool {
        has_tools && bool_of(self.get().get("parallel_tool_calls"), true)
    }

    /// `gm_suggest_options_enabled(settings?)` (default False).
    pub fn gm_suggest_options_enabled(&self, settings: Option<&SettingsMap>) -> bool {
        self.bool_setting(settings, "gm_suggest_options", false)
    }

    /// `stream_gm_content_enabled(settings?)` (default True).
    pub fn stream_gm_content_enabled(&self, settings: Option<&SettingsMap>) -> bool {
        self.bool_setting(settings, "stream_gm_content", true)
    }

    /// `tts_enabled(settings?)` (default False).
    pub fn tts_enabled(&self, settings: Option<&SettingsMap>) -> bool {
        self.bool_setting(settings, "tts_enabled", false)
    }

    /// `max_tool_hops(settings?)` — clamp `>= 0` (Python re-clamps at read).
    pub fn max_tool_hops(&self, settings: Option<&SettingsMap>) -> i64 {
        let owned;
        let values: &SettingsMap = match settings {
            Some(s) => s,
            None => {
                owned = self.get();
                &owned
            }
        };
        int_of(values.get("max_tool_hops"))
            .map(|n| n.max(0))
            .unwrap_or(0)
    }

    /// `max_output_tokens()` — clamp `>= 0`.
    pub fn max_output_tokens(&self) -> i64 {
        int_of(self.get().get("max_output_tokens"))
            .map(|n| n.max(0))
            .unwrap_or(0)
    }

    // --- internal helpers ---------------------------------------------------

    fn bool_setting(&self, settings: Option<&SettingsMap>, key: &str, default: bool) -> bool {
        let owned;
        let values: &SettingsMap = match settings {
            Some(s) => s,
            None => {
                owned = self.get();
                &owned
            }
        };
        bool_of(values.get(key), default)
    }

    fn load(&self) -> SettingsMap {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(map)) => self.normalize_value_map(&map),
                // not a dict, or any parse error -> normalize({})
                _ => self.normalize_value_map(&Map::new()),
            },
            // FileNotFound or any read error -> normalize({})
            Err(_) => self.normalize_value_map(&Map::new()),
        }
    }

    fn save(&self, data: &SettingsMap) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let body = serialize_sorted_pretty(data);
        // tempfile in the SAME dir, atomic rename (mirrors mkstemp + os.replace)
        let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
        match tempfile::Builder::new()
            .prefix(&format!(
                "{}.",
                self.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            ))
            .suffix(".tmp")
            .tempfile_in(dir)
        {
            Ok(mut tmp) => {
                use std::io::Write;
                // Python writes body then an extra "\n".
                let _ = tmp.write_all(body.as_bytes());
                let _ = tmp.write_all(b"\n");
                let _ = tmp.flush();
                let _ = tmp.persist(&self.path);
            }
            Err(_) => {
                // best-effort fallback: direct write (non-atomic)
                let mut full = body;
                full.push('\n');
                let _ = std::fs::write(&self.path, full);
            }
        }
    }

    // _normalize over a BTreeMap input (already sorted/owned)
    fn normalize_map(&self, data: &SettingsMap) -> SettingsMap {
        let as_value_map: Map<String, Value> =
            data.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        self.normalize_value_map(&as_value_map)
    }

    // _normalize(data): start from defaults, layer migrated-legacy cleaned, then
    // raw cleaned; finally force summary=none where effort==none.
    fn normalize_value_map(&self, data: &Map<String, Value>) -> SettingsMap {
        let mut normalized: SettingsMap = self.defaults.clone();

        let migrated = migrate_legacy_reasoning(data);
        for (k, v) in self.clean(&migrated, &normalized) {
            normalized.insert(k, v);
        }
        for (k, v) in self.clean(data, &normalized) {
            normalized.insert(k, v);
        }
        for role in reasoning_role_keys() {
            let effort_key = format!("{role}_reasoning_effort");
            let summary_key = format!("{role}_reasoning_summary");
            if normalized.get(&effort_key) == Some(&Value::from("none")) {
                normalized.insert(summary_key, Value::from("none"));
            }
        }
        normalized
    }

    // _clean(data, base): validate/normalize the known keys present in `data`.
    fn clean(&self, data: &Map<String, Value>, base: &SettingsMap) -> SettingsMap {
        let mut out: SettingsMap = BTreeMap::new();

        for role in reasoning_role_keys() {
            let effort_key = format!("{role}_reasoning_effort");
            let summary_key = format!("{role}_reasoning_summary");
            if data.contains_key(&effort_key) {
                let effort = string_of(data.get(&effort_key)).to_lowercase();
                // Codex accepts custom non-empty effort values from catalogs.
                if !effort.is_empty() {
                    out.insert(effort_key, Value::from(effort));
                }
            }
            if data.contains_key(&summary_key) {
                let summary = string_of(data.get(&summary_key)).to_lowercase();
                if REASONING_SUMMARIES.contains(&summary.as_str()) {
                    out.insert(summary_key, Value::from(summary));
                }
            }
        }

        if data.contains_key("text_verbosity") {
            let v = string_of(data.get("text_verbosity")).to_lowercase();
            if TEXT_VERBOSITIES.contains(&v.as_str()) {
                out.insert("text_verbosity".into(), Value::from(v));
            }
        }

        if data.contains_key("tool_choice") {
            let v = string_of(data.get("tool_choice")).to_lowercase();
            if TOOL_CHOICES.contains(&v.as_str()) {
                out.insert("tool_choice".into(), Value::from(v));
            }
        }

        for key in [
            "stream_gm_content",
            "parallel_tool_calls",
            "gm_suggest_options",
            "tts_enabled",
            "tts_autoplay",
        ] {
            if data.contains_key(key) {
                out.insert(key.into(), Value::from(bool_of(data.get(key), false)));
            }
        }

        if data.contains_key("max_tool_hops") {
            let cleaned = match clean_int(data.get("max_tool_hops")) {
                Some(n) => MAX_TOOL_HOPS_CAP.min(n.max(0)),
                None => int_of(base.get("max_tool_hops")).unwrap_or(0),
            };
            out.insert("max_tool_hops".into(), Value::from(cleaned));
        }

        if data.contains_key("max_output_tokens") {
            let cleaned = match clean_int(data.get("max_output_tokens")) {
                Some(n) => self.max_output_tokens_cap.min(n.max(0)),
                None => int_of(base.get("max_output_tokens")).unwrap_or(0),
            };
            out.insert("max_output_tokens".into(), Value::from(cleaned));
        }

        out
    }
}

/// `{effort, summary}` returned by `role_settings`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSettings {
    pub effort: String,
    pub summary: String,
}

// --- module-level free functions (mirrors of the Python module helpers) -----

/// `supported_reasoning_efforts(model)` — deduped supported effort strings from
/// model metadata (keys tried in order: supported_reasoning_levels,
/// supported_reasoning_efforts, reasoning_efforts). Items may be dicts.
pub fn supported_reasoning_efforts(model: Option<&Map<String, Value>>) -> Vec<String> {
    let Some(model) = model else {
        return Vec::new();
    };
    let raw = model
        .get("supported_reasoning_levels")
        .filter(|v| !is_empty_value(v))
        .or_else(|| {
            model
                .get("supported_reasoning_efforts")
                .filter(|v| !is_empty_value(v))
        })
        .or_else(|| {
            model
                .get("reasoning_efforts")
                .filter(|v| !is_empty_value(v))
        });

    let mut out: Vec<String> = Vec::new();
    if let Some(Value::Array(items)) = raw {
        for item in items {
            let effort = match item {
                Value::Object(obj) => {
                    let e = string_of(obj.get("effort"));
                    if !e.is_empty() {
                        e
                    } else {
                        let i = string_of(obj.get("id"));
                        if !i.is_empty() {
                            i
                        } else {
                            string_of(obj.get("value"))
                        }
                    }
                }
                other => string_of(Some(other)),
            };
            if !effort.is_empty() && !out.contains(&effort) {
                out.push(effort);
            }
        }
    }
    out
}

/// `_migrate_legacy_reasoning(data)` — map old global `reasoning_effort` /
/// `reasoning_summary` to active generation roles only (compact keeps default).
fn migrate_legacy_reasoning(data: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let legacy_effort = string_of(data.get("reasoning_effort")).to_lowercase();
    let legacy_summary = string_of(data.get("reasoning_summary")).to_lowercase();
    for role in [
        config::Role::Gm.as_str(),
        config::Role::Npc.as_str(),
        config::Role::Location.as_str(),
    ] {
        let effort_key = format!("{role}_reasoning_effort");
        let summary_key = format!("{role}_reasoning_summary");
        if !legacy_effort.is_empty() && !data.contains_key(&effort_key) {
            out.insert(effort_key, Value::from(legacy_effort.clone()));
        }
        if !legacy_summary.is_empty() && !data.contains_key(&summary_key) {
            out.insert(summary_key, Value::from(legacy_summary.clone()));
        }
    }
    out
}

/// `_role(role)` — lowercased role if known, else GM.
fn normalize_role(role: &str) -> &'static str {
    let r = role.trim().to_lowercase();
    for key in reasoning_role_keys() {
        if key == r {
            return key;
        }
    }
    config::Role::Gm.as_str()
}

// --- value coercion helpers (mirror Python _string/_bool/int) ---------------

/// `_string(value)` — `str(value or "").strip()`. For JSON: None/false/0/""/
/// empty containers become `""`; otherwise the Python `str()` of the scalar.
fn string_of(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::Bool(false)) => String::new(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::String(s)) => s.trim().to_string(),
        Some(Value::Number(n)) => {
            // Python `str(value or "")`: 0 -> "" (falsy), else str(n)
            if is_number_zero(n) {
                String::new()
            } else {
                n.to_string().trim().to_string()
            }
        }
        Some(Value::Array(a)) => {
            if a.is_empty() {
                String::new()
            } else {
                // Non-empty container is truthy; Python would str(list) — but
                // settings never carry arrays here. Fall back to debug repr.
                Value::Array(a.clone()).to_string().trim().to_string()
            }
        }
        Some(Value::Object(o)) => {
            if o.is_empty() {
                String::new()
            } else {
                Value::Object(o.clone()).to_string().trim().to_string()
            }
        }
    }
}

/// `_bool(value)` — bool stays; None -> False; else
/// `str(value).strip().lower() not in ("0","false","no","off","")`.
fn bool_of(value: Option<&Value>, default: bool) -> bool {
    match value {
        None => default,
        Some(Value::Null) => false, // Python: value is None -> False
        Some(Value::Bool(b)) => *b,
        Some(other) => {
            let s = py_str(other).trim().to_lowercase();
            !matches!(s.as_str(), "0" | "false" | "no" | "off" | "")
        }
    }
}

/// `str(value)` Python-style for the scalar JSON kinds `_bool` may see.
fn py_str(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// `int(value or 0)` for the clean path — returns `Some(n)` on success, `None`
/// on the Python `(TypeError, ValueError)` path so the caller can fall back to
/// `base`. Mirrors `int(data.get(key) or 0)`.
fn clean_int(value: Option<&Value>) -> Option<i64> {
    match value {
        None | Some(Value::Null) => Some(0), // `None or 0` -> 0
        Some(Value::Bool(b)) => Some(if *b { 1 } else { 0 }), // int(True/False)
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else {
                n.as_f64().map(|f| f.trunc() as i64)
            }
        }
        Some(Value::String(s)) => {
            let t = s.trim();
            if t.is_empty() {
                // "" or 0 -> falsy -> `"" or 0` -> 0
                Some(0)
            } else {
                // Python int("123"); int("1.5") raises ValueError -> None
                t.parse::<i64>().ok()
            }
        }
        Some(Value::Array(_)) | Some(Value::Object(_)) => None, // int(list) -> TypeError
    }
}

/// `int(value or 0)` for the read accessors (`max_tool_hops`/`max_output_tokens`
/// at read time). Same coercion, returns `None` on the error path.
fn int_of(value: Option<&Value>) -> Option<i64> {
    clean_int(value)
}

fn is_number_zero(n: &serde_json::Number) -> bool {
    if let Some(i) = n.as_i64() {
        i == 0
    } else if let Some(u) = n.as_u64() {
        u == 0
    } else if let Some(f) = n.as_f64() {
        f == 0.0
    } else {
        false
    }
}

fn is_empty_value(v: &Value) -> bool {
    // Python falsiness for the `model.get(a) or model.get(b)` chain.
    match v {
        Value::Null => true,
        Value::Bool(b) => !*b,
        Value::String(s) => s.is_empty(),
        Value::Number(n) => is_number_zero(n),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
    }
}

// --- defaults computation (the Python `_DEFAULTS` block) --------------------

fn compute_defaults(cfg: &Config) -> (SettingsMap, String) {
    // _BASE_REASONING_EFFORT = (config.CODEX_REASONING_EFFORT or "low").strip().lower() or "low"
    let base_effort = {
        let v = cfg.codex_reasoning_effort.trim().to_lowercase();
        if v.is_empty() {
            "low".to_string()
        } else {
            v
        }
    };
    // _BASE_REASONING_SUMMARY = (config.CODEX_REASONING_SUMMARY or "auto").strip().lower() or "auto"
    let base_summary = {
        let v = cfg.codex_reasoning_summary.trim().to_lowercase();
        if v.is_empty() {
            "auto".to_string()
        } else {
            v
        }
    };

    // _ROLE_REASONING_BASE: active generation roles inherit base; compact = ("none","none").
    let role_base = |role: &str| -> (String, String) {
        if role == config::Role::Compact.as_str() {
            ("none".to_string(), "none".to_string())
        } else {
            (base_effort.clone(), base_summary.clone())
        }
    };

    // _role_default(role, kind, base): env GM_<ROLE>_REASONING_<KIND>, .strip().lower() or base
    let role_default = |role: &str, kind: &str, base: &str| -> String {
        let var = format!(
            "GM_{}_REASONING_{}",
            role.to_uppercase(),
            kind.to_uppercase()
        );
        let env = std::env::var(&var).unwrap_or_else(|_| base.to_string());
        let v = env.trim().to_lowercase();
        if v.is_empty() {
            base.to_string()
        } else {
            v
        }
    };

    let mut defaults: SettingsMap = BTreeMap::new();
    for role in reasoning_role_keys() {
        let (eff_base, sum_base) = role_base(role);
        defaults.insert(
            format!("{role}_reasoning_effort"),
            Value::from(role_default(role, "effort", &eff_base)),
        );
        defaults.insert(
            format!("{role}_reasoning_summary"),
            Value::from(role_default(role, "summary", &sum_base)),
        );
    }

    // text_verbosity = GM_TEXT_VERBOSITY.strip().lower() or "default"
    defaults.insert(
        "text_verbosity".into(),
        Value::from(env_lower_or("GM_TEXT_VERBOSITY", "default")),
    );
    // tool_choice = GM_TOOL_CHOICE.strip().lower() or "auto"
    defaults.insert(
        "tool_choice".into(),
        Value::from(env_lower_or("GM_TOOL_CHOICE", "auto")),
    );
    defaults.insert(
        "stream_gm_content".into(),
        Value::from(cfg.stream_gm_content),
    );
    defaults.insert(
        "parallel_tool_calls".into(),
        Value::from(config::env_bool("GM_PARALLEL_TOOL_CALLS", true)),
    );
    defaults.insert(
        "gm_suggest_options".into(),
        Value::from(config::env_bool("GM_SUGGEST_OPTIONS", false)),
    );
    defaults.insert(
        "tts_enabled".into(),
        Value::from(config::env_bool("GM_TTS_ENABLED", false)),
    );
    defaults.insert(
        "tts_autoplay".into(),
        Value::from(config::env_bool("GM_TTS_AUTOPLAY", false)),
    );
    defaults.insert("max_tool_hops".into(), Value::from(0i64));
    // max_output_tokens = int(config.MAX_TOKENS or 0)
    defaults.insert(
        "max_output_tokens".into(),
        Value::from(cfg.max_tokens.max(0)), // int(MAX_TOKENS or 0)
    );

    (defaults, base_summary)
}

fn env_lower_or(name: &str, default: &str) -> String {
    let v = std::env::var(name).unwrap_or_else(|_| default.to_string());
    let v = v.trim().to_lowercase();
    if v.is_empty() {
        default.to_string()
    } else {
        v
    }
}

// --- settings path resolution ----------------------------------------------

/// Default settings-path resolution. Honors `GM_SETTINGS_PATH`; otherwise uses
/// the `directories` config dir (PORT_PLAN §3.2 — a deliberate change from the
/// Python "next to source" placement). Falls back to the current dir.
pub fn default_settings_path() -> PathBuf {
    if let Ok(p) = std::env::var("GM_SETTINGS_PATH") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if let Some(dirs) = directories::ProjectDirs::from("", "", "gm-lab") {
        return dirs.config_dir().join("gm_lab_settings.json");
    }
    PathBuf::from("gm_lab_settings.json")
}

// --- JSON serialization matching Python json.dumps(indent=2, sort_keys=True) -

/// Serialize a settings map exactly as Python `json.dumps(data,
/// ensure_ascii=False, indent=2, sort_keys=True)` would (without the trailing
/// newline, which the caller adds). Keys are already sorted via [`BTreeMap`].
///
/// Python's indented form uses `",\n"` item separators and `": "` key/value
/// separators, two-space indentation, and `ensure_ascii=False` (raw UTF-8).
/// serde_json's pretty printer matches this for scalar values, which is all the
/// settings dict ever contains.
pub fn serialize_sorted_pretty(data: &SettingsMap) -> String {
    // Build an ordered serde_json Map; with the `preserve_order` feature the
    // Map keeps our (sorted) insertion order, matching sort_keys=True.
    let mut map = Map::new();
    for (k, v) in data {
        map.insert(k.clone(), v.clone());
    }
    let value = Value::Object(map);
    // serde_json pretty == 2-space indent, ": " and ",\n" separators, raw UTF-8.
    serde_json::to_string_pretty(&value).expect("settings serialize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_settings() -> (RuntimeSettings, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = dir.path().join(format!("settings_{n}.json"));
        // Use a Config with codex base effort "low"/"auto" (Python defaults),
        // built from a clean env for the role-default vars.
        for role in ["GM", "NPC", "COMPACT", "LOCATION"] {
            std::env::remove_var(format!("GM_{role}_REASONING_EFFORT"));
            std::env::remove_var(format!("GM_{role}_REASONING_SUMMARY"));
        }
        std::env::remove_var("GM_CODEX_REASONING_EFFORT");
        std::env::remove_var("GM_CODEX_REASONING_SUMMARY");
        std::env::remove_var("GM_TEXT_VERBOSITY");
        std::env::remove_var("GM_TOOL_CHOICE");
        std::env::remove_var("GM_MAX_TOKENS");
        let cfg = Config::from_env();
        (RuntimeSettings::new(&cfg, path), dir)
    }

    #[test]
    fn defaults_shape_matches_python() {
        let (rs, _d) = temp_settings();
        let d = rs.defaults();
        assert_eq!(d.get("gm_reasoning_effort"), Some(&Value::from("low")));
        assert_eq!(d.get("gm_reasoning_summary"), Some(&Value::from("auto")));
        assert_eq!(d.get("npc_reasoning_effort"), Some(&Value::from("low")));
        assert_eq!(
            d.get("location_reasoning_effort"),
            Some(&Value::from("low"))
        );
        assert_eq!(
            d.get("location_reasoning_summary"),
            Some(&Value::from("auto"))
        );
        assert_eq!(
            d.get("compact_reasoning_effort"),
            Some(&Value::from("none"))
        );
        assert_eq!(
            d.get("compact_reasoning_summary"),
            Some(&Value::from("none"))
        );
        assert_eq!(d.get("text_verbosity"), Some(&Value::from("default")));
        assert_eq!(d.get("tool_choice"), Some(&Value::from("auto")));
        assert_eq!(d.get("stream_gm_content"), Some(&Value::from(true)));
        assert_eq!(d.get("parallel_tool_calls"), Some(&Value::from(true)));
        assert_eq!(d.get("gm_suggest_options"), Some(&Value::from(false)));
        assert_eq!(d.get("tts_enabled"), Some(&Value::from(false)));
        assert_eq!(d.get("tts_autoplay"), Some(&Value::from(false)));
        assert_eq!(d.get("max_tool_hops"), Some(&Value::from(0i64)));
        assert_eq!(d.get("max_output_tokens"), Some(&Value::from(0i64)));
    }

    #[test]
    fn clamp_and_normalize_int_knobs() {
        let (rs, _d) = temp_settings();
        // over-cap clamps
        let mut v = Map::new();
        v.insert("max_tool_hops".into(), Value::from(9999));
        v.insert("max_output_tokens".into(), Value::from(10_000_000_000i64));
        let out = rs.update(Some(&v));
        assert_eq!(out.get("max_tool_hops"), Some(&Value::from(100i64)));
        assert_eq!(
            out.get("max_output_tokens"),
            Some(&Value::from(rs.max_output_tokens_cap))
        );
        // negative clamps to 0
        let mut v2 = Map::new();
        v2.insert("max_tool_hops".into(), Value::from(-5));
        let out2 = rs.update(Some(&v2));
        assert_eq!(out2.get("max_tool_hops"), Some(&Value::from(0i64)));
        // invalid (non-int string) -> falls back to base value (0)
        let mut v3 = Map::new();
        v3.insert("max_tool_hops".into(), Value::from("not-a-number"));
        let out3 = rs.update(Some(&v3));
        assert_eq!(out3.get("max_tool_hops"), Some(&Value::from(0i64)));
    }

    #[test]
    fn effort_none_forces_summary_none() {
        let (rs, _d) = temp_settings();
        let mut v = Map::new();
        v.insert("gm_reasoning_effort".into(), Value::from("none"));
        v.insert("gm_reasoning_summary".into(), Value::from("detailed"));
        let out = rs.update(Some(&v));
        assert_eq!(out.get("gm_reasoning_effort"), Some(&Value::from("none")));
        assert_eq!(out.get("gm_reasoning_summary"), Some(&Value::from("none")));
        // role_settings reflects it
        let rsx = rs.role_settings("gm", Some(&out));
        assert_eq!(rsx.effort, "none");
        assert_eq!(rsx.summary, "none");
    }

    #[test]
    fn invalid_values_dropped_by_clean() {
        let (rs, _d) = temp_settings();
        let mut v = Map::new();
        v.insert("tool_choice".into(), Value::from("bogus")); // not in TOOL_CHOICES
        v.insert("text_verbosity".into(), Value::from("ULTRA")); // not valid
        v.insert("gm_reasoning_summary".into(), Value::from("weird")); // not a summary
        let out = rs.update(Some(&v));
        // unchanged defaults retained
        assert_eq!(out.get("tool_choice"), Some(&Value::from("auto")));
        assert_eq!(out.get("text_verbosity"), Some(&Value::from("default")));
        assert_eq!(out.get("gm_reasoning_summary"), Some(&Value::from("auto")));
    }

    #[test]
    fn legacy_migration_maps_to_active_generation_roles_only() {
        let (rs, _d) = temp_settings();
        // Write a legacy-shaped file then load it.
        let body = r#"{"reasoning_effort":"high","reasoning_summary":"detailed"}"#;
        std::fs::write(rs.settings_path(), body).unwrap();
        let loaded = rs.get();
        assert_eq!(
            loaded.get("gm_reasoning_effort"),
            Some(&Value::from("high"))
        );
        assert_eq!(
            loaded.get("gm_reasoning_summary"),
            Some(&Value::from("detailed"))
        );
        assert_eq!(
            loaded.get("npc_reasoning_effort"),
            Some(&Value::from("high"))
        );
        assert_eq!(
            loaded.get("npc_reasoning_summary"),
            Some(&Value::from("detailed"))
        );
        assert_eq!(
            loaded.get("location_reasoning_effort"),
            Some(&Value::from("high"))
        );
        assert_eq!(
            loaded.get("location_reasoning_summary"),
            Some(&Value::from("detailed"))
        );
        // compact keeps its new default (none/none), NOT migrated
        assert_eq!(
            loaded.get("compact_reasoning_effort"),
            Some(&Value::from("none"))
        );
        assert_eq!(
            loaded.get("compact_reasoning_summary"),
            Some(&Value::from("none"))
        );
    }

    #[test]
    fn json_roundtrip_sorted_keys_and_trailing_newline() {
        let (rs, _d) = temp_settings();
        let mut v = Map::new();
        v.insert("gm_suggest_options".into(), Value::from(true));
        v.insert("text_verbosity".into(), Value::from("high"));
        rs.update(Some(&v));

        let on_disk = std::fs::read_to_string(rs.settings_path()).unwrap();
        // trailing newline present
        assert!(on_disk.ends_with("\n"));
        // 2-space indent
        assert!(on_disk.contains("\n  \"compact_reasoning_effort\""));
        // keys appear in sorted order: compact_* before gm_*
        let i_compact = on_disk.find("compact_reasoning_effort").unwrap();
        let i_gm = on_disk.find("gm_reasoning_effort").unwrap();
        let i_location = on_disk.find("location_reasoning_effort").unwrap();
        let i_npc = on_disk.find("npc_reasoning_effort").unwrap();
        assert!(i_compact < i_gm && i_gm < i_location && i_location < i_npc);

        // Reload into a fresh instance -> identical normalized map
        let cfg = Config::from_env();
        let rs2 = RuntimeSettings::new(&cfg, rs.settings_path().to_path_buf());
        let reloaded = rs2.get();
        assert_eq!(reloaded.get("gm_suggest_options"), Some(&Value::from(true)));
        assert_eq!(reloaded.get("text_verbosity"), Some(&Value::from("high")));
    }

    #[test]
    fn serialize_matches_python_known_example() {
        // Reproduce the on-disk gm_lab_settings.json example byte shape (sorted,
        // 2-space, raw UTF-8). We just check the exact serialization of a known
        // map equals the expected pretty string (without trailing newline).
        let mut m: SettingsMap = BTreeMap::new();
        m.insert("b_flag".into(), Value::from(true));
        m.insert("a_num".into(), Value::from(0i64));
        m.insert("c_str".into(), Value::from("Ива")); // raw cyrillic, not \u
        let s = serialize_sorted_pretty(&m);
        assert_eq!(
            s,
            "{\n  \"a_num\": 0,\n  \"b_flag\": true,\n  \"c_str\": \"Ива\"\n}"
        );
        assert!(!s.contains("\\u"));
    }

    #[test]
    fn reconcile_supports_false_forces_none() {
        let (rs, _d) = temp_settings();
        // start from defaults (active generation roles effort=low)
        rs.get();
        let mut model = Map::new();
        model.insert("supports_reasoning_summaries".into(), Value::from(false));
        let out = rs.reconcile_for_model(Some(&model));
        assert_eq!(out.get("gm_reasoning_effort"), Some(&Value::from("none")));
        assert_eq!(out.get("gm_reasoning_summary"), Some(&Value::from("none")));
        assert_eq!(out.get("npc_reasoning_effort"), Some(&Value::from("none")));
        assert_eq!(
            out.get("location_reasoning_effort"),
            Some(&Value::from("none"))
        );
        assert_eq!(
            out.get("location_reasoning_summary"),
            Some(&Value::from("none"))
        );
        assert_eq!(
            out.get("compact_reasoning_effort"),
            Some(&Value::from("none"))
        );
    }

    #[test]
    fn reconcile_unsupported_effort_resets_to_default() {
        let (rs, _d) = temp_settings();
        // set gm effort to "xhigh"
        let mut v = Map::new();
        v.insert("gm_reasoning_effort".into(), Value::from("xhigh"));
        rs.update(Some(&v));
        // model supports only low/medium, default level medium
        let mut model = Map::new();
        model.insert(
            "supported_reasoning_levels".into(),
            Value::from(vec![Value::from("low"), Value::from("medium")]),
        );
        model.insert("default_reasoning_level".into(), Value::from("medium"));
        let out = rs.reconcile_for_model(Some(&model));
        assert_eq!(out.get("gm_reasoning_effort"), Some(&Value::from("medium")));
    }

    #[test]
    fn reconcile_non_dict_returns_get() {
        let (rs, _d) = temp_settings();
        let out = rs.reconcile_for_model(None);
        assert_eq!(out, rs.get());
    }

    #[test]
    fn supported_efforts_dict_and_scalar_items() {
        let mut model = Map::new();
        model.insert(
            "reasoning_efforts".into(),
            Value::from(vec![
                Value::from("low"),
                serde_json::json!({"effort": "high"}),
                serde_json::json!({"id": "medium"}),
                Value::from("low"), // dup ignored
            ]),
        );
        let out = supported_reasoning_efforts(Some(&model));
        assert_eq!(out, vec!["low", "high", "medium"]);
    }

    #[test]
    fn per_request_accessors() {
        let (rs, _d) = temp_settings();
        rs.get(); // defaults: gm effort low, summary auto
                  // reasoning_for_request(true, "gm") -> {effort:low, summary:auto}
        let r = rs.reasoning_for_request(Some(true), "gm").unwrap();
        assert_eq!(r.get("effort"), Some(&Value::from("low")));
        assert_eq!(r.get("summary"), Some(&Value::from("auto")));
        // think false -> None
        assert!(rs.reasoning_for_request(Some(false), "gm").is_none());
        // reasoning_enabled
        assert!(rs.reasoning_enabled(Some(true), "gm"));
        assert!(!rs.reasoning_enabled(Some(false), "gm"));
        assert!(rs.role_reasoning_enabled("gm"));
        // compact effort none -> disabled
        assert!(!rs.role_reasoning_enabled("compact"));
        // tool_choice
        assert_eq!(rs.tool_choice_for_request(false), "none");
        assert_eq!(rs.tool_choice_for_request(true), "auto");
        // parallel
        assert!(rs.parallel_tool_calls_for_request(true));
        assert!(!rs.parallel_tool_calls_for_request(false));
    }
}
