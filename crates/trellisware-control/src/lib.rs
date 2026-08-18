//! Read-only TrellisWare TW-950 management boundary for AVIAN.
//!
//! The TW-950 exposes an HTTPS `/agent/` endpoint using TNC encapsulation.
//! This crate reads identity and effective state only. Device writes are
//! deliberately absent because TW writes take effect immediately and the
//! deployed firmware/certificate/recovery procedure must be bench-verified.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use mesh_core::{
    NodeId, RadioCapabilities, RadioChannelCapability, RadioDeviceObservation, RadioDeviceStatus,
    RadioEffectiveState, RadioEvidenceLevel, RadioFrequencyRange, RadioIdentity,
    RadioManagementInterface, RadioNetworkMode, RadioVendorId, RADIO_DEVICE_SCHEMA_VERSION,
};
use p12_keystore::{KeyStore, Pkcs12ImportPolicy};
use reqwest::{Certificate, Identity};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use zeroize::Zeroizing;

pub const READ_PATHS: [&str; 9] = [
    "device/id",
    "device/model_number",
    "device/software/version",
    "device/operational_mode",
    "device/battery_level",
    "device/unified/configuration/active_preset/name",
    "identification/serial_number",
    "identification/alias",
    "tww/configuration/transmit_power_override_mw",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrellisWareModel {
    Tw950,
    Unknown,
}

impl TrellisWareModel {
    pub fn from_product_name(value: &str) -> Self {
        let normalized = value
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if normalized.contains("tw950") {
            Self::Tw950
        } else {
            Self::Unknown
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Tw950 => "tw-950",
            Self::Unknown => "unknown",
        }
    }

    /// Published planning evidence from the manufacturer data sheet. Device
    /// readback and local spectrum authorization remain authoritative.
    pub fn published_capabilities(self, observed_at_ms: u64) -> Option<RadioCapabilities> {
        if self != Self::Tw950 {
            return None;
        }
        Some(RadioCapabilities {
            schema_version: RADIO_DEVICE_SCHEMA_VERSION,
            observed_at_ms,
            vendor: RadioVendorId::trellisware(),
            model: self.id().to_owned(),
            firmware_version: None,
            evidence: RadioEvidenceLevel::Published,
            frequency_ranges: vec![
                RadioFrequencyRange {
                    minimum_mhz: 225.0,
                    maximum_mhz: 450.0,
                },
                RadioFrequencyRange {
                    minimum_mhz: 698.0,
                    maximum_mhz: 970.0,
                },
                RadioFrequencyRange {
                    minimum_mhz: 1_250.0,
                    maximum_mhz: 2_600.0,
                },
            ],
            channels: [1.2, 3.6, 10.0, 20.0, 40.0]
                .into_iter()
                .map(|bandwidth_mhz| RadioChannelCapability {
                    bandwidth_mhz,
                    measured_throughput_mbps: None,
                    receiver_sensitivity_dbm: None,
                })
                .collect(),
            network_modes: vec![RadioNetworkMode::Mesh],
            management_interfaces: vec![
                RadioManagementInterface::WebUi,
                RadioManagementInterface::VendorApi,
            ],
            maximum_total_transmit_power_dbm: Some(33.0),
            antenna_port_count: Some(1),
        })
    }
}

#[async_trait]
pub trait TrellisWareReadTransport: Send + Sync {
    async fn read(&self, paths: &[&str]) -> Result<BTreeMap<String, Value>, TrellisWareError>;
}

#[derive(Debug, Clone)]
pub struct TrellisWareReader<T> {
    transport: T,
}

impl<T> TrellisWareReader<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }
}

impl<T: TrellisWareReadTransport> TrellisWareReader<T> {
    pub async fn read_observation(
        &self,
        source: NodeId,
        management_ip: Option<String>,
        observed_at_ms: u64,
        simulated: bool,
    ) -> Result<RadioDeviceObservation, TrellisWareError> {
        let values = self.transport.read(&READ_PATHS).await?;
        let id = string_value(&values, "device/id")
            .ok_or(TrellisWareError::MissingField("device/id"))?;
        let reported_model = string_value(&values, "device/model_number").unwrap_or("TW-950");
        let model = TrellisWareModel::from_product_name(reported_model);
        let transmit_power_dbm =
            number_value(&values, "tww/configuration/transmit_power_override_mw")
                .filter(|milliwatts| *milliwatts > 0.0)
                .map(|milliwatts| 10.0 * milliwatts.log10());
        let battery_percent = number_value(&values, "device/battery_level")
            .filter(|value| (0.0..=100.0).contains(value))
            .map(|value| value.round() as u8);
        let network_mode = string_value(&values, "device/operational_mode")
            .filter(|mode| !mode.eq_ignore_ascii_case("off"))
            .map(|_| RadioNetworkMode::Mesh);

        Ok(RadioDeviceObservation {
            schema_version: RADIO_DEVICE_SCHEMA_VERSION,
            observed_at_ms,
            source,
            status: RadioDeviceStatus::Online,
            simulated,
            management_ip,
            identity: Some(RadioIdentity {
                vendor: RadioVendorId::trellisware(),
                model: model.id().to_owned(),
                serial_number: string_value(&values, "identification/serial_number")
                    .map(str::to_owned),
                firmware_version: string_value(&values, "device/software/version")
                    .map(str::to_owned),
                mac_address: Some(id.to_owned()),
                system_name: string_value(&values, "identification/alias").map(str::to_owned),
            }),
            effective: RadioEffectiveState {
                network_mode,
                transmit_power_dbm,
                battery_percent,
                active_profile: string_value(
                    &values,
                    "device/unified/configuration/active_preset/name",
                )
                .map(str::to_owned),
                ..RadioEffectiveState::default()
            },
            // HTTPS does not expose trustworthy RF-neighbor telemetry on the
            // lab contract. PEAT/IP reachability remains a separate overlay.
            neighbors: Vec::new(),
            error: None,
        })
    }
}

fn string_value<'a>(values: &'a BTreeMap<String, Value>, path: &str) -> Option<&'a str> {
    values.get(path)?.as_str()
}

fn number_value(values: &BTreeMap<String, Value>, path: &str) -> Option<f64> {
    let value = values.get(path)?;
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

#[derive(Debug, Clone)]
pub struct SimulatedTrellisWareTransport {
    values: BTreeMap<String, Value>,
}

impl SimulatedTrellisWareTransport {
    pub fn new(values: BTreeMap<String, Value>) -> Self {
        Self { values }
    }
}

#[async_trait]
impl TrellisWareReadTransport for SimulatedTrellisWareTransport {
    async fn read(&self, paths: &[&str]) -> Result<BTreeMap<String, Value>, TrellisWareError> {
        Ok(paths
            .iter()
            .filter_map(|path| {
                self.values
                    .get(*path)
                    .cloned()
                    .map(|value| ((*path).to_owned(), value))
            })
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct HttpsTncAgentTransport {
    base_url: String,
    client: reqwest::Client,
}

/// In-memory client identity input for the read-only management connection.
///
/// PKCS#12 passwords are borrowed so callers can keep them in a zeroizing
/// container. This type intentionally does not implement `Debug` because both
/// variants contain private-key material.
pub enum ClientIdentity<'a> {
    Pem(&'a [u8]),
    Pkcs12 { der: &'a [u8], password: &'a str },
}

impl HttpsTncAgentTransport {
    pub fn new(
        base_url: impl Into<String>,
        client_identity_pem: Option<&[u8]>,
        ca_certificate_pem: Option<&[u8]>,
        accept_invalid_server_certificate: bool,
    ) -> Result<Self, TrellisWareError> {
        Self::new_with_identity(
            base_url,
            client_identity_pem.map(ClientIdentity::Pem),
            ca_certificate_pem,
            accept_invalid_server_certificate,
        )
    }

    pub fn new_with_identity(
        base_url: impl Into<String>,
        client_identity: Option<ClientIdentity<'_>>,
        ca_certificate_pem: Option<&[u8]>,
        accept_invalid_server_certificate: bool,
    ) -> Result<Self, TrellisWareError> {
        let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(15));
        if let Some(identity) = client_identity {
            builder = builder.identity(parse_client_identity(identity)?);
        }
        if let Some(pem) = ca_certificate_pem {
            builder = builder
                .add_root_certificate(Certificate::from_pem(pem).map_err(TrellisWareError::Tls)?);
        }
        if accept_invalid_server_certificate {
            builder = builder.danger_accept_invalid_certs(true);
        }
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        if !(base_url.starts_with("https://")
            || base_url.starts_with("http://127.0.0.1")
            || base_url.starts_with("http://localhost"))
        {
            return Err(TrellisWareError::InvalidUrl);
        }
        Ok(Self {
            base_url,
            client: builder.build().map_err(TrellisWareError::Tls)?,
        })
    }
}

fn parse_client_identity(identity: ClientIdentity<'_>) -> Result<Identity, TrellisWareError> {
    match identity {
        ClientIdentity::Pem(pem) => {
            Identity::from_pem(pem).map_err(|_| TrellisWareError::InvalidClientIdentity)
        }
        ClientIdentity::Pkcs12 { der, password } => {
            let store = KeyStore::from_pkcs12(der, password, Pkcs12ImportPolicy::Strict)
                .map_err(|_| TrellisWareError::InvalidClientIdentity)?;
            let (_, chain) = store
                .private_key_chain()
                .ok_or(TrellisWareError::InvalidClientIdentity)?;
            if chain.certs().is_empty() {
                return Err(TrellisWareError::InvalidClientIdentity);
            }

            let mut pem = Zeroizing::new(Vec::new());
            for certificate in chain.certs() {
                append_pem_block(&mut pem, "CERTIFICATE", certificate.as_der());
            }
            append_pem_block(&mut pem, "PRIVATE KEY", chain.key().as_der());
            Identity::from_pem(&pem).map_err(|_| TrellisWareError::InvalidClientIdentity)
        }
    }
}

fn append_pem_block(output: &mut Vec<u8>, label: &str, der: &[u8]) {
    output.extend_from_slice(format!("-----BEGIN {label}-----\n").as_bytes());
    let encoded = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(der));
    for chunk in encoded.as_bytes().chunks(64) {
        output.extend_from_slice(chunk);
        output.push(b'\n');
    }
    output.extend_from_slice(format!("-----END {label}-----\n").as_bytes());
}

#[derive(Debug, Serialize)]
struct AgentRequest {
    id: u8,
    url: &'static str,
    ssl_credentials: &'static str,
    method: &'static str,
    body: String,
}

#[derive(Debug, Serialize)]
struct EncapsulationBody<'a> {
    protocol_version: &'static str,
    stump: &'static str,
    immediate: bool,
    partial: bool,
    requests: &'a [&'a str],
}

#[derive(Debug, Deserialize)]
struct AgentResponse {
    http_status: u16,
    body: String,
}

#[derive(Debug, Deserialize)]
struct EncapsulationResponse {
    result: EncapsulationResult,
    #[serde(default)]
    errors: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct EncapsulationResult {
    #[serde(default)]
    data: BTreeMap<String, Value>,
}

#[async_trait]
impl TrellisWareReadTransport for HttpsTncAgentTransport {
    async fn read(&self, paths: &[&str]) -> Result<BTreeMap<String, Value>, TrellisWareError> {
        let inner = serde_json::to_string(&EncapsulationBody {
            protocol_version: "0.5",
            stump: "TNC",
            immediate: true,
            partial: true,
            requests: paths,
        })
        .map_err(TrellisWareError::Json)?;
        let request = [AgentRequest {
            id: 0,
            url: "https://localhost/encapsulation/",
            ssl_credentials: "default",
            method: "POST",
            body: inner,
        }];
        let response = self
            .client
            .post(format!("{}/agent/", self.base_url))
            .json(&request)
            .send()
            .await
            .map_err(TrellisWareError::Http)?;
        if !response.status().is_success() {
            return Err(TrellisWareError::HttpStatus(response.status().as_u16()));
        }
        let outer: Vec<AgentResponse> = response.json().await.map_err(TrellisWareError::Http)?;
        let first = outer.first().ok_or(TrellisWareError::EmptyResponse)?;
        if first.http_status >= 400 {
            return Err(TrellisWareError::DeviceStatus(first.http_status));
        }
        let decoded: EncapsulationResponse =
            serde_json::from_str(&first.body).map_err(TrellisWareError::Json)?;
        if !decoded.errors.is_empty() && decoded.result.data.is_empty() {
            return Err(TrellisWareError::AgentPaths(decoded.errors));
        }
        Ok(decoded.result.data)
    }
}

#[derive(Debug, Error)]
pub enum TrellisWareError {
    #[error("TW-950 management URL must use HTTPS (HTTP is permitted only for loopback tests)")]
    InvalidUrl,
    #[error("TW-950 TLS configuration failed: {0}")]
    Tls(reqwest::Error),
    #[error("TW-950 client identity is invalid or its password was rejected")]
    InvalidClientIdentity,
    #[error("TW-950 HTTPS request failed: {0}")]
    Http(reqwest::Error),
    #[error("TW-950 agent returned HTTP {0}")]
    HttpStatus(u16),
    #[error("TW-950 device returned HTTP {0}")]
    DeviceStatus(u16),
    #[error("TW-950 returned an empty agent response")]
    EmptyResponse,
    #[error("TW-950 agent rejected requested paths: {0:?}")]
    AgentPaths(Vec<String>),
    #[error("TW-950 response JSON was invalid: {0}")]
    Json(serde_json::Error),
    #[error("TW-950 response is missing required field {0}")]
    MissingField(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;
    use p12_keystore::{
        Certificate as P12Certificate, EncryptionAlgorithm, KeyStoreEntry, PrivateKey,
        PrivateKeyChain,
    };
    use rcgen::{generate_simple_self_signed, CertifiedKey};
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn fixture() -> BTreeMap<String, Value> {
        serde_json::from_value(serde_json::json!({
            "device/id": "00:1e:3f:21:4e:d0",
            "device/model_number": "TW-950",
            "device/software/version": "TSP-1.1.0",
            "device/operational_mode": "operator",
            "device/battery_level": 87,
            "device/unified/configuration/active_preset/name": "TSM 20 MHz",
            "identification/serial_number": "303337",
            "identification/alias": "TW-Lab-1",
            "tww/configuration/transmit_power_override_mw": 2000
        }))
        .unwrap()
    }

    #[test]
    fn published_profile_includes_twenty_mhz_and_tsm_scale() {
        let profile = TrellisWareModel::Tw950.published_capabilities(10).unwrap();
        assert!(profile
            .channels
            .iter()
            .any(|channel| channel.bandwidth_mhz == 20.0));
        assert_eq!(profile.maximum_total_transmit_power_dbm, Some(33.0));
        profile.validate().unwrap();
    }

    fn generated_pkcs12(password: &str, algorithm: EncryptionAlgorithm) -> Vec<u8> {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(["localhost".to_owned()]).unwrap();
        let key = PrivateKey::from_der(&signing_key.serialize_der()).unwrap();
        let certificate = P12Certificate::from_der(cert.der().as_ref()).unwrap();
        let chain = PrivateKeyChain::new("test-key", key, [certificate]);
        let mut store = KeyStore::new();
        store.add_entry("client", KeyStoreEntry::PrivateKeyChain(chain));
        store
            .writer(password)
            .encryption_algorithm(algorithm)
            .write()
            .unwrap()
    }

    #[test]
    fn accepts_blank_password_modern_and_legacy_pkcs12_without_switching_tls_backend() {
        for algorithm in [
            EncryptionAlgorithm::PbeWithHmacSha256AndAes256,
            EncryptionAlgorithm::PbeWithShaAnd3KeyTripleDesCbc,
        ] {
            let der = generated_pkcs12("", algorithm);
            HttpsTncAgentTransport::new_with_identity(
                "http://127.0.0.1",
                Some(ClientIdentity::Pkcs12 {
                    der: &der,
                    password: "",
                }),
                None,
                false,
            )
            .unwrap();
        }
    }

    #[test]
    fn pkcs12_password_failures_do_not_echo_the_password() {
        let der = generated_pkcs12(
            "correct-password",
            EncryptionAlgorithm::PbeWithHmacSha256AndAes256,
        );
        let error = HttpsTncAgentTransport::new_with_identity(
            "http://127.0.0.1",
            Some(ClientIdentity::Pkcs12 {
                der: &der,
                password: "secret-wrong-password",
            }),
            None,
            false,
        )
        .unwrap_err();
        assert!(matches!(error, TrellisWareError::InvalidClientIdentity));
        assert!(!error.to_string().contains("secret-wrong-password"));
    }

    #[tokio::test]
    async fn normalizes_tw950_identity_and_read_only_state() {
        let observation = TrellisWareReader::new(SimulatedTrellisWareTransport::new(fixture()))
            .read_observation(
                NodeId::from("tw-ground-1"),
                Some("10.1.0.11".into()),
                1000,
                true,
            )
            .await
            .unwrap();
        let identity = observation.identity.unwrap();
        assert_eq!(identity.vendor, RadioVendorId::trellisware());
        assert_eq!(identity.model, "tw-950");
        assert_eq!(identity.system_name.as_deref(), Some("TW-Lab-1"));
        assert_eq!(observation.effective.battery_percent, Some(87));
        assert!((observation.effective.transmit_power_dbm.unwrap() - 33.0103).abs() < 0.001);
        assert!(observation.neighbors.is_empty());
    }

    #[test]
    fn refuses_cleartext_non_loopback_management() {
        assert!(matches!(
            HttpsTncAgentTransport::new("http://10.1.0.11", None, None, false),
            Err(TrellisWareError::InvalidUrl)
        ));
    }

    #[tokio::test]
    async fn live_transport_uses_agent_encapsulation_and_parses_device_data() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 4096];
            loop {
                let count = stream.read(&mut chunk).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                let Some(header_end) = request.windows(4).position(|value| value == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            let request_text = String::from_utf8_lossy(&request);
            assert!(request_text.starts_with("POST /agent/ HTTP/1.1"));
            assert!(request_text.contains("\\\"protocol_version\\\":\\\"0.5\\\""));
            assert!(request_text.contains("device/id"));

            let inner = serde_json::json!({
                "result": {"data": fixture(), "protocol_version": "0.5"},
                "protocol_version": "0.5", "errors": []
            })
            .to_string();
            let body =
                serde_json::json!([{"id": 0, "http_status": 200, "body": inner}]).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });

        let transport = HttpsTncAgentTransport::new(
            format!("http://127.0.0.1:{}", address.port()),
            None,
            None,
            false,
        )
        .unwrap();
        let observation = TrellisWareReader::new(transport)
            .read_observation(NodeId::from("tw-ground-1"), None, 1, false)
            .await
            .unwrap();
        server.join().unwrap();
        assert_eq!(observation.identity.unwrap().model, "tw-950");
        assert!(!observation.simulated);
    }
}
