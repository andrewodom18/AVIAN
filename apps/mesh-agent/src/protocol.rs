use mesh_core::DeliveryClass;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlResponse {
    Status { status: Box<AgentStatus> },
    Records { records: Vec<RecordView> },
    CommandIssued { command_id: String },
    Error { code: String, detail: String },
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
}
