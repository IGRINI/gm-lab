//! Golden turn-stream integration test.
//!
//! Builds a `Session` with the `MockClient`, pins the campaign RNG to seed
//! 20260622 (matching `tools/capture_turn_stream.py`), runs `run_turn` for each
//! reference player action, collects the events, and asserts:
//!   (a) the non-delta `(kind, agent)` sequence equals `*.skeleton.json` exactly;
//!   (b) deterministic event data matches `*.full.json` for non-timing events,
//!       comparing meta/meta_total structurally (label/scope/in/out) while
//!       ignoring wall-clock timing.

use std::sync::Arc;

use serde_json::{Map, Value};

use gml_config::{Config, RuntimeSettings};
use gml_llm::{Backend, MockClient};
use gml_orchestrator::{run_turn, Session};
use gml_stories::StoryStore;
use gml_world::World;

const FIXED_DICE_SEED: u128 = 20260622;

/// Default story seed from a HERMETIC store over a tempdir. There is no global
/// store; constructing a `StoryStore` materializes the builtins into the
/// throwaway directory, so this byte-golden turn test never touches the real
/// user library. The seed is byte-identical to the built-in package.
fn default_story_seed() -> serde_json::Value {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = StoryStore::new(dir.path()).expect("open store");
    store.default_seed()
}

fn reference_dir() -> std::path::PathBuf {
    // crate dir is .../crates/gml-orchestrator; reference is .../tests/reference.
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("reference")
        .join("turns")
}

/// Build the settings used to capture the golden fixtures: gm_suggest_options
/// enabled (the fixtures show ask_player gating + the "no ask_player" error),
/// stream_gm_content default true, max_tool_hops default 0 (unlimited).
fn capture_settings() -> RuntimeSettings {
    let mut cfg = Config::from_env();
    cfg.backend = "mock".to_string();
    let tmp = std::env::temp_dir().join(format!(
        "gml_orch_golden_settings_{}.json",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&tmp);
    let settings = RuntimeSettings::new(&cfg, tmp);
    let mut update = Map::new();
    update.insert("gm_suggest_options".to_string(), Value::Bool(true));
    settings.update(Some(&update));
    settings
}

/// Pin the world RNG to the fixed dice seed (the capture script does
/// `world.dice_seed = SEED; world._rng = random.Random(SEED)`; building the
/// world via `from_seed_with_dice_seed` seeds the MT exactly like
/// `random.Random(SEED)`).
fn pinned_session() -> Session {
    let client: Arc<dyn Backend> = Arc::new(MockClient::new());
    let world = World::from_seed_with_dice_seed(&default_story_seed(), FIXED_DICE_SEED);
    Session::with_world(
        client,
        world,
        Arc::new(|| Arc::new(MockClient::new()) as Arc<dyn Backend>),
    )
}

fn load_json(path: &std::path::Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// Meta/meta_total structural comparison: keep label/scope/in/out, drop timing
/// AND prompt-size token estimates. The `context` block and `sys_estimate` are
/// estimated token counts of the actual prompt TEXT; they legitimately shift
/// whenever the GM context wording changes (e.g. exits now carry canon
/// `transition_id`s) and are not a behavioural contract — the event skeleton,
/// narration content, tool calls and scene updates remain fully asserted.
fn strip_timing(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut out = Map::new();
            for (k, child) in m {
                // Drop wall-clock-derived fields and prompt-size estimates
                // anywhere in the meta payload.
                if matches!(
                    k.as_str(),
                    "secs"
                        | "tps"
                        | "eval_secs"
                        | "load_secs"
                        | "prompt_secs"
                        | "cached"
                        | "context"
                        | "sys_estimate"
                ) {
                    continue;
                }
                out.insert(k.clone(), strip_timing(child));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(strip_timing).collect()),
        other => other.clone(),
    }
}

/// Whether the event kind carries deterministic data we assert verbatim.
fn is_deterministic_data(kind: &str) -> bool {
    matches!(
        kind,
        "player"
            | "gm_thinking"
            | "gm_tool_call"
            | "npc_speech"
            | "tool_result"
            | "gm_reject"
            | "gm_narration"
            | "error"
            | "world_fact"
            | "dice"
            | "scene_update"
            | "npc_history"
            | "player_options"
            | "npc_start"
            | "npc_thinking"
    )
}

fn run_case(name: &str, action: &str) {
    let dir = reference_dir();
    let skeleton = load_json(&dir.join(format!("{name}.skeleton.json")));
    let full = load_json(&dir.join(format!("{name}.full.json")));

    let settings = capture_settings();
    let mut session = pinned_session();
    let events = tokio_block_on(run_turn(&mut session, &settings, action));

    if std::env::var("GML_BLESS").as_deref() == Ok("1") {
        let full_events: Vec<Value> = events
            .iter()
            .map(|e| serde_json::to_value(e).expect("event to value"))
            .collect();
        let skel: Vec<Value> = events
            .iter()
            .filter(|e| e.kind != "delta")
            .map(|e| serde_json::json!({"kind": e.kind, "agent": e.agent}))
            .collect();
        let full_str = serde_json::to_string_pretty(&Value::Array(full_events))
            .unwrap()
            .replace('\n', "\r\n")
            + "\r\n";
        let skel_str = serde_json::to_string_pretty(&Value::Array(skel))
            .unwrap()
            .replace('\n', "\r\n")
            + "\r\n";
        std::fs::write(dir.join(format!("{name}.full.json")), full_str).unwrap();
        std::fs::write(dir.join(format!("{name}.skeleton.json")), skel_str).unwrap();
        return;
    }

    // (a) non-delta (kind, agent) sequence equals skeleton.
    let got_skeleton: Vec<Value> = events
        .iter()
        .filter(|e| e.kind != "delta")
        .map(|e| {
            serde_json::json!({
                "kind": e.kind,
                "agent": e.agent,
            })
        })
        .collect();
    let want_skeleton = skeleton.as_array().expect("skeleton is array");
    assert_eq!(
        got_skeleton.len(),
        want_skeleton.len(),
        "[{name}] non-delta event count mismatch\n got: {:#?}\nwant: {:#?}",
        got_skeleton,
        want_skeleton
    );
    for (i, (got, want)) in got_skeleton.iter().zip(want_skeleton.iter()).enumerate() {
        assert_eq!(
            got, want,
            "[{name}] skeleton mismatch at non-delta index {i}"
        );
    }

    // (b) deterministic data matches the full fixture for non-delta events.
    let want_full: Vec<Value> = full
        .as_array()
        .expect("full is array")
        .iter()
        .filter(|e| e.get("kind").and_then(Value::as_str) != Some("delta"))
        .cloned()
        .collect();
    let got_full: Vec<Value> = events
        .iter()
        .filter(|e| e.kind != "delta")
        .map(|e| serde_json::to_value(e).expect("event to value"))
        .collect();
    assert_eq!(
        got_full.len(),
        want_full.len(),
        "[{name}] full non-delta count mismatch"
    );

    for (i, (got, want)) in got_full.iter().zip(want_full.iter()).enumerate() {
        let kind = want.get("kind").and_then(Value::as_str).unwrap_or("");
        assert_eq!(
            got.get("kind"),
            want.get("kind"),
            "[{name}] kind mismatch at index {i}"
        );
        assert_eq!(
            got.get("agent"),
            want.get("agent"),
            "[{name}] agent mismatch at index {i} (kind={kind})"
        );
        assert_eq!(
            got.get("sid"),
            want.get("sid"),
            "[{name}] sid mismatch at index {i} (kind={kind})"
        );

        let got_data = got.get("data").unwrap_or(&Value::Null);
        let want_data = want.get("data").unwrap_or(&Value::Null);

        if kind == "meta" || kind == "meta_total" {
            assert_eq!(
                strip_timing(got_data),
                strip_timing(want_data),
                "[{name}] meta structural mismatch at index {i} (kind={kind})"
            );
        } else if is_deterministic_data(kind) {
            assert_eq!(
                got_data, want_data,
                "[{name}] data mismatch at index {i} (kind={kind})"
            );
        }
    }
}

/// Minimal current-thread tokio runtime to run the async `run_turn`.
fn tokio_block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(fut)
}

#[test]
fn golden_accuse_borin() {
    run_case(
        "accuse_borin",
        "Громко, на весь зал, заявляю Борину: «Я знаю, что ты связан с убийством Алдрика!»",
    );
}

#[test]
fn golden_look_around() {
    run_case(
        "look_around",
        "Я осматриваю зал трактира и прислушиваюсь к разговорам.",
    );
}

#[test]
fn golden_ask_innkeeper() {
    run_case(
        "ask_innkeeper",
        "Подхожу к стойке и тихо спрашиваю трактирщика, что он видел прошлой ночью.",
    );
}
