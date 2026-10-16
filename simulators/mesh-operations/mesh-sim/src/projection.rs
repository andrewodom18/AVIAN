//! View contracts and projection of model state for the visualizer.
use crate::engine::{ordered_pair, RecordKey, SimNetwork};
use mesh_core::{FlightStack, NodeId, NodeRole};
use serde::Serialize;

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
    pub ground_partition_continuity_verified: bool,
    pub distributed_loss_nodes: usize,
    pub distributed_loss_continuity_verified: bool,
    pub recovery_converged: bool,
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

#[allow(clippy::too_many_arguments)]
pub(crate) fn visual_step(
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
                transport: "AVIAN".to_owned(),
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

pub(crate) fn chud_event(method: &str, path: &str, status: &str) -> VisualControlEvent {
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
