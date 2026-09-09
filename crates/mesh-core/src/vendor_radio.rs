//! Vendor-neutral radio contracts.
//!
//! Existing StreamCaster contracts remain stable while new radio families use
//! this boundary. Vendor adapters normalize hardware data into these types;
//! ARC owns operator intent and workflow, CHUD owns physical-radio discovery,
//! credentials, writes, readback, and effective state, and AVIAN consumes the
//! normalized evidence for attachment and topology decisions.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::NodeId;

pub const RADIO_DEVICE_SCHEMA_VERSION: u16 = 1;
pub const RADIO_DEVICE_OBSERVATION_SCHEMA_VERSION_V1: u16 = 1;
pub const RADIO_DEVICE_OBSERVATION_SCHEMA_VERSION: u16 = 2;
pub const RADIO_DISCOVERY_SCHEMA_VERSION_V1: u16 = 1;
pub const RADIO_DISCOVERY_SCHEMA_VERSION: u16 = 2;
pub const RADIO_DISCOVERY_INTAKE_SCHEMA_VERSION: u16 = 1;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioObservationAuthority {
    ChudAuthoritative,
    AvianDiagnostic,
    Simulation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RadioManagementLifecycle {
    Discovered,
    Reachable,
    Authenticated,
    Managed,
    Connected,
    Stale,
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
    pub vendor_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    pub reachability: RadioReachabilityStatus,
    pub management_authentication: RadioManagementAuthentication,
    pub management_endpoints: Vec<RadioManagementEndpoint>,
    pub discovery_methods: Vec<RadioDiscoveryMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_authority: Option<RadioObservationAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_lifecycle: Option<RadioManagementLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_driver_available: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

impl RadioDiscoveryObservation {
    pub fn validate(&self) -> Result<(), VendorRadioError> {
        if !matches!(
            self.schema_version,
            RADIO_DISCOVERY_SCHEMA_VERSION_V1 | RADIO_DISCOVERY_SCHEMA_VERSION
        ) {
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
        if self.schema_version == RADIO_DISCOVERY_SCHEMA_VERSION {
            validate_v2_metadata(
                self.observed_at_ms,
                self.source_authority,
                self.management_lifecycle,
                self.management_driver_available,
                self.observation_revision,
                self.expires_at_ms,
            )?;
        }
        Ok(())
    }

    pub fn is_fresh_at(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_some_and(|expiry| now_ms <= expiry)
    }

    /// Returns true only for a fresh CHUD record that is safe to use as the
    /// identity and management target for a configuration workflow.
    pub fn is_authoritative_for_configuration_at(&self, now_ms: u64) -> bool {
        self.validate().is_ok()
            && self.schema_version == RADIO_DISCOVERY_SCHEMA_VERSION
            && self.source_authority == Some(RadioObservationAuthority::ChudAuthoritative)
            && self.reachability == RadioReachabilityStatus::Reachable
            && self.management_authentication == RadioManagementAuthentication::Authenticated
            && matches!(
                self.management_lifecycle,
                Some(RadioManagementLifecycle::Managed | RadioManagementLifecycle::Connected)
            )
            && self.management_driver_available == Some(true)
            && self.is_fresh_at(now_ms)
    }

    pub fn v1_compatibility_record(&self) -> Self {
        let mut compatibility = self.clone();
        compatibility.schema_version = RADIO_DISCOVERY_SCHEMA_VERSION_V1;
        compatibility.vendor_node_id = None;
        compatibility.source_authority = None;
        compatibility.management_lifecycle = None;
        compatibility.management_driver_available = None;
        compatibility.observation_revision = None;
        compatibility.expires_at_ms = None;
        compatibility
    }
}

/// Candidate-only envelope for submitting AVIAN's host-interface observation
/// to CHUD. Transport authentication is external to this payload (for example,
/// a scoped CHUD API key); successful submission never changes the embedded
/// observation's authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioDiscoveryIntakeEnvelope {
    pub schema_version: u16,
    pub observation_id: String,
    pub source_instance_id: String,
    pub submitted_at_ms: u64,
    pub nonce: String,
    pub idempotency_key: String,
    pub observation: RadioDiscoveryObservation,
}

impl RadioDiscoveryIntakeEnvelope {
    pub fn validate_at(&self, now_ms: u64) -> Result<(), VendorRadioError> {
        if self.schema_version != RADIO_DISCOVERY_INTAKE_SCHEMA_VERSION {
            return Err(VendorRadioError::UnsupportedDiscoveryIntakeSchemaVersion(
                self.schema_version,
            ));
        }
        validate_token("observation_id", &self.observation_id)?;
        validate_token("source_instance_id", &self.source_instance_id)?;
        validate_token("nonce", &self.nonce)?;
        validate_token("idempotency_key", &self.idempotency_key)?;
        if self.submitted_at_ms < self.observation.observed_at_ms
            || self.submitted_at_ms > now_ms.saturating_add(300_000)
        {
            return Err(VendorRadioError::InvalidDiscoveryIntakeTimestamp);
        }
        self.observation.validate()?;
        if self.observation.schema_version != RADIO_DISCOVERY_SCHEMA_VERSION
            || self.observation.source_authority != Some(RadioObservationAuthority::AvianDiagnostic)
        {
            return Err(VendorRadioError::InvalidDiscoveryIntakeAuthority);
        }
        if !self.observation.is_fresh_at(now_ms) {
            return Err(VendorRadioError::StaleDiscoveryIntakeObservation);
        }
        Ok(())
    }

    pub fn deduplication_identity(&self) -> (&str, &str) {
        (&self.source_instance_id, &self.idempotency_key)
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
    pub vendor_node_id: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_authority: Option<RadioObservationAuthority>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_authority: Option<RadioObservationAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_lifecycle: Option<RadioManagementLifecycle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_driver_available: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

impl RadioDeviceObservation {
    pub fn validate(&self) -> Result<(), VendorRadioError> {
        if !matches!(
            self.schema_version,
            RADIO_DEVICE_OBSERVATION_SCHEMA_VERSION_V1 | RADIO_DEVICE_OBSERVATION_SCHEMA_VERSION
        ) {
            return Err(VendorRadioError::UnsupportedObservationSchemaVersion(
                self.schema_version,
            ));
        }
        if self.schema_version == RADIO_DEVICE_OBSERVATION_SCHEMA_VERSION {
            validate_v2_metadata(
                self.observed_at_ms,
                self.source_authority,
                self.management_lifecycle,
                self.management_driver_available,
                self.observation_revision,
                self.expires_at_ms,
            )?;
        }
        Ok(())
    }

    pub fn is_fresh_at(&self, now_ms: u64) -> bool {
        self.expires_at_ms.is_some_and(|expiry| now_ms <= expiry)
    }

    /// Measured links may influence live topology only when CHUD supplied a
    /// fresh, non-simulated observation. Published estimates remain separate.
    pub fn authoritative_neighbors_at(&self, now_ms: u64) -> &[RadioNeighborObservation] {
        if self.validate().is_ok()
            && !self.simulated
            && self.source_authority == Some(RadioObservationAuthority::ChudAuthoritative)
            && matches!(
                self.management_lifecycle,
                Some(RadioManagementLifecycle::Managed | RadioManagementLifecycle::Connected)
            )
            && self.is_fresh_at(now_ms)
        {
            &self.neighbors
        } else {
            &[]
        }
    }

    pub fn v1_compatibility_record(&self) -> Self {
        let mut compatibility = self.clone();
        compatibility.schema_version = RADIO_DEVICE_OBSERVATION_SCHEMA_VERSION_V1;
        compatibility.source_authority = None;
        compatibility.management_lifecycle = None;
        compatibility.management_driver_available = None;
        compatibility.observation_revision = None;
        compatibility.expires_at_ms = None;
        if let Some(identity) = compatibility.identity.as_mut() {
            identity.vendor_node_id = None;
        }
        for neighbor in &mut compatibility.neighbors {
            neighbor.observed_at_ms = None;
            neighbor.source_authority = None;
        }
        compatibility
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum VendorRadioError {
    #[error("unsupported vendor-radio schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("unsupported radio-discovery schema version {0}")]
    UnsupportedDiscoverySchemaVersion(u16),
    #[error("unsupported radio-discovery intake schema version {0}")]
    UnsupportedDiscoveryIntakeSchemaVersion(u16),
    #[error("unsupported radio-device observation schema version {0}")]
    UnsupportedObservationSchemaVersion(u16),
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
    #[error("radio-discovery intake timestamp is invalid")]
    InvalidDiscoveryIntakeTimestamp,
    #[error("radio-discovery intake accepts only v2 AVIAN diagnostic observations")]
    InvalidDiscoveryIntakeAuthority,
    #[error("radio-discovery intake observation is stale")]
    StaleDiscoveryIntakeObservation,
    #[error("v2 radio observations require an explicit source authority")]
    MissingSourceAuthority,
    #[error("v2 radio observations require a management lifecycle")]
    MissingManagementLifecycle,
    #[error("v2 radio observations require management-driver availability")]
    MissingManagementDriverAvailability,
    #[error("v2 radio observations require a positive revision")]
    InvalidObservationRevision,
    #[error("v2 radio observation expiry must be later than observed_at_ms")]
    InvalidObservationExpiry,
}

fn validate_v2_metadata(
    observed_at_ms: u64,
    source_authority: Option<RadioObservationAuthority>,
    management_lifecycle: Option<RadioManagementLifecycle>,
    management_driver_available: Option<bool>,
    observation_revision: Option<u64>,
    expires_at_ms: Option<u64>,
) -> Result<(), VendorRadioError> {
    source_authority.ok_or(VendorRadioError::MissingSourceAuthority)?;
    management_lifecycle.ok_or(VendorRadioError::MissingManagementLifecycle)?;
    management_driver_available.ok_or(VendorRadioError::MissingManagementDriverAvailability)?;
    if observation_revision.is_none_or(|revision| revision == 0) {
        return Err(VendorRadioError::InvalidObservationRevision);
    }
    if expires_at_ms.is_none_or(|expiry| expiry <= observed_at_ms) {
        return Err(VendorRadioError::InvalidObservationExpiry);
    }
    Ok(())
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
    use std::collections::BTreeSet;

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
            vendor_node_id: None,
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
            source_authority: Some(RadioObservationAuthority::AvianDiagnostic),
            management_lifecycle: Some(RadioManagementLifecycle::Reachable),
            management_driver_available: Some(false),
            observation_revision: Some(1),
            expires_at_ms: Some(10_001),
        };

        discovery.validate().unwrap();
        let encoded = serde_json::to_value(&discovery).unwrap();
        assert_eq!(encoded["reachability"], "reachable");
        assert_eq!(
            encoded["management_authentication"],
            "client_certificate_required"
        );
        assert_eq!(encoded["management_endpoints"][0]["interface_index"], 6);
        assert!(!discovery.is_authoritative_for_configuration_at(2));
    }

    #[test]
    fn only_fresh_chud_managed_discovery_can_drive_configuration() {
        let mut discovery = RadioDiscoveryObservation {
            schema_version: RADIO_DISCOVERY_SCHEMA_VERSION,
            observed_at_ms: 100,
            source: NodeId::from("chud/radio/001e3f209a10"),
            vendor: RadioVendorId::trellisware(),
            model_hint: "tw-950".into(),
            mac_address: "00:1e:3f:20:9a:10".into(),
            serial_number: Some("TW950-123".into()),
            vendor_node_id: Some("17".into()),
            hostname: None,
            reachability: RadioReachabilityStatus::Reachable,
            management_authentication: RadioManagementAuthentication::Authenticated,
            management_endpoints: vec![RadioManagementEndpoint {
                address: "10.1.0.2".into(),
                port: 443,
                interface: Some("Ethernet 2".into()),
                interface_index: Some(6),
            }],
            discovery_methods: vec![RadioDiscoveryMethod::NeighborTable],
            error_code: None,
            source_authority: Some(RadioObservationAuthority::ChudAuthoritative),
            management_lifecycle: Some(RadioManagementLifecycle::Managed),
            management_driver_available: Some(true),
            observation_revision: Some(7),
            expires_at_ms: Some(200),
        };
        assert!(discovery.is_authoritative_for_configuration_at(150));
        assert!(!discovery.is_authoritative_for_configuration_at(201));
        discovery.source_authority = Some(RadioObservationAuthority::Simulation);
        assert!(!discovery.is_authoritative_for_configuration_at(150));
    }

    #[test]
    fn measured_topology_rejects_diagnostic_and_simulated_records() {
        let neighbor = RadioNeighborObservation {
            peer_id: "peer-2".into(),
            peer_ip: Some("10.1.0.3".into()),
            rssi_dbm: Some(-60.0),
            snr_db: Some(20.0),
            tx_rate_mbps: Some(10.0),
            rx_rate_mbps: Some(9.0),
            observed_at_ms: Some(100),
            source_authority: Some(RadioObservationAuthority::ChudAuthoritative),
        };
        let mut observation = RadioDeviceObservation {
            schema_version: RADIO_DEVICE_OBSERVATION_SCHEMA_VERSION,
            observed_at_ms: 100,
            source: NodeId::from("chud/radio/1"),
            status: RadioDeviceStatus::Online,
            simulated: false,
            management_ip: Some("10.1.0.2".into()),
            identity: None,
            effective: RadioEffectiveState::default(),
            neighbors: vec![neighbor],
            error: None,
            source_authority: Some(RadioObservationAuthority::ChudAuthoritative),
            management_lifecycle: Some(RadioManagementLifecycle::Connected),
            management_driver_available: Some(true),
            observation_revision: Some(1),
            expires_at_ms: Some(200),
        };
        assert_eq!(observation.authoritative_neighbors_at(150).len(), 1);
        observation.simulated = true;
        assert!(observation.authoritative_neighbors_at(150).is_empty());

        let compatibility = observation.v1_compatibility_record();
        let encoded = serde_json::to_value(compatibility).unwrap();
        assert_eq!(encoded["schema_version"], 1);
        assert!(encoded.get("source_authority").is_none());
        assert!(encoded["neighbors"][0].get("observed_at_ms").is_none());
    }

    #[test]
    fn two_hundred_radios_with_one_factory_ip_keep_distinct_identities() {
        let identities = (0_u16..200)
            .map(|index| {
                let mac = format!("00:1e:3f:20:{:02x}:{:02x}", index / 256, index % 256);
                (mac, "10.1.0.2".to_owned())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            identities
                .iter()
                .map(|(mac, _)| mac)
                .collect::<BTreeSet<_>>()
                .len(),
            200
        );
        assert_eq!(
            identities
                .iter()
                .map(|(_, management_ip)| management_ip)
                .collect::<BTreeSet<_>>()
                .len(),
            1
        );
    }
}
