//! StreamCaster management boundary for AVIAN.
//!
//! This crate is read-only for physical hardware. Silvus mutation is owned by
//! the external radio-management API; only the in-process simulator can mutate
//! settings for isolated planning tests.

mod parsing;
mod simulator;

use parsing::*;
pub use simulator::{
    PreparedStreamCasterTransaction, SimulatedStreamCaster, SimulatedStreamCasterWriteApi,
    SimulatorFailure, StreamCasterTransactionEngine,
};

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use mesh_core::{
    ArcRadioConfiguration, ChannelBandwidthMhz, FleetActivationMechanism,
    StreamCasterActivationGates, StreamCasterCapabilities, StreamCasterControlError,
    StreamCasterDeviceAssignment, StreamCasterEffectiveSettings, StreamCasterFrequencyProfile,
    StreamCasterModel, StreamCasterRfLink, TransmitPowerMode,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;

const API_PATH: &str = "streamscape_api";
const LOGIN_PATH: &str = "login.sh";

#[derive(Clone)]
pub enum StreamCasterAuth {
    None,
    Password { username: String, password: String },
}

impl fmt::Debug for StreamCasterAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::Password { username, .. } => formatter
                .debug_struct("Password")
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
        }
    }
}

#[derive(Clone)]
pub struct StreamCasterClient {
    endpoint: reqwest::Url,
    login_endpoint: reqwest::Url,
    http: reqwest::Client,
    auth: StreamCasterAuth,
    cookie: Arc<Mutex<Option<String>>>,
    auth_lock: Arc<Mutex<()>>,
    request_id: Arc<AtomicU64>,
}

impl fmt::Debug for StreamCasterClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamCasterClient")
            .field("endpoint", &self.endpoint)
            .field("auth", &self.auth)
            .finish_non_exhaustive()
    }
}

impl StreamCasterClient {
    pub fn new(base_url: &str, auth: StreamCasterAuth) -> Result<Self, StreamCasterError> {
        let normalized = format!("{}/", base_url.trim_end_matches('/'));
        let parsed = reqwest::Url::parse(&normalized)
            .map_err(|error| StreamCasterError::InvalidEndpoint(error.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(StreamCasterError::InvalidEndpoint(base_url.to_owned()));
        }
        let endpoint = parsed
            .join(API_PATH)
            .map_err(|error| StreamCasterError::InvalidEndpoint(error.to_string()))?;
        let login_endpoint = parsed
            .join(LOGIN_PATH)
            .map_err(|error| StreamCasterError::InvalidEndpoint(error.to_string()))?;
        let allowed_origin = parsed.origin();
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::custom(move |attempt| {
                if attempt.previous().len() >= 3 || attempt.url().origin() != allowed_origin {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build()
            .map_err(StreamCasterError::Transport)?;
        Ok(Self {
            endpoint,
            login_endpoint,
            http,
            auth,
            cookie: Arc::new(Mutex::new(None)),
            auth_lock: Arc::new(Mutex::new(())),
            request_id: Arc::new(AtomicU64::new(1)),
        })
    }

    async fn rpc(&self, method: &str, params: Vec<String>) -> Result<Value, StreamCasterError> {
        self.rpc_params(
            method,
            Value::Array(params.into_iter().map(Value::String).collect()),
        )
        .await
    }

    async fn rpc_params(&self, method: &str, params: Value) -> Result<Value, StreamCasterError> {
        self.ensure_authenticated().await?;
        let first = self.send_rpc(method, params.clone()).await?;
        if !matches!(first.status, 401 | 403) {
            return first.into_result();
        }
        if matches!(self.auth, StreamCasterAuth::None) {
            return Err(StreamCasterError::Unauthorized);
        }

        // Exactly one caller refreshes the cookie. Every RPC retries at most
        // once, preventing bad credentials from creating an unbounded loop.
        let _guard = self.auth_lock.lock().await;
        self.authenticate().await?;
        let second = self.send_rpc(method, params).await?;
        if matches!(second.status, 401 | 403) {
            return Err(StreamCasterError::Unauthorized);
        }
        second.into_result()
    }

    /// Reads direct RF links only. Throughput probing is intentionally excluded
    /// from the periodic path because it can add traffic to the operational mesh.
    pub async fn read_rf_links(
        &self,
        local_node_id: Option<u32>,
        observed_at_ms: u64,
    ) -> Result<Vec<StreamCasterRfLink>, StreamCasterError> {
        let raw = self.rpc("network_status", vec![]).await?;
        let entries = raw.as_array().ok_or_else(|| {
            StreamCasterError::InvalidVendorShape("network_status must return an array".into())
        })?;
        if entries.len() % 3 != 0 {
            return Err(StreamCasterError::InvalidVendorShape(
                "network_status returned an incomplete link triple".into(),
            ));
        }
        let mut links = Vec::new();
        for triple in entries.chunks_exact(3).take(32) {
            let source_node_id = parse_u32(&triple[0], "network_status source")?;
            let target_node_id = parse_u32(&triple[1], "network_status target")?;
            let remote = match local_node_id {
                Some(local) if source_node_id == local => target_node_id,
                Some(local) if target_node_id == local => source_node_id,
                _ => target_node_id,
            };
            let parameter = remote.to_string();
            let (rssi, tx_mcs, rx_mcs) = tokio::join!(
                self.rpc("nbr_rssi", vec![parameter.clone()]),
                self.rpc("nbr_mcs", vec![parameter.clone()]),
                self.rpc("nbr_mcs_rx", vec![parameter]),
            );
            links.push(StreamCasterRfLink {
                source_node_id,
                target_node_id,
                snr_db: parse_number(&triple[2]).ok(),
                rssi_dbm: rssi
                    .ok()
                    .and_then(|value| parse_number_array(&value).ok())
                    .unwrap_or_default(),
                tx_mcs: tx_mcs.ok().and_then(|value| parse_u8_scalar(&value)),
                rx_mcs: rx_mcs.ok().and_then(|value| parse_u8_scalar(&value)),
                observed_at_ms,
            });
        }
        Ok(links)
    }

    /// Verifies only the non-secret enable/disable state. Key material is never
    /// requested or normalized into ARC status.
    pub async fn encryption_enabled(&self) -> Result<bool, StreamCasterError> {
        let value = self.rpc("enc_disable", vec![]).await?;
        let disabled = parse_u8_scalar(&value).ok_or_else(|| {
            StreamCasterError::InvalidVendorShape(
                "enc_disable did not return a scalar enable state".into(),
            )
        })?;
        Ok(disabled == 0)
    }

    async fn ensure_authenticated(&self) -> Result<(), StreamCasterError> {
        if matches!(self.auth, StreamCasterAuth::None) || self.cookie.lock().await.is_some() {
            return Ok(());
        }
        let _guard = self.auth_lock.lock().await;
        if self.cookie.lock().await.is_none() {
            self.authenticate().await?;
        }
        Ok(())
    }

    async fn authenticate(&self) -> Result<(), StreamCasterError> {
        let StreamCasterAuth::Password { username, password } = &self.auth else {
            return Err(StreamCasterError::Unauthorized);
        };
        *self.cookie.lock().await = None;
        let response = self
            .http
            .get(self.login_endpoint.clone())
            .query(&[
                ("username", username.as_str()),
                ("password", password.as_str()),
                ("Submit", "1"),
            ])
            .send()
            .await
            .map_err(|_| StreamCasterError::AuthenticationTransport)?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(StreamCasterError::AuthenticationFailed(status));
        }
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(cookie_pair)
            .ok_or(StreamCasterError::MissingSessionCookie)?;
        *self.cookie.lock().await = Some(cookie.to_owned());
        Ok(())
    }

    async fn send_rpc(
        &self,
        method: &str,
        params: Value,
    ) -> Result<RpcHttpResponse, StreamCasterError> {
        let request_id = self.request_id.fetch_add(1, Ordering::Relaxed).to_string();
        let payload = RpcRequest {
            jsonrpc: "2.0",
            method,
            params,
            id: &request_id,
        };
        let mut request = self.http.post(self.endpoint.clone()).json(&payload);
        if let Some(cookie) = self.cookie.lock().await.as_ref() {
            request = request.header(COOKIE, cookie);
        }
        let response = request.send().await.map_err(StreamCasterError::Transport)?;
        let status = response.status().as_u16();
        let set_cookie = response
            .headers()
            .get(SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if let Some(cookie) = set_cookie.as_deref().and_then(cookie_pair) {
            *self.cookie.lock().await = Some(cookie.to_owned());
        }
        let body = response
            .bytes()
            .await
            .map_err(StreamCasterError::Transport)?;
        let rpc = if body.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&body).map_err(StreamCasterError::Decode)?)
        };
        Ok(RpcHttpResponse { status, rpc })
    }
}

fn parse_u32(value: &Value, field: &str) -> Result<u32, StreamCasterError> {
    let parsed = integer_value(value)?;
    u32::try_from(parsed)
        .map_err(|_| StreamCasterError::InvalidVendorShape(format!("{field} exceeds u32")))
}

fn parse_u8_scalar(value: &Value) -> Option<u8> {
    let value = value
        .as_array()
        .and_then(|values| values.first())
        .unwrap_or(value);
    integer_value(value)
        .ok()
        .and_then(|value| u8::try_from(value).ok())
}

fn parse_number_array(value: &Value) -> Result<Vec<f64>, StreamCasterError> {
    value
        .as_array()
        .ok_or_else(|| StreamCasterError::InvalidVendorShape("expected number array".into()))?
        .iter()
        .map(parse_number)
        .collect()
}

fn cookie_pair(set_cookie: &str) -> Option<&str> {
    set_cookie
        .split(';')
        .next()
        .map(str::trim)
        .filter(|pair| pair.contains('=') && !pair.ends_with('='))
}

#[derive(Serialize)]
struct RpcRequest<'a> {
    jsonrpc: &'static str,
    method: &'a str,
    #[serde(skip_serializing_if = "Value::is_null")]
    params: Value,
    id: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
struct RpcResponse {
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Clone, Deserialize)]
struct RpcError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

#[derive(Clone)]
struct RpcHttpResponse {
    status: u16,
    rpc: Option<RpcResponse>,
}

impl RpcHttpResponse {
    fn into_result(self) -> Result<Value, StreamCasterError> {
        if !(200..300).contains(&self.status) {
            return Err(StreamCasterError::HttpStatus(self.status));
        }
        let rpc = self.rpc.ok_or(StreamCasterError::EmptyResponse)?;
        if let Some(error) = rpc.error {
            return Err(StreamCasterError::Rpc {
                code: error.code,
                message: error.message,
            });
        }
        Ok(unwrap_vendor_value(rpc.result))
    }
}

fn unwrap_vendor_value(value: Value) -> Value {
    if let Value::String(encoded) = &value {
        if encoded.trim_start().starts_with(['{', '[']) {
            serde_json::from_str(encoded).unwrap_or(value)
        } else {
            value
        }
    } else {
        value
    }
}

#[async_trait]
pub trait StreamCasterReadApi: Send + Sync {
    async fn read_capabilities(
        &self,
        observed_at_ms: u64,
    ) -> Result<StreamCasterCapabilities, StreamCasterError>;
    async fn read_effective_settings(
        &self,
        observed_at_ms: u64,
    ) -> Result<StreamCasterEffectiveSettings, StreamCasterError>;
}

#[async_trait]
impl StreamCasterReadApi for StreamCasterClient {
    async fn read_capabilities(
        &self,
        observed_at_ms: u64,
    ) -> Result<StreamCasterCapabilities, StreamCasterError> {
        let raw = self.rpc("supported_frequency_profiles", vec![]).await?;
        let version = self
            .rpc("version", vec![])
            .await
            .ok()
            .and_then(scalar_string);
        let model = self.rpc("model", vec![]).await.ok().and_then(scalar_string);
        parse_capabilities(raw, model.as_deref(), version, observed_at_ms)
    }

    async fn read_effective_settings(
        &self,
        observed_at_ms: u64,
    ) -> Result<StreamCasterEffectiveSettings, StreamCasterError> {
        let raw = self.rpc("print_all_settings", vec![]).await?;
        parse_effective_settings(raw, observed_at_ms)
    }
}

#[derive(Debug, Error)]
pub enum StreamCasterError {
    #[error("invalid StreamCaster endpoint: {0}")]
    InvalidEndpoint(String),
    #[error("StreamCaster HTTP transport: {0}")]
    Transport(#[source] reqwest::Error),
    #[error("StreamCaster response JSON: {0}")]
    Decode(#[source] serde_json::Error),
    #[error("StreamCaster returned HTTP {0}")]
    HttpStatus(u16),
    #[error("StreamCaster authentication failed with HTTP {0}")]
    AuthenticationFailed(u16),
    #[error("StreamCaster authentication transport failed")]
    AuthenticationTransport,
    #[error("StreamCaster session is unauthorized")]
    Unauthorized,
    #[error("StreamCaster login did not return a session cookie")]
    MissingSessionCookie,
    #[error("StreamCaster returned an empty response")]
    EmptyResponse,
    #[error("StreamCaster JSON-RPC error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("invalid StreamCaster vendor response: {0}")]
    InvalidVendorShape(String),
    #[error("invalid radio plan: {0}")]
    InvalidPlan(String),
    #[error("hardware activation gates are incomplete")]
    ActivationGatesFailed,
    #[error("scheduled StreamCaster activation has not been verified on this hardware")]
    ScheduledActivationUnverified,
    #[error("StreamCaster did not reconnect before the bounded timeout")]
    ReconnectTimeout,
    #[error("simulated StreamCaster apply failed")]
    SimulatedApplyFailure,
    #[error("simulated StreamCaster rollback failed")]
    SimulatedRollbackFailure,
    #[error("apply failed ({apply}) and rollback failed ({rollback})")]
    ApplyAndRollback { apply: String, rollback: String },
    #[error("effective settings drifted from desired settings")]
    VerificationDrift { desired: Value, effective: Value },
    #[error(transparent)]
    Control(#[from] StreamCasterControlError),
    #[error(transparent)]
    Plan(#[from] mesh_core::RadioConfigError),
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::{
        extract::State,
        http::StatusCode,
        response::IntoResponse,
        routing::{get, post},
        Json, Router,
    };
    use mesh_core::{
        RadioConfigAuthority, RadioFleetDefinition, RadioNodeGroup, RadioNodeRole,
        RadioRegulatoryProfile, RadioTrafficProfile, StreamCasterModel,
        StreamCasterNetworkSettings, STREAMCASTER_CONTROL_SCHEMA_VERSION,
    };
    use serde_json::json;

    use super::*;

    #[derive(Clone)]
    struct MockState {
        login_count: Arc<AtomicUsize>,
        read_count: Arc<AtomicUsize>,
    }

    async fn mock_login(State(state): State<MockState>) -> impl IntoResponse {
        state.login_count.fetch_add(1, Ordering::SeqCst);
        (
            StatusCode::OK,
            [(SET_COOKIE, "session=valid; Path=/")],
            "authenticated",
        )
    }

    async fn mock_rpc(
        State(state): State<MockState>,
        headers: axum::http::HeaderMap,
        Json(request): Json<Value>,
    ) -> impl IntoResponse {
        let method = request["method"].as_str().unwrap_or_default();
        let reads = state.read_count.load(Ordering::SeqCst);
        let expected_cookie = if reads == 0 {
            "session=valid"
        } else {
            "session=rolling"
        };
        if headers.get(COOKIE).and_then(|value| value.to_str().ok()) != Some(expected_cookie) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        state.read_count.fetch_add(1, Ordering::SeqCst);
        let result = match method {
            "supported_frequency_profiles" => json!([{
                "antenna_mask": "3",
                "bandwidth": "20",
                "frequencies": ["2440"]
            }]),
            "version" => json!(["5.0.1.12"]),
            "model" => json!(["SL5200"]),
            "print_all_settings" => json!([
                "nodeid",
                ["17"],
                "system_name",
                ["AIR-017"],
                "nw_name",
                ["ARC-RADIO"],
                "freq",
                ["2440"],
                "bw",
                ["20"],
                "max_link_distance",
                ["5750"],
                "tx_ant_mask",
                ["3"],
                "enable_max_power",
                ["0"],
                "power_dBm",
                ["27"],
                "enc_key",
                ["must-never-escape"]
            ]),
            "network_status" => json!([17, 42, 18.5]),
            "nbr_rssi" => json!([-61.0, -63.0]),
            "nbr_mcs" => json!([9]),
            "nbr_mcs_rx" => json!([8]),
            "enc_disable" => json!([0]),
            _ => Value::Null,
        };
        (
            [(SET_COOKIE, "session=rolling; Path=/")],
            Json(json!({"jsonrpc":"2.0","id":"1","result":result})),
        )
            .into_response()
    }

    async fn mock_server() -> (String, MockState, tokio::task::JoinHandle<()>) {
        let state = MockState {
            login_count: Arc::new(AtomicUsize::new(0)),
            read_count: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
            .route("/login.sh", get(mock_login))
            .route("/streamscape_api", post(mock_rpc))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{address}"), state, task)
    }

    fn plan() -> ArcRadioConfiguration {
        ArcRadioConfiguration {
            schema_version: mesh_core::RADIO_CONFIG_SCHEMA_VERSION,
            authority: RadioConfigAuthority::Arc,
            generation: 7,
            network: StreamCasterNetworkSettings {
                network_id: "ARC-RADIO".into(),
                center_frequency_mhz: 2_440.0,
                bandwidth_mhz: ChannelBandwidthMhz::Mhz20,
                average_node_distance_m: 2_000.0,
                maximum_node_distance_m: 5_000.0,
                link_distance_m: None,
                routing_beacon_period_ms: 500,
                encryption_required: true,
            },
            fleet: RadioFleetDefinition {
                total_nodes: 150,
                groups: vec![
                    RadioNodeGroup {
                        group_id: "air".into(),
                        node_id_prefix: "air".into(),
                        percentage: 98.0,
                        model: StreamCasterModel::Sl5200LiteEstimated,
                        role: RadioNodeRole::Airborne,
                        altitude_msl_ft: 10_000.0,
                        regulatory_profile: RadioRegulatoryProfile::LiveCapabilitiesRequired,
                        transmit_power: TransmitPowerMode::TargetDbm { dbm: 27 },
                        antenna_mask: 3,
                        beamforming: true,
                        estimated_installed_eirp_dbm: Some(34.44),
                        field_calibrated_udp_capacity_bps: None,
                    },
                    RadioNodeGroup {
                        group_id: "gcs".into(),
                        node_id_prefix: "gcs".into(),
                        percentage: 2.0,
                        model: StreamCasterModel::Sc4400,
                        role: RadioNodeRole::ControlStation,
                        altitude_msl_ft: 0.0,
                        regulatory_profile: RadioRegulatoryProfile::LiveCapabilitiesRequired,
                        transmit_power: TransmitPowerMode::MaxSupported,
                        antenna_mask: 15,
                        beamforming: true,
                        estimated_installed_eirp_dbm: Some(33.0),
                        field_calibrated_udp_capacity_bps: None,
                    },
                ],
            },
            traffic: RadioTrafficProfile::default(),
            persist_after_apply: false,
        }
    }

    fn assignment(enabled: bool) -> StreamCasterDeviceAssignment {
        StreamCasterDeviceAssignment {
            schema_version: STREAMCASTER_CONTROL_SCHEMA_VERSION,
            node_id: mesh_core::NodeId::from("air-017"),
            group_id: "air".into(),
            expected_model: StreamCasterModel::Sl5200LiteEstimated,
            management_interface: "eth1".into(),
            data_interface: "streamcaster0".into(),
            management_address: "192.168.169.11".into(),
            antenna_installation_profile_id: Some("airframe/u28-generic-v1".into()),
            credential_reference: Some("streamcaster/air-017.json".into()),
            hardware_apply_enabled: enabled,
        }
    }

    fn capabilities() -> StreamCasterCapabilities {
        StreamCasterCapabilities {
            observed_at_ms: 1,
            model: Some(StreamCasterModel::Sl5200LiteEstimated),
            firmware_version: Some("5.0.1.12".into()),
            supported_frequency_profiles: vec![StreamCasterFrequencyProfile {
                center_frequency_mhz: 2_440.0,
                bandwidth_mhz: ChannelBandwidthMhz::Mhz20,
                antenna_mask: 3,
            }],
            scheduled_activation_supported: false,
            dual_profile_supported: false,
        }
    }

    fn effective() -> StreamCasterEffectiveSettings {
        StreamCasterEffectiveSettings {
            observed_at_ms: 1,
            node_id: Some(17),
            system_name: Some("AIR-017".into()),
            network_id: "OLD".into(),
            center_frequency_mhz: 2_430.0,
            bandwidth_mhz: ChannelBandwidthMhz::Mhz10,
            link_distance_m: 4_000,
            antenna_mask: 3,
            max_power_enabled: Some(false),
            transmit_power_dbm_per_port: Some(24),
        }
    }

    fn base_gates() -> StreamCasterActivationGates {
        StreamCasterActivationGates {
            known_landed: true,
            regulatory_authorized: true,
            antenna_installation_resolved: true,
            credential_resolved: true,
            independent_management_reachable: true,
            preserves_control_bearer: true,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn live_client_refreshes_cookie_once_and_normalizes_read_only_calls() {
        let (base_url, state, task) = mock_server().await;
        let client = StreamCasterClient::new(
            &base_url,
            StreamCasterAuth::Password {
                username: "admin".into(),
                password: "secret".into(),
            },
        )
        .unwrap();

        let capabilities = client.read_capabilities(100).await.unwrap();
        let effective = client.read_effective_settings(101).await.unwrap();

        assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
        assert_eq!(state.read_count.load(Ordering::SeqCst), 4);
        assert_eq!(capabilities.supported_frequency_profiles.len(), 1);
        assert_eq!(effective.network_id, "ARC-RADIO");
        assert_eq!(effective.max_power_enabled, Some(false));
        assert_eq!(effective.transmit_power_dbm_per_port, Some(27));
        task.abort();
    }

    #[tokio::test]
    async fn live_client_normalizes_measured_rf_links_without_throughput_probes() {
        let (base_url, _state, task) = mock_server().await;
        let client = StreamCasterClient::new(
            &base_url,
            StreamCasterAuth::Password {
                username: "admin".into(),
                password: "secret".into(),
            },
        )
        .unwrap();

        let links = client.read_rf_links(Some(17), 200).await.unwrap();

        assert_eq!(links.len(), 1);
        assert_eq!(links[0].source_node_id, 17);
        assert_eq!(links[0].target_node_id, 42);
        assert_eq!(links[0].snr_db, Some(18.5));
        assert_eq!(links[0].rssi_dbm, vec![-61.0, -63.0]);
        assert_eq!(links[0].tx_mcs, Some(9));
        assert_eq!(links[0].rx_mcs, Some(8));
        task.abort();
    }

    #[tokio::test]
    async fn live_client_verifies_encryption_without_reading_key_material() {
        let (base_url, _state, task) = mock_server().await;
        let client = StreamCasterClient::new(
            &base_url,
            StreamCasterAuth::Password {
                username: "admin".into(),
                password: "secret".into(),
            },
        )
        .unwrap();

        assert!(client.encryption_enabled().await.unwrap());
        task.abort();
    }

    #[tokio::test]
    async fn preparation_blocks_a_live_model_outside_the_enrolled_family() {
        let mut wrong_capabilities = capabilities();
        wrong_capabilities.model = Some(StreamCasterModel::Sc4400);
        let simulator = SimulatedStreamCaster::new(wrong_capabilities, effective());
        let prepared = StreamCasterTransactionEngine::new(simulator)
            .prepare(&plan(), &assignment(true), base_gates(), 10)
            .await
            .unwrap();

        assert!(!prepared.gates.live_capability_match);
        assert!(!prepared.gates.ready_for_prepare());
    }

    #[tokio::test]
    async fn simulator_applies_and_verifies_only_with_complete_gates() {
        let simulator = SimulatedStreamCaster::new(capabilities(), effective());
        let engine = StreamCasterTransactionEngine::new(simulator.clone());
        let prepared = engine
            .prepare(&plan(), &assignment(true), base_gates(), 10)
            .await
            .unwrap();
        assert!(prepared.gates.ready_for_prepare());
        let applied = engine
            .apply_simulated(
                prepared,
                FleetActivationMechanism::IndependentManagement,
                11,
            )
            .await
            .unwrap();
        assert_eq!(applied.network_id, "ARC-RADIO");
        assert_eq!(applied.center_frequency_mhz, 2_440.0);
        assert_eq!(applied.link_distance_m, 5_750);
    }

    #[tokio::test]
    async fn simulator_rolls_back_when_verification_detects_drift() {
        let original = effective();
        let simulator = SimulatedStreamCaster::new(capabilities(), original.clone());
        simulator.set_failure(SimulatorFailure::VerifyDrift).await;
        let engine = StreamCasterTransactionEngine::new(simulator.clone());
        let prepared = engine
            .prepare(&plan(), &assignment(true), base_gates(), 10)
            .await
            .unwrap();
        let result = engine
            .apply_simulated(
                prepared,
                FleetActivationMechanism::IndependentManagement,
                11,
            )
            .await;
        assert!(matches!(
            result,
            Err(StreamCasterError::VerificationDrift { .. })
        ));
        assert_eq!(
            simulator
                .read_effective_settings(12)
                .await
                .unwrap()
                .network_id,
            original.network_id
        );
    }

    #[tokio::test]
    async fn hardware_apply_default_blocks_even_the_simulator() {
        let simulator = SimulatedStreamCaster::new(capabilities(), effective());
        let engine = StreamCasterTransactionEngine::new(simulator);
        let prepared = engine
            .prepare(&plan(), &assignment(false), base_gates(), 10)
            .await
            .unwrap();
        assert!(!prepared.gates.ready_for_prepare());
        assert!(matches!(
            engine
                .apply_simulated(
                    prepared,
                    FleetActivationMechanism::IndependentManagement,
                    11
                )
                .await,
            Err(StreamCasterError::ActivationGatesFailed)
        ));
    }

    #[test]
    fn debug_never_exposes_password() {
        let auth = StreamCasterAuth::Password {
            username: "admin".into(),
            password: "do-not-log-me".into(),
        };
        let debug = format!("{auth:?}");
        assert!(!debug.contains("do-not-log-me"));
        assert!(debug.contains("[REDACTED]"));
    }
}
