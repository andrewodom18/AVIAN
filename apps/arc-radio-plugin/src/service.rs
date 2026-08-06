use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use mesh_core::{
    ArcActivationAuthorization, ArcRadioConfiguration, DeliveryClass, MeshPayload, NodeId,
    StreamCasterActivationGates, StreamCasterApplyPhase, StreamCasterCapabilities,
    StreamCasterDeviceAssignment, StreamCasterEffectiveSettings, StreamCasterFrequencyProfile,
    StreamCasterMeshObservation, StreamCasterObservedNode, StreamCasterObservedPosition,
    StreamCasterObservedRadio, StreamCasterObservedStatus, StreamCasterOperationIntent,
    StreamCasterOperationRequest, StreamCasterOperationStatus, StreamCasterPeerLink,
    TransmitPowerMode, STREAMCASTER_CAPACITY_REQUIREMENT_NODES,
    STREAMCASTER_CONTROL_SCHEMA_VERSION, STREAMCASTER_MESH_OBSERVATION_SCHEMA_VERSION,
};
use mesh_peat::{AvianRecord, PeatNode, PeatNodeConfig, PeerDescriptor};
use serde::Deserialize;
use serde_json::json;
use streamcaster_control::{
    PreparedStreamCasterTransaction, SimulatedStreamCaster, StreamCasterAuth, StreamCasterClient,
    StreamCasterReadApi, StreamCasterTransactionEngine,
};
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

use super::Args;

const TOPIC_FLEET_PLAN: &str = "local/link/radio/streamcaster/fleet-plan/v1";
const TOPIC_DESIRED: &str = "local/link/radio/streamcaster/desired/v1";
const TOPIC_STATUS: &str = "local/link/radio/streamcaster/status/v1";
const TOPIC_EFFECTIVE_OBSERVATIONS: &str = "local/link/radio/streamcaster/observations/v1";
const TOPIC_MESH_OBSERVATIONS: &str = "local/link/radio/streamcaster/mesh-observations/v1";
const TOPIC_HEALTH: &str = "local/link/radio/streamcaster/plugin-health";
const TOPIC_TELEMETRY: &str = "local/telemetry";
const ARC_AUTHORIZATION_MAX_AGE_MS: u64 = 5 * 60 * 1_000;
const ARC_AUTHORIZATION_MAX_CLOCK_SKEW_MS: u64 = 5_000;
const POSITION_FRESH_MS: u64 = 30_000;
const PEAT_RECONNECT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    method: String,
    username: String,
    password: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegulatoryEvidence {
    authorized: bool,
    allowed_profiles: Vec<RegulatoryProfile>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegulatoryProfile {
    center_frequency_mhz: f64,
    bandwidth_mhz: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AntennaEvidence {
    profile_id: String,
    approved: bool,
    antenna_gain_dbi: f64,
    installation_loss_db: f64,
    calibrated_at_ms: u64,
}

#[derive(Clone)]
enum Backend {
    ContractOnly,
    LiveReadOnly(StreamCasterClient),
    Simulator(SimulatedStreamCaster),
}

struct ServiceState {
    source: NodeId,
    sequence: AtomicU64,
    backend: Backend,
    credential_resolved: bool,
    installation_evidence_dir: Option<PathBuf>,
    regulatory_evidence_file: Option<PathBuf>,
    prepared: Option<PreparedStreamCasterTransaction>,
    latest_status: Option<StreamCasterOperationStatus>,
    latest_plan_generation: Option<u64>,
    latest_mesh_observation: Option<serde_json::Value>,
    peat: Option<Arc<PeatNode>>,
    peat_peers: Vec<PeerDescriptor>,
    management_address: Option<String>,
    simulate_radio: bool,
    latest_position: Option<StreamCasterObservedPosition>,
}

pub async fn serve(args: Args) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .ok();
    if args.radio_url.is_some() && args.simulate_radio {
        bail!("--radio-url and --simulate-radio are mutually exclusive");
    }

    let auth = load_auth(args.credential_file.as_deref())?;
    let credential_resolved = !matches!(auth, StreamCasterAuth::None);
    let management_address = args.radio_url.as_deref().and_then(management_host);
    let backend = if args.simulate_radio {
        Backend::ContractOnly
    } else if let Some(url) = args.radio_url.as_deref() {
        Backend::LiveReadOnly(StreamCasterClient::new(url, auth)?)
    } else {
        Backend::ContractOnly
    };
    let (peat, peat_peers) = start_peat(&args).await?;

    let mut zenoh_config = zenoh::Config::default();
    zenoh_config
        .insert_json5("mode", r#""client""#)
        .map_err(|error| anyhow::anyhow!("zenoh mode: {error}"))?;
    zenoh_config
        .insert_json5(
            "connect/endpoints",
            &format!(r#"["{}"]"#, args.zenoh_endpoint),
        )
        .map_err(|error| anyhow::anyhow!("zenoh endpoint: {error}"))?;
    zenoh_config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| anyhow::anyhow!("zenoh multicast: {error}"))?;
    zenoh_config
        .insert_json5("scouting/gossip/enabled", "false")
        .map_err(|error| anyhow::anyhow!("zenoh gossip: {error}"))?;
    let session = zenoh::open(zenoh_config)
        .await
        .map_err(|error| anyhow::anyhow!("open Zenoh: {error}"))?;
    let desired_subscriber = session
        .declare_subscriber(TOPIC_DESIRED)
        .await
        .map_err(|error| anyhow::anyhow!("desired subscriber: {error}"))?;
    let telemetry_subscriber = session
        .declare_subscriber(TOPIC_TELEMETRY)
        .await
        .map_err(|error| anyhow::anyhow!("telemetry subscriber: {error}"))?;
    let health_queryable = session
        .declare_queryable(TOPIC_HEALTH)
        .await
        .map_err(|error| anyhow::anyhow!("health queryable: {error}"))?;

    let state = Arc::new(RwLock::new(ServiceState {
        source: NodeId::from(args.source),
        sequence: AtomicU64::new(1),
        backend,
        credential_resolved,
        installation_evidence_dir: args.installation_evidence_dir,
        regulatory_evidence_file: args.regulatory_evidence_file,
        prepared: None,
        latest_status: None,
        latest_plan_generation: None,
        latest_mesh_observation: None,
        peat: peat.clone(),
        peat_peers,
        management_address,
        simulate_radio: args.simulate_radio,
        latest_position: None,
    }));

    if args.simulate_radio {
        tracing::warn!("StreamCaster simulator enabled; no physical radio writes are possible");
    } else {
        tracing::info!("StreamCaster sidecar started with live writes disabled");
    }

    let mut peat_poll = tokio::time::interval(Duration::from_secs(2));
    let mut mesh_poll = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            sample = telemetry_subscriber.recv_async() => {
                let Ok(sample) = sample else { break; };
                if let Some(position) = observed_position(&sample.payload().to_bytes(), now_unix_ms()) {
                    state.write().await.latest_position = Some(position);
                }
            }
            sample = desired_subscriber.recv_async() => {
                let Ok(sample) = sample else { break; };
                let now_ms = now_unix_ms();
                let status = match serde_json::from_slice::<StreamCasterOperationRequest>(
                    &sample.payload().to_bytes(),
                ) {
                    Ok(request) => process_operation(&state, request, now_ms).await,
                    Err(error) => invalid_status(error.to_string(), now_ms),
                };
                {
                    state.write().await.latest_status = Some(status.clone());
                }
                let _ = session
                    .put(TOPIC_STATUS, serde_json::to_vec(&status).unwrap_or_default())
                    .await;
                if let Some(observation) = status.effective.as_ref() {
                    let _ = session
                        .put(
                            TOPIC_EFFECTIVE_OBSERVATIONS,
                            serde_json::to_vec(observation).unwrap_or_default(),
                        )
                        .await;
                }
            }
            query = health_queryable.recv_async() => {
                let Ok(query) = query else { break; };
                let health = {
                    let state = state.read().await;
                    let backend = match &state.backend {
                        Backend::ContractOnly => "contract_only",
                        Backend::LiveReadOnly(_) => "live_read_only",
                        Backend::Simulator(_) => "simulator",
                    };
                    json!({
                        "status": "healthy",
                        "schema_version": STREAMCASTER_CONTROL_SCHEMA_VERSION,
                        "backend": backend,
                        "live_writes_enabled": false,
                        "credential_resolved": state.credential_resolved,
                        "peat_enabled": state.peat.is_some(),
                        "fleet_generation": state.latest_plan_generation,
                        "latest_status": state.latest_status,
                        "latest_mesh_observation": state.latest_mesh_observation,
                    })
                };
                let _ = query
                    .reply(query.key_expr(), serde_json::to_vec(&health).unwrap_or_default())
                    .await;
            }
            _ = peat_poll.tick(), if peat.is_some() => {
                let peat_node = Arc::clone(peat.as_ref().expect("guarded PEAT node"));
                reconnect_peat_peers(Arc::clone(&peat_node), &state).await;
                if let Some(plan) = newest_peat_plan(peat_node.as_ref()).await? {
                    let generation = plan.generation;
                    let should_publish = state.read().await.latest_plan_generation
                        .is_none_or(|current| generation > current);
                    if should_publish {
                        state.write().await.latest_plan_generation = Some(generation);
                        let _ = session
                            .put(TOPIC_FLEET_PLAN, serde_json::to_vec(&plan).unwrap_or_default())
                            .await;
                    }
                }
                for observation in peat_mesh_observations(peat_node.as_ref()).await? {
                    let _ = session
                        .put(
                            TOPIC_MESH_OBSERVATIONS,
                            serde_json::to_vec(&observation).unwrap_or_default(),
                        )
                        .await;
                }
            }
            _ = mesh_poll.tick() => {
                if let Some(observation) = observe_local_mesh(&state, now_unix_ms()).await {
                    state.write().await.latest_mesh_observation = serde_json::to_value(&observation).ok();
                    let _ = session
                        .put(
                            TOPIC_MESH_OBSERVATIONS,
                            serde_json::to_vec(&observation).unwrap_or_default(),
                        )
                        .await;
                    if observation.node.status == StreamCasterObservedStatus::Online {
                        let _ = session
                            .put(
                                TOPIC_EFFECTIVE_OBSERVATIONS,
                                serde_json::to_vec(&effective_observation(&observation)).unwrap_or_default(),
                            )
                            .await;
                    }
                    persist_mesh_observation(&state, &observation).await;
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    if let Some(peat) = peat {
        peat.shutdown().await?;
    }
    Ok(())
}

async fn process_operation(
    state: &Arc<RwLock<ServiceState>>,
    request: StreamCasterOperationRequest,
    now_ms: u64,
) -> StreamCasterOperationStatus {
    if let Err(error) = request.validate() {
        return failed_status(&request, now_ms, "invalid_operation", error.to_string());
    }

    if matches!(request.intent, StreamCasterOperationIntent::Activate) {
        return activate_simulated(state, request, now_ms).await;
    }

    persist_plan(state, &request.fleet_plan, now_ms).await;
    let evidence = evidence_gates(state, &request).await;
    {
        let mut state = state.write().await;
        if state.simulate_radio && matches!(state.backend, Backend::ContractOnly) {
            match simulator_for(&request.fleet_plan, &request.assignment) {
                Ok(simulator) => state.backend = Backend::Simulator(simulator),
                Err(error) => {
                    return blocked_status(
                        &request,
                        now_ms,
                        evidence,
                        "simulator_initialization_failed",
                        &error.to_string(),
                    );
                }
            }
        }
    }
    let backend_kind = {
        let state = state.read().await;
        match &state.backend {
            Backend::ContractOnly => 0,
            Backend::LiveReadOnly(_) => 1,
            Backend::Simulator(_) => 2,
        }
    };

    if backend_kind == 0 {
        return blocked_status(
            &request,
            now_ms,
            evidence,
            "radio_backend_unavailable",
            "Contract validation succeeded; no radio URL or simulator is configured.",
        );
    }

    let prepared = {
        let state_guard = state.read().await;
        match &state_guard.backend {
            Backend::LiveReadOnly(client) => {
                StreamCasterTransactionEngine::new(client.clone())
                    .prepare(&request.fleet_plan, &request.assignment, evidence, now_ms)
                    .await
            }
            Backend::Simulator(simulator) => {
                StreamCasterTransactionEngine::new(simulator.clone())
                    .prepare(&request.fleet_plan, &request.assignment, evidence, now_ms)
                    .await
            }
            Backend::ContractOnly => unreachable!(),
        }
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            return blocked_status(
                &request,
                now_ms,
                evidence,
                "radio_inspection_failed",
                &error.to_string(),
            );
        }
    };
    let capabilities = read_capabilities(state, now_ms).await.ok();
    let effective = Some(prepared.snapshot.clone());
    let plugin_ready = prepared.gates.live_capability_match
        && prepared.gates.regulatory_authorized
        && prepared.gates.antenna_installation_resolved
        && prepared.gates.credential_resolved
        && prepared.gates.rollback_snapshot_staged;
    let status = StreamCasterOperationStatus {
        schema_version: STREAMCASTER_CONTROL_SCHEMA_VERSION,
        request_id: request.request_id.clone(),
        node_id: request.assignment.node_id.clone(),
        generation: request.fleet_plan.generation,
        observed_at_ms: now_ms,
        phase: if plugin_ready {
            StreamCasterApplyPhase::Prepared
        } else {
            StreamCasterApplyPhase::Blocked
        },
        gates: prepared.gates,
        capabilities,
        effective,
        error_code: (!plugin_ready).then(|| "activation_evidence_incomplete".into()),
        message: (!plugin_ready).then(|| {
            "Read-only inspection succeeded; regulatory, antenna, credential, or capability evidence is incomplete.".into()
        }),
    };
    state.write().await.prepared = Some(prepared);
    status
}

async fn activate_simulated(
    state: &Arc<RwLock<ServiceState>>,
    request: StreamCasterOperationRequest,
    now_ms: u64,
) -> StreamCasterOperationStatus {
    let (backend, prepared) = {
        let state = state.read().await;
        let backend = match &state.backend {
            Backend::Simulator(simulator) => Some(simulator.clone()),
            _ => None,
        };
        (backend, state.prepared.clone())
    };
    let Some(simulator) = backend else {
        return blocked_status(
            &request,
            now_ms,
            StreamCasterActivationGates::default(),
            "live_write_adapter_unverified",
            "Live StreamCaster writes remain disabled until radio-in-the-loop verification is complete.",
        );
    };
    let Some(prepared) = prepared else {
        return blocked_status(
            &request,
            now_ms,
            StreamCasterActivationGates::default(),
            "prepare_required",
            "The simulator requires a matching prepared transaction before activation.",
        );
    };
    if prepared.generation != request.fleet_plan.generation {
        return blocked_status(
            &request,
            now_ms,
            prepared.gates,
            "prepared_generation_mismatch",
            "The prepared radio transaction does not match the requested fleet generation.",
        );
    }
    let Some(authorization) = request.arc_authorization else {
        return blocked_status(
            &request,
            now_ms,
            prepared.gates,
            "arc_authorization_required",
            "ARC activation authorization is required.",
        );
    };
    if !arc_authorization_is_fresh(authorization, now_ms) {
        return blocked_status(
            &request,
            now_ms,
            prepared.gates,
            "arc_authorization_expired",
            "ARC activation authorization is expired or outside the allowed clock skew.",
        );
    }
    let Some(mut prepared) = state.write().await.prepared.take() else {
        return blocked_status(
            &request,
            now_ms,
            StreamCasterActivationGates::default(),
            "prepare_required",
            "The prepared transaction changed before activation; prepare the generation again.",
        );
    };
    prepared.gates.known_landed = authorization.known_landed;
    prepared.gates.preserves_control_bearer = authorization.preserves_control_bearer;
    prepared.gates.hardware_apply_enabled = request.assignment.hardware_apply_enabled;
    let result = StreamCasterTransactionEngine::new(simulator)
        .apply_simulated(prepared.clone(), request.activation, now_ms)
        .await;
    match result {
        Ok(effective) => StreamCasterOperationStatus {
            schema_version: STREAMCASTER_CONTROL_SCHEMA_VERSION,
            request_id: request.request_id,
            node_id: request.assignment.node_id,
            generation: request.fleet_plan.generation,
            observed_at_ms: now_ms,
            phase: StreamCasterApplyPhase::Effective,
            gates: prepared.gates,
            capabilities: None,
            effective: Some(effective),
            error_code: None,
            message: Some("Simulated transaction applied and verified.".into()),
        },
        Err(error) => failed_status(
            &request,
            now_ms,
            "simulated_apply_failed",
            error.to_string(),
        ),
    }
}

fn arc_authorization_is_fresh(authorization: ArcActivationAuthorization, now_ms: u64) -> bool {
    authorization.maintenance_window_authorized
        && authorization.known_landed
        && authorization.preserves_control_bearer
        && authorization.authorized_at_ms
            <= now_ms.saturating_add(ARC_AUTHORIZATION_MAX_CLOCK_SKEW_MS)
        && now_ms.saturating_sub(authorization.authorized_at_ms) <= ARC_AUTHORIZATION_MAX_AGE_MS
}

async fn read_capabilities(
    state: &Arc<RwLock<ServiceState>>,
    now_ms: u64,
) -> Result<StreamCasterCapabilities, streamcaster_control::StreamCasterError> {
    let state = state.read().await;
    match &state.backend {
        Backend::LiveReadOnly(client) => client.read_capabilities(now_ms).await,
        Backend::Simulator(simulator) => simulator.read_capabilities(now_ms).await,
        Backend::ContractOnly => unreachable!(),
    }
}

async fn persist_plan(
    state: &Arc<RwLock<ServiceState>>,
    plan: &ArcRadioConfiguration,
    now_ms: u64,
) {
    let (peat, source, sequence) = {
        let state = state.read().await;
        (
            state.peat.clone(),
            state.source.clone(),
            state.sequence.fetch_add(1, Ordering::Relaxed),
        )
    };
    let Some(peat) = peat else {
        return;
    };
    match AvianRecord::new(
        source,
        sequence,
        DeliveryClass::Mission,
        now_ms,
        MeshPayload::RadioConfiguration(plan.clone()),
    ) {
        Ok(record) => {
            if let Err(error) = peat.put("streamcaster-plan-current", &record).await {
                tracing::warn!(%error, "failed to persist StreamCaster plan through PEAT");
            } else if let Err(error) = peat.sync_now().await {
                tracing::warn!(%error, "failed to request PEAT plan sync");
            }
        }
        Err(error) => tracing::warn!(%error, "failed to create PEAT StreamCaster record"),
    }
}

async fn newest_peat_plan(peat: &PeatNode) -> anyhow::Result<Option<ArcRadioConfiguration>> {
    let mut plans = peat
        .scan(DeliveryClass::Mission)
        .await?
        .into_iter()
        .filter_map(|(_, record)| match record.payload {
            MeshPayload::RadioConfiguration(plan) => Some(plan),
            _ => None,
        })
        .collect::<Vec<_>>();
    plans.sort_by_key(|plan| plan.generation);
    Ok(plans.pop())
}

async fn persist_mesh_observation(
    state: &Arc<RwLock<ServiceState>>,
    observation: &StreamCasterMeshObservation,
) {
    let (peat, sequence) = {
        let state = state.read().await;
        (
            state.peat.clone(),
            state.sequence.fetch_add(1, Ordering::Relaxed),
        )
    };
    let Some(peat) = peat else {
        return;
    };
    let record = AvianRecord::new(
        observation.source.clone(),
        sequence,
        DeliveryClass::Telemetry,
        observation.observed_at_ms,
        MeshPayload::StreamCasterMeshObservation(observation.clone()),
    );
    let record_id = format!("streamcaster-mesh/{}", observation.source);
    match record {
        Ok(record) => {
            if let Err(error) = peat.put(&record_id, &record).await {
                tracing::warn!(%error, "failed to persist StreamCaster mesh observation through PEAT");
            }
        }
        Err(error) => tracing::warn!(%error, "failed to create PEAT StreamCaster mesh observation"),
    }
}

async fn peat_mesh_observations(
    peat: &PeatNode,
) -> anyhow::Result<Vec<StreamCasterMeshObservation>> {
    Ok(peat
        .scan(DeliveryClass::Telemetry)
        .await?
        .into_iter()
        .filter_map(|(_, record)| match record.payload {
            MeshPayload::StreamCasterMeshObservation(observation) => Some(observation),
            _ => None,
        })
        .collect())
}

async fn evidence_gates(
    state: &Arc<RwLock<ServiceState>>,
    request: &StreamCasterOperationRequest,
) -> StreamCasterActivationGates {
    let state = state.read().await;
    StreamCasterActivationGates {
        regulatory_authorized: state
            .regulatory_evidence_file
            .as_deref()
            .is_some_and(|path| regulatory_evidence_matches(path, &request.fleet_plan)),
        antenna_installation_resolved: state
            .installation_evidence_dir
            .as_deref()
            .zip(
                request
                    .assignment
                    .antenna_installation_profile_id
                    .as_deref(),
            )
            .is_some_and(|(directory, profile)| antenna_evidence_matches(directory, profile)),
        credential_resolved: state.credential_resolved,
        ..Default::default()
    }
}

fn regulatory_evidence_matches(path: &Path, plan: &ArcRadioConfiguration) -> bool {
    let Ok(encoded) = std::fs::read(path) else {
        return false;
    };
    let Ok(evidence) = serde_json::from_slice::<RegulatoryEvidence>(&encoded) else {
        return false;
    };
    evidence.authorized
        && evidence.allowed_profiles.iter().any(|profile| {
            profile.center_frequency_mhz.is_finite()
                && (profile.center_frequency_mhz - plan.network.center_frequency_mhz).abs() < 0.001
                && (profile.bandwidth_mhz - plan.network.bandwidth_mhz.as_mhz()).abs() < 0.001
        })
}

fn antenna_evidence_matches(directory: &Path, profile_id: &str) -> bool {
    if profile_id.contains("..") || profile_id.starts_with('/') || profile_id.contains('\\') {
        return false;
    }
    let path = directory.join(format!("{profile_id}.json"));
    let Ok(encoded) = std::fs::read(path) else {
        return false;
    };
    let Ok(evidence) = serde_json::from_slice::<AntennaEvidence>(&encoded) else {
        return false;
    };
    evidence.profile_id == profile_id
        && evidence.approved
        && evidence.antenna_gain_dbi.is_finite()
        && evidence.installation_loss_db.is_finite()
        && evidence.installation_loss_db >= 0.0
        && evidence.calibrated_at_ms > 0
}

fn load_auth(path: Option<&Path>) -> anyhow::Result<StreamCasterAuth> {
    let Some(path) = path else {
        return Ok(StreamCasterAuth::None);
    };
    validate_secret_permissions(path)?;
    let encoded = std::fs::read(path)
        .with_context(|| format!("read StreamCaster credential file {}", path.display()))?;
    let credential: CredentialFile = serde_json::from_slice(&encoded)
        .with_context(|| format!("parse StreamCaster credential file {}", path.display()))?;
    if credential.method.trim().is_empty()
        || credential.username.trim().is_empty()
        || credential.password.is_empty()
    {
        bail!("StreamCaster credential method, username, and password are required");
    }
    Ok(StreamCasterAuth::PasswordRpc {
        method: credential.method,
        username: credential.username,
        password: credential.password,
    })
}

#[cfg(unix)]
fn validate_secret_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 != 0 {
        bail!("StreamCaster credential file must not be accessible by group or other users");
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_secret_permissions(path: &Path) -> anyhow::Result<()> {
    if !path.is_file() {
        bail!("StreamCaster credential path must be a regular file");
    }
    Ok(())
}

async fn start_peat(args: &Args) -> anyhow::Result<(Option<Arc<PeatNode>>, Vec<PeerDescriptor>)> {
    let options_present = [
        args.peat_formation_id.is_some(),
        args.peat_formation_key_file.is_some(),
        args.peat_bind.is_some(),
        args.peat_storage.is_some(),
    ];
    if !options_present.iter().any(|present| *present) {
        return Ok((None, Vec::new()));
    }
    if !options_present.iter().all(|present| *present) {
        bail!("PEAT formation ID, key file, bind address, and storage are required together");
    }
    let key_path = args.peat_formation_key_file.as_ref().expect("checked");
    let key = std::fs::read_to_string(key_path)
        .with_context(|| format!("read PEAT formation key {}", key_path.display()))?;
    let peat = Arc::new(
        PeatNode::start(PeatNodeConfig {
            name: args.source.clone(),
            formation_id: args.peat_formation_id.clone().expect("checked"),
            base64_shared_key: key.trim().to_owned(),
            bind_address: args.peat_bind.expect("checked"),
            storage_path: args.peat_storage.clone().expect("checked"),
        })
        .await?,
    );
    let mut peers = Vec::with_capacity(args.peat_peer.len());
    for peer in &args.peat_peer {
        let peer: PeerDescriptor = peer.parse()?;
        peers.push(peer);
    }
    Ok((Some(peat), peers))
}

async fn reconnect_peat_peers(peat: Arc<PeatNode>, state: &Arc<RwLock<ServiceState>>) {
    let peers = state.read().await.peat_peers.clone();
    let mut attempts = tokio::task::JoinSet::new();
    for peer in peers {
        if peat.is_peer_connected(&peer) {
            continue;
        }
        let peat = Arc::clone(&peat);
        attempts.spawn(async move {
            let name = peer.name.clone();
            let result = tokio::time::timeout(PEAT_RECONNECT_TIMEOUT, peat.connect(&peer)).await;
            (name, result)
        });
    }
    while let Some(attempt) = attempts.join_next().await {
        match attempt {
            Ok((_, Ok(Ok(_)))) => {}
            Ok((peer, Ok(Err(error)))) => {
                tracing::debug!(%peer, %error, "PEAT peer remains unavailable");
            }
            Ok((peer, Err(_))) => {
                tracing::debug!(%peer, "PEAT peer connection attempt timed out");
            }
            Err(error) => {
                tracing::warn!(%error, "PEAT peer reconnect task failed");
            }
        }
    }
}

fn management_host(url: &str) -> Option<String> {
    let authority = url
        .split_once("://")
        .map_or(url, |(_, authority)| authority)
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?
        .trim();
    if authority.is_empty() {
        return None;
    }
    if let Some(bracketed) = authority.strip_prefix('[') {
        return bracketed.split(']').next().map(str::to_owned);
    }
    Some(authority.split(':').next().unwrap_or(authority).to_owned())
}

async fn observe_local_mesh(
    state: &Arc<RwLock<ServiceState>>,
    observed_at_ms: u64,
) -> Option<StreamCasterMeshObservation> {
    let (source, backend, management_address, peat, peers, latest_position) = {
        let state = state.read().await;
        (
            state.source.clone(),
            state.backend.clone(),
            state.management_address.clone(),
            state.peat.clone(),
            state.peat_peers.clone(),
            state.latest_position,
        )
    };

    let (capabilities, effective, error, simulated) = match backend {
        Backend::ContractOnly => return None,
        Backend::LiveReadOnly(api) => {
            let capabilities = api.read_capabilities(observed_at_ms).await;
            let effective = api.read_effective_settings(observed_at_ms).await;
            let error = capabilities
                .as_ref()
                .err()
                .map(ToString::to_string)
                .or_else(|| effective.as_ref().err().map(ToString::to_string));
            (capabilities.ok(), effective.ok(), error, false)
        }
        Backend::Simulator(api) => {
            let capabilities = api.read_capabilities(observed_at_ms).await.ok();
            let effective = api.read_effective_settings(observed_at_ms).await.ok();
            (capabilities, effective, None, true)
        }
    };

    let endpoint_id = peat.as_ref().map(|node| node.endpoint_id_hex());
    let links = peers
        .iter()
        .map(|peer| {
            let connected = peat
                .as_ref()
                .is_some_and(|node| node.is_peer_connected(peer));
            StreamCasterPeerLink {
                source: source.clone(),
                source_endpoint_id: endpoint_id.clone(),
                target: peer.name.clone(),
                target_endpoint_id: peer.endpoint_id_hex.clone(),
                target_addresses: peer.addresses().iter().map(ToString::to_string).collect(),
                transport: "peat_over_streamcaster".into(),
                state: if connected {
                    "connected"
                } else {
                    "disconnected"
                }
                .into(),
                observed_at_ms,
            }
        })
        .collect::<Vec<_>>();
    let status = if effective.is_some() {
        StreamCasterObservedStatus::Online
    } else {
        StreamCasterObservedStatus::Unreachable
    };

    Some(StreamCasterMeshObservation {
        schema_version: STREAMCASTER_MESH_OBSERVATION_SCHEMA_VERSION,
        observed_at_ms,
        source: source.clone(),
        capacity_requirement_nodes: STREAMCASTER_CAPACITY_REQUIREMENT_NODES,
        simulated,
        node: StreamCasterObservedNode {
            node_key: source,
            management_ip: management_address,
            status,
            last_seen_ms: observed_at_ms,
            peat_endpoint_id: endpoint_id,
            peat_connected_peers: peat.as_ref().map_or(0, |node| node.peer_count()),
            position: latest_position.filter(|position| {
                position.observed_at_ms
                    <= observed_at_ms.saturating_add(ARC_AUTHORIZATION_MAX_CLOCK_SKEW_MS)
                    && observed_at_ms.saturating_sub(position.observed_at_ms) <= POSITION_FRESH_MS
            }),
            radio: StreamCasterObservedRadio {
                node_id: effective.as_ref().and_then(|settings| settings.node_id),
                system_name: effective
                    .as_ref()
                    .and_then(|settings| settings.system_name.clone()),
                network_id: effective
                    .as_ref()
                    .map(|settings| settings.network_id.clone()),
                center_frequency_mhz: effective
                    .as_ref()
                    .map(|settings| settings.center_frequency_mhz),
                bandwidth_mhz: effective.as_ref().map(|settings| settings.bandwidth_mhz),
                link_distance_m: effective.as_ref().map(|settings| settings.link_distance_m),
                antenna_mask: effective.as_ref().map(|settings| settings.antenna_mask),
                transmit_power_dbm_per_port: effective
                    .as_ref()
                    .and_then(|settings| settings.transmit_power_dbm_per_port),
                model: capabilities.as_ref().and_then(|value| value.model),
                firmware_version: capabilities
                    .as_ref()
                    .and_then(|value| value.firmware_version.clone()),
            },
        },
        links,
        error,
    })
}

fn observed_position(payload: &[u8], received_at_ms: u64) -> Option<StreamCasterObservedPosition> {
    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let position = value.get("position")?;
    let latitude_deg = position.get("lat")?.as_f64()?;
    let longitude_deg = position.get("lon")?.as_f64()?;
    if !latitude_deg.is_finite()
        || !longitude_deg.is_finite()
        || !(-90.0..=90.0).contains(&latitude_deg)
        || !(-180.0..=180.0).contains(&longitude_deg)
    {
        return None;
    }
    let altitude_msl_m = position
        .get("alt_msl_m")
        .and_then(serde_json::Value::as_f64)
        .filter(|altitude| altitude.is_finite());
    Some(StreamCasterObservedPosition {
        latitude_deg,
        longitude_deg,
        altitude_msl_m,
        observed_at_ms: value
            .get("timestamp_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(received_at_ms),
    })
}

fn effective_observation(observation: &StreamCasterMeshObservation) -> serde_json::Value {
    json!({
        "observed_at_ms": observation.observed_at_ms,
        "node_id": observation.node.radio.node_id,
        "system_name": observation.node.radio.system_name,
        "network_id": observation.node.radio.network_id,
        "center_frequency_mhz": observation.node.radio.center_frequency_mhz,
        "bandwidth_mhz": observation.node.radio.bandwidth_mhz,
        "link_distance_m": observation.node.radio.link_distance_m,
        "antenna_mask": observation.node.radio.antenna_mask,
        "transmit_power_dbm_per_port": observation.node.radio.transmit_power_dbm_per_port,
        "model": observation.node.radio.model,
        "firmware_version": observation.node.radio.firmware_version,
    })
}

fn blocked_status(
    request: &StreamCasterOperationRequest,
    now_ms: u64,
    gates: StreamCasterActivationGates,
    code: &str,
    message: &str,
) -> StreamCasterOperationStatus {
    StreamCasterOperationStatus {
        schema_version: STREAMCASTER_CONTROL_SCHEMA_VERSION,
        request_id: request.request_id.clone(),
        node_id: request.assignment.node_id.clone(),
        generation: request.fleet_plan.generation,
        observed_at_ms: now_ms,
        phase: StreamCasterApplyPhase::Blocked,
        gates,
        capabilities: None,
        effective: None,
        error_code: Some(code.into()),
        message: Some(message.into()),
    }
}

fn failed_status(
    request: &StreamCasterOperationRequest,
    now_ms: u64,
    code: &str,
    message: String,
) -> StreamCasterOperationStatus {
    let mut status = blocked_status(
        request,
        now_ms,
        StreamCasterActivationGates::default(),
        code,
        &message,
    );
    status.phase = StreamCasterApplyPhase::Failed;
    status
}

fn invalid_status(message: String, now_ms: u64) -> StreamCasterOperationStatus {
    StreamCasterOperationStatus {
        schema_version: STREAMCASTER_CONTROL_SCHEMA_VERSION,
        request_id: "invalid".into(),
        node_id: NodeId::from("unknown"),
        generation: 0,
        observed_at_ms: now_ms,
        phase: StreamCasterApplyPhase::Failed,
        gates: StreamCasterActivationGates::default(),
        capabilities: None,
        effective: None,
        error_code: Some("invalid_operation_json".into()),
        message: Some(message),
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

fn simulator_for(
    plan: &ArcRadioConfiguration,
    assignment: &StreamCasterDeviceAssignment,
) -> anyhow::Result<SimulatedStreamCaster> {
    let group = plan
        .fleet
        .groups
        .iter()
        .find(|group| group.group_id == assignment.group_id)
        .context("simulator assignment group")?;
    let capabilities = StreamCasterCapabilities {
        observed_at_ms: now_unix_ms(),
        model: Some(assignment.expected_model),
        firmware_version: Some("simulated-v1".into()),
        supported_frequency_profiles: vec![StreamCasterFrequencyProfile {
            center_frequency_mhz: plan.network.center_frequency_mhz,
            bandwidth_mhz: plan.network.bandwidth_mhz,
            antenna_mask: group.antenna_mask,
        }],
        scheduled_activation_supported: true,
        dual_profile_supported: true,
    };
    let effective = StreamCasterEffectiveSettings {
        observed_at_ms: now_unix_ms(),
        node_id: None,
        system_name: Some(assignment.node_id.to_string()),
        network_id: "SIMULATOR-INITIAL".into(),
        center_frequency_mhz: plan.network.center_frequency_mhz,
        bandwidth_mhz: plan.network.bandwidth_mhz,
        link_distance_m: plan.network.link_distance_m.unwrap_or(1),
        antenna_mask: group.antenna_mask,
        transmit_power_dbm_per_port: match group.transmit_power {
            TransmitPowerMode::MaxSupported => None,
            TransmitPowerMode::TargetDbm { dbm } => Some(dbm),
        },
    };
    Ok(SimulatedStreamCaster::new(capabilities, effective))
}

#[cfg(test)]
mod tests {
    use mesh_core::FleetActivationMechanism;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn management_host_never_exposes_credentials_or_paths() {
        assert_eq!(
            management_host("https://operator:secret@10.20.30.40:443/api"),
            Some("10.20.30.40".into())
        );
        assert_eq!(
            management_host("http://[fd00::52]:8080/jsonrpc"),
            Some("fd00::52".into())
        );
        assert_eq!(management_host(""), None);
    }

    #[test]
    fn fused_telemetry_position_is_validated_without_raw_gps_fallback() {
        let position = observed_position(
            br#"{"timestamp_ms":1234,"position":{"lat":34.5,"lon":-86.7,"alt_msl_m":3048.0}}"#,
            9_000,
        )
        .unwrap();
        assert_eq!(position.latitude_deg, 34.5);
        assert_eq!(position.longitude_deg, -86.7);
        assert_eq!(position.altitude_msl_m, Some(3048.0));
        assert_eq!(position.observed_at_ms, 1_234);

        assert!(observed_position(
            br#"{"timestamp_ms":1234,"gps":{"lat":34.5,"lon":-86.7}}"#,
            9_000
        )
        .is_none());
        assert!(observed_position(br#"{"position":{"lat":91.0,"lon":-86.7}}"#, 9_000).is_none());
    }

    #[test]
    fn regulatory_evidence_requires_the_exact_frequency_bandwidth_tuple() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("regulatory.json");
        std::fs::write(
            &path,
            r#"{"authorized":true,"allowed_profiles":[{"center_frequency_mhz":2440.0,"bandwidth_mhz":20.0}]}"#,
        )
        .unwrap();
        let plan: ArcRadioConfiguration =
            serde_json::from_str(include_str!("../tests/fixtures/fleet-plan.v1.json")).unwrap();

        assert!(regulatory_evidence_matches(&path, &plan));
    }

    #[test]
    fn antenna_evidence_requires_matching_approved_calibration() {
        let directory = TempDir::new().unwrap();
        let evidence_path = directory.path().join("airframe");
        std::fs::create_dir_all(&evidence_path).unwrap();
        std::fs::write(
            evidence_path.join("u28-generic-v1.json"),
            r#"{"profile_id":"airframe/u28-generic-v1","approved":true,"antenna_gain_dbi":2.0,"installation_loss_db":1.25,"calibrated_at_ms":1}"#,
        )
        .unwrap();

        assert!(antenna_evidence_matches(
            directory.path(),
            "airframe/u28-generic-v1"
        ));
        assert!(!antenna_evidence_matches(directory.path(), "../escape"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sidecar_start_does_not_require_static_peers_to_be_online() {
        let storage = TempDir::new().unwrap();
        let key_path = storage.path().join("formation.key");
        let key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        std::fs::write(&key_path, key).unwrap();
        let peer_endpoint = mesh_peat::derive_peat_endpoint_id(key, "peer-node").unwrap();
        let args = Args {
            command: None,
            input: None,
            output: None,
            serve: true,
            zenoh_endpoint: "unused".into(),
            source: "local-node".into(),
            radio_url: None,
            simulate_radio: false,
            credential_file: None,
            installation_evidence_dir: None,
            regulatory_evidence_file: None,
            peat_formation_id: Some("arc-radio".into()),
            peat_formation_key_file: Some(key_path),
            peat_bind: Some("127.0.0.1:0".parse().unwrap()),
            peat_storage: Some(storage.path().join("peat")),
            peat_peer: vec![format!("peer-node={peer_endpoint}@127.0.0.1:9")],
        };

        let (peat, peers) = start_peat(&args).await.unwrap();

        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "peer-node");
        peat.unwrap().shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn simulator_prepares_and_applies_only_with_fresh_arc_authorization() {
        let evidence = TempDir::new().unwrap();
        let regulatory_path = evidence.path().join("regulatory.json");
        std::fs::write(
            &regulatory_path,
            r#"{"authorized":true,"allowed_profiles":[{"center_frequency_mhz":2440.0,"bandwidth_mhz":20.0}]}"#,
        )
        .unwrap();
        let installation_path = evidence.path().join("airframe");
        std::fs::create_dir_all(&installation_path).unwrap();
        std::fs::write(
            installation_path.join("u28-generic-v1.json"),
            r#"{"profile_id":"airframe/u28-generic-v1","approved":true,"antenna_gain_dbi":2.0,"installation_loss_db":1.25,"calibrated_at_ms":1}"#,
        )
        .unwrap();

        let mut request: StreamCasterOperationRequest =
            serde_json::from_str(include_str!("../tests/fixtures/operation-request.v1.json"))
                .unwrap();
        request.assignment.hardware_apply_enabled = true;
        let simulator = simulator_for(&request.fleet_plan, &request.assignment).unwrap();
        let state = Arc::new(RwLock::new(ServiceState {
            source: NodeId::from("sim-node"),
            sequence: AtomicU64::new(1),
            backend: Backend::Simulator(simulator),
            credential_resolved: true,
            installation_evidence_dir: Some(evidence.path().to_path_buf()),
            regulatory_evidence_file: Some(regulatory_path),
            prepared: None,
            latest_status: None,
            latest_plan_generation: None,
            latest_mesh_observation: None,
            peat: None,
            peat_peers: Vec::new(),
            management_address: Some("10.0.0.52".into()),
            simulate_radio: true,
            latest_position: Some(StreamCasterObservedPosition {
                latitude_deg: 34.5,
                longitude_deg: -86.7,
                altitude_msl_m: Some(3_048.0),
                observed_at_ms: 8_000,
            }),
        }));

        let observation = observe_local_mesh(&state, 9_000).await.unwrap();
        assert_eq!(observation.capacity_requirement_nodes, 150);
        assert_eq!(observation.node.management_ip.as_deref(), Some("10.0.0.52"));
        assert_eq!(observation.node.status, StreamCasterObservedStatus::Online);
        assert_eq!(
            observation
                .node
                .position
                .map(|position| position.altitude_msl_m),
            Some(Some(3_048.0))
        );
        assert!(observation.simulated);
        let effective = effective_observation(&observation);
        assert_eq!(effective["observed_at_ms"], 9_000);
        assert_eq!(effective["system_name"], "air-017");
        assert_eq!(effective["network_id"], "SIMULATOR-INITIAL");

        let stale_position = observe_local_mesh(&state, 40_001).await.unwrap();
        assert!(stale_position.node.position.is_none());

        let prepared = process_operation(&state, request.clone(), 10_000).await;
        assert_eq!(prepared.phase, StreamCasterApplyPhase::Prepared);

        request.intent = StreamCasterOperationIntent::Activate;
        request.activation = FleetActivationMechanism::Scheduled {
            activate_at_ms: 20_000,
        };
        request.arc_authorization = Some(ArcActivationAuthorization {
            maintenance_window_authorized: true,
            known_landed: true,
            preserves_control_bearer: true,
            authorized_at_ms: 1,
        });
        let stale = process_operation(&state, request.clone(), 400_001).await;
        assert_eq!(stale.phase, StreamCasterApplyPhase::Blocked);
        assert_eq!(
            stale.error_code.as_deref(),
            Some("arc_authorization_expired")
        );

        request.arc_authorization = Some(ArcActivationAuthorization {
            maintenance_window_authorized: true,
            known_landed: true,
            preserves_control_bearer: true,
            authorized_at_ms: 10_500,
        });
        let effective = process_operation(&state, request, 11_000).await;

        assert_eq!(effective.phase, StreamCasterApplyPhase::Effective);
        assert_eq!(
            effective.effective.unwrap().network_id,
            "ARC-RADIO".to_owned()
        );
    }
}
