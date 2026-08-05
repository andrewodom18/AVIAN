use serde::{Deserialize, Serialize};

use crate::{ChannelBandwidthMhz, NodeId, StreamCasterModel};

pub const STREAMCASTER_MESH_OBSERVATION_SCHEMA_VERSION: u16 = 1;
pub const STREAMCASTER_CAPACITY_REQUIREMENT_NODES: u16 = 150;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamCasterObservedStatus {
    Online,
    Unreachable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamCasterObservedRadio {
    pub node_id: Option<u32>,
    pub system_name: Option<String>,
    pub network_id: Option<String>,
    pub center_frequency_mhz: Option<f64>,
    pub bandwidth_mhz: Option<ChannelBandwidthMhz>,
    pub link_distance_m: Option<u32>,
    pub antenna_mask: Option<u8>,
    pub transmit_power_dbm_per_port: Option<u8>,
    pub model: Option<StreamCasterModel>,
    pub firmware_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StreamCasterObservedPosition {
    pub latitude_deg: f64,
    pub longitude_deg: f64,
    pub altitude_msl_m: Option<f64>,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamCasterObservedNode {
    pub node_key: NodeId,
    pub management_ip: Option<String>,
    pub status: StreamCasterObservedStatus,
    pub last_seen_ms: u64,
    pub peat_endpoint_id: Option<String>,
    pub peat_connected_peers: usize,
    #[serde(default)]
    pub position: Option<StreamCasterObservedPosition>,
    pub radio: StreamCasterObservedRadio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamCasterPeerLink {
    pub source: NodeId,
    pub source_endpoint_id: Option<String>,
    pub target: String,
    pub target_endpoint_id: String,
    pub target_addresses: Vec<String>,
    pub transport: String,
    pub state: String,
    pub observed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamCasterMeshObservation {
    pub schema_version: u16,
    pub observed_at_ms: u64,
    pub source: NodeId,
    pub capacity_requirement_nodes: u16,
    pub simulated: bool,
    pub node: StreamCasterObservedNode,
    pub links: Vec<StreamCasterPeerLink>,
    pub error: Option<String>,
}
