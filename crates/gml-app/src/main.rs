//! gml-app — the SHIPPED GM-Lab binary.
//!
//! One binary, two modes (PORT_PLAN §3.1):
//!
//!   * **Default** (no flag): launch the Tauri desktop window. An embedded axum
//!     server is bound on `127.0.0.1:<ephemeral-or-GM_PORT>`, then a
//!     `WebviewWindow` opens pointing at that loopback origin so the UNCHANGED
//!     React app (`web/src`) runs with real HTTP / SSE / binary semantics.
//!   * **`--server` / `GM_HEADLESS=1`**: skip Tauri, run the axum server on
//!     `GM_HOST:GM_PORT` (default `127.0.0.1:8000`), enabling the LAN HTTPS
//!     listener when exposed on `0.0.0.0` (phone-mic secure-context, mirroring
//!     `server.py main()`). People play via browser.
//!
//! App-data lives in per-OS dirs via the `directories` crate (`ProjectDirs
//! "gm-lab"`): settings.json, dialogs.sqlite3, embeddings.sqlite3, tts_cache,
//! `.tls`, codex creds. macOS bundle dirs are read-only, so we NEVER write next
//! to the binary. Existing `GM_*` path env overrides are honored by the
//! downstream crates' default-path helpers, so we only set those env vars when
//! the caller hasn't.
//!
//! TTS is optional: the sidecar is started best-effort at boot (no-op when
//! `tts_enabled` is false or it can't launch) and shut down on exit. The app
//! runs fully without it.
//!
//! The Tauri GUI path is behind the default-on `gui` feature. Build the
//! headless-only binary on hosts without the webview system deps with
//! `cargo build -p gml-app --no-default-features` — the `--server` mode always
//! builds and runs.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use gml_audio::{Sidecar, SidecarConfig};
use gml_config::{Config, RuntimeSettings};
use gml_llm::{ConnectorId, ConnectorRegistry, ModelBinding};
use gml_mock::MockConnector;
use gml_openai_compatible::OpenAICompatConnector;
use gml_persistence::{CharacterStore, DialogStore, WorldStore};
use gml_server::{build_router, AppState, TurnRegistry};
use gml_stories::StoryStore;

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 8000;
const DEFAULT_HTTPS_PORT: u16 = 8443;
/// Stable loopback port for the desktop (Tauri) window. The webview origin is
/// `http://127.0.0.1:<port>`, and browser storage (localStorage/IndexedDB — e.g.
/// the developer-mode UI prefs) is keyed by that origin. A fresh ephemeral port
/// each launch would mint a new origin and silently wipe every client-side
/// setting, so we bind a fixed, app-specific port (overridable via GM_PORT, with
/// an ephemeral fallback only if it's already in use). Only the desktop (gui)
/// path binds it; the headless `--server` path uses GM_PORT / DEFAULT_PORT.
#[cfg(feature = "gui")]
const DESKTOP_LOOPBACK_PORT: u16 = 8313;

fn main() {
    init_tracing();
    let args = Args::parse(std::env::args().skip(1));

    match args.mode {
        Mode::Help => {
            print_help();
        }
        Mode::Version => {
            println!("gml-app {}", env!("CARGO_PKG_VERSION"));
        }
        Mode::Server => {
            run_with_runtime(run_server());
        }
        Mode::Desktop => {
            // The desktop window path is feature-gated. When the GUI feature is
            // off (headless-only build), fall back to the server mode so the
            // binary is still useful.
            #[cfg(feature = "gui")]
            {
                run_desktop();
            }
            #[cfg(not(feature = "gui"))]
            {
                eprintln!(
                    "gml-app was built without the `gui` feature; falling back to --server mode. \
                     Rebuild with default features for the desktop window."
                );
                run_with_runtime(run_server());
            }
        }
    }
}

// =========================================================================
// argument parsing
// =========================================================================

#[derive(Debug, PartialEq, Eq)]
enum Mode {
    Desktop,
    Server,
    Help,
    Version,
}

struct Args {
    mode: Mode,
}

impl Args {
    fn parse<I: IntoIterator<Item = String>>(argv: I) -> Self {
        let mut mode = if env_flag("GM_HEADLESS") {
            Mode::Server
        } else {
            Mode::Desktop
        };
        for arg in argv {
            match arg.as_str() {
                "--server" | "--headless" | "serve" => mode = Mode::Server,
                "-h" | "--help" => return Args { mode: Mode::Help },
                "-V" | "--version" => {
                    return Args {
                        mode: Mode::Version,
                    }
                }
                _ => { /* ignore unknown args (Tauri/OS may pass extras) */ }
            }
        }
        Args { mode }
    }
}

fn print_help() {
    println!(
        "gml-app {ver} — GM-Lab desktop app + headless server\n\
\n\
USAGE:\n\
    gml-app [OPTIONS]\n\
\n\
OPTIONS:\n\
    (no flag)       Launch the Tauri desktop window (default).\n\
    --server        Run the headless HTTP/SSE server; play in a browser.\n\
    -h, --help      Print this help.\n\
    -V, --version   Print the version.\n\
\n\
ENVIRONMENT (headless --server):\n\
    GM_HEADLESS=1       Force --server mode.\n\
    GM_HOST             Bind host (default 127.0.0.1). Use 0.0.0.0 for LAN.\n\
    GM_PORT             HTTP port (default 8000).\n\
    GM_HTTPS            1=force HTTPS on, 0=force off (auto-on when GM_HOST=0.0.0.0).\n\
    GM_HTTPS_PORT       HTTPS port (default 8443).\n\
    GM_OPEN_BROWSER=1   Open the URL in the default browser.\n\
\n\
ENVIRONMENT (paths, both modes — default to per-OS app-data dirs):\n\
    GM_SETTINGS_PATH, GM_DIALOG_DB, GM_RAG_CACHE_PATH,\n\
    GM_TTS_CACHE_DIR, GM_CODEX_CREDENTIAL_PATH,\n\
    GM_SUPERGROK_CREDENTIAL_PATH, GM_PACKAGES_DIR,\n\
    GM_BACKEND, GM_MODEL\n",
        ver = env!("CARGO_PKG_VERSION"),
    );
}

// =========================================================================
// shared setup: app-data dirs, config, settings, store, make_client
// =========================================================================

/// Per-OS app-data directories (PORT_PLAN §3.2). macOS bundle dirs are
/// read-only, so all mutable state lives here — never next to the binary.
struct AppDirs {
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl AppDirs {
    fn resolve() -> Self {
        if let Some(dirs) = directories::ProjectDirs::from("", "", "gm-lab") {
            return AppDirs {
                config_dir: dirs.config_dir().to_path_buf(),
                data_dir: dirs.data_dir().to_path_buf(),
                cache_dir: dirs.cache_dir().to_path_buf(),
            };
        }
        // Last-resort fallback (no HOME / unusual env): a `gm-lab` dir in CWD.
        let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let base = base.join("gm-lab-data");
        AppDirs {
            config_dir: base.clone(),
            data_dir: base.clone(),
            cache_dir: base,
        }
    }

    /// Best-effort: ensure the dirs exist (ignore errors — downstream writes
    /// surface real problems with context).
    fn ensure(&self) {
        let _ = std::fs::create_dir_all(&self.config_dir);
        let _ = std::fs::create_dir_all(&self.data_dir);
        let _ = std::fs::create_dir_all(&self.cache_dir);
    }

    /// The `.tls` cert dir for the LAN HTTPS listener.
    fn tls_dir(&self) -> PathBuf {
        self.data_dir.join(".tls")
    }
}

/// Pre-seed the `GM_*` path env vars from the per-OS app-data dirs when the
/// caller hasn't set them, BEFORE `Config::from_env()` reads them. Downstream
/// crates honor these exact env names (see each crate's default-path helper),
/// so this is how we route their default paths into the app-data dirs while
/// still letting an explicit override win.
fn seed_path_env(dirs: &AppDirs) {
    let codex_credential_path = dirs
        .config_dir
        .join("connectors")
        .join("codex")
        .join("auth.json");
    migrate_legacy_credential(
        &dirs.config_dir.join("codex-oauth.json"),
        &codex_credential_path,
    );
    set_if_unset(
        "GM_SETTINGS_PATH",
        dirs.config_dir.join("gm_lab_settings.json"),
    );
    set_if_unset("GM_DIALOG_DB", dirs.data_dir.join("gm_lab_dialogs.sqlite3"));
    set_if_unset(
        "GM_RAG_CACHE_PATH",
        dirs.data_dir.join("gm_lab_embeddings.sqlite3"),
    );
    // Per-world RAG caches (`<id>.sqlite3`). Under the app-data dir, sibling to
    // the global cache — deliberately NOT under `library/` (export privacy).
    set_if_unset("GM_RAG_WORLDS_DIR", dirs.data_dir.join("rag_worlds"));
    set_if_unset("GM_TTS_CACHE_DIR", dirs.cache_dir.join("tts_cache"));
    set_if_unset("GM_CODEX_CREDENTIAL_PATH", codex_credential_path);
    set_if_unset("GM_PACKAGES_DIR", dirs.data_dir.join("library"));
}

/// Preserve an existing sign-in while moving connector-owned state into its
/// dedicated app-resource directory. The old file is left in place so older
/// application versions can still start.
fn migrate_legacy_credential(source: &std::path::Path, target: &std::path::Path) {
    if target.is_file() || !source.is_file() {
        return;
    }
    let Some(parent) = target.parent() else {
        return;
    };
    let Ok(contents) = std::fs::read(source) else {
        return;
    };
    if serde_json::from_slice::<serde_json::Value>(&contents).is_err()
        || std::fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let Ok(mut temporary) = tempfile::NamedTempFile::new_in(parent) else {
        return;
    };
    if temporary.write_all(&contents).is_err()
        || temporary.flush().is_err()
        || temporary.as_file().sync_all().is_err()
    {
        return;
    }
    // Do not overwrite a credential created concurrently by a newer build.
    let _ = temporary.persist_noclobber(target);
}

fn set_if_unset(key: &str, value: PathBuf) {
    let already = std::env::var(key)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    if !already {
        std::env::set_var(key, value);
    }
}

/// Everything the running server needs, plus the handles we shut down on exit.
struct App {
    state: AppState,
    sidecar: Arc<Sidecar>,
}

/// Build the full [`AppState`] from per-OS dirs, config, and connector registry.
/// This is the single construction site for both modes.
async fn build_app() -> Result<App, String> {
    let dirs = AppDirs::resolve();
    dirs.ensure();
    seed_path_env(&dirs);

    // Honor `.env` files like `config.py`'s module-init `_load_dotenv()`.
    gml_config::load_dotenv();

    let config = Arc::new(Config::from_env());
    let settings = Arc::new(RuntimeSettings::new(
        &config,
        gml_config::default_settings_path(),
    ));

    let (connectors, default_binding) =
        build_connectors(config.clone(), settings.clone(), &dirs).await?;

    let store = Arc::new(
        DialogStore::with_connectors(
            DialogStore::default_db_path(),
            connectors,
            default_binding,
            config.clone(),
        )
        .map_err(|e| format!("open dialog store: {e}"))?,
    );

    // Filesystem world package store (source of truth for worlds). On first run
    // import any legacy SQLite `worlds` rows into packages (one-time, idempotent).
    let world_store = Arc::new(
        WorldStore::new(WorldStore::default_root())
            .map_err(|e| format!("open world store: {e}"))?,
    );
    match world_store.migrate_from_sqlite(store.db_path()) {
        Ok(n) if n > 0 => tracing::info!("migrated {n} worlds from SQLite into packages"),
        Ok(_) => {}
        Err(e) => tracing::warn!("world migration failed (continuing): {e}"),
    }

    // Filesystem story package store (source of truth for stories): materializes
    // the three built-in default packages on first run, then scans the library.
    let story_store = Arc::new(std::sync::Mutex::new(
        StoryStore::new(StoryStore::default_root())
            .map_err(|e| format!("open story store: {e}"))?,
    ));

    // Filesystem character package store (K1): scans the library's `characters/`
    // (no built-in defaults). Mirrors the story store's Mutex-wrapped shape.
    let character_store = Arc::new(std::sync::Mutex::new(
        CharacterStore::new(CharacterStore::default_root())
            .map_err(|e| format!("open character store: {e}"))?,
    ));

    // Fold any legacy per-guest chats into the single shared scope (server.py
    // main(): `merge_all_chats_into_scope(CHAT_SCOPE_ID)`).
    let scope = gml_server::chat_scope_id();
    match store.merge_all_chats_into_scope(&scope) {
        Ok(n) if n > 0 => tracing::info!("merged {n} legacy dialogs into shared scope"),
        Ok(_) => {}
        Err(e) => tracing::warn!("merge_all_chats_into_scope failed (continuing): {e}"),
    }

    let mut state = AppState {
        store,
        world_store,
        story_store,
        character_store,
        config,
        settings,
        http: reqwest::Client::new(),
        sidecar: None,
        locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
        turn_registry: Arc::new(TurnRegistry::default()),
        index_html: Arc::new(resolve_index_html(&dirs)),
    };

    // Unified inference sidecar (serve.py: embeddings + rerank + TTS in one
    // process). Pass per-model quant + which models to load so the sidecar
    // mirrors the app's RAG/TTS settings; base_url/port/HF_HOME come from
    // SidecarConfig::from_env. Started best-effort at boot, killed on exit.
    let mut sidecar_cfg = SidecarConfig::from_env();
    let b01 = |on: bool| if on { "1" } else { "0" }.to_string();
    sidecar_cfg.envs.push((
        "EMBEDDER_MODEL".to_string(),
        state.config.rag_embeddings_model.clone(),
    ));
    sidecar_cfg.envs.push((
        "RERANKER_MODEL".to_string(),
        state.config.rag_rerank_model.clone(),
    ));
    sidecar_cfg.envs.push((
        "EMBEDDER_QUANT".to_string(),
        state.config.embedder_quant.clone(),
    ));
    sidecar_cfg.envs.push((
        "RERANKER_QUANT".to_string(),
        state.config.reranker_quant.clone(),
    ));
    sidecar_cfg.envs.push((
        "EMBEDDER_ENABLED".to_string(),
        b01(state.config.rag_enabled),
    ));
    sidecar_cfg.envs.push((
        "RERANKER_ENABLED".to_string(),
        b01(state.config.rag_enabled && state.config.rag_rerank_enabled),
    ));
    sidecar_cfg.envs.push((
        "TTS_ENABLED".to_string(),
        b01(state.settings.tts_enabled(None)),
    ));
    sidecar_cfg.envs.push((
        "IMAGE_ENABLED".to_string(),
        b01(state.config.image_enabled && state.settings.image_enabled(None)),
    ));
    sidecar_cfg.envs.push((
        "IMAGE_TIMEOUT_SECONDS".to_string(),
        state.config.image_timeout_seconds.to_string(),
    ));
    sidecar_cfg.envs.push((
        "IMAGE_MAX_WIDTH".to_string(),
        state.config.image_max_width.to_string(),
    ));
    sidecar_cfg.envs.push((
        "IMAGE_MAX_HEIGHT".to_string(),
        state.config.image_max_height.to_string(),
    ));
    sidecar_cfg.envs.push((
        "IMAGE_MAX_BATCH".to_string(),
        state.config.image_max_batch.to_string(),
    ));
    sidecar_cfg.envs.push((
        "IMAGE_MAX_STEPS".to_string(),
        state.config.image_max_steps.to_string(),
    ));
    if state.config.image_enabled && state.settings.image_enabled(None) {
        let image_timeout = if state.config.image_timeout_seconds.is_finite()
            && state.config.image_timeout_seconds > 0.0
        {
            state.config.image_timeout_seconds
        } else {
            300.0
        };
        let image_ready_timeout = std::time::Duration::from_secs_f64(image_timeout * 2.0 + 180.0);
        sidecar_cfg.ready_timeout = std::cmp::max(sidecar_cfg.ready_timeout, image_ready_timeout);
    }
    let sidecar = Arc::new(Sidecar::new(sidecar_cfg));
    state.sidecar = Some(sidecar.clone());

    Ok(App { state, sidecar })
}

async fn build_connectors(
    config: Arc<Config>,
    settings: Arc<RuntimeSettings>,
    dirs: &AppDirs,
) -> Result<(Arc<ConnectorRegistry>, ModelBinding), String> {
    let response_language_settings = settings.clone();
    let registry = Arc::new(ConnectorRegistry::with_response_language_source(
        move || response_language_settings.response_language(None),
    ));
    registry
        .register(Arc::new(MockConnector))
        .map_err(|error| error.to_string())?;
    registry
        .register(Arc::new(
            OpenAICompatConnector::discover(config.clone(), settings.clone()).await,
        ))
        .map_err(|error| error.to_string())?;
    registry
        .register(Arc::new(gml_codex::CodexConnector::new(
            config.clone(),
            settings,
        )))
        .map_err(|error| error.to_string())?;

    let mut xai_config = gml_supergrok::SuperGrokConfig::new(
        dirs.config_dir
            .join("connectors")
            .join("xai")
            .join("auth.json"),
    );
    apply_supergrok_env(&mut xai_config);
    let xai = gml_supergrok::SuperGrokConnector::new(Arc::new(xai_config))
        .map_err(|error| format!("initialize SuperGrok connector: {error}"))?;
    registry
        .register(Arc::new(xai))
        .map_err(|error| error.to_string())?;

    let connector_name = match config.backend.trim().to_ascii_lowercase().as_str() {
        "mock" => "mock",
        "codex" => "codex",
        "xai" | "supergrok" => "xai",
        _ => "openai-compatible",
    };
    let connector_id = ConnectorId::new(connector_name).map_err(|error| error.to_string())?;
    let binding = registry
        .default_binding(&connector_id)
        .map_err(|error| error.to_string())?;
    // Startup validates only construction. Live catalogs may require OAuth or
    // a local inference server and are validated when a history is created.
    registry
        .create_backend(&binding)
        .map_err(|error| error.to_string())?;
    Ok((registry, binding))
}

fn apply_supergrok_env(config: &mut gml_supergrok::SuperGrokConfig) {
    if let Some(path) =
        std::env::var_os("GM_SUPERGROK_CREDENTIAL_PATH").filter(|value| !value.is_empty())
    {
        config.credential_path = PathBuf::from(path);
    }
    let apply = |target: &mut String, key: &str| {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                *target = value.to_string();
            }
        }
    };
    apply(&mut config.inference_base_url, "GM_SUPERGROK_BASE_URL");
    apply(&mut config.model, "GM_SUPERGROK_MODEL");
    apply(&mut config.compact_model, "GM_SUPERGROK_COMPACT_MODEL");
    apply(
        &mut config.prompt_cache_key,
        "GM_SUPERGROK_PROMPT_CACHE_KEY",
    );
}

/// Resolve the built SPA `index.html`. Search, in order: exe-relative
/// `web/dist/index.html` (release layout), the workspace `web/dist` (dev), and
/// a copy staged under the app-data dir. Returns `None` if not found (the
/// server serves a "build the frontend" placeholder).
fn resolve_index_html(dirs: &AppDirs) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            candidates.push(exe_dir.join("web/dist/index.html"));
            candidates.push(exe_dir.join("../web/dist/index.html"));
            // cargo run layout: target/<profile>/gml-app -> workspace/web/dist
            candidates.push(exe_dir.join("../../web/dist/index.html"));
            candidates.push(exe_dir.join("../../../web/dist/index.html"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("web/dist/index.html"));
        candidates.push(cwd.join("../web/dist/index.html"));
    }
    candidates.push(dirs.data_dir.join("web/dist/index.html"));

    candidates.into_iter().find(|p| p.is_file())
}

// =========================================================================
// headless --server mode
// =========================================================================

async fn run_server() {
    let app = match build_app().await {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gml-app: failed to start: {e}");
            std::process::exit(1);
        }
    };

    // Start the inference sidecar in the BACKGROUND so a slow/wedged sidecar
    // (its readiness probe can take ~2 min for the TTS CUDA-graph capture) never
    // delays binding the HTTP listener. The app runs degraded without it.
    {
        let sidecar = app.sidecar.clone();
        let rag = app.state.config.rag_enabled;
        let tts = app.state.settings.tts_enabled(None);
        let image = app.state.config.image_enabled && app.state.settings.image_enabled(None);
        tokio::spawn(async move {
            if !(rag || tts || image) {
                tracing::info!("RAG + TTS + image all disabled; inference sidecar not started");
                return;
            }
            match sidecar.ensure_started(true).await {
                Ok(()) => {
                    tracing::info!("inference sidecar ready (rag={rag} tts={tts} image={image})")
                }
                Err(e) => {
                    tracing::warn!("inference sidecar not started ({e}); continuing degraded")
                }
            }
        });
    }

    let host = env_str("GM_HOST", DEFAULT_HOST);
    let port = env_u16("GM_PORT", DEFAULT_PORT);
    let lan_exposed = host.is_empty() || host == "0.0.0.0";
    let shown_host = if lan_exposed {
        "localhost"
    } else {
        host.as_str()
    };

    let default_binding = app.state.store.default_binding();
    let url = format!("http://{shown_host}:{port}");
    println!(
        "GM-Lab web UI: {url}  (default connector {}, model {})",
        default_binding.connector_id(),
        default_binding.model_id()
    );
    println!("SQLite dialogs: {}", app.state.store.db_path());
    println!("Shared chat scope: {}", gml_server::chat_scope_id());

    // HTTPS auto-enable rules (server.py main()): GM_HTTPS=1 forces on,
    // GM_HTTPS=0 forces off, otherwise on iff LAN-exposed (0.0.0.0).
    let https_flag = std::env::var("GM_HTTPS").ok();
    let want_https = match https_flag.as_deref() {
        Some("1") => true,
        Some("0") => false,
        _ => lan_exposed,
    };

    let bind_host: std::net::IpAddr = if lan_exposed {
        std::net::IpAddr::from([0, 0, 0, 0])
    } else {
        host.parse()
            .unwrap_or_else(|_| std::net::IpAddr::from([127, 0, 0, 1]))
    };

    // Optional LAN HTTPS listener (own port) — never touches the HTTP port.
    if want_https {
        let https_port = env_u16("GM_HTTPS_PORT", DEFAULT_HTTPS_PORT);
        let addr = std::net::SocketAddr::new(bind_host, https_port);
        let router = build_router(app.state.clone());
        let dirs = AppDirs::resolve();
        let cert_dir = dirs.tls_dir();
        println!("HTTPS (phone mic): https://{shown_host}:{https_port}");
        if lan_exposed {
            for ip in gml_server::tls::lan_ipv4() {
                println!("  from phone/tablet: https://{ip}:{https_port}  (accept the self-signed cert once)");
            }
        }
        tokio::spawn(async move {
            if let Err(e) = gml_server::run_https(addr, router, &cert_dir).await {
                eprintln!("HTTPS listener stopped: {e}");
            }
        });
    }

    if env_flag("GM_OPEN_BROWSER") {
        let _ = open::that(&url);
    }

    println!("Ctrl+C to stop.");

    // Graceful shutdown: kill the sidecar on Ctrl+C.
    let sidecar = app.sidecar.clone();
    let http_addr = std::net::SocketAddr::new(bind_host, port);
    let router = build_router(app.state.clone());

    tokio::select! {
        res = gml_server::run_http(http_addr, router) => {
            if let Err(e) = res {
                eprintln!("HTTP listener stopped: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            println!("\nstopping…");
        }
    }
    sidecar.shutdown().await;
}

// =========================================================================
// desktop (Tauri) mode — feature-gated
// =========================================================================

#[cfg(feature = "gui")]
fn run_desktop() {
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    // Tauri owns the runtime/event loop; we build our own multi-thread tokio
    // runtime for the embedded server and share it via a Handle.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let handle = runtime.handle().clone();

    // Build the app + bind the embedded server on a loopback port BEFORE the
    // window opens, so the React app has a live origin to talk to.
    let app = handle.block_on(build_app()).unwrap_or_else(|e| {
        eprintln!("gml-app: failed to start embedded server: {e}");
        std::process::exit(1);
    });

    // Loopback bind: GM_PORT if set, else a STABLE per-app port so the webview
    // origin stays constant across launches and client-side settings (e.g. the
    // developer-mode UI prefs in localStorage) survive. Fall back to an ephemeral
    // port only if the stable one is busy (a stray instance) — that session won't
    // persist client settings, but the app still starts.
    let requested_port = env_u16_opt("GM_PORT");
    let listener = handle
        .block_on(async move {
            let primary = requested_port.unwrap_or(DESKTOP_LOOPBACK_PORT);
            match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, primary)).await {
                Ok(listener) => Ok(listener),
                Err(_) if requested_port.is_none() => {
                    tracing::warn!(
                        "loopback port {primary} is busy; using an ephemeral port \
                         (client-side settings will not persist this session)"
                    );
                    tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await
                }
                Err(e) => Err(e),
            }
        })
        .unwrap_or_else(|e| {
            eprintln!("gml-app: failed to bind loopback server: {e}");
            std::process::exit(1);
        });
    let local_addr = listener.local_addr().expect("loopback addr");
    let origin = format!("http://127.0.0.1:{}", local_addr.port());
    tracing::info!("embedded server on {origin}");

    // Start the inference sidecar (best-effort) and the axum server on our runtime.
    let state = app.state.clone();
    let sidecar = app.sidecar.clone();
    handle.spawn(async move {
        let _ = sidecar
            .ensure_started(
                state.config.rag_enabled
                    || state.settings.tts_enabled(None)
                    || (state.config.image_enabled && state.settings.image_enabled(None)),
            )
            .await;
    });
    let router = build_router(app.state.clone());
    handle.spawn(async move {
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await;
    });

    // Keep the runtime + sidecar alive for the lifetime of the app, and shut
    // them down when the window closes.
    let sidecar_for_exit = app.sidecar.clone();
    let handle_for_exit = handle.clone();
    let origin_for_setup = origin.clone();

    let builder = tauri::Builder::default().setup(move |tauri_app| {
        let url =
            WebviewUrl::External(origin_for_setup.parse().expect("parse loopback origin url"));
        WebviewWindowBuilder::new(tauri_app, "main", url)
            .title("GM-Lab")
            .inner_size(1280.0, 860.0)
            .min_inner_size(900.0, 600.0)
            .on_new_window(|url, _features| {
                if is_external_browser_scheme(url.scheme()) {
                    if let Err(error) = open::that_detached(url.as_str()) {
                        tracing::warn!(
                            url = %url,
                            error = %error,
                            "failed to open URL in the system browser"
                        );
                    }
                } else {
                    tracing::warn!(url = %url, "blocked unsupported external URL scheme");
                }

                // External links must never create another embedded webview.
                tauri::webview::NewWindowResponse::Deny
            })
            .build()?;
        Ok(())
    });

    // Hold the runtime so its worker threads stay alive while Tauri runs.
    let _runtime_guard = runtime;

    builder
        .build(tauri::generate_context!())
        .expect("build tauri app")
        .run(move |_app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit = event {
                // Kill the TTS sidecar (process tree) on exit. The embedded
                // server tasks die with the runtime when the process exits.
                let sidecar = sidecar_for_exit.clone();
                handle_for_exit.block_on(async move {
                    sidecar.shutdown().await;
                });
            }
        });
}

// =========================================================================
// helpers
// =========================================================================

#[cfg(any(feature = "gui", test))]
fn is_external_browser_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https")
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}

/// Run an async future on a fresh multi-thread tokio runtime (headless mode).
fn run_with_runtime<F: std::future::Future>(fut: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    rt.block_on(fut)
}

fn env_str(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => default.to_string(),
    }
}

fn env_u16(key: &str, default: u16) -> u16 {
    env_u16_opt(key).unwrap_or(default)
}

fn env_u16_opt(key: &str) -> Option<u16> {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
}

/// Truthy env flag: set and not in the falsey set.
fn env_flag(key: &str) -> bool {
    match std::env::var(key) {
        Ok(v) => !matches!(
            v.trim().to_lowercase().as_str(),
            "" | "0" | "false" | "no" | "off"
        ),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_default_is_desktop() {
        std::env::remove_var("GM_HEADLESS");
        let a = Args::parse(Vec::<String>::new());
        assert_eq!(a.mode, Mode::Desktop);
    }

    #[test]
    fn args_server_flag() {
        let a = Args::parse(vec!["--server".to_string()]);
        assert_eq!(a.mode, Mode::Server);
    }

    #[test]
    fn args_help_and_version() {
        assert_eq!(Args::parse(vec!["--help".to_string()]).mode, Mode::Help);
        assert_eq!(Args::parse(vec!["-h".to_string()]).mode, Mode::Help);
        assert_eq!(
            Args::parse(vec!["--version".to_string()]).mode,
            Mode::Version
        );
        assert_eq!(Args::parse(vec!["-V".to_string()]).mode, Mode::Version);
    }

    #[test]
    fn args_unknown_ignored() {
        let a = Args::parse(vec!["--frobnicate".to_string(), "foo".to_string()]);
        assert_eq!(a.mode, Mode::Desktop);
    }

    #[test]
    fn env_flag_truthiness() {
        std::env::set_var("GML_APP_TEST_FLAG", "1");
        assert!(env_flag("GML_APP_TEST_FLAG"));
        std::env::set_var("GML_APP_TEST_FLAG", "0");
        assert!(!env_flag("GML_APP_TEST_FLAG"));
        std::env::set_var("GML_APP_TEST_FLAG", "false");
        assert!(!env_flag("GML_APP_TEST_FLAG"));
        std::env::remove_var("GML_APP_TEST_FLAG");
        assert!(!env_flag("GML_APP_TEST_FLAG"));
    }

    #[test]
    fn external_browser_allows_only_web_urls() {
        assert!(is_external_browser_scheme("https"));
        assert!(is_external_browser_scheme("http"));
        assert!(!is_external_browser_scheme("file"));
        assert!(!is_external_browser_scheme("javascript"));
        assert!(!is_external_browser_scheme("gmlab"));
    }

    #[test]
    fn env_u16_parsing() {
        std::env::set_var("GML_APP_TEST_PORT", "12345");
        assert_eq!(env_u16("GML_APP_TEST_PORT", 8000), 12345);
        std::env::set_var("GML_APP_TEST_PORT", "notaport");
        assert_eq!(env_u16("GML_APP_TEST_PORT", 8000), 8000);
        std::env::remove_var("GML_APP_TEST_PORT");
        assert_eq!(env_u16("GML_APP_TEST_PORT", 8000), 8000);
    }

    #[test]
    fn app_dirs_resolve_and_tls() {
        let dirs = AppDirs::resolve();
        // tls_dir is under data_dir
        assert!(dirs.tls_dir().starts_with(&dirs.data_dir));
    }

    #[test]
    fn legacy_credential_migration_is_validated_and_never_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("legacy.json");
        let target = directory.path().join("connectors/codex/auth.json");
        std::fs::write(&source, br#"{"access_token":"old"}"#).unwrap();

        migrate_legacy_credential(&source, &target);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            br#"{"access_token":"old"}"#
        );

        std::fs::write(&source, br#"{"access_token":"new"}"#).unwrap();
        migrate_legacy_credential(&source, &target);
        assert_eq!(
            std::fs::read(&target).unwrap(),
            br#"{"access_token":"old"}"#
        );

        std::fs::remove_file(&target).unwrap();
        std::fs::write(&source, b"not json").unwrap();
        migrate_legacy_credential(&source, &target);
        assert!(!target.exists());
    }
}
