//! PEAT-backed networking and policy mapping for the AVIAN domain.

mod node;

pub use node::{
    derive_peat_endpoint_id, AvianRecord, PeatNode, PeatNodeConfig, PeatNodeError, PeerDescriptor,
    AVIAN_SCHEMA_VERSION,
};

use std::time::Duration;

use mesh_core::{DeliveryClass, DeliveryPolicy};
use peat_mesh::transport::{
    MessagePriority, MessageRequirements, TransportManagerConfig, TransportMode, TransportPolicy,
};

/// Maps application delivery semantics into PEAT's transport-neutral routing
/// requirements. Link duplication is performed by the UAV link orchestrator.
pub fn requirements_for(class: DeliveryClass, message_size: usize) -> MessageRequirements {
    let policy = DeliveryPolicy::for_class(class);
    let priority = match class {
        DeliveryClass::Emergency => MessagePriority::Critical,
        DeliveryClass::Acknowledgement => MessagePriority::High,
        DeliveryClass::Mission => MessagePriority::High,
        DeliveryClass::Telemetry => MessagePriority::High,
        DeliveryClass::Bulk => MessagePriority::Background,
    };

    MessageRequirements {
        min_bandwidth_bps: match class {
            DeliveryClass::Bulk => 128_000,
            DeliveryClass::Telemetry => 4_000,
            DeliveryClass::Emergency | DeliveryClass::Acknowledgement | DeliveryClass::Mission => {
                1_000
            }
        },
        max_latency_ms: policy.max_latency_ms,
        message_size,
        reliable: policy.reliable,
        priority,
        power_sensitive: matches!(class, DeliveryClass::Telemetry),
        bypass_sync: matches!(class, DeliveryClass::Telemetry),
        ttl: policy.ttl_ms.map(Duration::from_millis),
    }
}

/// Default names are logical transport instances; hardware configuration maps
/// them to interfaces at deployment time.
pub fn uav_pace_config() -> TransportManagerConfig {
    let policy = TransportPolicy::new("avian-default")
        .primary(vec![
            "quic-silvus",
            "quic-microhard",
            "quic-wifi",
            "quic-ethernet",
        ])
        .alternate(vec!["quic-cellular", "quic-satellite"])
        .contingency(vec!["sub-ghz-data"])
        .emergency(vec!["ble-local"]);

    TransportManagerConfig::with_policy(policy).with_mode(TransportMode::Single)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emergency_is_reliable_and_critical() {
        let requirements = requirements_for(DeliveryClass::Emergency, 256);
        assert!(requirements.reliable);
        assert_eq!(requirements.priority, MessagePriority::Critical);
        assert_eq!(requirements.max_latency_ms, Some(250));
        assert!(!requirements.bypass_sync);
    }

    #[test]
    fn telemetry_is_expiring_latest_value_traffic() {
        let requirements = requirements_for(DeliveryClass::Telemetry, 512);
        assert!(!requirements.reliable);
        assert!(requirements.bypass_sync);
        assert_eq!(requirements.ttl, Some(Duration::from_secs(2)));
    }

    #[test]
    fn pace_policy_has_no_cloud_dependency() {
        let config = uav_pace_config();
        let policy = config.default_policy.expect("PACE policy");
        assert_eq!(policy.name, "avian-default");
        assert!(policy.ordered().all(|id| !id.as_str().contains("cloud")));
        assert!(policy.ordered().any(|id| id.as_str() == "quic-silvus"));
        assert!(policy.ordered().any(|id| id.as_str() == "quic-microhard"));
    }
}
