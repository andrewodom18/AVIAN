//! Vendor-neutral radio contracts.
//!
//! Existing StreamCaster contracts remain stable while new radio families use
//! this boundary. Vendor adapters normalize hardware data into these types;
//! ARC remains the authority for desired configuration and activation policy.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeSet;
use std::net::IpAddr;
use thiserror::Error;

use crate::NodeId;

pub const RADIO_DEVICE_SCHEMA_VERSION: u16 = 1;
pub const RADIO_DISCOVERY_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RadioVendorId(String);

impl RadioVendorId {
    pub fn new(value: impl Into<String>) -> Result<Self, VendorRadioError> {
        let value = value.into();
        validate_token("vendor", &value)?;
        Ok(Self(value))
    }

    pub fn silvus() -> Self {
        Self("silvus".to_owned())
    }

    pub fn microhard() -> Self {
        Self("microhard".to_owned())
    }

    pub fn trellisware() -> Self {
        Self("trellisware".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for RadioVendorId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RadioVendorId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioNetworkMode {
    PointToPoint,
    PointToMultipoint,
    Relay,
    Mesh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioManagementInterface {
    SerialConsole,
    WebUi,
    Ssh,
    Telnet,
    Snmp,
    VendorApi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioEvidenceLevel {
    Published,
    DeviceReported,
    FieldMeasured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioDiscoveryMethod {
    NeighborTable,
    Oui,
    Mdns,
    Dhcp,
    TlsFingerprint,
    TcpReachability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioReachabilityStatus {
    Reachable,
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioManagementAuthentication {
    Unknown,
    ClientCertificateRequired,
    Authenticated,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioManagementEndpoint {
    pub address: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_index: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioDiscoveryObservation {
    pub schema_version: u16,
    pub observed_at_ms: u64,
    pub source: NodeId,
    pub vendor: RadioVendorId,
    pub model_hint: String,
    pub mac_address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    pub reachability: RadioReachabilityStatus,
    pub management_authentication: RadioManagementAuthentication,
    pub management_endpoints: Vec<RadioManagementEndpoint>,
    pub discovery_methods: Vec<RadioDiscoveryMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl RadioDiscoveryObservation {
    pub fn validate(&self) -> Result<(), VendorRadioError> {
        if self.schema_version != RADIO_DISCOVERY_SCHEMA_VERSION {
            return Err(VendorRadioError::UnsupportedDiscoverySchemaVersion(
                self.schema_version,
            ));
        }
        validate_token("model_hint", &self.model_hint)?;
        if self.mac_address.trim().is_empty() {
            return Err(VendorRadioError::MissingMacAddress);
        }
        if self.management_endpoints.is_empty() {
            return Err(VendorRadioError::MissingManagementEndpoints);
        }
        if self
            .management_endpoints
            .iter()
            .any(|endpoint| endpoint.address.trim().is_empty() || endpoint.port == 0)
        {
            return Err(VendorRadioError::InvalidManagementEndpoint);
        }
        if self.discovery_methods.is_empty() {
            return Err(VendorRadioError::MissingDiscoveryMethods);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioFrequencyRange {
    pub minimum_mhz: f64,
    pub maximum_mhz: f64,
}

impl RadioFrequencyRange {
    pub fn validate(&self) -> Result<(), VendorRadioError> {
        if !self.minimum_mhz.is_finite()
            || !self.maximum_mhz.is_finite()
            || self.minimum_mhz <= 0.0
            || self.maximum_mhz < self.minimum_mhz
        {
            return Err(VendorRadioError::InvalidFrequencyRange {
                minimum_mhz: self.minimum_mhz,
                maximum_mhz: self.maximum_mhz,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioChannelCapability {
    pub bandwidth_mhz: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_throughput_mbps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receiver_sensitivity_dbm: Option<f64>,
}

impl RadioChannelCapability {
    pub fn validate(&self) -> Result<(), VendorRadioError> {
        if !self.bandwidth_mhz.is_finite() || self.bandwidth_mhz <= 0.0 {
            return Err(VendorRadioError::InvalidBandwidth(self.bandwidth_mhz));
        }
        validate_optional_positive("measured_throughput_mbps", self.measured_throughput_mbps)?;
        validate_optional_finite("receiver_sensitivity_dbm", self.receiver_sensitivity_dbm)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioCapabilities {
    pub schema_version: u16,
    pub observed_at_ms: u64,
    pub vendor: RadioVendorId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    pub evidence: RadioEvidenceLevel,
    pub frequency_ranges: Vec<RadioFrequencyRange>,
    pub channels: Vec<RadioChannelCapability>,
    pub network_modes: Vec<RadioNetworkMode>,
    pub management_interfaces: Vec<RadioManagementInterface>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maximum_total_transmit_power_dbm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub antenna_port_count: Option<u8>,
}

impl RadioCapabilities {
    pub fn validate(&self) -> Result<(), VendorRadioError> {
        if self.schema_version != RADIO_DEVICE_SCHEMA_VERSION {
            return Err(VendorRadioError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        validate_token("model", &self.model)?;
        if self.frequency_ranges.is_empty() {
            return Err(VendorRadioError::MissingFrequencyRanges);
        }
        for range in &self.frequency_ranges {
            range.validate()?;
        }
        if self.channels.is_empty() {
            return Err(VendorRadioError::MissingChannelCapabilities);
        }
        for channel in &self.channels {
            channel.validate()?;
        }
        validate_optional_finite(
            "maximum_total_transmit_power_dbm",
            self.maximum_total_transmit_power_dbm,
        )?;
        if self.antenna_port_count == Some(0) {
            return Err(VendorRadioError::InvalidAntennaPortCount);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioDeviceStatus {
    Online,
    Unreachable,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioIdentity {
    pub vendor: RadioVendorId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mac_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RadioEffectiveState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_mode: Option<RadioNetworkMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub center_frequency_mhz: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth_mhz: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transmit_power_dbm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_rssi_dbm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireless_rx_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireless_tx_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_percent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioNeighborObservation {
    pub peer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rssi_dbm: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snr_db: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tx_rate_mbps: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rx_rate_mbps: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioDeviceObservation {
    pub schema_version: u16,
    pub observed_at_ms: u64,
    pub source: NodeId,
    pub status: RadioDeviceStatus,
    pub simulated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_ip: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<RadioIdentity>,
    #[serde(default)]
    pub effective: RadioEffectiveState,
    #[serde(default)]
    pub neighbors: Vec<RadioNeighborObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl RadioDeviceObservation {
    pub fn validate(&self) -> Result<(), VendorRadioError> {
        if self.schema_version != RADIO_DEVICE_SCHEMA_VERSION {
            return Err(VendorRadioError::UnsupportedSchemaVersion(
                self.schema_version,
            ));
        }
        if self.observed_at_ms == 0 {
            return Err(VendorRadioError::InvalidObservationTimestamp);
        }
        if let Some(management_ip) = self.management_ip.as_deref() {
            validate_ip("management_ip", management_ip)?;
        }
        if self.status == RadioDeviceStatus::Online && self.identity.is_none() {
            return Err(VendorRadioError::MissingOnlineIdentity);
        }
        if let Some(identity) = &self.identity {
            validate_token("model", &identity.model)?;
            if let Some(mac) = identity.mac_address.as_deref() {
                validate_mac(mac)?;
            }
        }
        validate_optional_positive("center_frequency_mhz", self.effective.center_frequency_mhz)?;
        validate_optional_positive("bandwidth_mhz", self.effective.bandwidth_mhz)?;
        validate_optional_finite("transmit_power_dbm", self.effective.transmit_power_dbm)?;
        validate_optional_finite("reported_rssi_dbm", self.effective.reported_rssi_dbm)?;
        if self
            .effective
            .battery_percent
            .is_some_and(|value| value > 100)
        {
            return Err(VendorRadioError::InvalidBatteryPercent);
        }

        let mut peer_ids = BTreeSet::new();
        for neighbor in &self.neighbors {
            validate_token("peer_id", &neighbor.peer_id)?;
            if !peer_ids.insert(neighbor.peer_id.as_str()) {
                return Err(VendorRadioError::DuplicatePeerId(neighbor.peer_id.clone()));
            }
            if let Some(peer_ip) = neighbor.peer_ip.as_deref() {
                validate_ip("peer_ip", peer_ip)?;
            }
            validate_optional_finite("neighbor_rssi_dbm", neighbor.rssi_dbm)?;
            validate_optional_finite("neighbor_snr_db", neighbor.snr_db)?;
            validate_optional_positive("neighbor_tx_rate_mbps", neighbor.tx_rate_mbps)?;
            validate_optional_positive("neighbor_rx_rate_mbps", neighbor.rx_rate_mbps)?;
        }
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum VendorRadioError {
    #[error("unsupported vendor-radio schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("unsupported radio-discovery schema version {0}")]
    UnsupportedDiscoverySchemaVersion(u16),
    #[error("invalid {field} token {value:?}")]
    InvalidToken { field: &'static str, value: String },
    #[error("invalid radio frequency range {minimum_mhz}..={maximum_mhz} MHz")]
    InvalidFrequencyRange { minimum_mhz: f64, maximum_mhz: f64 },
    #[error("radio capability has no frequency ranges")]
    MissingFrequencyRanges,
    #[error("radio capability has no channel capabilities")]
    MissingChannelCapabilities,
    #[error("invalid radio channel bandwidth {0} MHz")]
    InvalidBandwidth(f64),
    #[error("{field} must be finite")]
    NonFinite { field: &'static str },
    #[error("{field} must be positive")]
    NonPositive { field: &'static str },
    #[error("antenna port count must be positive when supplied")]
    InvalidAntennaPortCount,
    #[error("radio discovery requires a MAC address")]
    MissingMacAddress,
    #[error("radio discovery requires at least one management endpoint")]
    MissingManagementEndpoints,
    #[error("radio discovery contains an invalid management endpoint")]
    InvalidManagementEndpoint,
    #[error("radio discovery requires at least one evidence method")]
    MissingDiscoveryMethods,
    #[error("radio observation timestamp must be positive")]
    InvalidObservationTimestamp,
    #[error("online radio observation requires an identity")]
    MissingOnlineIdentity,
    #[error("invalid {field} IP address {value:?}")]
    InvalidIpAddress { field: &'static str, value: String },
    #[error("invalid radio MAC address {0:?}")]
    InvalidMacAddress(String),
    #[error("radio battery percent must be between 0 and 100")]
    InvalidBatteryPercent,
    #[error("duplicate radio neighbor peer ID {0:?}")]
    DuplicatePeerId(String),
}

fn validate_ip(field: &'static str, value: &str) -> Result<(), VendorRadioError> {
    value
        .parse::<IpAddr>()
        .map(|_| ())
        .map_err(|_| VendorRadioError::InvalidIpAddress {
            field,
            value: value.to_owned(),
        })
}

fn validate_mac(value: &str) -> Result<(), VendorRadioError> {
    let bytes = value.as_bytes();
    let valid = bytes.len() == 17
        && bytes.iter().enumerate().all(|(index, byte)| {
            if matches!(index, 2 | 5 | 8 | 11 | 14) {
                *byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        });
    if valid {
        Ok(())
    } else {
        Err(VendorRadioError::InvalidMacAddress(value.to_owned()))
    }
}

fn validate_token(field: &'static str, value: &str) -> Result<(), VendorRadioError> {
    let valid = !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(VendorRadioError::InvalidToken {
            field,
            value: value.to_owned(),
        })
    }
}

fn validate_optional_finite(
    field: &'static str,
    value: Option<f64>,
) -> Result<(), VendorRadioError> {
    if value.is_some_and(|value| !value.is_finite()) {
        return Err(VendorRadioError::NonFinite { field });
    }
    Ok(())
}

fn validate_optional_positive(
    field: &'static str,
    value: Option<f64>,
) -> Result<(), VendorRadioError> {
    validate_optional_finite(field, value)?;
    if value.is_some_and(|value| value <= 0.0) {
        return Err(VendorRadioError::NonPositive { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendor_ids_are_extensible_but_safe_for_topics_and_records() {
        assert_eq!(RadioVendorId::microhard().as_str(), "microhard");
        assert_eq!(RadioVendorId::trellisware().as_str(), "trellisware");
        assert!(RadioVendorId::new("future-radio_1").is_ok());
        assert!(RadioVendorId::new("future radio").is_err());
        assert!(serde_json::from_str::<RadioVendorId>(r#""future radio""#).is_err());
    }

    #[test]
    fn capabilities_accept_vendor_specific_channel_widths() {
        let capabilities = RadioCapabilities {
            schema_version: RADIO_DEVICE_SCHEMA_VERSION,
            observed_at_ms: 1,
            vendor: RadioVendorId::microhard(),
            model: "pmddl2460".into(),
            firmware_version: None,
            evidence: RadioEvidenceLevel::Published,
            frequency_ranges: vec![RadioFrequencyRange {
                minimum_mhz: 2_402.0,
                maximum_mhz: 2_478.0,
            }],
            channels: vec![RadioChannelCapability {
                bandwidth_mhz: 40.0,
                measured_throughput_mbps: None,
                receiver_sensitivity_dbm: None,
            }],
            network_modes: vec![RadioNetworkMode::Mesh],
            management_interfaces: vec![RadioManagementInterface::Snmp],
            maximum_total_transmit_power_dbm: Some(30.0),
            antenna_port_count: Some(4),
        };

        capabilities.validate().unwrap();
    }

    #[test]
    fn discovery_record_preserves_reachable_but_certificate_required_state() {
        let discovery = RadioDiscoveryObservation {
            schema_version: RADIO_DISCOVERY_SCHEMA_VERSION,
            observed_at_ms: 1,
            source: NodeId::from("radio/trellisware/001e3f209a10"),
            vendor: RadioVendorId::trellisware(),
            model_hint: "tw-950".into(),
            mac_address: "00:1e:3f:20:9a:10".into(),
            serial_number: None,
            hostname: None,
            reachability: RadioReachabilityStatus::Reachable,
            management_authentication: RadioManagementAuthentication::ClientCertificateRequired,
            management_endpoints: vec![RadioManagementEndpoint {
                address: "10.1.0.2".into(),
                port: 443,
                interface: Some("Ethernet 2".into()),
                interface_index: Some(6),
            }],
            discovery_methods: vec![
                RadioDiscoveryMethod::NeighborTable,
                RadioDiscoveryMethod::Oui,
                RadioDiscoveryMethod::TcpReachability,
            ],
            error_code: Some("client_certificate_required".into()),
        };

        discovery.validate().unwrap();
        let encoded = serde_json::to_value(&discovery).unwrap();
        assert_eq!(encoded["reachability"], "reachable");
        assert_eq!(
            encoded["management_authentication"],
            "client_certificate_required"
        );
        assert_eq!(encoded["management_endpoints"][0]["interface_index"], 6);
    }

    #[test]
    fn device_observation_rejects_invalid_identity_and_telemetry() {
        let mut observation = RadioDeviceObservation {
            schema_version: RADIO_DEVICE_SCHEMA_VERSION,
            observed_at_ms: 1,
            source: NodeId::from("radio/trellisware/001e3f209a10"),
            status: RadioDeviceStatus::Online,
            simulated: false,
            management_ip: Some("10.1.0.2".into()),
            identity: Some(RadioIdentity {
                vendor: RadioVendorId::trellisware(),
                model: "tw-950".into(),
                serial_number: None,
                firmware_version: None,
                mac_address: Some("00:1e:3f:20:9a:10".into()),
                system_name: None,
            }),
            effective: RadioEffectiveState::default(),
            neighbors: vec![],
            error: None,
        };
        observation.validate().unwrap();

        observation.effective.battery_percent = Some(101);
        assert_eq!(
            observation.validate(),
            Err(VendorRadioError::InvalidBatteryPercent)
        );
        observation.effective.battery_percent = None;
        observation.identity.as_mut().unwrap().mac_address = Some("not-a-mac".into());
        assert!(matches!(
            observation.validate(),
            Err(VendorRadioError::InvalidMacAddress(_))
        ));
    }
}
