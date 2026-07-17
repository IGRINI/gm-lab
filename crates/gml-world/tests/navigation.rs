use gml_world::canon::engine;
use gml_world::{
    plan_travel, Action, ActiveJourney, District, PassageDirectionality, Place, ProposedAction,
    Region, Settlement, Transition, TravelAccess, TravelAnchor, TravelLink,
    TravelLinkValidationError, TravelNetwork, TravelPlanError, WorldCanon,
};

fn place(place_id: &str, visited: bool) -> Place {
    let mut place = Place {
        place_id: place_id.to_string(),
        name: place_id.to_string(),
        ..Default::default()
    };
    if visited {
        place.mark_visited();
    }
    place
}

fn network(network_id: &str, default_for_normal_travel: bool) -> TravelNetwork {
    TravelNetwork {
        network_id: network_id.to_string(),
        scope_id: "city".to_string(),
        default_for_normal_travel,
        passable: true,
        ..Default::default()
    }
}

fn anchor(anchor_id: &str, network_id: &str) -> TravelAnchor {
    TravelAnchor {
        anchor_id: anchor_id.to_string(),
        network_id: network_id.to_string(),
        passable: true,
        ..Default::default()
    }
}

fn access(access_id: &str, place_id: &str, anchor_id: &str) -> TravelAccess {
    TravelAccess {
        access_id: access_id.to_string(),
        place_id: place_id.to_string(),
        anchor_id: anchor_id.to_string(),
        passable: true,
        ..Default::default()
    }
}

fn link(link_id: &str, anchor_a: &str, anchor_b: &str, minutes: i64, risk: &str) -> TravelLink {
    TravelLink {
        link_id: link_id.to_string(),
        anchor_a: anchor_a.to_string(),
        anchor_b: anchor_b.to_string(),
        time_cost_minutes: minutes,
        risk: risk.to_string(),
        passable: true,
        ..Default::default()
    }
}

fn city_fixture() -> WorldCanon {
    let mut canon = WorldCanon {
        player_place_id: "center".to_string(),
        ..Default::default()
    };
    canon.insert_place(place("center", true));
    canon.insert_place(place("shop", true));

    canon.insert_travel_network(network("surface", true));
    canon.insert_travel_network(network("sewer", false));

    for value in [
        anchor("surface_center", "surface"),
        anchor("surface_shop", "surface"),
        anchor("sewer_center", "sewer"),
        anchor("sewer_shop", "sewer"),
    ] {
        canon.insert_travel_anchor(value);
    }
    for value in [
        access("center_surface", "center", "surface_center"),
        access("shop_surface", "shop", "surface_shop"),
        access("center_sewer", "center", "sewer_center"),
        access("shop_sewer", "shop", "sewer_shop"),
    ] {
        canon.insert_travel_access(value);
    }
    canon.insert_travel_link(link(
        "surface_route",
        "surface_center",
        "surface_shop",
        20,
        "low",
    ));
    canon.insert_travel_link(link("sewer_route", "sewer_center", "sewer_shop", 5, "high"));
    canon
}

#[test]
fn normal_travel_uses_only_the_explicit_default_network() {
    let canon = city_fixture();

    let normal = plan_travel(&canon, "shop", None).expect("surface route");
    assert_eq!(normal.network_id, "surface");
    assert_eq!(normal.link_ids, ["surface_route"]);
    assert_eq!(normal.total_time_minutes, 20);
    assert_eq!(normal.risk, "low");

    let sewer = plan_travel(&canon, "shop", Some("sewer")).expect("explicit sewer route");
    assert_eq!(sewer.network_id, "sewer");
    assert_eq!(sewer.link_ids, ["sewer_route"]);
    assert_eq!(sewer.total_time_minutes, 5);
    assert_eq!(sewer.risk, "high");
}

#[test]
fn an_undirected_link_has_one_profile_in_both_directions() {
    let mut canon = city_fixture();
    let outward = plan_travel(&canon, "shop", None).expect("outward route");

    canon.player_place_id = "shop".to_string();
    let returning = plan_travel(&canon, "center", None).expect("return route");

    assert_eq!(returning.link_ids, outward.link_ids);
    assert_eq!(returning.total_time_minutes, outward.total_time_minutes);
    assert_eq!(returning.risk, outward.risk);
    assert_eq!(
        returning.anchor_ids,
        outward.anchor_ids.into_iter().rev().collect::<Vec<_>>()
    );
    assert_eq!(
        canon
            .travel_link("surface_route")
            .and_then(|route| route.other_anchor("surface_shop")),
        Some("surface_center")
    );
}

#[test]
fn destination_must_have_been_visited() {
    let mut canon = city_fixture();
    canon
        .places
        .get_mut("shop")
        .expect("shop")
        .state_flags
        .remove("visited");

    assert_eq!(
        plan_travel(&canon, "shop", None),
        Err(TravelPlanError::DestinationNotVisited("shop".to_string()))
    );
}

#[test]
fn malformed_mechanics_are_rejected_without_prose_inference() {
    let mut canon = city_fixture();
    canon
        .travel_links
        .get_mut("surface_route")
        .expect("surface route")
        .time_cost_minutes = 0;
    assert_eq!(
        plan_travel(&canon, "shop", None),
        Err(TravelPlanError::InvalidLink {
            link_id: "surface_route".to_string(),
            reason: TravelLinkValidationError::NonPositiveTime,
        })
    );

    let route = canon
        .travel_links
        .get_mut("surface_route")
        .expect("surface route");
    route.time_cost_minutes = 7;
    route.risk = "dangerous road".to_string();
    assert_eq!(
        route.validate(),
        Err(TravelLinkValidationError::InvalidRisk)
    );
}

#[test]
fn blocked_links_are_skipped_and_unknown_intermediate_anchors_need_no_place() {
    let mut canon = city_fixture();
    canon
        .travel_links
        .get_mut("surface_route")
        .expect("surface route")
        .blocked_by = "closed_bridge_fact".to_string();
    canon.insert_travel_anchor(anchor("surface_junction", "surface"));
    canon.insert_travel_link(link(
        "surface_leg_a",
        "surface_center",
        "surface_junction",
        8,
        "none",
    ));
    canon.insert_travel_link(link(
        "surface_leg_b",
        "surface_junction",
        "surface_shop",
        9,
        "medium",
    ));

    let plan = plan_travel(&canon, "shop", None).expect("detour");
    assert_eq!(plan.link_ids, ["surface_leg_a", "surface_leg_b"]);
    assert_eq!(plan.anchor_ids[1], "surface_junction");
    assert!(!canon.places.contains_key("surface_junction"));
    assert_eq!(plan.total_time_minutes, 17);
    assert_eq!(plan.risk, "medium");
}

#[test]
fn normal_travel_never_falls_back_to_a_non_default_network() {
    let mut canon = city_fixture();
    canon.travel_links.remove("surface_route");

    assert!(matches!(
        plan_travel(&canon, "shop", None),
        Err(TravelPlanError::NoRoute { .. })
    ));
    assert!(plan_travel(&canon, "shop", Some("sewer")).is_ok());
}

#[test]
fn active_journey_preserves_route_progress_across_an_interruption() {
    let canon = city_fixture();
    let plan = plan_travel(&canon, "shop", None).expect("route");
    let mut journey = ActiveJourney::from_plan(&canon, "journey_1", &plan).expect("active journey");

    assert_eq!(journey.remaining_minutes_on_link, 20);
    journey.remaining_minutes_on_link = 11;
    journey.elapsed_minutes = 9;
    journey.interrupt_at("street_incident");
    assert!(journey.is_interrupted());
    assert_eq!(journey.remaining_minutes_on_link, 11);
    assert_eq!(journey.link_ids, ["surface_route"]);

    journey.resume();
    assert!(!journey.is_interrupted());
    assert_eq!(journey.remaining_minutes_on_link, 11);
}

#[test]
fn travel_action_interrupts_without_creating_a_fake_place_to_place_exit() {
    let mut canon = city_fixture();
    canon.world_seed = "navigation-interruption".to_string();
    let route = canon
        .travel_links
        .get_mut("surface_route")
        .expect("surface route");
    route.time_cost_minutes = 48 * 60;
    route.risk = "certain".to_string();

    let events = engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::TravelPlayer {
                destination_place_id: "shop".to_string(),
                network_id: None,
            },
            "test",
            "cross town",
        ),
        1,
    )
    .expect("planned travel");

    assert!(events.iter().any(|event| event.kind == "travel_situation"));
    assert_ne!(canon.player_place_id, "shop");
    assert!(canon
        .transitions
        .values()
        .all(|transition| transition.from_place != "center" || transition.to_place != "shop"));
    let journey = canon.active_journey.as_ref().expect("active journey");
    assert_eq!(journey.network_id, "surface");
    assert_eq!(journey.destination_place_id, "shop");
    assert_eq!(journey.interruption_place_id, canon.player_place_id);

    let continue_transition_id = canon
        .exits_from(&canon.player_place_id)
        .into_iter()
        .find(|transition| transition.to_place == "shop")
        .expect("continue journey exit")
        .transition_id
        .clone();
    canon.gen_budget.max_events_per_turn = 0;
    engine::apply(
        &mut canon,
        &ProposedAction::new(
            Action::MovePlayer {
                transition_id: continue_transition_id,
            },
            "test",
            "continue journey",
        ),
        2,
    )
    .expect("continue journey");

    assert_eq!(canon.player_place_id, "shop");
    assert!(canon.active_journey.is_none());
}

#[test]
fn navigation_fields_default_for_old_saves_and_count_as_canon_state() {
    let mut canon: WorldCanon = serde_json::from_value(serde_json::json!({}))
        .expect("an old save without navigation fields");
    assert!(canon.travel_networks.is_empty());
    assert!(canon.travel_anchors.is_empty());
    assert!(canon.travel_accesses.is_empty());
    assert!(canon.travel_links.is_empty());
    assert!(canon.active_journey.is_none());
    assert!(canon.is_empty());

    canon.insert_travel_network(network("surface", true));
    assert!(!canon.is_empty());
}

#[test]
fn district_network_returns_to_known_places_without_replaying_exploration_chain() {
    let mut canon = WorldCanon {
        player_place_id: "castle_front".to_string(),
        ..Default::default()
    };
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

    for (place_id, district_id) in [
        ("market", "market_district"),
        ("tavern", "market_district"),
        ("brothel", "market_district"),
        ("castle", "castle_district"),
        ("castle_front", "castle_district"),
    ] {
        let mut value = place(place_id, true);
        value.region_id = "region".to_string();
        value.district_id = district_id.to_string();
        canon.insert_place(value);
    }
    for (district_id, name, place_ids) in [
        (
            "market_district",
            "Market District",
            vec!["market", "tavern", "brothel"],
        ),
        (
            "castle_district",
            "Castle District",
            vec!["castle", "castle_front"],
        ),
    ] {
        canon
            .insert_district(District {
                district_id: district_id.to_string(),
                name: name.to_string(),
                settlement_id: "city".to_string(),
                region_id: "region".to_string(),
                place_ids: place_ids.into_iter().map(str::to_string).collect(),
                ..Default::default()
            })
            .expect("explicit district fixture");
    }

    // This is the route the player actually explored: market -> tavern ->
    // brothel -> one-way secret passage -> castle -> castle front. Normal
    // return travel must not replay it.
    for (transition_id, from_place, to_place) in [
        ("market_to_tavern", "market", "tavern"),
        ("tavern_to_brothel", "tavern", "brothel"),
        ("secret_to_castle", "brothel", "castle"),
        ("castle_to_front", "castle", "castle_front"),
    ] {
        canon.insert_transition(Transition {
            transition_id: transition_id.to_string(),
            passage_id: transition_id.to_string(),
            directionality: PassageDirectionality::OneWay,
            from_place: from_place.to_string(),
            to_place: to_place.to_string(),
            visible: true,
            passable: true,
            time_cost: 1,
            risk: "none".to_string(),
            ..Default::default()
        });
    }

    canon.insert_travel_network(TravelNetwork {
        network_id: "city_surface".to_string(),
        scope_id: "city".to_string(),
        default_for_normal_travel: true,
        passable: true,
        ..Default::default()
    });
    for anchor_id in ["castle_gate", "market_square", "tavern_street"] {
        canon.insert_travel_anchor(anchor(anchor_id, "city_surface"));
    }
    for value in [
        access("castle_front_access", "castle_front", "castle_gate"),
        access("market_access", "market", "market_square"),
        access("tavern_access", "tavern", "tavern_street"),
    ] {
        canon.insert_travel_access(value);
    }
    canon.insert_travel_link(link(
        "castle_to_market_streets",
        "castle_gate",
        "market_square",
        15,
        "low",
    ));
    canon.insert_travel_link(link(
        "market_to_tavern_street",
        "market_square",
        "tavern_street",
        3,
        "none",
    ));

    let market = plan_travel(&canon, "market", None).expect("surface route to market");
    assert_eq!(market.link_ids, ["castle_to_market_streets"]);
    assert_eq!(market.total_time_minutes, 15);
    assert!(!market.link_ids.iter().any(|id| id == "secret_to_castle"));

    let tavern = plan_travel(&canon, "tavern", None).expect("surface route to tavern");
    assert_eq!(
        tavern.link_ids,
        ["castle_to_market_streets", "market_to_tavern_street"]
    );
    assert_eq!(tavern.total_time_minutes, 18);
}
