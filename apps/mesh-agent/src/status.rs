use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{CommandMode, ConfiguredNodeRole, Underlay};

pub const STATUS_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentStatus {
    pub schema_version: u16,
    pub ready: bool,
    pub node: NodeStatus,
    pub peers: Vec<PeerStatus>,
    pub underlays: BTreeMap<String, UnderlayStatus>,
    pub mavlink: MavlinkStatus,
    pub telemetry: PublicationStatus,
    pub payload: PayloadStatus,
    pub commands: CommandStatus,
    pub radio: RadioStatus,
    pub last_errors: Vec<StatusError>,
}

impl AgentStatus {
    pub fn new(
        name: String,
        role: ConfiguredNodeRole,
        started_at_ms: u64,
        command_mode: CommandMode,
        mavlink_required: bool,
        radio_required: bool,
    ) -> Self {
        Self {
            schema_version: STATUS_SCHEMA_VERSION,
            ready: false,
            node: NodeStatus {
                name,
                role,
                endpoint_id: None,
                started_at_ms,
                uptime_ms: 0,
            },
            peers: Vec::new(),
            underlays: BTreeMap::new(),
            mavlink: MavlinkStatus {
                required: mavlink_required,
                connected: false,
                target_system_id: None,
                last_message_at_ms: None,
                last_error: None,
            },
            telemetry: PublicationStatus::default(),
            payload: PayloadStatus::default(),
            commands: CommandStatus {
                mode: command_mode,
                accepted: 0,
                rejected: 0,
                last_command_at_ms: None,
                last_result: None,
            },
            radio: RadioStatus {
                required: radio_required,
                fresh: !radio_required,
                last_observation_at_ms: None,
                devices: Vec::new(),
                degradation_reasons: Vec::new(),
            },
            last_errors: Vec::new(),
        }
    }

    pub fn snapshot(&self, now_ms: u64) -> Self {
        let mut snapshot = self.clone();
        snapshot.node.uptime_ms = now_ms.saturating_sub(snapshot.node.started_at_ms);
        snapshot.refresh_ready(now_ms);
        snapshot
    }

    pub fn refresh_ready(&mut self, now_ms: u64) {
        let peers_ready = self.peers.iter().all(|peer| peer.connected);
        let mavlink_ready = !self.mavlink.required
            || (self.mavlink.connected
                && self.mavlink.target_system_id.is_some()
                && self
                    .mavlink
                    .last_message_at_ms
                    .is_some_and(|at| now_ms.saturating_sub(at) <= 5_000));
        self.ready = self.node.endpoint_id.is_some()
            && peers_ready
            && mavlink_ready
            && (!self.radio.required || self.radio.fresh);
    }

    pub fn record_error(&mut self, component: &str, detail: impl Into<String>, at_ms: u64) {
        self.last_errors.push(StatusError {
            component: component.to_owned(),
            detail: detail.into(),
            at_ms,
        });
        if self.last_errors.len() > 20 {
            self.last_errors.remove(0);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeStatus {
    pub name: String,
    pub role: ConfiguredNodeRole,
    pub endpoint_id: Option<String>,
    pub started_at_ms: u64,
    pub uptime_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerStatus {
    pub name: String,
    pub endpoint_id: String,
    pub addresses: Vec<PeerAddressStatus>,
    pub connected: bool,
    pub last_transition_at_ms: u64,
    pub selected_underlay: Option<Underlay>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerAddressStatus {
    pub underlay: Option<Underlay>,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnderlayStatus {
    pub reachable: bool,
    pub last_observed_at_ms: Option<u64>,
    pub latency_ms: Option<f64>,
    pub loss_ratio: Option<f64>,
    pub goodput_bps: Option<u64>,
    pub stability: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MavlinkStatus {
    pub required: bool,
    pub connected: bool,
    pub target_system_id: Option<u8>,
    pub last_message_at_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationStatus {
    pub published: u64,
    pub last_published_at_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadStatus {
    pub accepted: u64,
    pub rejected: u64,
    pub last_event_at_ms: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandStatus {
    pub mode: CommandMode,
    pub accepted: u64,
    pub rejected: u64,
    pub last_command_at_ms: Option<u64>,
    pub last_result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioStatus {
    pub required: bool,
    pub fresh: bool,
    pub last_observation_at_ms: Option<u64>,
    pub devices: Vec<RadioDeviceStatus>,
    pub degradation_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioDeviceStatus {
    pub name: String,
    pub model: Option<String>,
    pub firmware: Option<String>,
    pub api_fresh: bool,
    pub neighbors: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusError {
    pub component: String,
    pub detail: String,
    pub at_ms: u64,
}
