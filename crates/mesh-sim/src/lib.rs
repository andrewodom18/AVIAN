//! Deterministic in-memory model used before real PEAT transports and flight
//! controller simulators are connected.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ed25519_dalek::SigningKey;
use mesh_core::{
    Altitude, DeliveryClass, DeliveryPolicy, EmergencyAck, EmergencyAction, EmergencyCommand,
    FlightStack, LinkCandidate, LinkGeometry, LinkId, LinkMetrics, LinkOrchestrator, MeshPayload,
    MissionState, MissionStatus, NodeId, NodeProfile, ReplayGuard, Telemetry, TransportKind,
    SYSTEM_MAX_MSL_M,
};
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;
use vehicle_adapters::{SimulatedVehicleAdapter, VehicleAdapter};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecordKey(String);

impl From<&str> for RecordKey {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    logical_clock: u64,
    author: NodeId,
}

#[derive(Debug, Clone, PartialEq)]
struct VersionedRecord {
    version: Version,
    payload: MeshPayload,
    expires_at_ms: Option<u64>,
}

#[derive(Debug)]
struct SimNode {
    profile: NodeProfile,
    online: bool,
    records: BTreeMap<RecordKey, VersionedRecord>,
}

#[derive(Debug, Default)]
pub struct SimNetwork {
    nodes: BTreeMap<NodeId, SimNode>,
    links: BTreeMap<(NodeId, NodeId), bool>,
    logical_clock: u64,
    now_ms: u64,
}

impl SimNetwork {
    pub fn add_node(&mut self, profile: NodeProfile) {
        self.nodes.insert(
            profile.node_id.clone(),
            SimNode {
                profile,
                online: true,
                records: BTreeMap::new(),
            },
        );
    }

    pub fn connect(&mut self, left: &NodeId, right: &NodeId) -> Result<(), SimulationError> {
        self.require_node(left)?;
        self.require_node(right)?;
        self.links.insert(ordered_pair(left, right), true);
        Ok(())
    }

    pub fn set_link_enabled(
        &mut self,
        left: &NodeId,
        right: &NodeId,
        enabled: bool,
    ) -> Result<(), SimulationError> {
        let pair = ordered_pair(left, right);
        let link = self
            .links
            .get_mut(&pair)
            .ok_or_else(|| SimulationError::UnknownLink(left.clone(), right.clone()))?;
        *link = enabled;
        Ok(())
    }

    pub fn set_node_online(
        &mut self,
        node_id: &NodeId,
        online: bool,
    ) -> Result<(), SimulationError> {
        self.nodes
            .get_mut(node_id)
            .ok_or_else(|| SimulationError::UnknownNode(node_id.clone()))?
            .online = online;
        Ok(())
    }

    pub fn advance_time(&mut self, milliseconds: u64) {
        self.now_ms = self.now_ms.saturating_add(milliseconds);
    }

    pub fn publish(
        &mut self,
        author: &NodeId,
        key: RecordKey,
        payload: MeshPayload,
        class: DeliveryClass,
    ) -> Result<(), SimulationError> {
        let node = self
            .nodes
            .get_mut(author)
            .ok_or_else(|| SimulationError::UnknownNode(author.clone()))?;
        if !node.online {
            return Err(SimulationError::OfflineNode(author.clone()));
        }
        self.logical_clock = self.logical_clock.saturating_add(1);
        let policy = DeliveryPolicy::for_class(class);
        let expires_at_ms = policy.ttl_ms.map(|ttl| self.now_ms.saturating_add(ttl));
        node.records.insert(
            key,
            VersionedRecord {
                version: Version {
                    logical_clock: self.logical_clock,
                    author: author.clone(),
                },
                payload,
                expires_at_ms,
            },
        );
        Ok(())
    }

    /// Converges every currently connected component. Version ordering is a
    /// deterministic stand-in for PEAT/Automerge during the v0.1 harness.
    pub fn synchronize(&mut self) {
        let components = self.connected_components();
        for component in components {
            let mut merged: BTreeMap<RecordKey, VersionedRecord> = BTreeMap::new();
            for node_id in &component {
                let node = &self.nodes[node_id];
                for (key, record) in &node.records {
                    if record
                        .expires_at_ms
                        .is_some_and(|expires_at| expires_at <= self.now_ms)
                    {
                        continue;
                    }
                    let replace = merged
                        .get(key)
                        .is_none_or(|existing| record.version > existing.version);
                    if replace {
                        merged.insert(key.clone(), record.clone());
                    }
                }
            }
            for node_id in component {
                self.nodes
                    .get_mut(&node_id)
                    .expect("component node exists")
                    .records = merged.clone();
            }
        }
    }

    pub fn payload(&self, node_id: &NodeId, key: &RecordKey) -> Option<&MeshPayload> {
        self.nodes
            .get(node_id)?
            .records
            .get(key)
            .filter(|record| {
                record
                    .expires_at_ms
                    .is_none_or(|expires_at| expires_at > self.now_ms)
            })
            .map(|record| &record.payload)
    }

    pub fn connected_component_count(&self) -> usize {
        self.connected_components().len()
    }

    pub fn node_profile(&self, node_id: &NodeId) -> Option<&NodeProfile> {
        self.nodes.get(node_id).map(|node| &node.profile)
    }

    fn require_node(&self, node_id: &NodeId) -> Result<(), SimulationError> {
        self.nodes
            .contains_key(node_id)
            .then_some(())
            .ok_or_else(|| SimulationError::UnknownNode(node_id.clone()))
    }

    fn connected_components(&self) -> Vec<Vec<NodeId>> {
        let online: BTreeSet<NodeId> = self
            .nodes
            .iter()
            .filter(|(_, node)| node.online)
            .map(|(id, _)| id.clone())
            .collect();
        let mut unseen = online.clone();
        let mut components = Vec::new();

        while let Some(start) = unseen.first().cloned() {
            let mut queue = VecDeque::from([start.clone()]);
            let mut component = Vec::new();
            unseen.remove(&start);

            while let Some(current) = queue.pop_front() {
                component.push(current.clone());
                for candidate in &online {
                    if unseen.contains(candidate)
                        && self
                            .links
                            .get(&ordered_pair(&current, candidate))
                            .copied()
                            .unwrap_or(false)
                    {
                        unseen.remove(candidate);
                        queue.push_back(candidate.clone());
                    }
                }
            }
            components.push(component);
        }
        components
    }
}

fn ordered_pair(left: &NodeId, right: &NodeId) -> (NodeId, NodeId) {
    if left <= right {
        (left.clone(), right.clone())
    } else {
        (right.clone(), left.clone())
    }
}

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

fn sample_telemetry(source: NodeId, timestamp_ms: u64) -> Result<Telemetry, SimulationError> {
    Ok(Telemetry {
        source,
        timestamp_ms,
        latitude_deg: 35.0,
        longitude_deg: -106.0,
        altitude: Altitude::new(2_000.0, 500.0, 450.0)?,
        velocity_ned_mps: [10.0, 0.0, 0.0],
        attitude_rpy_deg: [0.0, 0.0, 90.0],
        battery_remaining: 0.75,
        control_link_quality: 0.8,
        armed: true,
        landed: false,
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
    #[error(transparent)]
    Profile(#[from] mesh_core::ProfileError),
    #[error(transparent)]
    Altitude(#[from] mesh_core::AltitudeError),
    #[error(transparent)]
    Command(#[from] mesh_core::CommandError),
    #[error(transparent)]
    Adapter(#[from] vehicle_adapters::AdapterError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reference_scenario_passes() {
        let report = run_reference_scenario().await.unwrap();
        assert!(report.passed(), "{report:#?}");
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
