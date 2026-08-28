//! Vendor-neutral assertion that binds an onboard AVIAN identity to the
//! explicitly provisioned radio physically attached to the same aircraft.

use serde::{Deserialize, Serialize};

use crate::NodeId;

pub const RADIO_ATTACHMENT_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioAttachmentAssertion {
    pub schema_version: u16,
    pub observed_at_ms: u64,
    pub avian_node_id: NodeId,
    pub drone_id: String,
    pub radio_mac: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio_node_id: Option<String>,
}

impl RadioAttachmentAssertion {
    pub fn new(
        observed_at_ms: u64,
        avian_node_id: NodeId,
        drone_id: impl Into<String>,
        radio_mac: impl Into<String>,
        radio_node_id: Option<String>,
    ) -> Result<Self, RadioAttachmentError> {
        let drone_id = drone_id.into().trim().to_owned();
        if drone_id.is_empty() {
            return Err(RadioAttachmentError::MissingDroneId);
        }
        let radio_mac = normalize_mac(&radio_mac.into())?;
        let radio_node_id = radio_node_id
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        Ok(Self {
            schema_version: RADIO_ATTACHMENT_SCHEMA_VERSION,
            observed_at_ms,
            avian_node_id,
            drone_id,
            radio_mac,
            radio_node_id,
        })
    }

    pub fn validate(&self) -> Result<(), RadioAttachmentError> {
        if self.schema_version != RADIO_ATTACHMENT_SCHEMA_VERSION {
            return Err(RadioAttachmentError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.drone_id.trim().is_empty() {
            return Err(RadioAttachmentError::MissingDroneId);
        }
        normalize_mac(&self.radio_mac)?;
        Ok(())
    }
}

fn normalize_mac(value: &str) -> Result<String, RadioAttachmentError> {
    let compact = value
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .collect::<String>();
    if compact.len() != 12 {
        return Err(RadioAttachmentError::InvalidMacAddress);
    }
    Ok(compact
        .as_bytes()
        .chunks(2)
        .map(|chunk| {
            std::str::from_utf8(chunk)
                .unwrap_or_default()
                .to_ascii_lowercase()
        })
        .collect::<Vec<_>>()
        .join(":"))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RadioAttachmentError {
    #[error("unsupported radio attachment schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("drone_id is required")]
    MissingDroneId,
    #[error("radio_mac must contain exactly 12 hexadecimal digits")]
    InvalidMacAddress,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_mac_and_keeps_identities_distinct() {
        let assertion = RadioAttachmentAssertion::new(
            42,
            NodeId::from("avian-air-7"),
            "drone-7",
            "001E3F209A10",
            Some("17".into()),
        )
        .unwrap();
        assert_eq!(assertion.radio_mac, "00:1e:3f:20:9a:10");
        assert_eq!(assertion.drone_id, "drone-7");
        assert_eq!(assertion.avian_node_id, NodeId::from("avian-air-7"));
    }

    #[test]
    fn rejects_ip_addresses_as_radio_identity() {
        assert_eq!(
            RadioAttachmentAssertion::new(1, NodeId::from("air"), "drone", "10.0.0.2", None),
            Err(RadioAttachmentError::InvalidMacAddress)
        );
    }
}
