//! Build script for `gml-app`.
//!
//! `tauri_build::build()` reads `tauri.conf.json` + capabilities and generates
//! the context the desktop window path consumes. It only runs with the `gui`
//! feature — under `--no-default-features` (the headless `--server` build) this
//! is a no-op so the binary compiles with no Tauri system dependency at all.

fn main() {
    #[cfg(feature = "gui")]
    tauri_build::build();
}
