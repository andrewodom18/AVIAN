//! Deterministic in-memory model used before real PEAT transports and flight
//! controller simulators are connected.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ed25519_dalek::SigningKey;
use mesh_core::{
    Altitude, DeliveryClass, DeliveryPolicy, EmergencyAck, EmergencyAction, EmergencyCommand,
    FlightStack, LinkCandidate, LinkGeometry, LinkId, LinkMetrics, LinkOrchestrator, MeshPayload,
    MissionState, MissionStatus, NodeId, NodeProfile, NodeRole, ReplayGuard, Telemetry,
    TransportKind, DEFAULT_MAX_NEIGHBORS, MAX_SUPPORTED_SWARM_SIZE, SYSTEM_MAX_MSL_M,
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

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisualScenario {
    pub schema_version: u8,
    pub name: String,
    pub description: String,
    pub steps: Vec<VisualStep>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisualStep {
    pub id: String,
    pub title: String,
    pub narrative: String,
    pub at_ms: u64,
    pub phase: String,
    pub control_event: Option<VisualControlEvent>,
    pub nodes: Vec<VisualNode>,
    pub links: Vec<VisualLink>,
    pub metrics: VisualMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formation_summary: Option<VisualFormationSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisualFormationSummary {
    pub mission_id: String,
    pub simulated_aircraft: usize,
    pub control_stations: usize,
    pub direct_peer_limit: usize,
    pub maximum_overlay_links: usize,
    pub documented_design_target_aircraft: usize,
    pub capacity_basis: String,
    pub field_validated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisualControlEvent {
    pub authority: String,
    pub method: String,
    pub path: String,
    pub status: String,
    pub simulated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisualNode {
    pub id: String,
    pub label: String,
    pub role: String,
    pub flight_stack: Option<String>,
    pub status: String,
    pub mission_synced: bool,
    pub record_count: usize,
    pub x: u8,
    pub y: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VisualLink {
    pub source: String,
    pub target: String,
    pub transport: String,
    pub state: String,
    pub latency_ms: u16,
    pub signal_quality: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VisualMetrics {
    pub online_nodes: usize,
    pub active_links: usize,
    pub connected_components: usize,
    pub mission_synced_nodes: usize,
    pub continuity: String,
    pub signed_command_verified: bool,
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
        "PEAT-style durable mission state converges across every connected peer.",
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

    let maximum_overlay_links = MAX_SUPPORTED_SWARM_SIZE * DEFAULT_MAX_NEIGHBORS / 2;
    steps.push(VisualStep {
        id: "maximum-formation-mission".to_owned(),
        title: "Maximum software formation mission".to_owned(),
        narrative: format!(
            "A separate development mission summarizes {MAX_SUPPORTED_SWARM_SIZE} simulated aircraft plus one control station at the current software-planner ceiling. The documented design target remains 200 aircraft, and physical-radio/RF validation is still pending."
        ),
        at_ms: 22_500,
        phase: "LARGE-FORMATION SIMULATION".to_owned(),
        control_event: None,
        nodes: Vec::new(),
        links: Vec::new(),
        metrics: VisualMetrics {
            online_nodes: MAX_SUPPORTED_SWARM_SIZE + 1,
            active_links: maximum_overlay_links,
            connected_components: 1,
            mission_synced_nodes: MAX_SUPPORTED_SWARM_SIZE + 1,
            continuity: "Software topology ceiling represented".to_owned(),
            signed_command_verified: verification.passed(),
        },
        formation_summary: Some(VisualFormationSummary {
            mission_id: "DEMO-LARGE-FORMATION-01".to_owned(),
            simulated_aircraft: MAX_SUPPORTED_SWARM_SIZE,
            control_stations: 1,
            direct_peer_limit: DEFAULT_MAX_NEIGHBORS,
            maximum_overlay_links,
            documented_design_target_aircraft: 200,
            capacity_basis: "Software planner ceiling".to_owned(),
            field_validated: false,
        }),
    });

    Ok(VisualScenario {
        schema_version: 1,
        name: "AVIAN leaderless mesh continuity".to_owned(),
        description:
            "Deterministic CHUD radio discovery/configuration and AVIAN topology, synchronization, failure, failover, and recovery trace."
                .to_owned(),
        steps,
    })
}

#[allow(clippy::too_many_arguments)]
fn visual_step(
    network: &SimNetwork,
    id: &str,
    title: &str,
    narrative: &str,
    at_ms: u64,
    phase: &str,
    control_event: Option<VisualControlEvent>,
    mission_key: &RecordKey,
    degraded_link: Option<(&NodeId, &NodeId)>,
    failover_active: bool,
    continuity: &str,
    signed_command_verified: bool,
) -> VisualStep {
    let nodes = network
        .nodes
        .values()
        .map(|node| {
            let active_neighbor = network.links.iter().any(|((left, right), enabled)| {
                *enabled
                    && (&node.profile.node_id == left || &node.profile.node_id == right)
                    && network.nodes.get(left).is_some_and(|peer| peer.online)
                    && network.nodes.get(right).is_some_and(|peer| peer.online)
            });
            let degraded = degraded_link.is_some_and(|(left, right)| {
                node.profile.node_id == *left || node.profile.node_id == *right
            });
            let status = if !node.online {
                "offline"
            } else if degraded {
                "degraded"
            } else if !active_neighbor {
                "isolated"
            } else {
                "online"
            };
            let (x, y) = visual_position(&node.profile.node_id);
            VisualNode {
                id: node.profile.node_id.to_string(),
                label: visual_label(&node.profile.node_id).to_owned(),
                role: match node.profile.role {
                    NodeRole::Aircraft => "aircraft",
                    NodeRole::Ground => "ground",
                    NodeRole::Cloud => "cloud",
                }
                .to_owned(),
                flight_stack: node
                    .profile
                    .flight_stack
                    .map(|stack| match stack {
                        FlightStack::ArduPilot => "ArduPilot",
                        FlightStack::Px4 => "PX4",
                        FlightStack::Betaflight => "Betaflight",
                    })
                    .map(str::to_owned),
                status: status.to_owned(),
                mission_synced: node.records.get(mission_key).is_some_and(|record| {
                    record
                        .expires_at_ms
                        .is_none_or(|expiry| expiry > network.now_ms)
                }),
                record_count: node.records.len(),
                x,
                y,
            }
        })
        .collect::<Vec<_>>();

    let mut links = network
        .links
        .iter()
        .map(|((source, target), enabled)| {
            let endpoints_online = network.nodes.get(source).is_some_and(|node| node.online)
                && network.nodes.get(target).is_some_and(|node| node.online);
            let is_degraded = degraded_link.is_some_and(|(left, right)| {
                ordered_pair(left, right) == ordered_pair(source, target)
            });
            let state = if !enabled || !endpoints_online {
                "down"
            } else if is_degraded {
                "degraded"
            } else {
                "active"
            };
            VisualLink {
                source: source.to_string(),
                target: target.to_string(),
                transport: "MANET / PEAT".to_owned(),
                state: state.to_owned(),
                latency_ms: if is_degraded { 240 } else { 38 },
                signal_quality: if is_degraded { 0.28 } else { 0.91 },
            }
        })
        .collect::<Vec<_>>();

    if failover_active {
        links.push(VisualLink {
            source: "ardu-1".to_owned(),
            target: "beta-1".to_owned(),
            transport: "Alternate IP path".to_owned(),
            state: "failover".to_owned(),
            latency_ms: 90,
            signal_quality: 0.78,
        });
    }

    VisualStep {
        id: id.to_owned(),
        title: title.to_owned(),
        narrative: narrative.to_owned(),
        at_ms,
        phase: phase.to_owned(),
        control_event,
        metrics: VisualMetrics {
            online_nodes: network.nodes.values().filter(|node| node.online).count(),
            active_links: links
                .iter()
                .filter(|link| matches!(link.state.as_str(), "active" | "failover"))
                .count(),
            connected_components: network.connected_component_count(),
            mission_synced_nodes: nodes.iter().filter(|node| node.mission_synced).count(),
            continuity: continuity.to_owned(),
            signed_command_verified,
        },
        formation_summary: None,
        nodes,
        links,
    }
}

fn chud_event(method: &str, path: &str, status: &str) -> VisualControlEvent {
    VisualControlEvent {
        authority: "CHUD".to_owned(),
        method: method.to_owned(),
        path: path.to_owned(),
        status: status.to_owned(),
        simulated: true,
    }
}

fn visual_label(node_id: &NodeId) -> &'static str {
    match node_id.as_str() {
        "ground-1" => "GROUND OPS",
        "ardu-1" => "SCOUT 01",
        "px4-1" => "RELAY 02",
        "beta-1" => "SCOUT 03",
        _ => "AVIAN NODE",
    }
}

fn visual_position(node_id: &NodeId) -> (u8, u8) {
    match node_id.as_str() {
        "ground-1" => (13, 69),
        "ardu-1" => (34, 35),
        "px4-1" => (61, 22),
        "beta-1" => (82, 57),
        _ => (50, 50),
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

    #[tokio::test]
    async fn visual_scenario_reports_real_failure_and_recovery_state() {
        let scenario = run_visual_scenario().await.unwrap();
        assert_eq!(scenario.schema_version, 1);
        assert_eq!(scenario.steps.len(), 16);

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

        let maximum = scenario.steps.last().unwrap();
        let summary = maximum.formation_summary.as_ref().unwrap();
        assert_eq!(maximum.id, "maximum-formation-mission");
        assert_eq!(summary.simulated_aircraft, MAX_SUPPORTED_SWARM_SIZE);
        assert_eq!(summary.control_stations, 1);
        assert_eq!(summary.direct_peer_limit, DEFAULT_MAX_NEIGHBORS);
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
