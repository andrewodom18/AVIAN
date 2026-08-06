//! Read-only Microhard radio management boundary for AVIAN.
//!
//! The public pMDDL documentation establishes an AT command surface, but it
//! does not establish a current, secure production transport for every model.
//! This crate therefore normalizes known read-only responses behind a transport
//! trait. Hardware writes remain intentionally unavailable until the exact
//! model, firmware manual, MIB, and recovery procedure are validated.

use std::collections::BTreeMap;

use async_trait::async_trait;
use mesh_core::{
    NodeId, RadioCapabilities, RadioChannelCapability, RadioDeviceObservation, RadioDeviceStatus,
    RadioEffectiveState, RadioEvidenceLevel, RadioFrequencyRange, RadioIdentity,
    RadioManagementInterface, RadioNetworkMode, RadioVendorId, RADIO_DEVICE_SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SYSTEM_SUMMARY_COMMAND: &str = "AT+MSSYSI";
pub const MODEM_RECORD_COMMAND: &str = "AT+MSGMR";
pub const RADIO_RSSI_COMMAND: &str = "AT+MWRSSI";
pub const TRANSMIT_POWER_COMMAND: &str = "AT+MWTXPOWER?";
pub const WIRELESS_TRAFFIC_COMMAND: &str = "AT+MSTR=0";

pub const READ_ONLY_COMMANDS: [&str; 5] = [
    SYSTEM_SUMMARY_COMMAND,
    MODEM_RECORD_COMMAND,
    RADIO_RSSI_COMMAND,
    TRANSMIT_POWER_COMMAND,
    WIRELESS_TRAFFIC_COMMAND,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MicrohardModel {
    Pmddl2460,
    Pmddl4000,
    Fddl1624,
    Fddl9324,
    Pmddl900,
    Unknown,
}

impl MicrohardModel {
    pub fn from_product_name(value: &str) -> Self {
        let normalized: String = value
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        if normalized.contains("pmddl2460") {
            Self::Pmddl2460
        } else if normalized.contains("pmddl4000") {
            Self::Pmddl4000
        } else if normalized.contains("fddl1624") {
            Self::Fddl1624
        } else if normalized.contains("fddl9324") {
            Self::Fddl9324
        } else if normalized.contains("pmddl900") {
            Self::Pmddl900
        } else {
            Self::Unknown
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Pmddl2460 => "pmddl2460",
            Self::Pmddl4000 => "pmddl4000",
            Self::Fddl1624 => "fddl1624",
            Self::Fddl9324 => "fddl9324",
            Self::Pmddl900 => "pmddl900",
            Self::Unknown => "unknown",
        }
    }

    /// Published profiles are planning evidence only. A device-reported
    /// capability read and regulatory authorization are required before apply.
    pub fn published_capabilities(self, observed_at_ms: u64) -> Option<RadioCapabilities> {
        let common_modes = vec![
            RadioNetworkMode::PointToPoint,
            RadioNetworkMode::PointToMultipoint,
            RadioNetworkMode::Relay,
            RadioNetworkMode::Mesh,
        ];
        let management_interfaces = vec![
            RadioManagementInterface::SerialConsole,
            RadioManagementInterface::WebUi,
            RadioManagementInterface::Telnet,
            RadioManagementInterface::Snmp,
        ];
        let (frequency_ranges, channels, maximum_total_transmit_power_dbm, antenna_port_count) =
            match self {
                Self::Pmddl2460 => (
                    vec![range(2_402.0, 2_478.0), range(5_000.0, 6_000.0)],
                    vec![
                        channel(4.0, Some(16.0), Some(-90.0)),
                        channel(5.0, None, None),
                        channel(8.0, Some(32.0), Some(-87.0)),
                        channel(10.0, None, None),
                        channel(20.0, None, None),
                        channel(40.0, None, None),
                    ],
                    Some(30.0),
                    Some(4),
                ),
                Self::Pmddl4000 => (
                    vec![range(3_200.0, 4_800.0)],
                    vec![
                        channel(4.0, Some(10.0), Some(-83.5)),
                        channel(8.0, Some(21.0), Some(-81.0)),
                        channel(18.0, Some(43.0), Some(-78.0)),
                    ],
                    Some(33.0),
                    Some(2),
                ),
                Self::Fddl1624 => (
                    vec![
                        range(1_625.0, 1_725.0),
                        range(1_780.0, 1_850.0),
                        range(2_020.0, 2_110.0),
                        range(2_200.0, 2_300.0),
                        range(2_300.0, 2_390.0),
                        range(2_400.0, 2_500.0),
                    ],
                    narrowband_channels(),
                    None,
                    Some(2),
                ),
                Self::Fddl9324 => (
                    vec![
                        range(902.0, 928.0),
                        range(1_350.0, 1_400.0),
                        range(1_625.0, 1_725.0),
                        range(1_780.0, 1_850.0),
                        range(2_020.0, 2_110.0),
                        range(2_200.0, 2_300.0),
                        range(2_300.0, 2_390.0),
                        range(2_400.0, 2_500.0),
                    ],
                    narrowband_channels(),
                    Some(30.0),
                    Some(2),
                ),
                Self::Pmddl900 => (
                    vec![range(902.0, 928.0)],
                    vec![
                        channel(4.0, Some(10.0), Some(-83.5)),
                        channel(8.0, Some(21.0), Some(-78.0)),
                    ],
                    Some(30.0),
                    Some(2),
                ),
                Self::Unknown => return None,
            };
        let capabilities = RadioCapabilities {
            schema_version: RADIO_DEVICE_SCHEMA_VERSION,
            observed_at_ms,
            vendor: RadioVendorId::microhard(),
            model: self.id().to_owned(),
            firmware_version: None,
            evidence: RadioEvidenceLevel::Published,
            frequency_ranges,
            channels,
            network_modes: common_modes,
            management_interfaces,
            maximum_total_transmit_power_dbm,
            antenna_port_count,
        };
        debug_assert!(capabilities.validate().is_ok());
        Some(capabilities)
    }
}

fn range(minimum_mhz: f64, maximum_mhz: f64) -> RadioFrequencyRange {
    RadioFrequencyRange {
        minimum_mhz,
        maximum_mhz,
    }
}

fn channel(
    bandwidth_mhz: f64,
    measured_throughput_mbps: Option<f64>,
    receiver_sensitivity_dbm: Option<f64>,
) -> RadioChannelCapability {
    RadioChannelCapability {
        bandwidth_mhz,
        measured_throughput_mbps,
        receiver_sensitivity_dbm,
    }
}

fn narrowband_channels() -> Vec<RadioChannelCapability> {
    vec![
        channel(1.0, None, None),
        channel(2.0, None, None),
        channel(4.0, Some(2.0), Some(-100.0)),
        channel(8.0, Some(21.0), Some(-78.0)),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MicrohardDiscoveryRecord {
    pub mac_address: String,
    pub ip_address: String,
    pub description: String,
    pub product_name: String,
    pub firmware_version: String,
    pub operating_mode: String,
    pub network_id: String,
}

#[async_trait]
pub trait MicrohardCommandTransport: Send + Sync {
    async fn query(&self, command: &str) -> Result<String, MicrohardError>;
}

#[derive(Debug, Clone)]
pub struct MicrohardReader<T> {
    transport: T,
}

impl<T> MicrohardReader<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

impl<T: MicrohardCommandTransport> MicrohardReader<T> {
    pub async fn read_observation(
        &self,
        source: NodeId,
        management_ip: Option<String>,
        observed_at_ms: u64,
        simulated: bool,
    ) -> Result<RadioDeviceObservation, MicrohardError> {
        let summary = self.transport.query(SYSTEM_SUMMARY_COMMAND).await?;
        let modem_record = self
            .transport
            .query(MODEM_RECORD_COMMAND)
            .await
            .unwrap_or_default();
        let fields = parse_labeled_fields(&format!("{summary}\n{modem_record}"));
        let product_name = first_field(&fields, &["product", "device"])
            .ok_or(MicrohardError::MissingField("product"))?;
        let model = MicrohardModel::from_product_name(product_name);
        let firmware_version = first_field(&fields, &["software", "firmware"]).map(str::to_owned);
        let identity = RadioIdentity {
            vendor: RadioVendorId::microhard(),
            model: model.id().to_owned(),
            serial_number: first_field(&fields, &["serial", "serial number"]).map(str::to_owned),
            firmware_version,
            mac_address: first_field(&fields, &["mac"]).map(str::to_owned),
        };

        let rssi = self
            .transport
            .query(RADIO_RSSI_COMMAND)
            .await
            .ok()
            .and_then(|value| parse_dbm(&value));
        let transmit_power_dbm = self
            .transport
            .query(TRANSMIT_POWER_COMMAND)
            .await
            .ok()
            .and_then(|value| parse_dbm(&value));
        let traffic = self
            .transport
            .query(WIRELESS_TRAFFIC_COMMAND)
            .await
            .ok()
            .map(|value| parse_labeled_fields(&value))
            .unwrap_or_default();

        Ok(RadioDeviceObservation {
            schema_version: RADIO_DEVICE_SCHEMA_VERSION,
            observed_at_ms,
            source,
            status: RadioDeviceStatus::Online,
            simulated,
            management_ip,
            identity: Some(identity),
            effective: RadioEffectiveState {
                transmit_power_dbm,
                reported_rssi_dbm: rssi,
                wireless_rx_bytes: first_field(&traffic, &["rx bytes"]).and_then(parse_human_bytes),
                wireless_tx_bytes: first_field(&traffic, &["tx bytes"]).and_then(parse_human_bytes),
                ..RadioEffectiveState::default()
            },
            neighbors: Vec::new(),
            error: None,
        })
    }
}

/// Deterministic transport for contract tests and ARC development without a
/// physical radio. It accepts only explicitly supplied command responses.
#[derive(Debug, Clone, Default)]
pub struct SimulatedMicrohardTransport {
    responses: BTreeMap<String, String>,
}

impl SimulatedMicrohardTransport {
    pub fn from_responses(responses: BTreeMap<String, String>) -> Self {
        Self { responses }
    }

    pub fn with_response(
        mut self,
        command: impl Into<String>,
        response: impl Into<String>,
    ) -> Self {
        self.responses.insert(command.into(), response.into());
        self
    }
}

#[async_trait]
impl MicrohardCommandTransport for SimulatedMicrohardTransport {
    async fn query(&self, command: &str) -> Result<String, MicrohardError> {
        self.responses
            .get(command)
            .cloned()
            .ok_or_else(|| MicrohardError::UnsupportedCommand(command.to_owned()))
    }
}

fn parse_labeled_fields(value: &str) -> BTreeMap<String, String> {
    value
        .lines()
        .filter_map(|line| {
            let (key, value) = line.trim().split_once(':')?;
            let key = key.trim().trim_start_matches('+').to_ascii_lowercase();
            let value = value.trim();
            (!key.is_empty() && !value.is_empty()).then(|| (key, value.to_owned()))
        })
        .collect()
}

fn first_field<'a>(fields: &'a BTreeMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| fields.get(*key).map(String::as_str))
}

fn parse_dbm(value: &str) -> Option<f64> {
    let end = value.to_ascii_lowercase().find("dbm")?;
    value[..end]
        .split(|character: char| {
            character.is_ascii_whitespace()
                || matches!(character, ':' | ',' | '[' | ']' | '(' | ')')
        })
        .filter_map(|token| token.parse::<f64>().ok())
        .next_back()
        .filter(|value| value.is_finite())
}

fn parse_human_bytes(value: &str) -> Option<u64> {
    let normalized = value.trim().to_ascii_uppercase();
    let split = normalized
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(normalized.len());
    let amount = normalized[..split].parse::<f64>().ok()?;
    let unit = normalized[split..].trim();
    let multiplier = match unit {
        "" | "B" => 1.0,
        "KB" | "KIB" => 1_024.0,
        "MB" | "MIB" => 1_048_576.0,
        "GB" | "GIB" => 1_073_741_824.0,
        _ => return None,
    };
    let bytes = amount * multiplier;
    (bytes.is_finite() && bytes >= 0.0 && bytes <= u64::MAX as f64).then_some(bytes as u64)
}

#[derive(Debug, Error)]
pub enum MicrohardError {
    #[error("Microhard transport failed: {0}")]
    Transport(String),
    #[error("Microhard command is unavailable in this transport: {0}")]
    UnsupportedCommand(String),
    #[error("Microhard response is missing required field {0}")]
    MissingField(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_model_names_without_punctuation_or_case_assumptions() {
        assert_eq!(
            MicrohardModel::from_product_name("Microhard pMDDL-2460 SWP"),
            MicrohardModel::Pmddl2460
        );
        assert_eq!(
            MicrohardModel::from_product_name("fDDL9324"),
            MicrohardModel::Fddl9324
        );
    }

    #[test]
    fn published_2460_profile_retains_twenty_mhz_support_without_inventing_throughput() {
        let profile = MicrohardModel::Pmddl2460
            .published_capabilities(100)
            .unwrap();
        let twenty = profile
            .channels
            .iter()
            .find(|channel| channel.bandwidth_mhz == 20.0)
            .unwrap();
        assert_eq!(twenty.measured_throughput_mbps, None);
        assert_eq!(profile.antenna_port_count, Some(4));
        profile.validate().unwrap();
    }

    #[tokio::test]
    async fn simulator_normalizes_documented_read_only_at_responses() {
        let transport = SimulatedMicrohardTransport::default()
            .with_response(
                SYSTEM_SUMMARY_COMMAND,
                "MAC : 00:0F:92:04:1A:E0\nProduct : pMDDL2460\nSoftware : v1.4.0 build 1013-1\nOK",
            )
            .with_response(MODEM_RECORD_COMMAND, "Hardware : Rev A\nOK")
            .with_response(RADIO_RSSI_COMMAND, "+MWRSSI: [0] -74 dBm\nOK")
            .with_response(TRANSMIT_POWER_COMMAND, "+MWTXPOWER: 30 dBm\nOK")
            .with_response(
                WIRELESS_TRAFFIC_COMMAND,
                "WIFI RX packets: 408\nRX bytes : 57.301KB\nTX packets: 12\nTX bytes : 3KB\nOK",
            );

        let observation = MicrohardReader::new(transport)
            .read_observation(
                NodeId::from("air-001"),
                Some("192.168.168.1".into()),
                1_000,
                true,
            )
            .await
            .unwrap();

        let identity = observation.identity.unwrap();
        assert_eq!(identity.vendor, RadioVendorId::microhard());
        assert_eq!(identity.model, "pmddl2460");
        assert_eq!(identity.mac_address.as_deref(), Some("00:0F:92:04:1A:E0"));
        assert_eq!(observation.effective.reported_rssi_dbm, Some(-74.0));
        assert_eq!(observation.effective.transmit_power_dbm, Some(30.0));
        assert_eq!(observation.effective.wireless_tx_bytes, Some(3_072));
    }

    #[test]
    fn all_declared_commands_are_queries_and_never_persist_configuration() {
        assert!(READ_ONLY_COMMANDS.iter().all(|command| *command != "AT&W"));
        assert!(READ_ONLY_COMMANDS
            .iter()
            .all(|command| !command.contains('=') || *command == WIRELESS_TRAFFIC_COMMAND));
    }

    #[test]
    fn sample_observation_matches_the_vendor_neutral_contract() {
        let encoded = include_str!("../../../examples/microhard-observation.sample.json");
        let observation: RadioDeviceObservation = serde_json::from_str(encoded).unwrap();
        assert_eq!(observation.schema_version, RADIO_DEVICE_SCHEMA_VERSION);
        assert_eq!(
            observation.identity.unwrap().vendor,
            RadioVendorId::microhard()
        );
    }
}
