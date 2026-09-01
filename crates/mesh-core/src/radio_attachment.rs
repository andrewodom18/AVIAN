//! Vendor-neutral assertion that binds an onboard AVIAN identity to the
//! explicitly provisioned radio physically attached to the same aircraft.

use serde::{Deserialize, Serialize};

use crate::NodeId;

pub const RADIO_ATTACHMENT_SCHEMA_VERSION_V1: u16 = 1;
pub const RADIO_ATTACHMENT_SCHEMA_VERSION: u16 = 2;

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio_serial_number: Option<String>,
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
            radio_serial_number: None,
        })
    }

    pub fn with_serial_number(mut self, serial_number: impl Into<String>) -> Self {
        self.radio_serial_number =
            Some(serial_number.into().trim().to_owned()).filter(|value| !value.is_empty());
        self
    }

    pub fn validate(&self) -> Result<(), RadioAttachmentError> {
        if !matches!(
            self.schema_version,
            RADIO_ATTACHMENT_SCHEMA_VERSION_V1 | RADIO_ATTACHMENT_SCHEMA_VERSION
        ) {
            return Err(RadioAttachmentError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.drone_id.trim().is_empty() {
            return Err(RadioAttachmentError::MissingDroneId);
        }
        normalize_mac(&self.radio_mac)?;
        if self
            .radio_node_id
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(RadioAttachmentError::InvalidRadioNodeId);
        }
        if self
            .radio_serial_number
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(RadioAttachmentError::InvalidSerialNumber);
        }
        Ok(())
    }

    pub fn matches_observed_identity(
        &self,
        mac_address: &str,
        radio_node_id: Option<&str>,
        serial_number: Option<&str>,
    ) -> RadioAttachmentMatch {
        let Ok(expected_mac) = normalize_mac(&self.radio_mac) else {
            return RadioAttachmentMatch::MacMismatch;
        };
        if normalize_mac(mac_address).ok().as_deref() != Some(expected_mac.as_str()) {
            return RadioAttachmentMatch::MacMismatch;
        }
        if let Some(expected) = self.radio_node_id.as_deref() {
            if radio_node_id != Some(expected) {
                return RadioAttachmentMatch::NodeIdMismatch;
            }
        }
        if let Some(expected) = self.radio_serial_number.as_deref() {
            if serial_number != Some(expected) {
                return RadioAttachmentMatch::SerialMismatch;
            }
        }
        RadioAttachmentMatch::Matched
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioAttachmentMatch {
    Matched,
    MacMismatch,
    NodeIdMismatch,
    SerialMismatch,
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
    #[error("radio_node_id cannot be empty when supplied")]
    InvalidRadioNodeId,
    #[error("radio_serial_number cannot be empty when supplied")]
    InvalidSerialNumber,
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

    #[test]
    fn detects_identity_drift_without_using_management_ip() {
        let assertion = RadioAttachmentAssertion::new(
            42,
            NodeId::from("avian-air-7"),
            "drone-7",
            "00:1e:3f:20:9a:10",
            Some("17".into()),
        )
        .unwrap()
        .with_serial_number("TW950-123");
        assert_eq!(
            assertion.matches_observed_identity("00-1E-3F-20-9A-10", Some("17"), Some("TW950-123")),
            RadioAttachmentMatch::Matched
        );
        assert_eq!(
            assertion.matches_observed_identity("00:1e:3f:20:9a:10", Some("18"), Some("TW950-123")),
            RadioAttachmentMatch::NodeIdMismatch
        );
    }
}
