//! Validate state_record_hash against captured Python sha256 hexdigests.

use gml_world::{state_record_hash, StateRecord};
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
    let raw = std::fs::read_to_string(reference_dir().join(name)).unwrap();
    serde_json::from_str(&raw).unwrap()
}

#[test]
fn state_record_hash_matches_python() {
    let cases = load("state_record_hash.json");
    for case in cases.as_array().unwrap() {
        let record: StateRecord =
            serde_json::from_value(case["record"].clone()).expect("deserialize StateRecord");
        let expected = case["hash"].as_str().unwrap();
        assert_eq!(
            state_record_hash(&record),
            expected,
            "hash mismatch for record {}",
            record.record_id
        );
    }
}
