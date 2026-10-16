//! Deterministic mesh models, demonstration scenarios, and validation tools.
mod engine;
pub mod mesh_validation;
mod projection;
mod scenarios;
pub mod validation;

pub use engine::{RecordKey, SimNetwork};
use mesh_core::NodeId;
pub use projection::{
    VisualControlEvent, VisualFormationSummary, VisualLink, VisualMetrics, VisualNode,
    VisualScenario, VisualStep,
};
pub use scenarios::{run_reference_scenario, run_visual_scenario, ScenarioReport};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SimulationError {
    #[error("unknown node {0}")]
    UnknownNode(NodeId),
    #[error("node {0} is offline")]
    OfflineNode(NodeId),
    #[error("unknown link between {0} and {1}")]
    UnknownLink(NodeId, NodeId),
    #[error("signed command was not delivered")]
    CommandNotDelivered,
    #[error("large-formation verification failed: {0}")]
    LargeFormationVerification(&'static str),
    #[error(transparent)]
    Profile(#[from] mesh_core::ProfileError),
    #[error(transparent)]
    Altitude(#[from] mesh_core::AltitudeError),
    #[error(transparent)]
    Command(#[from] mesh_core::CommandError),
    #[error(transparent)]
    Topology(#[from] mesh_core::TopologyError),
    #[error(transparent)]
    Adapter(#[from] vehicle_adapters::AdapterError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenarios::{sample_telemetry, DEMONSTRATED_AIRCRAFT_COUNT};
    use mesh_core::{DeliveryClass, MeshPayload, NodeId, NodeProfile};

    #[tokio::test]
    async fn reference_scenario_passes() {
        let report = run_reference_scenario().await.unwrap();
        assert!(report.passed(), "{report:#?}");
    }

    #[tokio::test]
    async fn visual_scenario_reports_real_failure_and_recovery_state() {
        let scenario = run_visual_scenario().await.unwrap();
        assert_eq!(scenario.schema_version, 1);
        assert_eq!(scenario.steps.len(), 18);

        let connected = &scenario.steps[0];
        assert_eq!(connected.id, "radio-connected");
        assert_eq!(connected.nodes.len(), 1);
        assert_eq!(
            connected.control_event.as_ref().unwrap().path,
            "/api/radio/devices"
        );

        let configured = scenario
            .steps
            .iter()
            .find(|step| step.id == "radio-config-confirmed")
            .unwrap();
        assert_eq!(configured.nodes.len(), 4);
        assert!(configured.control_event.as_ref().unwrap().simulated);

        let partition = scenario
            .steps
            .iter()
            .find(|step| step.id == "ground-partitioned")
            .unwrap();
        assert_eq!(partition.metrics.connected_components, 2);
        assert_eq!(partition.metrics.mission_synced_nodes, 4);

        let failure = scenario
            .steps
            .iter()
            .find(|step| step.id == "node-failure")
            .unwrap();
        assert_eq!(failure.metrics.online_nodes, 3);
        assert!(failure.nodes.iter().any(|node| node.status == "offline"));

        let recovery = scenario
            .steps
            .iter()
            .find(|step| step.id == "command-acknowledged")
            .unwrap();
        assert_eq!(recovery.metrics.connected_components, 1);
        assert_eq!(recovery.metrics.mission_synced_nodes, 4);
        assert!(recovery.metrics.signed_command_verified);

        let scale_online = scenario
            .steps
            .iter()
            .find(|step| step.id == "maximum-formation-online")
            .unwrap();
        assert_eq!(scale_online.metrics.online_nodes, 201);
        assert_eq!(scale_online.metrics.mission_synced_nodes, 201);

        let scale_rerouting = scenario
            .steps
            .iter()
            .find(|step| step.id == "maximum-formation-rerouting")
            .unwrap();
        assert_eq!(scale_rerouting.metrics.online_nodes, 180);
        assert_eq!(scale_rerouting.metrics.connected_components, 1);
        assert_eq!(scale_rerouting.metrics.mission_synced_nodes, 180);
        assert_eq!(
            scale_rerouting
                .nodes
                .iter()
                .filter(|node| node.status == "offline")
                .count(),
            21
        );

        let maximum = scenario.steps.last().unwrap();
        let summary = maximum.formation_summary.as_ref().unwrap();
        assert_eq!(maximum.id, "maximum-formation-mission");
        assert_eq!(summary.simulated_aircraft, DEMONSTRATED_AIRCRAFT_COUNT);
        assert_eq!(summary.control_stations, 1);
        assert_eq!(summary.direct_peer_limit, 8);
        assert_eq!(summary.maximum_overlay_links, 800);
        assert_eq!(maximum.nodes.len(), DEMONSTRATED_AIRCRAFT_COUNT + 1);
        assert_eq!(maximum.links.len(), 801);
        assert_eq!(
            maximum.metrics.online_nodes,
            DEMONSTRATED_AIRCRAFT_COUNT + 1
        );
        assert_eq!(maximum.metrics.active_links, 801);
        assert_eq!(maximum.metrics.connected_components, 1);
        assert_eq!(
            maximum.metrics.mission_synced_nodes,
            DEMONSTRATED_AIRCRAFT_COUNT + 1
        );
        assert!(maximum.nodes.iter().all(|node| node.mission_synced));
        assert!(summary.ground_partition_continuity_verified);
        assert_eq!(summary.distributed_loss_nodes, 20);
        assert!(summary.distributed_loss_continuity_verified);
        assert!(summary.recovery_converged);
        assert!(!summary.field_validated);
    }

    #[test]
    fn expired_telemetry_does_not_reappear_after_partition() {
        let left = NodeId::from("left");
        let right = NodeId::from("right");
        let mut network = SimNetwork::default();
        network.add_node(NodeProfile::ground(left.clone()));
        network.add_node(NodeProfile::ground(right.clone()));
        network.connect(&left, &right).unwrap();
        network.set_link_enabled(&left, &right, false).unwrap();
        let key = RecordKey::from("telemetry/left");
        network
            .publish(
                &left,
                key.clone(),
                MeshPayload::Telemetry(sample_telemetry(left.clone(), 0).unwrap()),
                DeliveryClass::Telemetry,
            )
            .unwrap();
        network.advance_time(2_001);
        network.set_link_enabled(&left, &right, true).unwrap();
        network.synchronize();

        assert!(network.payload(&right, &key).is_none());
    }
}
