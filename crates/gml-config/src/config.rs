//! Static startup configuration — faithful port of `gm-lab/config.py`.
//!
//! `config.py` loads a local `.env` once (without overriding the real
//! environment), then reads ~90 environment variables into module-level
//! constants. Here that becomes:
//!
//! - [`load_dotenv`] — hand-rolled `.env` loader matching `_load_dotenv`
//!   byte-for-byte (real env wins, leading `export ` strip, key regex, quote
//!   strip, inline ` #` comment strip, exe/config dir then cwd, dedup). NOT
//!   `dotenvy` — its precedence/comment semantics differ.
//! - [`env_bool`] — port of `_env_bool` (false set = `{"0","false","no","off",""}`).
//! - [`Config`] — an immutable struct built once, holding every env-derived
//!   constant with the EXACT env var names and defaults from `config.py`.
//!
//! Roles live in `gml_types::Role` / `REASONING_ROLES` (the single source of
//! truth, ported from `config.ROLE_GM/NPC/COMPACT`); they are re-exported here
//! for convenience but never redefined.

use std::env;
use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;

pub use gml_types::{Role, REASONING_ROLES};

/// Sampling preset (`config.SAMPLING_THINK` / `SAMPLING_PLAIN`).
///
/// Values are stored exactly as the Python dicts. `min_p` is `0` in Python
/// (an int); `presence_penalty` is `1.5`. We keep them as `f64` to match the
/// JSON shape sent to the backend.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SamplingPreset {
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: i64,
    pub min_p: f64,
    pub presence_penalty: f64,
}

// config.SAMPLING_THINK = {"temperature": 0.6, "top_p": 0.95, "top_k": 20,
//                          "min_p": 0, "presence_penalty": 1.5}
pub const SAMPLING_THINK: SamplingPreset = SamplingPreset {
    temperature: 0.6,
    top_p: 0.95,
    top_k: 20,
    min_p: 0.0,
    presence_penalty: 1.5,
};

// config.SAMPLING_PLAIN = {"temperature": 0.7, "top_p": 0.80, "top_k": 20,
//                          "min_p": 0, "presence_penalty": 1.5}
pub const SAMPLING_PLAIN: SamplingPreset = SamplingPreset {
    temperature: 0.7,
    top_p: 0.80,
    top_k: 20,
    min_p: 0.0,
    presence_penalty: 1.5,
};

static KEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").expect("valid key regex"));

/// Load `.env` files into the process environment, without overriding real
/// environment variables. Faithful port of `config._load_dotenv`.
///
/// Search order mirrors Python:
///   1. `<exe dir>/.env`  (Python: `Path(__file__).with_name(".env")`)
///   2. `<cwd>/.env`      (Python: `Path.cwd() / ".env"`)
///
/// Paths are deduped (a single combined dir == cwd is only processed once).
/// For each existing file, lines are stripped; blank lines and `#` comments are
/// skipped; a leading `export ` is stripped and re-`lstrip`ped; the line is
/// split on the first `=`; the key is trimmed and validated against
/// `^[A-Za-z_][A-Za-z0-9_]*$` and skipped if already present in the
/// environment; the value is trimmed, then if it is wrapped in matching
/// single/double quotes (len >= 2) the quotes are stripped, otherwise a
/// trailing ` #` inline comment is stripped and the result re-trimmed.
///
/// In Python `__file__` is the source dir; the closest Rust analogue for a
/// shipped binary is the executable's directory (PORT_PLAN §3.2: "exe/config
/// dir then cwd"). We use the executable's parent directory.
pub fn load_dotenv() {
    let mut paths: Vec<PathBuf> = Vec::new();

    if let Some(exe_dir) = exe_dir() {
        paths.push(exe_dir.join(".env"));
    }
    if let Ok(cwd) = env::current_dir() {
        let cwd_env = cwd.join(".env");
        // Python dedups by Path equality before checking existence.
        if !paths.contains(&cwd_env) {
            paths.push(cwd_env);
        }
    }

    for path in paths {
        load_dotenv_file(&path);
    }
}

fn exe_dir() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// Parse and apply a single `.env` file. Public for testing; honors the same
/// "real env wins" rule by checking [`env::var_os`] per key.
pub fn load_dotenv_file(path: &Path) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return, // mirrors `if not path.exists(): continue`
    };
    for (key, value) in parse_dotenv(&content) {
        // Python: `key in os.environ` — skip if already set (real env wins).
        if env::var_os(&key).is_none() {
            env::set_var(&key, &value);
        }
    }
}

/// Pure parser for `.env` content — returns the `(key, value)` pairs the
/// Python loader would have considered (real-env precedence is applied by the
/// caller). Faithful to `_load_dotenv`'s line handling.
pub fn parse_dotenv(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    // Python str.splitlines() splits on \n, \r, \r\n, and other unicode line
    // boundaries; for .env files the practical set is \n / \r\n. Normalize \r\n
    // and split on \n / \r.
    for raw in split_python_lines(content) {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = if let Some(rest) = line.strip_prefix("export ") {
            // Python: line[len("export "):].lstrip()
            trim_start_python(rest)
        } else {
            line
        };
        let Some((key_raw, value_raw)) = line.split_once('=') else {
            continue; // `if "=" not in line: continue`
        };
        let key = key_raw.trim();
        if !KEY_RE.is_match(key) {
            continue;
        }
        let value = value_raw.trim();
        let value: String = {
            let bytes: Vec<char> = value.chars().collect();
            if bytes.len() >= 2
                && bytes[0] == bytes[bytes.len() - 1]
                && (bytes[0] == '\'' || bytes[0] == '"')
            {
                // strip surrounding matching quotes
                bytes[1..bytes.len() - 1].iter().collect()
            } else {
                // strip trailing inline " #" comment, then trim
                let v = value.split(" #").next().unwrap_or("");
                v.trim().to_string()
            }
        };
        out.push((key.to_string(), value));
    }
    out
}

fn split_python_lines(s: &str) -> Vec<&str> {
    // Handle \r\n by treating \r and \n as separators; collapse the empty piece
    // produced between \r and \n. Practical equivalent of str.splitlines().
    let mut lines = Vec::new();
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\n' || b == b'\r' {
            lines.push(&s[start..i]);
            if b == b'\r' && i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                i += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

fn trim_start_python(s: &str) -> &str {
    s.trim_start()
}

/// Port of `config._env_bool`.
///
/// Returns `default` if the variable is unset. Otherwise returns `true` unless
/// the stripped, lowercased value is one of `{"0","false","no","off",""}`.
pub fn env_bool(name: &str, default: bool) -> bool {
    match env::var(name) {
        Err(_) => default, // includes NotUnicode? -> see note below
        Ok(raw) => {
            let v = raw.trim().to_lowercase();
            !matches!(v.as_str(), "0" | "false" | "no" | "off" | "")
        }
    }
}

// --- small env helpers replicating Python's `int(... or "0")` idioms --------

fn env_str(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_string())
}

/// Python `int(os.environ.get(name, default))` — no `or` fallback; a present
/// but non-int value would raise in Python. We mirror by falling back to the
/// default's parse on error (defaults are always valid ints).
fn env_int_strict(name: &str, default: i64) -> i64 {
    match env::var(name) {
        Ok(v) => v.trim().parse::<i64>().unwrap_or(default),
        Err(_) => default,
    }
}

/// Python `int(os.environ.get(name, "d") or "d")` — empty string falls back to
/// the default before parsing.
fn env_int_or(name: &str, default: i64) -> i64 {
    let raw = env::var(name).unwrap_or_default();
    let raw = if raw.is_empty() {
        default.to_string()
    } else {
        raw
    };
    raw.trim().parse::<i64>().unwrap_or(default)
}

/// Python `float(os.environ.get(name, "d") or "d")`.
fn env_float_or(name: &str, default: f64) -> f64 {
    let raw = env::var(name).unwrap_or_default();
    let raw = if raw.is_empty() {
        default.to_string()
    } else {
        raw
    };
    raw.trim().parse::<f64>().unwrap_or(default)
}

fn rstrip_slash(s: &str) -> String {
    s.trim_end_matches('/').to_string()
}

/// Immutable startup configuration. Build once via [`Config::from_env`] (after
/// calling [`load_dotenv`]). Field names mirror the `config.py` constants.
#[derive(Debug, Clone)]
pub struct Config {
    // --- Модель ---
    pub model: String,                   // GM_MODEL, default ""
    pub llama_host: String,              // LLAMA_HOST
    pub api_base: String,                // GM_API_BASE (rstrip '/')
    pub api_key: String,                 // GM_API_KEY
    pub max_tokens: i64,                 // GM_MAX_TOKENS, "0" or "0"
    pub max_output_tokens_cap: i64,      // GM_MAX_OUTPUT_TOKENS_CAP, 200000
    pub backend: String,                 // GM_BACKEND, "codex"
    pub use_llama_template_kwargs: bool, // GM_LLAMA_TEMPLATE_KWARGS, default (backend=="llamacpp")
    pub stream_gm_content: bool,         // GM_STREAM_GM_CONTENT, true

    // --- History / compaction ---
    pub gm_history_tokens: i64,       // GM_HISTORY_TOKENS, 100000
    pub gm_keep_turns: i64,           // GM_KEEP_TURNS, 3
    pub npc_history_tokens: i64,      // NPC_HISTORY_TOKENS, 64000
    pub npc_keep_exchanges: i64,      // NPC_KEEP_EXCHANGES, 6
    pub compact_model: String,        // GM_COMPACT_MODEL, "" = use main model
    pub events_cap: i64,              // GM_EVENTS_CAP, 400
    pub rumors_cap: i64,              // GM_RUMORS_CAP, 80
    pub chars_per_token: i64,         // GM_CHARS_PER_TOKEN, 3
    pub compact_input_chars: i64,     // GM_COMPACT_INPUT_CHARS, 12000
    pub prelude_callbrief_chars: i64, // GM_PRELUDE_CALLBRIEF_CHARS, 4000

    // --- RAG / embeddings ---
    pub rag_enabled: bool,            // GM_RAG_ENABLED, true
    pub rag_embeddings_url: String,   // GM_RAG_EMBEDDINGS_URL
    pub rag_embeddings_model: String, // GM_RAG_EMBEDDINGS_MODEL
    pub rag_encoding_format: String,  // GM_RAG_ENCODING_FORMAT, base64
    pub rag_cache_path: String,       // GM_RAG_CACHE_PATH
    // Directory holding per-world embedding caches (`<id>.sqlite3`), routed by
    // `world.world_ref`. GM_RAG_WORLDS_DIR, default `<data_dir>/rag_worlds`.
    // Resolved INDEPENDENTLY of `rag_cache_path` (NOT derived from its parent):
    // tests point the global cache into %TEMP% and must not drag the per-world
    // dir along; this dir also never lives under `library/` (export privacy).
    pub rag_worlds_dir: String, // GM_RAG_WORLDS_DIR
    pub rag_batch_size: i64,          // GM_RAG_BATCH_SIZE, 16
    pub rag_timeout_seconds: f64,     // GM_RAG_TIMEOUT_SECONDS, 20
    pub rag_top_k: i64,               // GM_RAG_TOP_K, 4 (final facts to the prompt)
    pub rag_min_dense_score: f64,     // GM_RAG_MIN_DENSE_SCORE, 0.60
    pub rag_rrf_k: i64,               // GM_RAG_RRF_K, 60
    pub rag_keyword_tiebreak: f64,    // GM_RAG_KEYWORD_TIEBREAK, 0.002
    pub rag_dense_tiebreak: f64,      // GM_RAG_DENSE_TIEBREAK, 0.001
    pub rag_status_boost: f64,        // GM_RAG_STATUS_BOOST, 1.04
    pub rag_fact_select_k: i64,       // GM_RAG_FACT_SELECT_K, 4
    pub rag_rerank_url: String,       // GM_RAG_RERANK_URL (unified sidecar /rerank)
    pub rag_rerank_model: String,     // GM_RAG_RERANK_MODEL
    pub rag_rerank_enabled: bool,     // GM_RAG_RERANK_ENABLED, true
    pub rag_rerank_candidates: i64, // GM_RAG_RERANK_CANDIDATES, 64 (RRF top-N fed to the reranker)

    // --- Unified inference sidecar ---
    pub infer_base_url: String, // GM_INFER_URL — single source for the sidecar URLs
    pub embedder_quant: String, // GM_EMBEDDER_QUANT, bf16 | nf4 (default bf16)
    pub reranker_quant: String, // GM_RERANKER_QUANT, bf16 | nf4 (default bf16)
    pub image_enabled: bool,    // GM_IMAGE_ENABLED, true
    pub image_timeout_seconds: f64, // GM_IMAGE_TIMEOUT_SECONDS, 300
    pub image_max_width: i64,   // GM_IMAGE_MAX_WIDTH, 2048
    pub image_max_height: i64,  // GM_IMAGE_MAX_HEIGHT, 2048
    pub image_max_batch: i64,   // GM_IMAGE_MAX_BATCH, 4
    pub image_max_steps: i64,   // GM_IMAGE_MAX_STEPS, 50

    // --- Prompt cache hints ---
    pub prompt_cache_key: String,       // GM_PROMPT_CACHE_KEY, ""
    pub prompt_cache_retention: String, // GM_PROMPT_CACHE_RETENTION, ""
    pub llama_cache_reuse: i64,         // GM_LLAMA_CACHE_REUSE, 0

    // --- ChatGPT Codex OAuth ---
    pub codex_base_url: String,          // GM_CODEX_BASE_URL (rstrip '/')
    pub codex_client_id: String,         // GM_CODEX_CLIENT_ID
    pub codex_client_version: String,    // GM_CODEX_CLIENT_VERSION
    pub codex_originator: String,        // GM_CODEX_ORIGINATOR
    pub codex_user_agent: String,        // GM_CODEX_USER_AGENT, default codex_cli_rs/{ver} (GM-Lab)
    pub codex_auth_port: i64,            // GM_CODEX_AUTH_PORT, 1455
    pub codex_auto_open_browser: bool,   // GM_CODEX_AUTO_OPEN_BROWSER, true
    pub codex_model: String,             // GM_CODEX_MODEL, gpt-5.4-mini
    pub codex_compact_model: String,     // GM_CODEX_COMPACT_MODEL, default COMPACT_MODEL
    pub codex_reasoning_effort: String,  // GM_CODEX_REASONING_EFFORT, "low"
    pub codex_reasoning_summary: String, // GM_CODEX_REASONING_SUMMARY, "auto"
    pub codex_prompt_cache_key: String,  // GM_CODEX_PROMPT_CACHE_KEY, default PROMPT_CACHE_KEY

    // --- Reasoning toggles by role ---
    pub gm_think: bool,  // GM_THINK, true
    pub npc_think: bool, // NPC_THINK, false
}

impl Config {
    /// Build the configuration from the current process environment, exactly as
    /// `config.py` does at import time. Call [`load_dotenv`] first if you want
    /// `.env` files honored (matching Python's module-init `_load_dotenv()`).
    pub fn from_env() -> Self {
        let max_output_tokens_cap = env_int_or("GM_MAX_OUTPUT_TOKENS_CAP", 200_000);

        let backend = env_str("GM_BACKEND", "codex");
        let use_llama_template_kwargs = env_bool("GM_LLAMA_TEMPLATE_KWARGS", backend == "llamacpp");

        let prompt_cache_key = env_str("GM_PROMPT_CACHE_KEY", "");

        let codex_client_version = env_str("GM_CODEX_CLIENT_VERSION", "0.133.0");
        // default: f"codex_cli_rs/{CODEX_CLIENT_VERSION} (GM-Lab)"
        let codex_user_agent = env_str(
            "GM_CODEX_USER_AGENT",
            &format!("codex_cli_rs/{codex_client_version} (GM-Lab)"),
        );

        // CODEX_REASONING_EFFORT = env.get(..., "low").strip() or "low"
        let codex_reasoning_effort = {
            let v = env_str("GM_CODEX_REASONING_EFFORT", "low");
            let v = v.trim().to_string();
            if v.is_empty() {
                "low".to_string()
            } else {
                v
            }
        };
        // CODEX_REASONING_SUMMARY = env.get(..., "auto").strip().lower() or "auto"
        let codex_reasoning_summary = {
            let v = env_str("GM_CODEX_REASONING_SUMMARY", "auto");
            let v = v.trim().to_lowercase();
            if v.is_empty() {
                "auto".to_string()
            } else {
                v
            }
        };

        let codex_prompt_cache_key = env_str("GM_CODEX_PROMPT_CACHE_KEY", &prompt_cache_key);

        // RAG_CACHE_PATH default: str(Path(__file__).with_name("gm_lab_embeddings.sqlite3"))
        let rag_cache_path = match env::var("GM_RAG_CACHE_PATH") {
            Ok(v) => v,
            Err(_) => default_data_path("gm_lab_embeddings.sqlite3"),
        };
        // Per-world cache dir: resolved off the data dir like `rag_cache_path`,
        // NOT off that path's parent (see field doc / RAG_PER_WORLD_TZ §2.1, §5).
        let rag_worlds_dir = match env::var("GM_RAG_WORLDS_DIR") {
            Ok(v) if !v.trim().is_empty() => v,
            _ => default_data_path("rag_worlds"),
        };

        // Single source of truth for the unified sidecar location: the embeddings
        // and rerank URLs default off this base, so moving GM_INFER_URL moves both.
        let infer_base_url = rstrip_slash(&env_str("GM_INFER_URL", "http://127.0.0.1:8077"));
        let compact_model = env_str("GM_COMPACT_MODEL", "");

        Config {
            model: env_str("GM_MODEL", ""),
            llama_host: env_str("LLAMA_HOST", "http://localhost:8080"),
            api_base: rstrip_slash(&env_str("GM_API_BASE", "")),
            api_key: env_str("GM_API_KEY", ""),
            max_tokens: env_int_or("GM_MAX_TOKENS", 0),
            max_output_tokens_cap,
            backend,
            use_llama_template_kwargs,
            stream_gm_content: env_bool("GM_STREAM_GM_CONTENT", true),

            gm_history_tokens: env_int_strict("GM_HISTORY_TOKENS", 100_000),
            gm_keep_turns: env_int_strict("GM_KEEP_TURNS", 3),
            npc_history_tokens: env_int_strict("NPC_HISTORY_TOKENS", 64_000),
            npc_keep_exchanges: env_int_strict("NPC_KEEP_EXCHANGES", 6),
            compact_model: compact_model.clone(),
            events_cap: env_int_strict("GM_EVENTS_CAP", 400),
            rumors_cap: env_int_strict("GM_RUMORS_CAP", 80),
            chars_per_token: env_int_or("GM_CHARS_PER_TOKEN", 3),
            compact_input_chars: env_int_or("GM_COMPACT_INPUT_CHARS", 12_000),
            prelude_callbrief_chars: env_int_or("GM_PRELUDE_CALLBRIEF_CHARS", 4_000),

            rag_enabled: env_bool("GM_RAG_ENABLED", true),
            // Unified inference sidecar (serve.py) hosts embeddings + rerank; both
            // URLs default off GM_INFER_URL so they always agree on host:port.
            rag_embeddings_url: env_str(
                "GM_RAG_EMBEDDINGS_URL",
                &format!("{infer_base_url}/v1/embeddings"),
            ),
            rag_embeddings_model: env_str("GM_RAG_EMBEDDINGS_MODEL", "Qwen/Qwen3-Embedding-0.6B"),
            rag_encoding_format: env_str("GM_RAG_ENCODING_FORMAT", "base64"),
            rag_cache_path,
            rag_worlds_dir,
            rag_batch_size: env_int_or("GM_RAG_BATCH_SIZE", 16),
            rag_timeout_seconds: env_float_or("GM_RAG_TIMEOUT_SECONDS", 20.0),
            rag_top_k: env_int_or("GM_RAG_TOP_K", 4),
            rag_min_dense_score: env_float_or("GM_RAG_MIN_DENSE_SCORE", 0.60),
            rag_rrf_k: env_int_or("GM_RAG_RRF_K", 60),
            rag_keyword_tiebreak: env_float_or("GM_RAG_KEYWORD_TIEBREAK", 0.002),
            rag_dense_tiebreak: env_float_or("GM_RAG_DENSE_TIEBREAK", 0.001),
            rag_status_boost: env_float_or("GM_RAG_STATUS_BOOST", 1.04),
            rag_fact_select_k: env_int_or("GM_RAG_FACT_SELECT_K", 4),
            rag_rerank_url: env_str("GM_RAG_RERANK_URL", &format!("{infer_base_url}/rerank")),
            rag_rerank_model: env_str("GM_RAG_RERANK_MODEL", "jinaai/jina-reranker-v3"),
            rag_rerank_enabled: env_bool("GM_RAG_RERANK_ENABLED", true),
            rag_rerank_candidates: env_int_or("GM_RAG_RERANK_CANDIDATES", 64),
            infer_base_url,

            // Per-model quant for the unified sidecar; only bf16 | nf4 (int8 dropped).
            embedder_quant: {
                let v = env_str("GM_EMBEDDER_QUANT", "bf16").trim().to_lowercase();
                if v == "nf4" {
                    "nf4".to_string()
                } else {
                    "bf16".to_string()
                }
            },
            reranker_quant: {
                let v = env_str("GM_RERANKER_QUANT", "bf16").trim().to_lowercase();
                if v == "nf4" {
                    "nf4".to_string()
                } else {
                    "bf16".to_string()
                }
            },
            image_enabled: env_bool("GM_IMAGE_ENABLED", true),
            image_timeout_seconds: env_float_or("GM_IMAGE_TIMEOUT_SECONDS", 300.0),
            image_max_width: env_int_or("GM_IMAGE_MAX_WIDTH", 2048),
            image_max_height: env_int_or("GM_IMAGE_MAX_HEIGHT", 2048),
            image_max_batch: env_int_or("GM_IMAGE_MAX_BATCH", 4),
            image_max_steps: env_int_or("GM_IMAGE_MAX_STEPS", 50),

            prompt_cache_key,
            prompt_cache_retention: env_str("GM_PROMPT_CACHE_RETENTION", ""),
            llama_cache_reuse: env_int_or("GM_LLAMA_CACHE_REUSE", 0),

            codex_base_url: rstrip_slash(&env_str(
                "GM_CODEX_BASE_URL",
                "https://chatgpt.com/backend-api/codex",
            )),
            codex_client_id: env_str("GM_CODEX_CLIENT_ID", "app_EMoamEEZ73f0CkXaXp7hrann"),
            codex_client_version,
            codex_originator: env_str("GM_CODEX_ORIGINATOR", "codex_cli_rs"),
            codex_user_agent,
            codex_auth_port: env_int_or("GM_CODEX_AUTH_PORT", 1455),
            codex_auto_open_browser: env_bool("GM_CODEX_AUTO_OPEN_BROWSER", true),
            codex_model: env_str("GM_CODEX_MODEL", "gpt-5.4-mini"),
            codex_compact_model: env_str("GM_CODEX_COMPACT_MODEL", &compact_model),
            codex_reasoning_effort,
            codex_reasoning_summary,
            codex_prompt_cache_key,

            gm_think: env_bool("GM_THINK", true),
            npc_think: env_bool("NPC_THINK", false),
        }
    }

    /// Sampling preset for a request, by think-mode (port of the SAMPLING_THINK
    /// / SAMPLING_PLAIN selection used at call sites).
    pub fn sampling(&self, think: bool) -> SamplingPreset {
        if think {
            SAMPLING_THINK
        } else {
            SAMPLING_PLAIN
        }
    }
}

/// Resolve a default app-data file path. In Python this was
/// `Path(__file__).with_name(name)` (next to the source). Per PORT_PLAN §3.2 we
/// deliberately unify on the `directories` crate's data dir for shipped
/// binaries; if it is unavailable we fall back to the executable's directory,
/// then the current directory, to keep behavior close to the Python "next to
/// the program" placement. This is a documented deviation.
pub fn default_data_path(name: &str) -> String {
    if let Some(dirs) = directories::ProjectDirs::from("", "", "gm-lab") {
        let p = dirs.data_dir().join(name);
        return p.to_string_lossy().into_owned();
    }
    if let Some(dir) = exe_dir() {
        return dir.join(name).to_string_lossy().into_owned();
    }
    name.to_string()
}

/// Resolve the default packages/library directory (`<data_dir>/library`),
/// where filesystem world/story packages live. Mirrors [`default_data_path`]'s
/// resolution order (app-data dir, else exe dir, else CWD-relative). An explicit
/// `GM_PACKAGES_DIR` override is honored by callers, not here.
pub fn default_library_dir() -> String {
    if let Some(dirs) = directories::ProjectDirs::from("", "", "gm-lab") {
        let p = dirs.data_dir().join("library");
        return p.to_string_lossy().into_owned();
    }
    if let Some(dir) = exe_dir() {
        return dir.join("library").to_string_lossy().into_owned();
    }
    "library".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serializes tests that read/mutate overlapping process-global env vars via
    /// `Config::from_env`. Without this, parallel runs race: one test removes a
    /// var to assert the default while another sets it, producing intermittent
    /// failures. Tests that only touch uniquely-named vars don't need the guard.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Snapshot the current values of `keys`, run `f`, then restore each key to
    /// its original state (re-set if it was present, remove if it was absent).
    /// Keeps env mutation from leaking into other tests.
    fn with_env_snapshot<F: FnOnce()>(keys: &[&str], f: F) {
        // Poisoning is irrelevant here (the guarded data is `()`), so recover.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<String>)> =
            keys.iter().map(|&k| (k, env::var(k).ok())).collect();
        f();
        for (k, v) in saved {
            match v {
                Some(val) => env::set_var(k, val),
                None => env::remove_var(k),
            }
        }
    }

    #[test]
    fn env_bool_false_set_and_truthiness() {
        // unset -> default
        env::remove_var("GML_TEST_BOOL_X");
        assert!(env_bool("GML_TEST_BOOL_X", true));
        assert!(!env_bool("GML_TEST_BOOL_X", false));

        for falsey in ["0", "false", "no", "off", "", "  FALSE ", "Off", "NO "] {
            env::set_var("GML_TEST_BOOL_X", falsey);
            assert!(
                !env_bool("GML_TEST_BOOL_X", true),
                "{falsey:?} should be false"
            );
        }
        for truthy in ["1", "true", "yes", "on", "anything", " 2 "] {
            env::set_var("GML_TEST_BOOL_X", truthy);
            assert!(
                env_bool("GML_TEST_BOOL_X", false),
                "{truthy:?} should be true"
            );
        }
        env::remove_var("GML_TEST_BOOL_X");
    }

    #[test]
    fn dotenv_parsing_edge_cases() {
        let content = "\
# a comment
   # indented comment

export FOO=bar
BAZ = qux
QUOTED='he said \"hi\"'
DQUOTED=\"a b c\"
INLINE=value # trailing comment
NOSPACEHASH=val#notcomment
=missingkey
1BAD=nope
GOOD_KEY=ok
EMPTY=
EXPORTSPACED=export   spaced
";
        let pairs = parse_dotenv(content);
        let map: std::collections::HashMap<_, _> = pairs.into_iter().collect();

        assert_eq!(map.get("FOO").map(String::as_str), Some("bar"));
        // key trimmed
        assert_eq!(map.get("BAZ").map(String::as_str), Some("qux"));
        // surrounding single quotes stripped, inner content kept verbatim
        assert_eq!(
            map.get("QUOTED").map(String::as_str),
            Some("he said \"hi\"")
        );
        assert_eq!(map.get("DQUOTED").map(String::as_str), Some("a b c"));
        // inline " #" comment stripped + trimmed
        assert_eq!(map.get("INLINE").map(String::as_str), Some("value"));
        // "#" without a leading space is NOT a comment
        assert_eq!(
            map.get("NOSPACEHASH").map(String::as_str),
            Some("val#notcomment")
        );
        // line with no valid key dropped
        assert!(!map.contains_key(""));
        // key starting with a digit is invalid -> dropped
        assert!(!map.contains_key("1BAD"));
        assert_eq!(map.get("GOOD_KEY").map(String::as_str), Some("ok"));
        // empty value preserved
        assert_eq!(map.get("EMPTY").map(String::as_str), Some(""));
        // only the FIRST "export " is stripped; the rest is value (with inline
        // comment rules) -> "export   spaced" has no " #" so stays as-is
        assert_eq!(
            map.get("EXPORTSPACED").map(String::as_str),
            Some("export   spaced")
        );
    }

    #[test]
    fn dotenv_real_env_wins() {
        env::set_var("GML_DOTENV_WINS", "from_env");
        // simulate the per-key precedence rule used by load_dotenv_file
        for (k, v) in parse_dotenv("GML_DOTENV_WINS=from_file\nGML_DOTENV_NEW=fresh\n") {
            if env::var_os(&k).is_none() {
                env::set_var(&k, &v);
            }
        }
        assert_eq!(env::var("GML_DOTENV_WINS").unwrap(), "from_env");
        assert_eq!(env::var("GML_DOTENV_NEW").unwrap(), "fresh");
        env::remove_var("GML_DOTENV_WINS");
        env::remove_var("GML_DOTENV_NEW");
    }

    #[test]
    fn config_defaults_match_python() {
        // Vars this test removes to assert defaults. Snapshot+restore them and
        // hold ENV_LOCK so concurrent `from_env` tests don't race on them.
        const KEYS: &[&str] = &[
            "GM_MODEL",
            "GM_HISTORY_TOKENS",
            "GM_KEEP_TURNS",
            "NPC_HISTORY_TOKENS",
            "NPC_KEEP_EXCHANGES",
            "GM_COMPACT_MODEL",
            "GM_EVENTS_CAP",
            "GM_RUMORS_CAP",
            "GM_CHARS_PER_TOKEN",
            "GM_COMPACT_INPUT_CHARS",
            "GM_PRELUDE_CALLBRIEF_CHARS",
            "GM_BACKEND",
            "GM_THINK",
            "NPC_THINK",
            "GM_MAX_OUTPUT_TOKENS_CAP",
            "GM_CODEX_MODEL",
            "GM_CODEX_COMPACT_MODEL",
            "GM_CODEX_REASONING_EFFORT",
            "GM_CODEX_REASONING_SUMMARY",
            "GM_RAG_MIN_DENSE_SCORE",
            "GM_RAG_STATUS_BOOST",
            // also read by from_env with a backend-dependent default
            "GM_LLAMA_TEMPLATE_KWARGS",
            "GM_CODEX_USER_AGENT",
            "GM_CODEX_CLIENT_VERSION",
        ];
        with_env_snapshot(KEYS, || {
            // Build with a clean-ish env: remove the vars we assert on.
            for k in KEYS {
                env::remove_var(k);
            }
            let c = Config::from_env();
            assert_eq!(c.model, "");
            assert_eq!(c.gm_history_tokens, 100_000);
            assert_eq!(c.gm_keep_turns, 3);
            assert_eq!(c.npc_history_tokens, 64_000);
            assert_eq!(c.npc_keep_exchanges, 6);
            assert_eq!(c.compact_model, "");
            assert_eq!(c.events_cap, 400);
            assert_eq!(c.rumors_cap, 80);
            assert_eq!(c.chars_per_token, 3);
            assert_eq!(c.compact_input_chars, 12_000);
            assert_eq!(c.prelude_callbrief_chars, 4_000);
            assert_eq!(c.backend, "codex");
            assert!(c.gm_think);
            assert!(!c.npc_think);
            assert_eq!(c.max_output_tokens_cap, 200_000);
            assert_eq!(c.codex_model, "gpt-5.4-mini");
            assert_eq!(c.codex_compact_model, "");
            assert_eq!(c.codex_reasoning_effort, "low");
            assert_eq!(c.codex_reasoning_summary, "auto");
            assert_eq!(c.codex_user_agent, "codex_cli_rs/0.133.0 (GM-Lab)");
            assert!((c.rag_min_dense_score - 0.60).abs() < 1e-12);
            assert!((c.rag_status_boost - 1.04).abs() < 1e-12);
            // default for use_llama_template_kwargs follows backend=="llamacpp"
            assert!(!c.use_llama_template_kwargs);
        });
    }

    #[test]
    fn config_env_overrides() {
        const KEYS: &[&str] = &[
            "GM_BACKEND",
            "GM_LLAMA_TEMPLATE_KWARGS",
            "GM_HISTORY_TOKENS",
            "GM_COMPACT_MODEL",
            "GM_CODEX_COMPACT_MODEL",
        ];
        with_env_snapshot(KEYS, || {
            env::set_var("GM_BACKEND", "llamacpp");
            env::remove_var("GM_LLAMA_TEMPLATE_KWARGS");
            env::set_var("GM_HISTORY_TOKENS", "5");
            env::set_var("GM_COMPACT_MODEL", "gpt-5.4-mini");
            env::set_var("GM_CODEX_COMPACT_MODEL", "gpt-5.4-mini-codex");
            let c = Config::from_env();
            assert_eq!(c.backend, "llamacpp");
            // default now true because backend == llamacpp
            assert!(c.use_llama_template_kwargs);
            assert_eq!(c.gm_history_tokens, 5);
            assert_eq!(c.compact_model, "gpt-5.4-mini");
            assert_eq!(c.codex_compact_model, "gpt-5.4-mini-codex");
        });
    }

    #[test]
    fn sampling_presets_exact() {
        let t = SAMPLING_THINK;
        assert_eq!(t.temperature, 0.6);
        assert_eq!(t.top_p, 0.95);
        assert_eq!(t.top_k, 20);
        assert_eq!(t.min_p, 0.0);
        assert_eq!(t.presence_penalty, 1.5);
        let p = SAMPLING_PLAIN;
        assert_eq!(p.temperature, 0.7);
        assert_eq!(p.top_p, 0.80);
    }
}
