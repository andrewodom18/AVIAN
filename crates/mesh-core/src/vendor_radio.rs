//! Vendor-neutral radio contracts.
//!
//! Existing StreamCaster contracts remain stable while new radio families use
//! this boundary. Vendor adapters normalize hardware data into these types;
//! ARC remains the authority for desired configuration and activation policy.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use thiserror::Error;

use crate::NodeId;

pub const RADIO_DEVICE_SCHEMA_VERSION: u16 = 1;
pub const RADIO_DISCOVERY_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RadioDiscoveryPolicy {
    pub max_age_ms: u64,
    pub max_future_skew_ms: u64,
    pub max_entries: usize,
}

impl Default for RadioDiscoveryPolicy {
    fn default() -> Self {
        Self {
            max_age_ms: 15_000,
            max_future_skew_ms: 5_000,
            max_entries: 1_024,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RadioObservationFreshness {
    Fresh,
    Stale,
    Future,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
        if self.observed_at_ms == 0 {
            return Err(VendorRadioError::InvalidObservationTimestamp);
        }
        validate_token("model_hint", &self.model_hint)?;
        if self.mac_address.trim().is_empty() {
            return Err(VendorRadioError::MissingMacAddress);
        }
        validate_mac(&self.mac_address)?;
        if self.management_endpoints.is_empty() {
            return Err(VendorRadioError::MissingManagementEndpoints);
        }
        for endpoint in &self.management_endpoints {
            if endpoint.address.trim().is_empty() || endpoint.port == 0 {
                return Err(VendorRadioError::InvalidManagementEndpoint);
            }
            validate_ip("management_endpoint", &endpoint.address)?;
        }
        if self.discovery_methods.is_empty() {
            return Err(VendorRadioError::MissingDiscoveryMethods);
        }
        if let Some(code) = self.error_code.as_deref() {
            validate_public_error_code(code)?;
        }
        Ok(())
    }

    pub fn freshness_at(
        &self,
        now_ms: u64,
        policy: RadioDiscoveryPolicy,
    ) -> RadioObservationFreshness {
        if self.observed_at_ms > now_ms.saturating_add(policy.max_future_skew_ms) {
            RadioObservationFreshness::Future
        } else if now_ms.saturating_sub(self.observed_at_ms) > policy.max_age_ms {
            RadioObservationFreshness::Stale
        } else {
            RadioObservationFreshness::Fresh
        }
    }

    pub fn is_current_live(&self, now_ms: u64, policy: RadioDiscoveryPolicy) -> bool {
        self.freshness_at(now_ms, policy) == RadioObservationFreshness::Fresh
            && self.reachability == RadioReachabilityStatus::Reachable
    }
}

pub fn normalize_radio_mac(value: &str) -> Result<String, VendorRadioError> {
    if value.trim() != value {
        return Err(VendorRadioError::InvalidMacAddress(value.to_owned()));
    }
    let compact = if value.contains(':') {
        normalize_mac_groups(value, ':', 6, 2)
    } else if value.contains('-') {
        normalize_mac_groups(value, '-', 6, 2)
    } else if value.contains('.') {
        normalize_mac_groups(value, '.', 3, 4)
    } else if value.len() == 12 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(value.to_owned())
    } else {
        None
    }
    .ok_or_else(|| VendorRadioError::InvalidMacAddress(value.to_owned()))?;
    Ok(compact
        .as_bytes()
        .chunks(2)
        .map(|chunk| std::str::from_utf8(chunk).expect("ASCII hex was validated"))
        .collect::<Vec<_>>()
        .join(":")
        .to_ascii_lowercase())
}

fn normalize_mac_groups(
    value: &str,
    separator: char,
    expected_groups: usize,
    group_length: usize,
) -> Option<String> {
    let groups = value.split(separator).collect::<Vec<_>>();
    (groups.len() == expected_groups
        && groups.iter().all(|group| {
            group.len() == group_length && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        }))
    .then(|| groups.concat())
}

pub fn stable_radio_source(
    vendor: &RadioVendorId,
    mac_address: &str,
) -> Result<NodeId, VendorRadioError> {
    let compact = normalize_radio_mac(mac_address)?.replace(':', "");
    Ok(NodeId::from(format!("radio/{}/{compact}", vendor.as_str())))
}

/// Reduce one discovery snapshot without introducing a second fleet store.
///
/// Newer evidence always wins, including a newer unreachable observation. For
/// equal timestamps, stronger direct evidence wins. Stale and future-skewed
/// records are omitted, and the result is bounded and deterministically sorted.
pub fn reduce_radio_discoveries(
    observations: impl IntoIterator<Item = RadioDiscoveryObservation>,
    now_ms: u64,
    policy: RadioDiscoveryPolicy,
) -> Result<Vec<RadioDiscoveryObservation>, VendorRadioError> {
    if policy.max_age_ms == 0 || policy.max_entries == 0 {
        return Err(VendorRadioError::InvalidDiscoveryPolicy);
    }
    let mut latest = BTreeMap::<(RadioVendorId, String), RadioDiscoveryObservation>::new();
    for mut observation in observations {
        observation.mac_address = normalize_radio_mac(&observation.mac_address)?;
        observation.source = stable_radio_source(&observation.vendor, &observation.mac_address)?;
        observation.validate()?;
        if observation.freshness_at(now_ms, policy) != RadioObservationFreshness::Fresh {
            continue;
        }
        let key = (observation.vendor.clone(), observation.mac_address.clone());
        let replace = latest
            .get(&key)
            .is_none_or(|current| discovery_precedes(current, &observation));
        if replace {
            latest.insert(key, observation);
        }
    }

    let mut reduced = latest.into_values().collect::<Vec<_>>();
    reduced.sort_by(|left, right| {
        right
            .observed_at_ms
            .cmp(&left.observed_at_ms)
            .then_with(|| left.mac_address.cmp(&right.mac_address))
    });
    reduced.truncate(policy.max_entries);
    reduced.sort_by(|left, right| left.mac_address.cmp(&right.mac_address));
    Ok(reduced)
}

fn discovery_precedes(
    current: &RadioDiscoveryObservation,
    candidate: &RadioDiscoveryObservation,
) -> bool {
    candidate.observed_at_ms > current.observed_at_ms
        || (candidate.observed_at_ms == current.observed_at_ms
            && discovery_evidence_key(candidate)
                .cmp(&discovery_evidence_key(current))
                .is_gt())
}

fn discovery_evidence_key(
    observation: &RadioDiscoveryObservation,
) -> (u8, u8, u8, usize, usize, String) {
    let reachability = u8::from(observation.reachability == RadioReachabilityStatus::Reachable);
    let authentication = match observation.management_authentication {
        RadioManagementAuthentication::Unknown => 0,
        RadioManagementAuthentication::Rejected => 1,
        RadioManagementAuthentication::ClientCertificateRequired => 2,
        RadioManagementAuthentication::Authenticated => 3,
    };
    let method_score = observation
        .discovery_methods
        .iter()
        .map(|method| match method {
            RadioDiscoveryMethod::Oui => 1_u8,
            RadioDiscoveryMethod::NeighborTable => 2,
            RadioDiscoveryMethod::Dhcp => 3,
            RadioDiscoveryMethod::Mdns => 4,
            RadioDiscoveryMethod::TcpReachability => 5,
            RadioDiscoveryMethod::TlsFingerprint => 6,
        })
        .max()
        .unwrap_or_default();
    let unique_methods = observation
        .discovery_methods
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .len();
    let deterministic_tie_break = serde_json::to_string(observation).unwrap_or_default();
    (
        reachability,
        authentication,
        method_score,
        unique_methods,
        observation.management_endpoints.len(),
        deterministic_tie_break,
    )
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
        if let Some(code) = self.error.as_deref() {
            validate_public_error_code(code)?;
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
    #[error("radio discovery policy must have a positive age and entry bound")]
    InvalidDiscoveryPolicy,
    #[error("radio observation error code is not safe for publication")]
    UnsafeErrorCode,
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
    normalize_radio_mac(value).map(|_| ())
}

fn validate_public_error_code(value: &str) -> Result<(), VendorRadioError> {
    let lower = value.to_ascii_lowercase();
    let has_sensitive_marker = [
        "password",
        "private_key",
        "credential",
        "session",
        "secret",
        "token",
    ]
    .iter()
    .any(|marker| lower.contains(marker));
    let is_public_token = !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'));
    if has_sensitive_marker || !is_public_token {
        return Err(VendorRadioError::UnsafeErrorCode);
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
    use super::*;

    fn valid_discovery(observed_at_ms: u64, mac_address: &str) -> RadioDiscoveryObservation {
        let vendor = RadioVendorId::trellisware();
        RadioDiscoveryObservation {
            schema_version: RADIO_DISCOVERY_SCHEMA_VERSION,
            observed_at_ms,
            source: stable_radio_source(&vendor, mac_address).unwrap(),
            vendor,
            model_hint: "tw-950".into(),
            mac_address: mac_address.into(),
            serial_number: None,
            hostname: None,
            reachability: RadioReachabilityStatus::Reachable,
            management_authentication: RadioManagementAuthentication::Unknown,
            management_endpoints: vec![RadioManagementEndpoint {
                address: "10.1.0.2".into(),
                port: 443,
                interface: None,
                interface_index: None,
            }],
            discovery_methods: vec![RadioDiscoveryMethod::NeighborTable],
            error_code: None,
        }
    }

    fn valid_device_observation() -> RadioDeviceObservation {
        RadioDeviceObservation {
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
        }
    }

    #[test]
    fn vendor_ids_are_extensible_but_safe_for_topics_and_records() {
        assert_eq!(RadioVendorId::microhard().as_str(), "microhard");
        assert_eq!(RadioVendorId::trellisware().as_str(), "trellisware");
        assert!(RadioVendorId::new("future-radio_1").is_ok());
        assert!(RadioVendorId::new("future radio").is_err());
        assert!(serde_json::from_str::<RadioVendorId>(r#""future radio""#).is_err());
    }

    #[test]
    fn mac_variants_normalize_to_one_physical_identity() {
        let variants = [
            "00:1E:3F:20:9A:10",
            "00-1e-3f-20-9a-10",
            "001e.3f20.9a10",
            "001e3f209a10",
        ];
        for variant in variants {
            assert_eq!(normalize_radio_mac(variant).unwrap(), "00:1e:3f:20:9a:10");
            assert_eq!(
                stable_radio_source(&RadioVendorId::trellisware(), variant).unwrap(),
                NodeId::from("radio/trellisware/001e3f209a10")
            );
        }
        for malformed in [
            "0:01e:3f:20:9a:10",
            "00:1e-3f:20:9a:10",
            "001e.3f20.9a1",
            "001e3f209a1z",
        ] {
            assert!(normalize_radio_mac(malformed).is_err());
        }
    }

    #[test]
    fn reducer_stabilizes_source_and_keeps_vendor_namespaces_distinct() {
        let mut trellisware = valid_discovery(10_000, "00-1E-3F-20-9A-10");
        trellisware.source = NodeId::from("unstable-neighbor-source");
        let mut microhard = trellisware.clone();
        microhard.vendor = RadioVendorId::microhard();
        microhard.model_hint = "pmddl2450".into();

        let reduced = reduce_radio_discoveries(
            [trellisware, microhard],
            10_000,
            RadioDiscoveryPolicy::default(),
        )
        .unwrap();
        assert_eq!(reduced.len(), 2);
        assert_eq!(
            reduced[0].source,
            stable_radio_source(&reduced[0].vendor, &reduced[0].mac_address).unwrap()
        );
        assert_eq!(
            reduced[1].source,
            stable_radio_source(&reduced[1].vendor, &reduced[1].mac_address).unwrap()
        );
    }

    #[test]
    fn reducer_prefers_newest_evidence_even_when_radio_became_unreachable() {
        let mut older = valid_discovery(9_000, "00:1e:3f:20:9a:10");
        older.management_authentication = RadioManagementAuthentication::Authenticated;
        let mut newer = valid_discovery(9_500, "001e.3f20.9a10");
        newer.reachability = RadioReachabilityStatus::Unreachable;

        let reduced = reduce_radio_discoveries(
            [older.clone(), newer.clone()],
            10_000,
            RadioDiscoveryPolicy::default(),
        )
        .unwrap();
        assert_eq!(reduced.len(), 1);
        assert_eq!(reduced[0].observed_at_ms, 9_500);
        assert_eq!(reduced[0].mac_address, "00:1e:3f:20:9a:10");
        assert!(!reduced[0].is_current_live(10_000, RadioDiscoveryPolicy::default()));

        let reverse_order =
            reduce_radio_discoveries([newer, older], 10_000, RadioDiscoveryPolicy::default())
                .unwrap();
        assert_eq!(reverse_order.len(), 1);
        assert_eq!(reverse_order[0].observed_at_ms, 9_500);
        assert_eq!(
            reverse_order[0].reachability,
            RadioReachabilityStatus::Unreachable
        );
    }

    #[test]
    fn reducer_uses_stronger_evidence_for_equal_timestamps() {
        let weak = valid_discovery(10_000, "00:1e:3f:20:9a:10");
        let mut strong = weak.clone();
        strong.management_authentication = RadioManagementAuthentication::Authenticated;
        strong
            .discovery_methods
            .push(RadioDiscoveryMethod::TlsFingerprint);

        let reduced = reduce_radio_discoveries(
            [weak.clone(), strong.clone()],
            10_000,
            RadioDiscoveryPolicy::default(),
        )
        .unwrap();
        assert_eq!(
            reduced[0].management_authentication,
            RadioManagementAuthentication::Authenticated
        );

        let reverse_order =
            reduce_radio_discoveries([strong, weak], 10_000, RadioDiscoveryPolicy::default())
                .unwrap();
        assert_eq!(
            reverse_order[0].management_authentication,
            RadioManagementAuthentication::Authenticated
        );
    }

    #[test]
    fn reducer_omits_stale_and_future_observations_and_enforces_bound() {
        let policy = RadioDiscoveryPolicy {
            max_age_ms: 1_000,
            max_future_skew_ms: 100,
            max_entries: 2,
        };
        let observations = [
            valid_discovery(8_999, "00:1e:3f:20:9a:01"),
            valid_discovery(9_100, "00:1e:3f:20:9a:02"),
            valid_discovery(9_200, "00:1e:3f:20:9a:03"),
            valid_discovery(9_300, "00:1e:3f:20:9a:04"),
            valid_discovery(10_101, "00:1e:3f:20:9a:05"),
        ];
        let reduced = reduce_radio_discoveries(observations, 10_000, policy).unwrap();
        assert_eq!(
            reduced
                .iter()
                .map(|observation| observation.mac_address.as_str())
                .collect::<Vec<_>>(),
            ["00:1e:3f:20:9a:03", "00:1e:3f:20:9a:04"]
        );
    }

    #[test]
    fn reducer_rejects_unbounded_or_timeless_policy() {
        assert_eq!(
            reduce_radio_discoveries(
                std::iter::empty(),
                10_000,
                RadioDiscoveryPolicy {
                    max_age_ms: 0,
                    ..RadioDiscoveryPolicy::default()
                }
            ),
            Err(VendorRadioError::InvalidDiscoveryPolicy)
        );
        assert_eq!(
            reduce_radio_discoveries(
                std::iter::empty(),
                10_000,
                RadioDiscoveryPolicy {
                    max_entries: 0,
                    ..RadioDiscoveryPolicy::default()
                }
            ),
            Err(VendorRadioError::InvalidDiscoveryPolicy)
        );
    }

    #[test]
    fn observation_contracts_reject_secret_fields_and_sensitive_error_text() {
        let discovery = valid_discovery(1, "00:1e:3f:20:9a:10");
        let mut discovery_json = serde_json::to_value(&discovery).unwrap();
        discovery_json["private_key"] = serde_json::json!("must-not-enter-observation");
        assert!(serde_json::from_value::<RadioDiscoveryObservation>(discovery_json).is_err());

        let mut nested_discovery_json = serde_json::to_value(&discovery).unwrap();
        nested_discovery_json["management_endpoints"][0]["credential"] =
            serde_json::json!("must-not-enter-endpoint");
        assert!(
            serde_json::from_value::<RadioDiscoveryObservation>(nested_discovery_json).is_err()
        );

        let mut device_json = serde_json::to_value(valid_device_observation()).unwrap();
        device_json["credential"] = serde_json::json!("must-not-enter-observation");
        assert!(serde_json::from_value::<RadioDeviceObservation>(device_json).is_err());

        let mut nested_device_json = serde_json::to_value(valid_device_observation()).unwrap();
        nested_device_json["identity"]["private_key"] =
            serde_json::json!("must-not-enter-identity");
        assert!(serde_json::from_value::<RadioDeviceObservation>(nested_device_json).is_err());

        let mut unsafe_discovery = discovery;
        unsafe_discovery.error_code = Some("password_rejected".into());
        assert_eq!(
            unsafe_discovery.validate(),
            Err(VendorRadioError::UnsafeErrorCode)
        );

        unsafe_discovery.error_code = Some("password rejected: hunter2".into());
        let error = unsafe_discovery.validate().unwrap_err();
        assert_eq!(error, VendorRadioError::UnsafeErrorCode);
        assert!(!error.to_string().contains("hunter2"));

        for invalid_code in ["", "not public"] {
            unsafe_discovery.error_code = Some(invalid_code.into());
            assert_eq!(
                unsafe_discovery.validate(),
                Err(VendorRadioError::UnsafeErrorCode)
            );
        }
        unsafe_discovery.error_code = Some("a".repeat(97));
        assert_eq!(
            unsafe_discovery.validate(),
            Err(VendorRadioError::UnsafeErrorCode)
        );
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
    fn discovery_record_accepts_ipv4_and_ipv6_management_endpoints() {
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
            management_authentication: RadioManagementAuthentication::Unknown,
            management_endpoints: vec![
                RadioManagementEndpoint {
                    address: "10.1.0.2".into(),
                    port: 443,
                    interface: Some("Ethernet 2".into()),
                    interface_index: Some(6),
                },
                RadioManagementEndpoint {
                    address: "fe80::21e:3fff:fe20:9a10".into(),
                    port: 443,
                    interface: Some("Ethernet 2".into()),
                    interface_index: Some(6),
                },
            ],
            discovery_methods: vec![RadioDiscoveryMethod::NeighborTable],
            error_code: None,
        };

        discovery.validate().unwrap();
    }

    #[test]
    fn discovery_record_rejects_invalid_timestamp_mac_and_endpoint_address() {
        let mut discovery = RadioDiscoveryObservation {
            schema_version: RADIO_DISCOVERY_SCHEMA_VERSION,
            observed_at_ms: 0,
            source: NodeId::from("radio/trellisware/001e3f209a10"),
            vendor: RadioVendorId::trellisware(),
            model_hint: "tw-950".into(),
            mac_address: "00:1e:3f:20:9a:10".into(),
            serial_number: None,
            hostname: None,
            reachability: RadioReachabilityStatus::Reachable,
            management_authentication: RadioManagementAuthentication::Unknown,
            management_endpoints: vec![RadioManagementEndpoint {
                address: "10.1.0.2".into(),
                port: 443,
                interface: None,
                interface_index: None,
            }],
            discovery_methods: vec![RadioDiscoveryMethod::NeighborTable],
            error_code: None,
        };

        assert_eq!(
            discovery.validate(),
            Err(VendorRadioError::InvalidObservationTimestamp)
        );
        discovery.observed_at_ms = 1;
        discovery.mac_address = "not-a-mac".into();
        assert!(matches!(
            discovery.validate(),
            Err(VendorRadioError::InvalidMacAddress(_))
        ));
        discovery.mac_address = "00:1e:3f:20:9a:10".into();
        discovery.management_endpoints[0].address = "10.1.0.999".into();
        assert!(matches!(
            discovery.validate(),
            Err(VendorRadioError::InvalidIpAddress {
                field: "management_endpoint",
                ..
            })
        ));
    }

    #[test]
    fn device_observation_rejects_invalid_identity_and_telemetry() {
        let mut observation = valid_device_observation();
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

    #[test]
    fn device_observation_rejects_invalid_network_and_neighbor_evidence() {
        let mut observation = valid_device_observation();
        observation.management_ip = Some("not-an-ip".into());
        assert!(matches!(
            observation.validate(),
            Err(VendorRadioError::InvalidIpAddress {
                field: "management_ip",
                ..
            })
        ));

        observation.management_ip = Some("fe80::21e:3fff:fe20:9a10".into());
        observation.neighbors = vec![
            RadioNeighborObservation {
                peer_id: "peer-1".into(),
                peer_ip: Some("10.1.0.3".into()),
                rssi_dbm: Some(-60.0),
                snr_db: Some(18.0),
                tx_rate_mbps: Some(5.0),
                rx_rate_mbps: Some(5.0),
            },
            RadioNeighborObservation {
                peer_id: "peer-1".into(),
                peer_ip: Some("fe80::21e:3fff:fe17:abf0".into()),
                rssi_dbm: Some(-61.0),
                snr_db: Some(17.0),
                tx_rate_mbps: Some(4.0),
                rx_rate_mbps: Some(4.0),
            },
        ];
        assert_eq!(
            observation.validate(),
            Err(VendorRadioError::DuplicatePeerId("peer-1".into()))
        );

        observation.neighbors.pop();
        observation.neighbors[0].peer_ip = Some("invalid".into());
        assert!(matches!(
            observation.validate(),
            Err(VendorRadioError::InvalidIpAddress {
                field: "peer_ip",
                ..
            })
        ));
        observation.neighbors[0].peer_ip = Some("10.1.0.3".into());
        observation.neighbors[0].tx_rate_mbps = Some(0.0);
        assert_eq!(
            observation.validate(),
            Err(VendorRadioError::NonPositive {
                field: "neighbor_tx_rate_mbps"
            })
        );
    }
}
