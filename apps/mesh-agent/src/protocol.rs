use std::net::SocketAddr;

use mesh_core::DeliveryClass;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Underlay;
use crate::status::AgentStatus;

pub const LOCAL_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlRequest {
    Status {
        #[serde(default)]
        require_ready: bool,
    },
    ListRecords {
        class: DeliveryClass,
        #[serde(default = "default_record_limit")]
        limit: u16,
    },
    EmergencyRtl {
        target: String,
    },
    ConfigurePeer {
        formation_id: String,
        name: String,
        endpoint_id: String,
        addresses: Vec<PeerConnectionAddress>,
    },
    ListPairedPeers,
    RemovePeer {
        name: String,
    },
    ConnectionInfo {
        addresses: Vec<PeerConnectionAddress>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlResponse {
    Status {
        status: Box<AgentStatus>,
    },
    Records {
        records: Vec<RecordView>,
    },
    CommandIssued {
        command_id: String,
    },
    PeerConfigured {
        name: String,
        connected: bool,
    },
    PairedPeers {
        names: Vec<String>,
    },
    PeerRemoved {
        name: String,
    },
    ConnectionInfo {
        formation_id: String,
        name: String,
        endpoint_id: String,
        addresses: Vec<PeerConnectionAddress>,
    },
    Error {
        code: String,
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerConnectionAddress {
    pub underlay: Underlay,
    pub address: SocketAddr,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalEnvelope<T> {
    pub protocol_version: u16,
    pub body: T,
}

impl<T> LocalEnvelope<T> {
    pub fn new(body: T) -> Self {
        Self {
            protocol_version: LOCAL_PROTOCOL_VERSION,
            body,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordView {
    pub record_id: String,
    pub record: Value,
}

pub fn decode_request(encoded: &[u8]) -> anyhow::Result<ControlRequest> {
    let envelope: LocalEnvelope<ControlRequest> = serde_json::from_slice(encoded)?;
    anyhow::ensure!(
        envelope.protocol_version == LOCAL_PROTOCOL_VERSION,
        "unsupported local protocol version {}",
        envelope.protocol_version
    );
    Ok(envelope.body)
}

pub fn encode_request(request: ControlRequest) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&LocalEnvelope::new(request))?)
}

pub fn decode_response(encoded: &[u8]) -> anyhow::Result<ControlResponse> {
    let envelope: LocalEnvelope<ControlResponse> = serde_json::from_slice(encoded)?;
    anyhow::ensure!(
        envelope.protocol_version == LOCAL_PROTOCOL_VERSION,
        "unsupported local protocol version {}",
        envelope.protocol_version
    );
    Ok(envelope.body)
}

pub fn encode_response(response: ControlResponse) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(&LocalEnvelope::new(response))?)
}

fn default_record_limit() -> u16 {
    100
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_fields_and_versions() {
        let unknown = br#"{"protocol_version":1,"body":{"type":"status","surprise":true}}"#;
        assert!(decode_request(unknown).is_err());
        let wrong = br#"{"protocol_version":2,"body":{"type":"status"}}"#;
        assert!(decode_request(wrong).is_err());
    }

    #[test]
    fn connection_request_is_strict_and_versioned() {
        let encoded = br#"{"protocol_version":1,"body":{"type":"configure_peer","formation_id":"mission-alpha","name":"aircraft-001","endpoint_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","addresses":[{"underlay":"ethernet","address":"192.0.2.4:9000"}]}}"#;
        let request = decode_request(encoded).unwrap();
        assert!(
            matches!(request, ControlRequest::ConfigurePeer { name, .. } if name == "aircraft-001")
        );

        let unknown = br#"{"protocol_version":1,"body":{"type":"configure_peer","formation_id":"mission-alpha","name":"aircraft-001","endpoint_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","addresses":[{"underlay":"ethernet","address":"192.0.2.4:9000","secret":"no"}]}}"#;
        assert!(decode_request(unknown).is_err());
    }

    #[test]
    fn paired_peer_management_requests_are_strict_and_versioned() {
        let list = br#"{"protocol_version":1,"body":{"type":"list_paired_peers"}}"#;
        assert!(matches!(
            decode_request(list).unwrap(),
            ControlRequest::ListPairedPeers
        ));

        let remove =
            br#"{"protocol_version":1,"body":{"type":"remove_peer","name":"aircraft-001"}}"#;
        assert!(matches!(
            decode_request(remove).unwrap(),
            ControlRequest::RemovePeer { name } if name == "aircraft-001"
        ));

        let unknown = br#"{"protocol_version":1,"body":{"type":"remove_peer","name":"aircraft-001","force":true}}"#;
        assert!(decode_request(unknown).is_err());
    }
}
