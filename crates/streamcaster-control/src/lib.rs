//! StreamCaster management boundary for AVIAN.
//!
//! The live HTTP client intentionally implements read-only inspection. The
//! write trait is implemented by the simulator only until representative
//! 4200/4400/5200 radios prove every vendor write, reconnect, and persistence
//! behavior on the bench.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use mesh_core::{
    ArcRadioConfiguration, ChannelBandwidthMhz, FleetActivationMechanism,
    StreamCasterActivationGates, StreamCasterCapabilities, StreamCasterControlError,
    StreamCasterDeviceAssignment, StreamCasterEffectiveSettings, StreamCasterFrequencyProfile,
    TransmitPowerMode,
};
use reqwest::header::{COOKIE, SET_COOKIE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::Mutex;

const API_PATH: &str = "streamscape_api";

#[derive(Clone)]
pub enum StreamCasterAuth {
    None,
    /// Vendor login method and positional parameters are configurable because
    /// deployed firmware families may expose different login method names.
    PasswordRpc {
        method: String,
        username: String,
        password: String,
    },
}

impl fmt::Debug for StreamCasterAuth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("None"),
            Self::PasswordRpc {
                method, username, ..
            } => formatter
                .debug_struct("PasswordRpc")
                .field("method", method)
                .field("username", username)
                .field("password", &"[REDACTED]")
                .finish(),
        }
    }
}

#[derive(Clone)]
pub struct StreamCasterClient {
    endpoint: String,
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
        let base_url = base_url.trim_end_matches('/');
        let parsed = reqwest::Url::parse(base_url)
            .map_err(|error| StreamCasterError::InvalidEndpoint(error.to_string()))?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(StreamCasterError::InvalidEndpoint(base_url.to_owned()));
        }
        let endpoint = format!("{base_url}/{API_PATH}");
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(3))
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(StreamCasterError::Transport)?;
        Ok(Self {
            endpoint,
            http,
            auth,
            cookie: Arc::new(Mutex::new(None)),
            auth_lock: Arc::new(Mutex::new(())),
            request_id: Arc::new(AtomicU64::new(1)),
        })
    }

    pub async fn rpc(&self, method: &str, params: Vec<String>) -> Result<Value, StreamCasterError> {
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

    async fn authenticate(&self) -> Result<(), StreamCasterError> {
        let StreamCasterAuth::PasswordRpc {
            method,
            username,
            password,
        } = &self.auth
        else {
            return Err(StreamCasterError::Unauthorized);
        };
        *self.cookie.lock().await = None;
        let response = self
            .send_rpc(method, vec![username.clone(), password.clone()])
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(StreamCasterError::AuthenticationFailed(response.status));
        }
        response.clone().into_result()?;
        let cookie = response
            .set_cookie
            .as_deref()
            .and_then(cookie_pair)
            .ok_or(StreamCasterError::MissingSessionCookie)?;
        *self.cookie.lock().await = Some(cookie.to_owned());
        Ok(())
    }

    async fn send_rpc(
        &self,
        method: &str,
        params: Vec<String>,
    ) -> Result<RpcHttpResponse, StreamCasterError> {
        let request_id = self.request_id.fetch_add(1, Ordering::Relaxed).to_string();
        let payload = RpcRequest {
            jsonrpc: "2.0",
            method,
            params,
            id: &request_id,
        };
        let mut request = self.http.post(&self.endpoint).json(&payload);
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
        let body = response
            .bytes()
            .await
            .map_err(StreamCasterError::Transport)?;
        let rpc = if body.is_empty() {
            None
        } else {
            Some(serde_json::from_slice(&body).map_err(StreamCasterError::Decode)?)
        };
        Ok(RpcHttpResponse {
            status,
            set_cookie,
            rpc,
        })
    }
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    params: Vec<String>,
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
    set_cookie: Option<String>,
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
    let value = match value {
        Value::Array(mut values) if values.len() == 1 => values.remove(0),
        other => other,
    };
    if let Value::String(encoded) = &value {
        serde_json::from_str(encoded).unwrap_or(value)
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
        parse_capabilities(raw, observed_at_ms)
    }

    async fn read_effective_settings(
        &self,
        observed_at_ms: u64,
    ) -> Result<StreamCasterEffectiveSettings, StreamCasterError> {
        let raw = self.rpc("print_all_settings", vec![]).await?;
        parse_effective_settings(raw, observed_at_ms)
    }
}

fn parse_capabilities(
    raw: Value,
    observed_at_ms: u64,
) -> Result<StreamCasterCapabilities, StreamCasterError> {
    let root = raw.as_object().ok_or_else(|| {
        StreamCasterError::InvalidVendorShape(
            "supported_frequency_profiles must return an object".into(),
        )
    })?;
    let profiles = root
        .get("supported_frequency_profiles")
        .or_else(|| root.get("profiles"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            StreamCasterError::InvalidVendorShape(
                "supported_frequency_profiles array is missing".into(),
            )
        })?;
    let mut normalized = Vec::with_capacity(profiles.len());
    for profile in profiles {
        let profile = profile.as_object().ok_or_else(|| {
            StreamCasterError::InvalidVendorShape("frequency profile must be an object".into())
        })?;
        let center_frequency_mhz = number(profile, &["center_frequency_mhz", "freq", "frequency"])?;
        let bandwidth_mhz = bandwidth(value(profile, &["bandwidth_mhz", "bw", "bandwidth"])?)?;
        let antenna_mask = integer(profile, &["antenna_mask", "antennas"])?;
        normalized.push(StreamCasterFrequencyProfile {
            center_frequency_mhz,
            bandwidth_mhz,
            antenna_mask: u8::try_from(antenna_mask).map_err(|_| {
                StreamCasterError::InvalidVendorShape("antenna mask exceeds u8".into())
            })?,
        });
    }
    Ok(StreamCasterCapabilities {
        observed_at_ms,
        model: None,
        firmware_version: string_optional(root, &["firmware_version", "version"]),
        supported_frequency_profiles: normalized,
        scheduled_activation_supported: boolean_optional(root, &["scheduled_activation_supported"])
            .unwrap_or(false),
        dual_profile_supported: boolean_optional(root, &["dual_profile_supported"])
            .unwrap_or(false),
    })
}

fn parse_effective_settings(
    raw: Value,
    observed_at_ms: u64,
) -> Result<StreamCasterEffectiveSettings, StreamCasterError> {
    let root = raw.as_object().ok_or_else(|| {
        StreamCasterError::InvalidVendorShape("print_all_settings must return an object".into())
    })?;
    Ok(StreamCasterEffectiveSettings {
        observed_at_ms,
        node_id: integer_optional(root, &["nodeid", "node_id"])
            .map(u32::try_from)
            .transpose()
            .map_err(|_| StreamCasterError::InvalidVendorShape("node ID exceeds u32".into()))?,
        system_name: string_optional(root, &["system_name", "name"]),
        network_id: string(root, &["network_id", "networkid"])?,
        center_frequency_mhz: number(root, &["center_frequency_mhz", "freq", "frequency"])?,
        bandwidth_mhz: bandwidth(value(root, &["bandwidth_mhz", "bw", "bandwidth"])?)?,
        link_distance_m: u32::try_from(integer(root, &["link_distance_m", "link_distance"])?)
            .map_err(|_| {
                StreamCasterError::InvalidVendorShape("link distance exceeds u32".into())
            })?,
        antenna_mask: u8::try_from(integer(root, &["antenna_mask", "antennas"])?)
            .map_err(|_| StreamCasterError::InvalidVendorShape("antenna mask exceeds u8".into()))?,
        transmit_power_dbm_per_port: integer_optional(
            root,
            &["transmit_power_dbm_per_port", "power_dBm"],
        )
        .map(u8::try_from)
        .transpose()
        .map_err(|_| StreamCasterError::InvalidVendorShape("power exceeds u8".into()))?,
    })
}

fn value<'a>(
    map: &'a serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<&'a Value, StreamCasterError> {
    names
        .iter()
        .find_map(|name| map.get(*name))
        .ok_or_else(|| StreamCasterError::InvalidVendorShape(format!("missing {}", names[0])))
}

fn number(map: &serde_json::Map<String, Value>, names: &[&str]) -> Result<f64, StreamCasterError> {
    parse_number(value(map, names)?)
}

fn parse_number(value: &Value) -> Result<f64, StreamCasterError> {
    let parsed = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .filter(|number: &f64| number.is_finite())
        .ok_or_else(|| StreamCasterError::InvalidVendorShape("expected finite number".into()))?;
    Ok(parsed)
}

fn integer(map: &serde_json::Map<String, Value>, names: &[&str]) -> Result<u64, StreamCasterError> {
    integer_value(value(map, names)?)
}

fn integer_optional(map: &serde_json::Map<String, Value>, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| map.get(*name))
        .and_then(|value| integer_value(value).ok())
}

fn integer_value(value: &Value) -> Result<u64, StreamCasterError> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .ok_or_else(|| StreamCasterError::InvalidVendorShape("expected unsigned integer".into()))
}

fn string(
    map: &serde_json::Map<String, Value>,
    names: &[&str],
) -> Result<String, StreamCasterError> {
    string_optional(map, names)
        .ok_or_else(|| StreamCasterError::InvalidVendorShape(format!("missing {}", names[0])))
}

fn string_optional(map: &serde_json::Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| map.get(*name))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn boolean_optional(map: &serde_json::Map<String, Value>, names: &[&str]) -> Option<bool> {
    names
        .iter()
        .find_map(|name| map.get(*name))
        .and_then(Value::as_bool)
}

fn bandwidth(value: &Value) -> Result<ChannelBandwidthMhz, StreamCasterError> {
    let number = parse_number(value)?;
    match number {
        value if (value - 1.25).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz1_25),
        value if (value - 2.5).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz2_5),
        value if (value - 5.0).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz5),
        value if (value - 10.0).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz10),
        value if (value - 20.0).abs() < 0.001 => Ok(ChannelBandwidthMhz::Mhz20),
        _ => Err(StreamCasterError::InvalidVendorShape(format!(
            "unsupported bandwidth {number}"
        ))),
    }
}

#[async_trait]
pub trait SimulatedStreamCasterWriteApi: StreamCasterReadApi {
    async fn replace_effective_settings(
        &self,
        desired: StreamCasterEffectiveSettings,
    ) -> Result<(), StreamCasterError>;
    async fn restore_effective_settings(
        &self,
        snapshot: StreamCasterEffectiveSettings,
    ) -> Result<(), StreamCasterError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SimulatorFailure {
    None,
    Apply,
    VerifyDrift,
    Rollback,
}

#[derive(Clone)]
pub struct SimulatedStreamCaster {
    capabilities: Arc<Mutex<StreamCasterCapabilities>>,
    effective: Arc<Mutex<StreamCasterEffectiveSettings>>,
    failure: Arc<Mutex<SimulatorFailure>>,
}

impl SimulatedStreamCaster {
    pub fn new(
        capabilities: StreamCasterCapabilities,
        effective: StreamCasterEffectiveSettings,
    ) -> Self {
        Self {
            capabilities: Arc::new(Mutex::new(capabilities)),
            effective: Arc::new(Mutex::new(effective)),
            failure: Arc::new(Mutex::new(SimulatorFailure::None)),
        }
    }

    pub async fn set_failure(&self, failure: SimulatorFailure) {
        *self.failure.lock().await = failure;
    }
}

#[async_trait]
impl StreamCasterReadApi for SimulatedStreamCaster {
    async fn read_capabilities(
        &self,
        observed_at_ms: u64,
    ) -> Result<StreamCasterCapabilities, StreamCasterError> {
        let mut value = self.capabilities.lock().await.clone();
        value.observed_at_ms = observed_at_ms;
        Ok(value)
    }

    async fn read_effective_settings(
        &self,
        observed_at_ms: u64,
    ) -> Result<StreamCasterEffectiveSettings, StreamCasterError> {
        let mut value = self.effective.lock().await.clone();
        value.observed_at_ms = observed_at_ms;
        Ok(value)
    }
}

#[async_trait]
impl SimulatedStreamCasterWriteApi for SimulatedStreamCaster {
    async fn replace_effective_settings(
        &self,
        mut desired: StreamCasterEffectiveSettings,
    ) -> Result<(), StreamCasterError> {
        match *self.failure.lock().await {
            SimulatorFailure::Apply => return Err(StreamCasterError::SimulatedApplyFailure),
            SimulatorFailure::VerifyDrift => desired.network_id.push_str("-DRIFT"),
            _ => {}
        }
        *self.effective.lock().await = desired;
        Ok(())
    }

    async fn restore_effective_settings(
        &self,
        snapshot: StreamCasterEffectiveSettings,
    ) -> Result<(), StreamCasterError> {
        if *self.failure.lock().await == SimulatorFailure::Rollback {
            return Err(StreamCasterError::SimulatedRollbackFailure);
        }
        *self.effective.lock().await = snapshot;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedStreamCasterTransaction {
    pub generation: u64,
    pub snapshot: StreamCasterEffectiveSettings,
    pub desired: StreamCasterEffectiveSettings,
    pub gates: StreamCasterActivationGates,
}

pub struct StreamCasterTransactionEngine<A> {
    api: A,
}

impl<A> StreamCasterTransactionEngine<A> {
    pub fn new(api: A) -> Self {
        Self { api }
    }
}

impl<A: StreamCasterReadApi> StreamCasterTransactionEngine<A> {
    pub async fn prepare(
        &self,
        plan: &ArcRadioConfiguration,
        assignment: &StreamCasterDeviceAssignment,
        mut gates: StreamCasterActivationGates,
        observed_at_ms: u64,
    ) -> Result<PreparedStreamCasterTransaction, StreamCasterError> {
        assignment.validate()?;
        plan.assess()?;
        let group = plan
            .fleet
            .groups
            .iter()
            .find(|group| group.group_id == assignment.group_id)
            .ok_or_else(|| StreamCasterError::InvalidPlan("assigned group is missing".into()))?;
        if group.model != assignment.expected_model {
            return Err(StreamCasterError::InvalidPlan(
                "assignment model differs from fleet group".into(),
            ));
        }
        let capabilities = self.api.read_capabilities(observed_at_ms).await?;
        gates.live_capability_match = capabilities
            .supported_frequency_profiles
            .iter()
            .any(|profile| profile.supports(plan, group.antenna_mask));
        gates.scheduled_activation_supported = capabilities.scheduled_activation_supported;
        gates.hardware_apply_enabled = assignment.hardware_apply_enabled;
        gates.antenna_installation_resolved &= assignment
            .antenna_installation_profile_id
            .as_ref()
            .is_some_and(|value| !value.is_empty());
        // Credential references are deliberately not forwarded by ARC. The
        // sidecar resolves its protected local mount and supplies this gate.
        let snapshot = self.api.read_effective_settings(observed_at_ms).await?;
        gates.rollback_snapshot_staged = true;
        let desired = StreamCasterEffectiveSettings {
            observed_at_ms,
            node_id: snapshot.node_id,
            system_name: snapshot.system_name.clone(),
            network_id: plan.network.network_id.clone(),
            center_frequency_mhz: plan.network.center_frequency_mhz,
            bandwidth_mhz: plan.network.bandwidth_mhz,
            link_distance_m: plan
                .network
                .link_distance_m
                .unwrap_or_else(|| (plan.network.maximum_node_distance_m * 1.15).ceil() as u32),
            antenna_mask: group.antenna_mask,
            transmit_power_dbm_per_port: match group.transmit_power {
                TransmitPowerMode::MaxSupported => None,
                TransmitPowerMode::TargetDbm { dbm } => Some(dbm),
            },
        };
        Ok(PreparedStreamCasterTransaction {
            generation: plan.generation,
            snapshot,
            desired,
            gates,
        })
    }
}

impl<A: SimulatedStreamCasterWriteApi> StreamCasterTransactionEngine<A> {
    pub async fn apply_simulated(
        &self,
        prepared: PreparedStreamCasterTransaction,
        mechanism: FleetActivationMechanism,
        observed_at_ms: u64,
    ) -> Result<StreamCasterEffectiveSettings, StreamCasterError> {
        if !prepared.gates.ready_for_prepare() || !prepared.gates.supports(mechanism) {
            return Err(StreamCasterError::ActivationGatesFailed);
        }
        if let Err(error) = self
            .api
            .replace_effective_settings(prepared.desired.clone())
            .await
        {
            self.api
                .restore_effective_settings(prepared.snapshot)
                .await
                .map_err(|rollback| StreamCasterError::ApplyAndRollback {
                    apply: error.to_string(),
                    rollback: rollback.to_string(),
                })?;
            return Err(error);
        }
        let effective = self.api.read_effective_settings(observed_at_ms).await?;
        if !settings_match(&effective, &prepared.desired) {
            self.api
                .restore_effective_settings(prepared.snapshot)
                .await
                .map_err(|rollback| StreamCasterError::ApplyAndRollback {
                    apply: "verification drift".into(),
                    rollback: rollback.to_string(),
                })?;
            return Err(StreamCasterError::VerificationDrift {
                desired: serde_json::to_value(prepared.desired).unwrap_or(Value::Null),
                effective: serde_json::to_value(effective).unwrap_or(Value::Null),
            });
        }
        Ok(effective)
    }
}

fn settings_match(
    effective: &StreamCasterEffectiveSettings,
    desired: &StreamCasterEffectiveSettings,
) -> bool {
    effective.network_id == desired.network_id
        && (effective.center_frequency_mhz - desired.center_frequency_mhz).abs() < 0.001
        && effective.bandwidth_mhz == desired.bandwidth_mhz
        && effective.link_distance_m == desired.link_distance_m
        && effective.antenna_mask == desired.antenna_mask
        && effective.transmit_power_dbm_per_port == desired.transmit_power_dbm_per_port
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
        extract::State, http::StatusCode, response::IntoResponse, routing::post, Json, Router,
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

    async fn mock_rpc(
        State(state): State<MockState>,
        headers: axum::http::HeaderMap,
        Json(request): Json<Value>,
    ) -> impl IntoResponse {
        let method = request["method"].as_str().unwrap_or_default();
        if method == "login" {
            state.login_count.fetch_add(1, Ordering::SeqCst);
            return (
                StatusCode::OK,
                [(SET_COOKIE, "session=valid; Path=/")],
                Json(json!({"jsonrpc":"2.0","id":"1","result":""})),
            )
                .into_response();
        }
        if headers.get(COOKIE).and_then(|value| value.to_str().ok()) != Some("session=valid") {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        state.read_count.fetch_add(1, Ordering::SeqCst);
        let result = match method {
            "supported_frequency_profiles" => json!({
                "supported_frequency_profiles": [{
                    "freq": "2440",
                    "bw": "20",
                    "antenna_mask": 3
                }],
                "version": "5.0.1.12",
                "scheduled_activation_supported": false
            }),
            "print_all_settings" => json!({
                "nodeid": "17",
                "system_name": "AIR-017",
                "networkid": "ARC-RADIO",
                "freq": "2440",
                "bw": "20",
                "link_distance": "5750",
                "antenna_mask": "3",
                "power_dBm": "27"
            }),
            _ => Value::Null,
        };
        Json(json!({"jsonrpc":"2.0","id":"1","result":result})).into_response()
    }

    async fn mock_server() -> (String, MockState, tokio::task::JoinHandle<()>) {
        let state = MockState {
            login_count: Arc::new(AtomicUsize::new(0)),
            read_count: Arc::new(AtomicUsize::new(0)),
        };
        let app = Router::new()
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
            StreamCasterAuth::PasswordRpc {
                method: "login".into(),
                username: "admin".into(),
                password: "secret".into(),
            },
        )
        .unwrap();

        let capabilities = client.read_capabilities(100).await.unwrap();
        let effective = client.read_effective_settings(101).await.unwrap();

        assert_eq!(state.login_count.load(Ordering::SeqCst), 1);
        assert_eq!(state.read_count.load(Ordering::SeqCst), 2);
        assert_eq!(capabilities.supported_frequency_profiles.len(), 1);
        assert_eq!(effective.network_id, "ARC-RADIO");
        assert_eq!(effective.transmit_power_dbm_per_port, Some(27));
        task.abort();
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
        let auth = StreamCasterAuth::PasswordRpc {
            method: "login".into(),
            username: "admin".into(),
            password: "do-not-log-me".into(),
        };
        let debug = format!("{auth:?}");
        assert!(!debug.contains("do-not-log-me"));
        assert!(debug.contains("[REDACTED]"));
    }
}
