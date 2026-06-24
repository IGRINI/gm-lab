//! gml-config — startup `Config` + dynamic `RuntimeSettings`.
//!
//! Faithful port of `gm-lab/config.py` and `gm-lab/runtime_settings.py`
//! (PORT_PLAN.md §3.2, §4.2). Two layers:
//!
//! - [`config`] — the static layer: hand-rolled `.env` loader ([`load_dotenv`]),
//!   [`env_bool`], and the immutable [`Config`] struct holding every
//!   env-derived constant with exact env var names and defaults.
//! - [`runtime_settings`] — the dynamic layer: [`RuntimeSettings`] with atomic
//!   JSON persistence (sorted keys, raw UTF-8, trailing newline), a thread-safe
//!   cache, validation/clamp/normalize/migration, and model-capability
//!   reconciliation.
//!
//! Role strings come from `gml_types::Role` / `REASONING_ROLES` (the single
//! source of truth); they are re-exported here for convenience.

pub mod config;
pub mod runtime_settings;

pub use config::{
    env_bool, load_dotenv, load_dotenv_file, parse_dotenv, Config, SamplingPreset, REASONING_ROLES,
    SAMPLING_PLAIN, SAMPLING_THINK,
};
pub use gml_types::Role;
pub use runtime_settings::{
    default_settings_path, supported_reasoning_efforts, RoleSettings, RuntimeSettings, SettingsMap,
    MAX_TOOL_HOPS_CAP, REASONING_EFFORTS, REASONING_SUMMARIES, TEXT_VERBOSITIES, TOOL_CHOICES,
};
