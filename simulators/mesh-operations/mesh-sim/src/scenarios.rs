//! Scripted reference and visual demonstration scenarios.
use crate::projection::{
    chud_event, visual_step, VisualFormationSummary, VisualScenario, VisualStep,
};
use crate::{RecordKey, SimNetwork, SimulationError};
use ed25519_dalek::SigningKey;
use mesh_core::{
    Altitude, DeliveryClass, EmergencyAck, EmergencyAction, EmergencyCommand, FlightStack,
    LinkCandidate, LinkGeometry, LinkId, LinkMetrics, LinkOrchestrator, MeshPayload, MissionState,
    MissionStatus, NodeId, NodeProfile, ReplayGuard, Telemetry, TopologyPlanner, TransportKind,
    SYSTEM_MAX_MSL_M,
};
use serde::Serialize;
use std::collections::BTreeSet;
use uuid::Uuid;
use vehicle_adapters::{SimulatedVehicleAdapter, VehicleAdapter};
pub(crate) const DEMONSTRATED_AIRCRAFT_COUNT: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScenarioReport {
    pub mission_reached_all_nodes: bool,
    pub ground_partitioned_without_stopping_aircraft: bool,
    pub crashed_node_did_not_stop_mesh: bool,
    pub degraded_link_failed_over: bool,
    pub betaflight_command_verified: bool,
    pub betaflight_gps_rescue_executed: bool,
    pub emergency_ack_returned: bool,
    pub recovered_node_converged: bool,
}

impl ScenarioReport {
    pub fn passed(&self) -> bool {
        self.mission_reached_all_nodes
            && self.ground_partitioned_without_stopping_aircraft
            && self.crashed_node_did_not_stop_mesh
            && self.degraded_link_failed_over
            && self.betaflight_command_verified
            && self.betaflight_gps_rescue_executed
            && self.emergency_ack_returned
            && self.recovered_node_converged
    }
}

/// Produces a replayable topology trace for the stakeholder visualizer. Every
/// connectivity and synchronization value is read back from `SimNetwork`; the
/// UI never invents node or link state.
pub async fn run_visual_scenario() -> Result<VisualScenario, SimulationError> {
    let ground = NodeId::from("ground-1");
    let ardupilot = NodeId::from("ardu-1");
    let px4 = NodeId::from("px4-1");
    let betaflight = NodeId::from("beta-1");
    let mission_key = RecordKey::from("mission/current");

    let mut network = SimNetwork::default();
    network.add_node(NodeProfile::ground(ground.clone()));
    let mut steps = vec![visual_step(
        &network,
        "radio-connected",
        "Radio connected to the CHUD host",
        "A physical radio is plugged into the management network. CHUD begins a vendor-aware inventory refresh without exposing credentials to AVIAN.",
        0,
        "RADIO DISCOVERY",
        Some(chud_event("GET", "/api/radio/devices", "LOCAL RADIO DETECTED")),
        &mission_key,
        None,
        false,
        "Management link detected",
        false,
    )];

    network.add_node(NodeProfile::aircraft(
        ardupilot.clone(),
        FlightStack::ArduPilot,
        SYSTEM_MAX_MSL_M,
    )?);
    network.add_node(NodeProfile::aircraft(
        px4.clone(),
        FlightStack::Px4,
        SYSTEM_MAX_MSL_M,
    )?);
    network.add_node(NodeProfile::aircraft(
        betaflight.clone(),
        FlightStack::Betaflight,
        SYSTEM_MAX_MSL_M,
    )?);
    steps.push(visual_step(
        &network,
        "radios-discovered",
        "New radio nodes discovered",
        "CHUD returns the reachable radio inventory. Each physical MAC becomes a distinct candidate node before any mesh path is assumed.",
        1_500,
        "RADIO DISCOVERY",
        Some(chud_event("GET", "/api/radio/devices", "4 RADIOS DISCOVERED")),
        &mission_key,
        None,
        false,
        "Inventory discovered",
        false,
    ));
    steps.push(visual_step(
        &network,
        "radio-snapshot",
        "Current radio settings captured",
        "CHUD snapshots the live configuration and capabilities so the desired mesh plan can be checked before any change is applied.",
        3_000,
        "CHUD CONFIGURATION",
        Some(chud_event("GET", "/api/radio/snapshot", "SNAPSHOT VERIFIED")),
        &mission_key,
        None,
        false,
        "Pre-change snapshot captured",
        false,
    ));
    steps.push(visual_step(
        &network,
        "radio-config-applied",
        "Mesh configuration applied through CHUD",
        "CHUD applies the validated desired configuration to the discovered radios while retaining the pre-change snapshot for recovery.",
        4_500,
        "CHUD CONFIGURATION",
        Some(chud_event("POST", "/api/radio/apply", "APPLY COMPLETE")),
        &mission_key,
        None,
        false,
        "Radio configuration applied",
        false,
    ));
    steps.push(visual_step(
        &network,
        "radio-config-confirmed",
        "Radio configuration verified and confirmed",
        "CHUD reads the effective settings back, confirms the guarded transaction, and releases the radios for AVIAN formation startup.",
        6_000,
        "CHUD CONFIGURATION",
        Some(chud_event("POST", "/api/radio/confirm", "READBACK CONFIRMED")),
        &mission_key,
        None,
        false,
        "Radios ready for formation",
        false,
    ));

    steps.push(visual_step(
        &network,
        "formation-ready",
        "Formation identities ready",
        "Four authenticated AVIAN identities are online. No peer path is assumed until a link is observed.",
        7_500,
        "AVIAN FORMATION",
        None,
        &mission_key,
        None,
        false,
        "Awaiting peer links",
        false,
    ));

    network.connect(&ground, &ardupilot)?;
    network.connect(&ardupilot, &px4)?;
    network.connect(&px4, &betaflight)?;
    network.connect(&betaflight, &ardupilot)?;
    steps.push(visual_step(
        &network,
        "mesh-formed",
        "Leaderless mesh formed",
        "Observed peer paths create one connected component without assigning a permanent leader.",
        9_000,
        "AVIAN FORMATION",
        None,
        &mission_key,
        None,
        false,
        "All nodes connected",
        false,
    ));

    network.publish(
        &ground,
        mission_key.clone(),
        MeshPayload::Mission(MissionState {
            mission_id: Uuid::from_u128(10),
            objective: "visual mesh continuity demonstration".to_owned(),
            generation: 1,
            status: MissionStatus::Active,
        }),
        DeliveryClass::Mission,
    )?;
    network.synchronize();
    steps.push(visual_step(
        &network,
        "mission-synchronized",
        "Mission state synchronized",
        "Durable mission state converges across every connected peer.",
        10_500,
        "MISSION SYNCHRONIZATION",
        None,
        &mission_key,
        None,
        false,
        "Mission synchronized",
        false,
    ));

    network.set_link_enabled(&ground, &ardupilot, false)?;
    steps.push(visual_step(
        &network,
        "ground-partitioned",
        "Ground station disconnected",
        "The airborne component remains connected and retains the current mission while ground is isolated.",
        12_000,
        "CONTINUITY TEST",
        None,
        &mission_key,
        None,
        false,
        "Airborne mesh autonomous",
        false,
    ));

    let telemetry_key = RecordKey::from("telemetry/ardu-1");
    network.publish(
        &ardupilot,
        telemetry_key,
        MeshPayload::Telemetry(sample_telemetry(ardupilot.clone(), 5_000)?),
        DeliveryClass::Telemetry,
    )?;
    network.synchronize();
    steps.push(visual_step(
        &network,
        "airborne-continuity",
        "Airborne peers continue exchanging state",
        "Fresh telemetry moves through the remaining peer graph even though ground is unavailable.",
        13_500,
        "CONTINUITY TEST",
        None,
        &mission_key,
        None,
        false,
        "Telemetry flowing peer-to-peer",
        false,
    ));

    steps.push(visual_step(
        &network,
        "link-degraded",
        "Primary path degraded",
        "Measured link health crosses the policy threshold and AVIAN prepares the alternate path.",
        15_000,
        "FAILOVER TEST",
        None,
        &mission_key,
        Some((&px4, &betaflight)),
        false,
        "Failover evaluating",
        false,
    ));

    network.set_link_enabled(&px4, &betaflight, false)?;
    let failover_selected = failover_demonstration();
    steps.push(visual_step(
        &network,
        "path-failover",
        "Traffic moved to a healthy path",
        "The degraded hop is removed from service while the alternate peer path preserves the airborne component.",
        16_500,
        "FAILOVER TEST",
        None,
        &mission_key,
        None,
        failover_selected,
        "Alternate path active",
        false,
    ));

    network.set_node_online(&px4, false)?;
    steps.push(visual_step(
        &network,
        "node-failure",
        "Aircraft node lost",
        "One aircraft drops out. Remaining peers continue without electing a replacement leader.",
        18_000,
        "NODE FAILURE TEST",
        None,
        &mission_key,
        None,
        true,
        "Mesh operating with one node lost",
        false,
    ));

    network.set_link_enabled(&ground, &ardupilot, true)?;
    network.set_link_enabled(&px4, &betaflight, true)?;
    network.set_node_online(&px4, true)?;
    network.synchronize();
    steps.push(visual_step(
        &network,
        "network-recovered",
        "Network healed and converged",
        "Ground and the recovered aircraft rejoin, then reconcile the durable mission state.",
        19_500,
        "RECOVERY TEST",
        None,
        &mission_key,
        None,
        false,
        "Full formation restored",
        false,
    ));

    let verification = run_reference_scenario().await?;
    steps.push(visual_step(
        &network,
        "command-acknowledged",
        "Signed emergency action acknowledged",
        "The command signature, expiry, replay guard, vehicle action, and acknowledgement path all validate.",
        21_000,
        "COMMAND VERIFICATION",
        None,
        &mission_key,
        None,
        false,
        "Mission-capable mesh verified",
        verification.passed(),
    ));

    steps.extend(run_large_formation_steps(verification.passed())?);

    Ok(VisualScenario {
        schema_version: 1,
        name: "AVIAN leaderless mesh continuity".to_owned(),
        description:
            "Deterministic CHUD radio discovery/configuration and AVIAN topology, synchronization, failure, failover, and recovery trace."
                .to_owned(),
        steps,
    })
}

fn run_large_formation_steps(
    signed_command_verified: bool,
) -> Result<Vec<VisualStep>, SimulationError> {
    let ground = NodeId::from("scale-ground-1");
    let aircraft = (1..=DEMONSTRATED_AIRCRAFT_COUNT)
        .map(|index| NodeId::from(format!("scale-aircraft-{index:03}")))
        .collect::<Vec<_>>();
    let plan = TopologyPlanner::default().plan(&aircraft)?;
    let planned_aircraft_links = plan.edge_count();
    let failed_link = (
        aircraft[0].clone(),
        plan.neighbors(&aircraft[0])
            .and_then(BTreeSet::first)
            .expect("a planned 200-aircraft node has neighbors")
            .clone(),
    );

    let mut network = SimNetwork::default();
    network.add_node(NodeProfile::ground(ground.clone()));
    for (index, node_id) in aircraft.iter().enumerate() {
        let flight_stack = match index % 3 {
            0 => FlightStack::ArduPilot,
            1 => FlightStack::Px4,
            _ => FlightStack::Betaflight,
        };
        network.add_node(NodeProfile::aircraft(
            node_id.clone(),
            flight_stack,
            SYSTEM_MAX_MSL_M,
        )?);
    }

    for node_id in &aircraft {
        for peer_id in plan
            .neighbors(node_id)
            .expect("planned aircraft is present in the topology")
        {
            if node_id < peer_id {
                network.connect(node_id, peer_id)?;
            }
        }
    }
    network.connect(&ground, &aircraft[0])?;

    let mission_key = RecordKey::from("mission/large-formation");
    network.publish(
        &ground,
        mission_key.clone(),
        MeshPayload::Mission(MissionState {
            mission_id: Uuid::from_u128(30),
            objective: "200-aircraft mesh scale demonstration".to_owned(),
            generation: 1,
            status: MissionStatus::Active,
        }),
        DeliveryClass::Mission,
    )?;
    network.synchronize();

    let initially_converged = std::iter::once(&ground)
        .chain(aircraft.iter())
        .all(|node_id| mission_generation(&network, node_id, &mission_key) == Some(1));
    if !initially_converged {
        return Err(SimulationError::LargeFormationVerification(
            "initial mission state did not reach all 201 nodes",
        ));
    }

    let mut online_step = visual_step(
        &network,
        "maximum-formation-online",
        "200-aircraft mesh online",
        "AVIAN is executing 200 in-memory aircraft nodes plus one control station. Every node has converged on mission generation one.",
        22_500,
        "LARGE-FORMATION SIMULATION",
        None,
        &mission_key,
        None,
        false,
        "200 aircraft online and synchronized",
        signed_command_verified,
    );
    online_step.formation_summary = Some(VisualFormationSummary {
        mission_id: "DEMO-LARGE-FORMATION-01".to_owned(),
        simulated_aircraft: DEMONSTRATED_AIRCRAFT_COUNT,
        control_stations: 1,
        direct_peer_limit: plan.max_degree(),
        maximum_overlay_links: planned_aircraft_links,
        documented_design_target_aircraft: DEMONSTRATED_AIRCRAFT_COUNT,
        capacity_basis: "Executed SimNetwork state and TopologyPlanner output".to_owned(),
        ground_partition_continuity_verified: false,
        distributed_loss_nodes: 0,
        distributed_loss_continuity_verified: false,
        recovery_converged: false,
        field_validated: false,
    });
    let mut steps = vec![online_step];

    network.set_node_online(&ground, false)?;
    let ground_partition_continuity_verified = network.connected_component_count() == 1
        && aircraft
            .iter()
            .all(|node_id| mission_generation(&network, node_id, &mission_key) == Some(1));
    if !ground_partition_continuity_verified {
        return Err(SimulationError::LargeFormationVerification(
            "aircraft did not remain connected after ground loss",
        ));
    }

    network.set_link_enabled(&failed_link.0, &failed_link.1, false)?;
    let distributed_loss = aircraft
        .iter()
        .skip(9)
        .step_by(10)
        .cloned()
        .collect::<Vec<_>>();
    for node_id in &distributed_loss {
        network.set_node_online(node_id, false)?;
    }
    let distributed_loss_continuity_verified = distributed_loss.len() == 20
        && network.connected_component_count() == 1
        && network.nodes.values().filter(|node| node.online).count() == 180;
    if !distributed_loss_continuity_verified {
        return Err(SimulationError::LargeFormationVerification(
            "the surviving aircraft disconnected after distributed node and link loss",
        ));
    }

    network.publish(
        &aircraft[0],
        mission_key.clone(),
        MeshPayload::Mission(MissionState {
            mission_id: Uuid::from_u128(30),
            objective: "200-aircraft mesh scale demonstration".to_owned(),
            generation: 2,
            status: MissionStatus::Active,
        }),
        DeliveryClass::Mission,
    )?;
    network.synchronize();
    let surviving_nodes_converged = aircraft.iter().all(|node_id| {
        distributed_loss.contains(node_id)
            || mission_generation(&network, node_id, &mission_key) == Some(2)
    });
    if !surviving_nodes_converged {
        return Err(SimulationError::LargeFormationVerification(
            "updated mission state did not reach every surviving aircraft",
        ));
    }

    let mut reroute_step = visual_step(
        &network,
        "maximum-formation-rerouting",
        "20 aircraft leave; mesh paths reroute",
        "Twenty distributed aircraft and the ground station are offline. The 180 surviving aircraft remain one connected component and converge on mission generation two through the remaining paths.",
        25_000,
        "LARGE-FORMATION CONTINUITY",
        None,
        &mission_key,
        Some((&failed_link.0, &failed_link.1)),
        false,
        "180 aircraft rerouted and synchronized",
        signed_command_verified,
    );
    for node in &mut reroute_step.nodes {
        if node.status == "offline" {
            node.mission_synced = false;
        }
    }
    reroute_step.metrics.mission_synced_nodes = 180;
    reroute_step.formation_summary = Some(VisualFormationSummary {
        mission_id: "DEMO-LARGE-FORMATION-01".to_owned(),
        simulated_aircraft: DEMONSTRATED_AIRCRAFT_COUNT,
        control_stations: 1,
        direct_peer_limit: plan.max_degree(),
        maximum_overlay_links: planned_aircraft_links,
        documented_design_target_aircraft: DEMONSTRATED_AIRCRAFT_COUNT,
        capacity_basis: "Executed SimNetwork state and TopologyPlanner output".to_owned(),
        ground_partition_continuity_verified,
        distributed_loss_nodes: distributed_loss.len(),
        distributed_loss_continuity_verified,
        recovery_converged: false,
        field_validated: false,
    });
    steps.push(reroute_step);

    network.set_node_online(&ground, true)?;
    network.set_link_enabled(&failed_link.0, &failed_link.1, true)?;
    for node_id in &distributed_loss {
        network.set_node_online(node_id, true)?;
    }
    network.synchronize();
    let recovery_converged = network.connected_component_count() == 1
        && std::iter::once(&ground)
            .chain(aircraft.iter())
            .all(|node_id| mission_generation(&network, node_id, &mission_key) == Some(2));
    if !recovery_converged {
        return Err(SimulationError::LargeFormationVerification(
            "recovered nodes did not reconcile the latest mission generation",
        ));
    }

    let mut step = visual_step(
        &network,
        "maximum-formation-mission",
        "200-aircraft simulation completed",
        "AVIAN executed 200 in-memory aircraft nodes plus one control station, retained airborne continuity through ground, link, and 20-aircraft loss, then recovered all 201 nodes onto the latest mission generation.",
        27_500,
        "LARGE-FORMATION SIMULATION",
        None,
        &mission_key,
        None,
        false,
        "200-aircraft loss and recovery verified",
        signed_command_verified,
    );
    step.formation_summary = Some(VisualFormationSummary {
        mission_id: "DEMO-LARGE-FORMATION-01".to_owned(),
        simulated_aircraft: DEMONSTRATED_AIRCRAFT_COUNT,
        control_stations: 1,
        direct_peer_limit: plan.max_degree(),
        maximum_overlay_links: planned_aircraft_links,
        documented_design_target_aircraft: DEMONSTRATED_AIRCRAFT_COUNT,
        capacity_basis: "Executed SimNetwork state and TopologyPlanner output".to_owned(),
        ground_partition_continuity_verified,
        distributed_loss_nodes: distributed_loss.len(),
        distributed_loss_continuity_verified,
        recovery_converged,
        field_validated: false,
    });
    steps.push(step);
    Ok(steps)
}

fn mission_generation(
    network: &SimNetwork,
    node_id: &NodeId,
    mission_key: &RecordKey,
) -> Option<u64> {
    match network.payload(node_id, mission_key) {
        Some(MeshPayload::Mission(mission)) => Some(mission.generation),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_reference_scenario() -> Result<ScenarioReport, SimulationError> {
    let ground = NodeId::from("ground-1");
    let ardupilot = NodeId::from("ardu-1");
    let px4 = NodeId::from("px4-1");
    let betaflight = NodeId::from("beta-1");

    let beta_profile = NodeProfile::aircraft(
        betaflight.clone(),
        FlightStack::Betaflight,
        SYSTEM_MAX_MSL_M,
    )?;
    let beta_adapter = SimulatedVehicleAdapter::new(
        beta_profile.clone(),
        sample_telemetry(betaflight.clone(), 1_000)?,
    )?;

    let mut network = SimNetwork::default();
    network.add_node(NodeProfile::ground(ground.clone()));
    network.add_node(NodeProfile::aircraft(
        ardupilot.clone(),
        FlightStack::ArduPilot,
        SYSTEM_MAX_MSL_M,
    )?);
    network.add_node(NodeProfile::aircraft(
        px4.clone(),
        FlightStack::Px4,
        SYSTEM_MAX_MSL_M,
    )?);
    network.add_node(beta_profile);

    network.connect(&ground, &ardupilot)?;
    network.connect(&ardupilot, &px4)?;
    network.connect(&px4, &betaflight)?;
    network.connect(&betaflight, &ardupilot)?;

    let mission_key = RecordKey::from("mission/current");
    network.publish(
        &ground,
        mission_key.clone(),
        MeshPayload::Mission(MissionState {
            mission_id: Uuid::from_u128(10),
            objective: "mesh continuity demonstration".to_owned(),
            generation: 1,
            status: MissionStatus::Active,
        }),
        DeliveryClass::Mission,
    )?;
    network.synchronize();
    let mission_reached_all_nodes = [&ground, &ardupilot, &px4, &betaflight]
        .iter()
        .all(|node| network.payload(node, &mission_key).is_some());

    network.set_link_enabled(&ground, &ardupilot, false)?;
    let ground_partitioned_without_stopping_aircraft = network.connected_component_count() == 2;
    network.set_node_online(&px4, false)?;

    let telemetry_key = RecordKey::from("telemetry/ardu-1");
    network.publish(
        &ardupilot,
        telemetry_key.clone(),
        MeshPayload::Telemetry(sample_telemetry(ardupilot.clone(), 2_000)?),
        DeliveryClass::Telemetry,
    )?;
    network.synchronize();
    let crashed_node_did_not_stop_mesh = network.payload(&betaflight, &telemetry_key).is_some()
        && network.payload(&ground, &telemetry_key).is_none();

    let degraded_link_failed_over = failover_demonstration();

    network.set_link_enabled(&ground, &ardupilot, true)?;
    network.synchronize();

    let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
    let command = EmergencyCommand::issue(
        &signing_key,
        Uuid::from_u128(20),
        ground.clone(),
        betaflight.clone(),
        2_000,
        7_000,
        1,
        EmergencyAction::GpsRescue,
    )?;
    let command_key = RecordKey::from("emergency/00000000-0000-0000-0000-000000000014");
    network.publish(
        &ground,
        command_key.clone(),
        MeshPayload::EmergencyCommand(command.clone()),
        DeliveryClass::Emergency,
    )?;
    network.synchronize();

    let delivered_command = match network.payload(&betaflight, &command_key) {
        Some(MeshPayload::EmergencyCommand(command)) => command.clone(),
        _ => return Err(SimulationError::CommandNotDelivered),
    };
    let mut replay_guard = ReplayGuard::default();
    let betaflight_command_verified = replay_guard
        .accept(
            &delivered_command,
            &betaflight,
            &signing_key.verifying_key(),
            2_100,
        )
        .is_ok();
    let execution = beta_adapter
        .execute_emergency(delivered_command.action)
        .await?;
    let betaflight_gps_rescue_executed = execution.native_action == "gps_rescue";

    let ack_key = RecordKey::from("ack/beta-1/00000000-0000-0000-0000-000000000014");
    network.publish(
        &betaflight,
        ack_key.clone(),
        MeshPayload::EmergencyAck(EmergencyAck {
            command_id: delivered_command.command_id,
            node_id: betaflight.clone(),
            accepted: true,
            verified: true,
            executed: true,
            command_mode: Some("simulator".into()),
            mavlink_result: None,
            detail: execution.native_action.to_owned(),
            timestamp_ms: 2_100,
        }),
        DeliveryClass::Acknowledgement,
    )?;
    network.synchronize();
    let emergency_ack_returned = network.payload(&ground, &ack_key).is_some();

    network.set_node_online(&px4, true)?;
    network.synchronize();
    let recovered_node_converged =
        network.payload(&px4, &mission_key).is_some() && network.payload(&px4, &ack_key).is_some();

    Ok(ScenarioReport {
        mission_reached_all_nodes,
        ground_partitioned_without_stopping_aircraft,
        crashed_node_did_not_stop_mesh,
        degraded_link_failed_over,
        betaflight_command_verified,
        betaflight_gps_rescue_executed,
        emergency_ack_returned,
        recovered_node_converged,
    })
}

pub(crate) fn sample_telemetry(
    source: NodeId,
    timestamp_ms: u64,
) -> Result<Telemetry, SimulationError> {
    Ok(Telemetry {
        source,
        timestamp_ms,
        latitude_deg: 35.0,
        longitude_deg: -106.0,
        altitude: Altitude::new(2_000.0, 500.0, 450.0)?,
        velocity_ned_mps: [10.0, 0.0, 0.0],
        attitude_rpy_deg: [0.0, 0.0, 90.0],
        battery_remaining: Some(0.75),
        control_link_quality: Some(0.8),
        armed: true,
        landed: Some(false),
        failsafe: false,
    })
}

fn failover_demonstration() -> bool {
    let wifi = link("wifi", TransportKind::Wifi, false, 30.0, 0.01);
    let cellular = link("cellular", TransportKind::Cellular, true, 90.0, 0.03);
    LinkOrchestrator::default()
        .select(
            &[wifi, cellular],
            DeliveryClass::Telemetry,
            Some(&LinkId::from("wifi")),
        )
        .is_some_and(|plan| plan.primary == LinkId::from("cellular"))
}

fn link(
    id: &str,
    transport: TransportKind,
    available: bool,
    latency_ms: f32,
    loss_ratio: f32,
) -> LinkCandidate {
    LinkCandidate {
        id: LinkId::from(id),
        transport,
        available,
        metrics: LinkMetrics {
            latency_ms,
            loss_ratio,
            goodput_bps: 2_000_000.0,
            signal_quality: 0.8,
            stability: 0.9,
            energy_cost: 0.4,
        },
        geometry: LinkGeometry {
            distance_m: 25_000.0,
            line_of_sight: true,
            fresnel_clearance_ratio: 0.9,
        },
    }
}
