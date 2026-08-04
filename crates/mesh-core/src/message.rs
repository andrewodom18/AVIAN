use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    Altitude, EmergencyCommand, InFlightRelayDecision, MissionAllocation, NodeId, NodeProfile,
    RelayLinkObservation, RelayRuntimeConfiguration, SwarmStatusSummary,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryClass {
    Emergency,
    Acknowledgement,
    Mission,
    Telemetry,
    Bulk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryPolicy {
    pub durable: bool,
    pub reliable: bool,
    pub redundant_paths: u8,
    pub latest_only: bool,
    pub ttl_ms: Option<u64>,
    pub max_latency_ms: Option<u32>,
}

impl DeliveryPolicy {
    pub fn for_class(class: DeliveryClass) -> Self {
        match class {
            DeliveryClass::Emergency => Self {
                durable: true,
                reliable: true,
                redundant_paths: 2,
                latest_only: false,
                ttl_ms: Some(5_000),
                max_latency_ms: Some(250),
            },
            DeliveryClass::Acknowledgement => Self {
                durable: true,
                reliable: true,
                redundant_paths: 1,
                latest_only: false,
                ttl_ms: None,
                max_latency_ms: Some(1_000),
            },
            DeliveryClass::Mission => Self {
                durable: true,
                reliable: true,
                redundant_paths: 1,
                latest_only: false,
                ttl_ms: None,
                max_latency_ms: Some(2_000),
            },
            DeliveryClass::Telemetry => Self {
                durable: false,
                reliable: false,
                redundant_paths: 1,
                latest_only: true,
                ttl_ms: Some(2_000),
                max_latency_ms: Some(500),
            },
            DeliveryClass::Bulk => Self {
                durable: true,
                reliable: true,
                redundant_paths: 1,
                latest_only: false,
                ttl_ms: None,
                max_latency_ms: None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Telemetry {
    pub source: NodeId,
    pub timestamp_ms: u64,
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub altitude: Altitude,
    pub velocity_ned_mps: [f32; 3],
    pub attitude_rpy_deg: [f32; 3],
    /// Normalized range 0.0-1.0, or `None` when the controller reports unknown.
    pub battery_remaining: Option<f32>,
    /// Normalized range 0.0-1.0, or `None` when receiver RSSI is unavailable.
    pub control_link_quality: Option<f32>,
    pub armed: bool,
    /// `None` until the controller explicitly reports landed state.
    pub landed: Option<bool>,
    pub failsafe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    Proposed,
    Active,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionState {
    pub mission_id: Uuid,
    pub objective: String,
    pub generation: u64,
    pub status: MissionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmergencyAck {
    pub command_id: Uuid,
    pub node_id: NodeId,
    pub accepted: bool,
    pub detail: String,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MeshPayload {
    NodeAdvertisement(NodeProfile),
    Telemetry(Telemetry),
    /// Compact, bounded operator-facing state from a rotating set of peers.
    /// It deliberately does not duplicate every aircraft's position stream.
    SwarmStatusSummary(SwarmStatusSummary),
    /// Latest rolling radio observation used to build a shared in-flight relay
    /// snapshot. The record is telemetry-class data, not a mission command.
    RelayLinkObservation(RelayLinkObservation),
    Mission(MissionState),
    MissionAllocation(MissionAllocation),
    /// Durable ARC-supplied policy used by companions to rebuild an in-flight
    /// request from synchronized telemetry and radio observations.
    RelayRuntimeConfiguration(RelayRuntimeConfiguration),
    RelayReconfiguration(InFlightRelayDecision),
    EmergencyCommand(EmergencyCommand),
    EmergencyAck(EmergencyAck),
}
