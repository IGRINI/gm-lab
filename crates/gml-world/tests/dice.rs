//! Dice / grading fixtures — validate against dice_grades.json (margin->grade)
//! and dice_rolls.json (seed 424242 + forced overrides -> exact total/detail).

use gml_world::dice::grade_from_margin;
use gml_world::rng::MersenneTwister;
use gml_world::World;
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
fn grade_ladder_matches_python() {
    let grades = load("dice_grades.json");
    let obj = grades.as_object().unwrap();
    for (margin_str, expected) in obj {
        let margin: i64 = margin_str.parse().unwrap();
        assert_eq!(
            grade_from_margin(margin),
            expected.as_str().unwrap(),
            "grade mismatch at margin {margin}"
        );
    }
}

#[test]
fn rolls_match_python_seed_424242() {
    // World.__new__ bypass + random.Random(424242).
    let mut world = World::empty_with_rng(MersenneTwister::from_u128_seed(424242));
    let cases = load("dice_rolls.json");
    for case in cases.as_array().unwrap() {
        let notation = case["notation"].as_str().unwrap();
        if let Some(f) = case.get("forced_die_next").and_then(|v| v.as_i64()) {
            world.forced_die_next = Some(f);
        }
        if let Some(f) = case.get("forced_die_all").and_then(|v| v.as_i64()) {
            world.forced_die_all = Some(f);
        }
        let (total, detail) = world.roll(notation);
        assert_eq!(
            total,
            case["total"].as_i64().unwrap(),
            "total mismatch for {notation}"
        );
        assert_eq!(
            detail,
            case["detail"].as_str().unwrap(),
            "detail mismatch for {notation}"
        );
    }
}
