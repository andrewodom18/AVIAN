//! AVIAN-owned formation-authenticated networking and durable state replication.
//!
//! Replication merges complete versioned records using a deterministic total
//! order. Domain code remains responsible for signatures, expiry, and command
//! authorization. The native store is not compatible with earlier state formats.

mod node;
mod record;
mod store;

pub use node::{derive_endpoint_id, MeshNode, MeshNodeError};
pub use record::{AvianRecord, MeshNodeConfig, PeerDescriptor, AVIAN_SCHEMA_VERSION};

use mesh_core::{DeliveryClass, DeliveryPolicy};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessagePriority {
    Critical,
    High,
    Background,
}

/// Application routing hints; actual path policy remains in the orchestrator.
#[derive(Debug, Clone)]
pub struct MessageRequirements {
    pub min_bandwidth_bps: u64,
    pub max_latency_ms: Option<u32>,
    pub message_size: usize,
    pub reliable: bool,
    pub priority: MessagePriority,
    pub power_sensitive: bool,
    pub bypass_sync: bool,
    pub ttl: Option<Duration>,
}

pub fn requirements_for(class: DeliveryClass, message_size: usize) -> MessageRequirements {
    let policy = DeliveryPolicy::for_class(class);
    MessageRequirements {
        min_bandwidth_bps: match class {
            DeliveryClass::Bulk => 128_000,
            DeliveryClass::Telemetry => 4_000,
            _ => 1_000,
        },
        max_latency_ms: policy.max_latency_ms,
        message_size,
        reliable: policy.reliable,
        priority: match class {
            DeliveryClass::Emergency => MessagePriority::Critical,
            DeliveryClass::Bulk => MessagePriority::Background,
            _ => MessagePriority::High,
        },
        power_sensitive: matches!(class, DeliveryClass::Telemetry),
        bypass_sync: matches!(class, DeliveryClass::Telemetry),
        ttl: policy.ttl_ms.map(Duration::from_millis),
    }
}

#[derive(Debug, Clone)]
pub struct TransportPolicy {
    pub name: String,
    pub primary: Vec<String>,
    pub alternate: Vec<String>,
    pub contingency: Vec<String>,
    pub emergency: Vec<String>,
}
impl TransportPolicy {
    pub fn ordered(&self) -> impl Iterator<Item = &String> {
        self.primary
            .iter()
            .chain(&self.alternate)
            .chain(&self.contingency)
            .chain(&self.emergency)
    }
}
#[derive(Debug, Clone)]
pub struct TransportManagerConfig {
    pub default_policy: Option<TransportPolicy>,
}

pub fn uav_pace_config() -> TransportManagerConfig {
    TransportManagerConfig {
        default_policy: Some(TransportPolicy {
            name: "avian-default".to_owned(),
            primary: [
                "quic-silvus",
                "quic-microhard",
                "quic-wifi",
                "quic-ethernet",
            ]
            .map(str::to_owned)
            .to_vec(),
            alternate: ["quic-cellular", "quic-satellite"]
                .map(str::to_owned)
                .to_vec(),
            contingency: vec!["sub-ghz-data".to_owned()],
            emergency: vec!["ble-local".to_owned()],
        }),
    }
}

#[cfg(test)]
mod tests;
