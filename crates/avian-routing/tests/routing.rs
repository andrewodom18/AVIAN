use avian_routing::*;

fn node(node_id: &str) -> NetworkNode {
    NetworkNode {
        node_id: node_id.into(),
        relay_allowed: true,
    }
}

fn link(
    link_id: &str,
    source: &str,
    destination: &str,
    latency_ms: f64,
    capacity_bps: u64,
) -> LinkObservation {
    LinkObservation {
        link_id: link_id.into(),
        source: source.into(),
        destination: destination.into(),
        interface_name: "radio0".into(),
        available: true,
        metered: false,
        observed_at_ms: 10_000,
        round_trip_time_ms: Some(latency_ms),
        loss_fraction: Some(0.0),
        jitter_ms: None,
        available_capacity_bps: Some(capacity_bps),
        topology_valid_until_ms: None,
        capacity_valid_until_ms: None,
        capacity_source: None,
    }
}

fn diamond() -> NetworkSnapshot {
    NetworkSnapshot {
        nodes: ["ground", "fast", "wide", "drone"].map(node).into(),
        links: vec![
            link("ground-fast", "ground", "fast", 3.0, 1_000_000),
            link("fast-drone", "fast", "drone", 3.0, 1_000_000),
            link("ground-wide", "ground", "wide", 20.0, 20_000_000),
            link("wide-drone", "wide", "drone", 20.0, 20_000_000),
        ],
    }
}

#[test]
fn control_and_video_choose_different_paths_without_fabricating_jitter() {
    let planner = RoutePlanner::default();
    let snapshot = diamond();
    let control = planner
        .plan(&snapshot, "ground", "drone", TrafficClass::Control, 10_000)
        .unwrap();
    let video = planner
        .plan(&snapshot, "ground", "drone", TrafficClass::Video, 10_000)
        .unwrap();
    assert_eq!(control.link_ids, ["ground-fast", "fast-drone"]);
    assert_eq!(video.link_ids, ["ground-wide", "wide-drone"]);
    assert_eq!(video.bottleneck_capacity_bps, Some(20_000_000));
    assert_eq!(control.estimated_jitter_ms, None);
}

#[test]
fn direct_cellular_cannot_bypass_video_or_bulk_restrictions() {
    let mut snapshot = diamond();
    let mut cellular = link("cellular", "ground", "drone", 1.0, 500_000_000);
    cellular.metered = true;
    cellular.interface_name = "wwan0".into();
    snapshot.links.push(cellular);
    let planner = RoutePlanner::default();
    assert_eq!(
        planner
            .plan(&snapshot, "ground", "drone", TrafficClass::Control, 10_000)
            .unwrap()
            .link_ids,
        ["cellular"]
    );
    for traffic_class in [TrafficClass::Video, TrafficClass::Bulk] {
        assert!(
            !planner
                .plan(&snapshot, "ground", "drone", traffic_class, 10_000)
                .unwrap()
                .uses_metered_link
        );
    }
    snapshot.links.retain(|edge| edge.metered);
    assert!(matches!(
        planner.plan(&snapshot, "ground", "drone", TrafficClass::Video, 10_000),
        Err(RoutingError::NoRoute { .. })
    ));
    let enabled = RoutePlanner::new(RoutingPolicy {
        allow_metered_video: true,
        ..Default::default()
    });
    assert!(
        enabled
            .plan(&snapshot, "ground", "drone", TrafficClass::Video, 10_000)
            .unwrap()
            .uses_metered_link
    );
    assert!(
        enabled
            .plan(&snapshot, "ground", "drone", TrafficClass::Bulk, 10_000)
            .is_err()
    );
}

#[test]
fn reverse_direction_enforces_the_same_metered_policy() {
    let mut snapshot = diamond();
    snapshot.links = snapshot
        .links
        .iter()
        .map(|edge| {
            let mut reverse = edge.clone();
            std::mem::swap(&mut reverse.source, &mut reverse.destination);
            reverse
        })
        .collect();
    snapshot.links[1].metered = true;
    let route = RoutePlanner::default()
        .plan(&snapshot, "drone", "ground", TrafficClass::Video, 10_000)
        .unwrap();
    assert_eq!(route.link_ids, ["wide-drone", "ground-wide"]);
}

#[test]
fn parallel_physical_connections_are_selected_independently() {
    let mut cellular = link("cellular", "ground", "drone", 2.0, 1_000_000);
    cellular.metered = true;
    cellular.interface_name = "wwan0".into();
    let snapshot = NetworkSnapshot {
        nodes: vec![node("ground"), node("drone")],
        links: vec![cellular, link("radio", "ground", "drone", 8.0, 10_000_000)],
    };
    let planner = RoutePlanner::default();
    assert_eq!(
        planner
            .plan(&snapshot, "ground", "drone", TrafficClass::Control, 10_000)
            .unwrap()
            .link_ids,
        ["cellular"]
    );
    assert_eq!(
        planner
            .plan(&snapshot, "ground", "drone", TrafficClass::Video, 10_000)
            .unwrap()
            .link_ids,
        ["radio"]
    );
}

#[test]
fn downstream_bottleneck_does_not_hide_a_more_reliable_prefix() {
    let mut unreliable = link("wide-lossy", "source", "middle", 1.0, 100);
    unreliable.loss_fraction = Some(0.1);
    let snapshot = NetworkSnapshot {
        nodes: vec![node("source"), node("middle"), node("destination")],
        links: vec![
            unreliable,
            link("narrow-reliable", "source", "middle", 10.0, 50),
            link("bottleneck", "middle", "destination", 1.0, 10),
        ],
    };
    let route = RoutePlanner::default()
        .plan(
            &snapshot,
            "source",
            "destination",
            TrafficClass::Video,
            10_000,
        )
        .unwrap();
    assert_eq!(route.link_ids, ["narrow-reliable", "bottleneck"]);
    assert_eq!(route.estimated_loss_fraction, 0.0);
}

#[test]
fn expired_topology_future_total_loss_and_unavailable_links_do_not_create_paths() {
    let planner = RoutePlanner::default();
    let baseline = NetworkSnapshot {
        nodes: vec![node("source"), node("destination")],
        links: vec![link("radio", "source", "destination", 5.0, 100)],
    };
    for mode in ["stale", "future", "total_loss", "unavailable"] {
        let mut snapshot = baseline.clone();
        match mode {
            "stale" => snapshot.links[0].observed_at_ms = 0,
            "future" => snapshot.links[0].observed_at_ms = 20_000,
            "total_loss" => snapshot.links[0].loss_fraction = Some(1.0),
            "unavailable" => snapshot.links[0].available = false,
            _ => unreachable!(),
        }
        assert!(
            matches!(
                planner.plan(
                    &snapshot,
                    "source",
                    "destination",
                    TrafficClass::Control,
                    10_000
                ),
                Err(RoutingError::NoRoute { .. })
            ),
            "{mode}"
        );
    }
    let mut unknown_capacity = baseline;
    unknown_capacity.links[0].available_capacity_bps = None;
    assert!(
        planner
            .plan(
                &unknown_capacity,
                "source",
                "destination",
                TrafficClass::Video,
                10_000
            )
            .unwrap()
            .degraded
    );
    assert_eq!(
        planner
            .plan(
                &unknown_capacity,
                "source",
                "destination",
                TrafficClass::Control,
                10_000
            )
            .unwrap()
            .bottleneck_capacity_bps,
        None
    );
}

#[test]
fn relay_permissions_do_not_prevent_endpoint_access() {
    let mut snapshot = diamond();
    snapshot
        .nodes
        .iter_mut()
        .filter(|node| node.node_id == "fast" || node.node_id == "drone")
        .for_each(|node| node.relay_allowed = false);
    let planner = RoutePlanner::default();
    assert_eq!(
        planner
            .plan(&snapshot, "ground", "drone", TrafficClass::Control, 10_000)
            .unwrap()
            .node_ids,
        ["ground", "wide", "drone"]
    );
    assert!(
        planner
            .plan(&snapshot, "ground", "fast", TrafficClass::Control, 10_000)
            .is_ok()
    );
}

#[test]
fn cycles_and_input_order_do_not_change_deterministic_routes() {
    let mut snapshot = diamond();
    snapshot
        .links
        .push(link("cycle", "fast", "ground", 0.0, 20_000_000));
    let planner = RoutePlanner::default();
    let expected = planner
        .plan(&snapshot, "ground", "drone", TrafficClass::Control, 10_000)
        .unwrap();
    snapshot.nodes.reverse();
    snapshot.links.reverse();
    assert_eq!(
        planner
            .plan(&snapshot, "ground", "drone", TrafficClass::Control, 10_000)
            .unwrap(),
        expected
    );
    assert_eq!(expected.node_ids.len(), 3);
}

#[test]
fn invalid_values_and_duplicate_identifiers_are_rejected() {
    for value in [f64::NAN, f64::INFINITY, -1.0] {
        let mut snapshot = diamond();
        snapshot.links[0].round_trip_time_ms = Some(value);
        assert!(matches!(
            RoutePlanner::default().plan(
                &snapshot,
                "ground",
                "drone",
                TrafficClass::Control,
                10_000
            ),
            Err(RoutingError::InvalidSnapshot(_))
        ));
    }
    let mut snapshot = diamond();
    snapshot.links.push(snapshot.links[0].clone());
    assert!(matches!(
        RoutePlanner::default().plan(&snapshot, "ground", "drone", TrafficClass::Control, 10_000),
        Err(RoutingError::InvalidSnapshot(_))
    ));
}

#[test]
fn overflowing_latency_does_not_appear_as_a_valid_route() {
    let snapshot = NetworkSnapshot {
        nodes: vec![node("source"), node("middle"), node("destination")],
        links: vec![
            link("first", "source", "middle", f64::MAX, 100),
            link("second", "middle", "destination", f64::MAX, 100),
        ],
    };
    assert!(matches!(
        RoutePlanner::default().plan(
            &snapshot,
            "source",
            "destination",
            TrafficClass::Control,
            10_000
        ),
        Err(RoutingError::NoRoute { .. })
    ));
}

fn flow(flow_id: &str) -> FlowRequest {
    FlowRequest {
        flow_id: flow_id.into(),
        source: "ground".into(),
        destination: "drone".into(),
        traffic_class: TrafficClass::Control,
    }
}

fn refresh(snapshot: &mut NetworkSnapshot, now_ms: u64) {
    for link in &mut snapshot.links {
        link.observed_at_ms = now_ms;
    }
}

#[test]
fn flow_improvements_wait_but_failed_paths_switch_immediately() {
    let mut snapshot = diamond();
    let mut router = FlowRouter::new(RoutePlanner::default(), FlowPolicy::default()).unwrap();
    assert_eq!(
        router
            .route(&snapshot, flow("control"), 10_000)
            .unwrap()
            .reason,
        RouteChangeReason::NewFlow
    );
    snapshot.links[0].round_trip_time_ms = Some(100.0);
    refresh(&mut snapshot, 11_000);
    let pending = router.route(&snapshot, flow("control"), 11_000).unwrap();
    assert_eq!(pending.reason, RouteChangeReason::ImprovementPending);
    assert_eq!(pending.route.link_ids, ["ground-fast", "fast-drone"]);
    refresh(&mut snapshot, 16_000);
    assert_eq!(
        router
            .route(&snapshot, flow("control"), 16_000)
            .unwrap()
            .reason,
        RouteChangeReason::ImprovedPath
    );
    snapshot.links[2].available = false;
    let fallback = router.route(&snapshot, flow("control"), 16_001).unwrap();
    assert_eq!(fallback.reason, RouteChangeReason::PreviousPathUnavailable);
    assert_eq!(fallback.route.link_ids, ["ground-fast", "fast-drone"]);
    snapshot.links[0].available = false;
    assert!(matches!(
        router.route(&snapshot, flow("control"), 16_002),
        Err(RoutingError::NoRoute { .. })
    ));
    assert_eq!(router.active_flow_count(), 0);
}

#[test]
fn active_flow_budget_and_expiry_do_not_limit_network_membership() {
    let snapshot = diamond();
    let mut router = FlowRouter::new(
        RoutePlanner::default(),
        FlowPolicy {
            maximum_active_flows: 1,
            idle_timeout_ms: 1_000,
            ..Default::default()
        },
    )
    .unwrap();
    router.route(&snapshot, flow("first"), 10_000).unwrap();
    assert_eq!(
        router.route(&snapshot, flow("second"), 10_001),
        Err(RoutingError::FlowCapacityReached(1))
    );
    let mut changed = flow("first");
    changed.traffic_class = TrafficClass::Telemetry;
    assert_eq!(
        router.route(&snapshot, changed, 10_002),
        Err(RoutingError::FlowConflict)
    );
    assert_eq!(router.expire_idle_flows(11_000), 1);
    assert!(router.route(&snapshot, flow("second"), 11_000).is_ok());
}

#[test]
fn a_long_chain_preserves_class_and_hop_order() {
    let labels: Vec<String> = (0..96).map(|number| format!("node-{number}")).collect();
    let snapshot = NetworkSnapshot {
        nodes: labels.iter().map(|label| node(label)).collect(),
        links: labels
            .windows(2)
            .enumerate()
            .map(|(number, pair)| link(&format!("edge-{number}"), &pair[0], &pair[1], 1.0, 1_000))
            .collect(),
    };
    let route = RoutePlanner::default()
        .plan(
            &snapshot,
            &labels[0],
            labels.last().unwrap(),
            TrafficClass::Video,
            10_000,
        )
        .unwrap();
    assert_eq!(route.node_ids, labels);
    assert_eq!(route.traffic_class, TrafficClass::Video);
    assert_eq!(route.link_ids.len(), 95);
}

#[test]
fn authenticated_topology_outlives_quality_but_unknown_video_is_explicitly_degraded() {
    let mut snapshot = NetworkSnapshot {
        nodes: vec![node("source"), node("destination")],
        links: vec![link("radio", "source", "destination", 5.0, 100)],
    };
    snapshot.links[0].observed_at_ms = 0;
    snapshot.links[0].topology_valid_until_ms = Some(20_000);
    let planner = RoutePlanner::default();
    for class in [TrafficClass::Control, TrafficClass::Video] {
        let route = planner
            .plan(&snapshot, "source", "destination", class, 10_000)
            .unwrap();
        assert!(route.degraded);
        assert!(
            route
                .degraded_reasons
                .iter()
                .any(|reason| reason == "quality_stale")
        );
        assert_eq!(route.bottleneck_capacity_bps, None);
    }
    assert!(
        planner
            .plan(
                &snapshot,
                "source",
                "destination",
                TrafficClass::Video,
                20_001
            )
            .is_err()
    );
    snapshot.links[0].metered = true;
    assert!(
        planner
            .plan(
                &snapshot,
                "source",
                "destination",
                TrafficClass::Video,
                10_000
            )
            .is_err()
    );
}

#[test]
fn passive_activity_is_not_capacity_and_probe_evidence_expires_separately() {
    let mut snapshot = diamond();
    for link in &mut snapshot.links {
        link.available_capacity_bps = None;
    }
    let planner = RoutePlanner::default();
    let unknown = planner
        .plan(&snapshot, "ground", "drone", TrafficClass::Video, 10_000)
        .unwrap();
    assert!(unknown.degraded);
    assert_eq!(unknown.bottleneck_capacity_bps, None);
    for link in &mut snapshot.links {
        link.available_capacity_bps = Some(2_000_000);
        link.capacity_valid_until_ms = Some(10_100);
        link.capacity_source = Some(avian_routing::CapacitySource::BoundedProbe);
    }
    assert_eq!(
        planner
            .plan(&snapshot, "ground", "drone", TrafficClass::Video, 10_000)
            .unwrap()
            .capacity_source,
        Some(avian_routing::CapacitySource::BoundedProbe)
    );
    assert!(
        planner
            .plan(&snapshot, "ground", "drone", TrafficClass::Video, 10_101)
            .unwrap()
            .degraded
    );
}

#[test]
fn expired_capacity_returns_to_healthy_fallback_after_hold_without_noise_churn() {
    let mut snapshot = diamond();
    let mut router = FlowRouter::new(RoutePlanner::default(), FlowPolicy::default()).unwrap();
    let request = FlowRequest {
        flow_id: "video".into(),
        source: "ground".into(),
        destination: "drone".into(),
        traffic_class: TrafficClass::Video,
    };
    assert_eq!(
        router
            .route(&snapshot, request.clone(), 10_000)
            .unwrap()
            .route
            .node_ids[1],
        "wide"
    );
    for link in &mut snapshot.links {
        link.available_capacity_bps = None;
    }
    refresh(&mut snapshot, 11_000);
    assert_eq!(
        router
            .route(&snapshot, request.clone(), 11_000)
            .unwrap()
            .reason,
        RouteChangeReason::ImprovementPending
    );
    refresh(&mut snapshot, 16_000);
    assert_eq!(
        router
            .route(&snapshot, request, 16_000)
            .unwrap()
            .route
            .node_ids[1],
        "fast"
    );
}

#[test]
fn small_capacity_noise_does_not_start_improvement_hold() {
    let mut snapshot = diamond();
    let mut router = FlowRouter::new(RoutePlanner::default(), FlowPolicy::default()).unwrap();
    let request = FlowRequest {
        flow_id: "video".into(),
        source: "ground".into(),
        destination: "drone".into(),
        traffic_class: TrafficClass::Video,
    };
    router.route(&snapshot, request.clone(), 10_000).unwrap();
    snapshot.links[0].available_capacity_bps = Some(20_500_000);
    snapshot.links[1].available_capacity_bps = Some(20_500_000);
    refresh(&mut snapshot, 11_000);
    assert_eq!(
        router
            .route(&snapshot, request.clone(), 11_000)
            .unwrap()
            .reason,
        RouteChangeReason::Unchanged
    );
    refresh(&mut snapshot, 20_000);
    assert_eq!(
        router
            .route(&snapshot, request, 20_000)
            .unwrap()
            .route
            .node_ids[1],
        "wide"
    );
}

#[test]
fn batched_plans_match_single_destination_semantics_on_seeded_graphs() {
    let mut random_state = 0x5eed_u64;
    let mut next = || {
        random_state = random_state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        random_state
    };
    for _ in 0..96 {
        let count = 4 + (next() % 6) as usize;
        let mut snapshot = NetworkSnapshot {
            nodes: (0..count)
                .map(|index| NetworkNode {
                    node_id: index.to_string(),
                    relay_allowed: next() % 5 != 0,
                })
                .collect(),
            links: Vec::new(),
        };
        for source in 0..count {
            for destination in 0..count {
                if source == destination || next() % 3 == 0 {
                    continue;
                }
                let identifier = format!("{source}-{destination}");
                let mut edge = link(
                    &identifier,
                    &source.to_string(),
                    &destination.to_string(),
                    (next() % 80) as f64,
                    (next() % 6) * 100_000,
                );
                edge.loss_fraction = match next() % 6 {
                    0 => None,
                    1 => Some(1.0),
                    value => Some(value as f64 / 20.0),
                };
                edge.available = next() % 9 != 0;
                edge.metered = next() % 5 == 0;
                if next() % 4 == 0 {
                    edge.available_capacity_bps = None;
                }
                if next() % 5 == 0 {
                    edge.observed_at_ms = 0;
                    edge.topology_valid_until_ms = Some(20_000);
                }
                snapshot.links.push(edge);
            }
        }
        for class in [
            TrafficClass::Control,
            TrafficClass::Telemetry,
            TrafficClass::Video,
            TrafficClass::Bulk,
        ] {
            let planner = RoutePlanner::default();
            let batch = planner.plan_all(&snapshot, "0", class, 10_000).unwrap();
            for destination in &snapshot.nodes {
                match planner.plan(&snapshot, "0", &destination.node_id, class, 10_000) {
                    Ok(single) => assert_eq!(batch.get(&destination.node_id), Some(&single)),
                    Err(RoutingError::NoRoute { .. }) => {
                        assert!(!batch.contains_key(&destination.node_id))
                    }
                    Err(error) => panic!("unexpected single-route error: {error}"),
                }
            }
        }
    }
}

#[test]
fn batched_flow_transitions_match_individual_hysteresis_and_failures() {
    let mut snapshot = diamond();
    let requests: Vec<_> = [TrafficClass::Control, TrafficClass::Video]
        .into_iter()
        .map(|class| FlowRequest {
            flow_id: format!("{class:?}"),
            source: "ground".into(),
            destination: "drone".into(),
            traffic_class: class,
        })
        .collect();
    let mut batch = FlowRouter::new(RoutePlanner::default(), FlowPolicy::default()).unwrap();
    let mut individual = FlowRouter::new(RoutePlanner::default(), FlowPolicy::default()).unwrap();
    for (index, now) in [10_000, 11_000, 16_000, 16_001, 17_000]
        .into_iter()
        .enumerate()
    {
        if index == 1 {
            snapshot.links[0].round_trip_time_ms = Some(120.0);
        }
        if index == 3 {
            snapshot.links[2].available = false;
        }
        if index == 4 {
            snapshot.nodes[1].relay_allowed = false;
        }
        refresh(&mut snapshot, now);
        let batched = batch.route_batch(&snapshot, requests.clone(), now).unwrap();
        for (request, result) in requests.iter().cloned().zip(batched) {
            assert_eq!(result, individual.route(&snapshot, request, now));
        }
    }
}

fn parallel_improvement_snapshot() -> NetworkSnapshot {
    let mut snapshot = diamond();
    snapshot
        .links
        .push(link("parallel-wide", "ground", "wide", 21.0, 20_000_000));
    for link in &mut snapshot.links {
        link.jitter_ms = Some(0.0);
    }
    snapshot
}

#[test]
fn equivalent_parallel_candidates_cannot_starve_a_sustained_healthy_improvement() {
    let mut snapshot = parallel_improvement_snapshot();
    let mut router = FlowRouter::new(RoutePlanner::default(), FlowPolicy::default()).unwrap();
    router.route(&snapshot, flow("control"), 10_000).unwrap();
    snapshot.links[0].loss_fraction = Some(0.35);
    for now in [11_000, 12_000, 13_000, 14_000, 15_000] {
        snapshot.links[2].round_trip_time_ms = Some(if now % 2_000 == 0 { 21.0 } else { 20.0 });
        snapshot.links[4].round_trip_time_ms = Some(if now % 2_000 == 0 { 20.0 } else { 21.0 });
        refresh(&mut snapshot, now);
        let pending = router.route(&snapshot, flow("control"), now).unwrap();
        assert_eq!(pending.reason, RouteChangeReason::ImprovementPending);
        assert_eq!(pending.route.link_ids, ["ground-fast", "fast-drone"]);
    }
    refresh(&mut snapshot, 16_000);
    let changed = router.route(&snapshot, flow("control"), 16_000).unwrap();
    assert_eq!(changed.reason, RouteChangeReason::ImprovedPath);
    assert_eq!(changed.route.link_ids, ["ground-wide", "wide-drone"]);
    assert_eq!(changed.improvement_hold_ms, Some(5_000));
    // The hold never protects a failed connection from immediate replacement.
    snapshot.links[2].available = false;
    let failed = router.route(&snapshot, flow("control"), 16_001).unwrap();
    assert_eq!(failed.reason, RouteChangeReason::PreviousPathUnavailable);
    assert_eq!(failed.route.link_ids, ["parallel-wide", "wide-drone"]);
}

#[test]
fn a_materially_better_candidate_starts_a_new_healthy_hold() {
    let mut snapshot = parallel_improvement_snapshot();
    let mut router = FlowRouter::new(RoutePlanner::default(), FlowPolicy::default()).unwrap();
    router.route(&snapshot, flow("control"), 10_000).unwrap();
    snapshot.links[0].loss_fraction = Some(0.35);
    refresh(&mut snapshot, 11_000);
    router.route(&snapshot, flow("control"), 11_000).unwrap();
    snapshot.links[4].round_trip_time_ms = Some(1.0);
    for now in [13_000, 16_000] {
        refresh(&mut snapshot, now);
        assert_eq!(
            router
                .route(&snapshot, flow("control"), now)
                .unwrap()
                .reason,
            RouteChangeReason::ImprovementPending
        );
    }
    refresh(&mut snapshot, 18_000);
    let changed = router.route(&snapshot, flow("control"), 18_000).unwrap();
    assert_eq!(changed.reason, RouteChangeReason::ImprovedPath);
    assert_eq!(changed.route.link_ids, ["parallel-wide", "wide-drone"]);
    assert_eq!(changed.improvement_hold_ms, Some(5_000));
}

#[test]
fn revoked_pending_relay_cannot_reuse_its_healthy_hold() {
    let mut snapshot = diamond();
    snapshot.nodes.push(node("alternate"));
    snapshot.links.push(link(
        "ground-alternate",
        "ground",
        "alternate",
        21.0,
        20_000_000,
    ));
    snapshot.links.push(link(
        "alternate-drone",
        "alternate",
        "drone",
        21.0,
        20_000_000,
    ));
    for link in &mut snapshot.links {
        link.jitter_ms = Some(0.0);
    }
    let mut router = FlowRouter::new(RoutePlanner::default(), FlowPolicy::default()).unwrap();
    router.route(&snapshot, flow("control"), 10_000).unwrap();
    snapshot.links[0].loss_fraction = Some(0.35);
    refresh(&mut snapshot, 11_000);
    router.route(&snapshot, flow("control"), 11_000).unwrap();
    snapshot
        .nodes
        .iter_mut()
        .find(|node| node.node_id == "wide")
        .unwrap()
        .relay_allowed = false;
    for now in [13_000, 16_000] {
        refresh(&mut snapshot, now);
        assert_eq!(
            router
                .route(&snapshot, flow("control"), now)
                .unwrap()
                .reason,
            RouteChangeReason::ImprovementPending
        );
    }
    refresh(&mut snapshot, 18_000);
    let changed = router.route(&snapshot, flow("control"), 18_000).unwrap();
    assert_eq!(
        changed.route.link_ids,
        ["ground-alternate", "alternate-drone"]
    );
    assert_eq!(changed.improvement_hold_ms, Some(5_000));
}
