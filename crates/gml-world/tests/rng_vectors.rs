//! Validate the CPython-compatible MT19937 against captured Python vectors.
//!
//! Fixtures live in `<workspace>/tests/reference/` (captured from the Python
//! `random.Random`). We resolve them relative to `CARGO_MANIFEST_DIR`.

use gml_world::rng::{MersenneTwister, RngState};
use serde_json::Value;
use std::path::PathBuf;

fn reference_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("reference")
}

fn load(name: &str) -> Value {
    let path = reference_dir().join(name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap()
}

fn state_from_json(v: &Value) -> RngState {
    RngState {
        version: v["version"].as_u64().unwrap() as u32,
        internal: v["internal"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_u64().unwrap())
            .collect(),
        gauss: v["gauss"].as_f64(),
    }
}

#[test]
fn seed_state_and_roll_sequences_match_python() {
    let vectors = load("rng_vectors.json");
    let arr = vectors.as_array().unwrap();
    assert!(!arr.is_empty());

    // The d-side order Python captured the sequences in.
    let side_order = ["2", "4", "6", "8", "10", "12", "20", "100"];

    for entry in arr {
        let seed_str = entry["seed"].as_str().unwrap();
        let seed: u128 = seed_str.parse().unwrap();

        let mut rng = MersenneTwister::from_u128_seed(seed);

        // state_after_seed: getstate() right after seeding.
        let expected_seed_state = state_from_json(&entry["state_after_seed"]);
        let got_seed_state = rng.getstate();
        assert_eq!(
            got_seed_state.internal, expected_seed_state.internal,
            "state_after_seed mismatch for seed {seed_str}"
        );
        assert_eq!(got_seed_state.gauss, expected_seed_state.gauss);
        assert_eq!(got_seed_state.version, 3);

        // randint sequences, in side order.
        let seqs = &entry["randint_sequences"];
        for side in side_order {
            let sides: i64 = side.parse().unwrap();
            let expected: Vec<i64> = seqs[side]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_i64().unwrap())
                .collect();
            let got: Vec<i64> = (0..expected.len()).map(|_| rng.randint(1, sides)).collect();
            assert_eq!(got, expected, "randint(1,{sides}) mismatch for seed {seed_str}");
        }

        // state_after_rolls: getstate() after all sequences.
        let expected_after = state_from_json(&entry["state_after_rolls"]);
        let got_after = rng.getstate();
        assert_eq!(
            got_after.internal, expected_after.internal,
            "state_after_rolls mismatch for seed {seed_str}"
        );
    }
}

#[test]
fn setstate_roundtrip_reproduces_next_d20() {
    let fixture = load("rng_setstate_roundtrip.json");
    let saved = state_from_json(&fixture["saved_state"]);
    let expected: Vec<i64> = fixture["next_d20"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_i64().unwrap())
        .collect();

    let mut rng = MersenneTwister::from_u128_seed(0);
    rng.setstate(&saved).expect("setstate");
    let got: Vec<i64> = (0..expected.len()).map(|_| rng.randint(1, 20)).collect();
    assert_eq!(got, expected, "next_d20 after setstate mismatch");
}
