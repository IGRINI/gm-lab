use gml_world::canon::engine;
use gml_world::{
    Action, District, DistrictValidationError, Place, ProposedAction, Provenance, Region,
    Settlement, WorldCanon, WorldSpec,
};

fn district_canon() -> WorldCanon {
    let mut canon = WorldCanon::default();
    canon.regions.insert(
        "region".to_string(),
        Region {
            region_id: "region".to_string(),
            name: "Region".to_string(),
            settlement_ids: vec!["city".to_string()],
            ..Default::default()
        },
    );
    canon.settlements.insert(
        "city".to_string(),
        Settlement {
            settlement_id: "city".to_string(),
            name: "City".to_string(),
            region_id: "region".to_string(),
            ..Default::default()
        },
    );
    canon
}

fn district(place_ids: Vec<String>) -> District {
    District {
        district_id: "market_district".to_string(),
        name: "Market District".to_string(),
        settlement_id: "city".to_string(),
        region_id: "region".to_string(),
        kind: "commercial".to_string(),
        place_ids,
        provenance: Provenance::by("test", "district fixture", 0),
    }
}

#[test]
fn district_insertion_is_strict_and_maintains_cross_links() {
    let mut canon = district_canon();
    canon.insert_place(Place {
        place_id: "market".to_string(),
        name: "Market".to_string(),
        region_id: "region".to_string(),
        district_id: "market_district".to_string(),
        ..Default::default()
    });

    canon
        .insert_district(district(vec!["market".to_string()]))
        .expect("explicit district");

    assert_eq!(
        canon
            .district_for_place("market")
            .map(|value| value.district_id.as_str()),
        Some("market_district")
    );
    assert_eq!(
        canon
            .settlement_for_place("market")
            .map(|value| value.settlement_id.as_str()),
        Some("city")
    );
    assert_eq!(canon.settlements["city"].district_ids, ["market_district"]);
    assert_eq!(canon.settlements["city"].place_ids, ["market"]);

    assert_eq!(
        canon.insert_district(district(Vec::new())),
        Err(DistrictValidationError::DuplicateId(
            "market_district".to_string()
        ))
    );
}

#[test]
fn district_insertion_rejects_implicit_or_conflicting_membership() {
    let mut canon = district_canon();
    canon.insert_place(Place {
        place_id: "market".to_string(),
        name: "Market".to_string(),
        region_id: "region".to_string(),
        ..Default::default()
    });

    assert_eq!(
        canon.insert_district(district(vec!["market".to_string()])),
        Err(DistrictValidationError::PlaceMembershipMismatch {
            place_id: "market".to_string(),
            district_id: "market_district".to_string(),
        })
    );
    assert!(canon.districts.is_empty());
    assert!(canon.settlements["city"].district_ids.is_empty());
}

#[test]
fn create_place_accepts_only_an_exact_existing_district() {
    let mut canon = district_canon();
    canon
        .insert_district(district(Vec::new()))
        .expect("empty district is valid");

    engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::CreatePlace {
                place_id: "tavern".to_string(),
                name: "Tavern".to_string(),
                kind: "building".to_string(),
                parent: "market_district".to_string(),
                region_id: "region".to_string(),
                district_id: "market_district".to_string(),
                description: "A known tavern".to_string(),
                features: Vec::new(),
                visited: true,
                shell: false,
            },
            "location_generator",
            "create a place in an exact district",
        ),
        1,
    )
    .expect("place in exact district");

    assert_eq!(canon.places["tavern"].district_id, "market_district");
    assert_eq!(canon.districts["market_district"].place_ids, ["tavern"]);
    assert_eq!(canon.settlements["city"].place_ids, ["tavern"]);

    let rejection = engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::CreatePlace {
                place_id: "invented".to_string(),
                name: "Invented".to_string(),
                kind: String::new(),
                parent: "city".to_string(),
                region_id: "region".to_string(),
                district_id: "district_from_a_name".to_string(),
                description: String::new(),
                features: Vec::new(),
                visited: false,
                shell: false,
            },
            "location_generator",
            "must not invent district ids",
        ),
        2,
    )
    .expect_err("unknown district must be rejected");
    assert_eq!(rejection.code, "unknown_district");
    assert!(!canon.places.contains_key("invented"));
}

#[test]
fn old_saves_default_new_district_fields() {
    let place: Place = serde_json::from_value(serde_json::json!({
        "place_id": "legacy_place",
        "name": "Legacy"
    }))
    .expect("legacy place");
    let settlement: Settlement = serde_json::from_value(serde_json::json!({
        "settlement_id": "legacy_city",
        "name": "Legacy City"
    }))
    .expect("legacy settlement");
    let canon: WorldCanon =
        serde_json::from_value(serde_json::json!({})).expect("legacy world canon");

    assert!(place.district_id.is_empty());
    assert!(settlement.district_ids.is_empty());
    assert!(canon.districts.is_empty());
}

#[test]
fn seeded_world_has_an_explicit_consistent_district() {
    let canon = gml_world::canon::generate(&WorldSpec::from_seed("district-seed"));
    let start = canon
        .place(&canon.player_place_id)
        .expect("seeded start place");
    let district = canon
        .district_for_place(&start.place_id)
        .expect("seeded start district");
    let settlement = canon
        .settlement(&district.settlement_id)
        .expect("district settlement");

    assert!(!start.district_id.is_empty());
    assert!(district.place_ids.contains(&start.place_id));
    assert!(settlement.district_ids.contains(&district.district_id));
    for place_id in &settlement.place_ids {
        assert_eq!(
            canon
                .place(place_id)
                .map(|place| place.district_id.as_str()),
            Some(district.district_id.as_str()),
            "seeded settlement place '{place_id}' needs explicit district membership"
        );
    }
}
