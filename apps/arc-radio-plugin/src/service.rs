mod evidence;
mod observation;
mod operations;
mod replication;

use evidence::*;
use observation::*;
use operations::*;
use replication::*;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use mesh_core::{
    ArcRadioConfiguration, DeliveryClass, MeshPayload, NodeId, StreamCasterActivationGates,
    StreamCasterApplyPhase, StreamCasterCapabilities, StreamCasterDeviceAssignment,
    StreamCasterEffectiveSettings, StreamCasterFrequencyProfile, StreamCasterMeshObservation,
    StreamCasterObservedNode, StreamCasterObservedPosition, StreamCasterObservedRadio,
    StreamCasterObservedStatus, StreamCasterOperationIntent, StreamCasterOperationRequest,
    StreamCasterOperationStatus, StreamCasterPeerLink, TransmitPowerMode,
    STREAMCASTER_CAPACITY_REQUIREMENT_NODES, STREAMCASTER_CONTROL_SCHEMA_VERSION,
    STREAMCASTER_MESH_OBSERVATION_SCHEMA_VERSION,
};
use mesh_transport::{AvianRecord, MeshNode, MeshNodeConfig, PeerDescriptor};
use serde::Deserialize;
use serde_json::json;
use streamcaster_control::{
    SimulatedStreamCaster, StreamCasterReadApi, StreamCasterTransactionEngine,
};
use tokio::sync::RwLock;
use tracing_subscriber::EnvFilter;

use super::Args;

const TOPIC_FLEET_PLAN: &str = "local/link/radio/streamcaster/fleet-plan/v1";
const TOPIC_DESIRED: &str = "local/link/radio/streamcaster/desired/v1";
const TOPIC_STATUS: &str = "local/link/radio/streamcaster/status/v1";
const TOPIC_EFFECTIVE_OBSERVATIONS: &str = "local/link/radio/streamcaster/observations/v1";
const TOPIC_MESH_OBSERVATIONS: &str = "local/link/radio/streamcaster/mesh-observations/v2";
const TOPIC_HEALTH: &str = "local/link/radio/streamcaster/plugin-health";
const TOPIC_TELEMETRY: &str = "local/telemetry";
const POSITION_MAX_CLOCK_SKEW_MS: u64 = 5_000;
const POSITION_FRESH_MS: u64 = 30_000;
const MESH_RECONNECT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
enum Backend {
    ContractOnly,
    Simulator(SimulatedStreamCaster),
}

struct ServiceState {
    source: NodeId,
    sequence: AtomicU64,
    backend: Backend,
    installation_evidence_dir: Option<PathBuf>,
    regulatory_evidence_file: Option<PathBuf>,
    latest_status: Option<StreamCasterOperationStatus>,
    latest_plan_generation: Option<u64>,
    latest_mesh_observation: Option<serde_json::Value>,
    mesh: Option<Arc<MeshNode>>,
    mesh_peers: Vec<PeerDescriptor>,
    management_address: Option<String>,
    simulate_radio: bool,
    latest_position: Option<StreamCasterObservedPosition>,
}

pub async fn serve(args: Args) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init()
        .ok();
    let backend = Backend::ContractOnly;
    let (mesh, mesh_peers) = start_mesh(&args).await?;

    let session = super::zenoh_client::open(&args.zenoh_endpoint).await?;
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
        installation_evidence_dir: args.installation_evidence_dir,
        regulatory_evidence_file: args.regulatory_evidence_file,
        latest_status: None,
        latest_plan_generation: None,
        latest_mesh_observation: None,
        mesh: mesh.clone(),
        mesh_peers,
        management_address: None,
        simulate_radio: args.simulate_radio,
        latest_position: None,
    }));

    if args.simulate_radio {
        tracing::warn!(
            "StreamCaster planning simulator enabled; no physical radio writes are possible"
        );
    } else {
        tracing::info!(
            "AVIAN planning sidecar started; Silvus hardware configuration is owned by the external radio-management API"
        );
    }

    let mut replication_poll = tokio::time::interval(Duration::from_secs(2));
    let mut observation_poll = tokio::time::interval(Duration::from_secs(5));
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
                    .put(TOPIC_STATUS, serde_json::to_vec(&status)?)
                    .await;
                if let Some(observation) = status.effective.as_ref() {
                    let _ = session
                        .put(
                            TOPIC_EFFECTIVE_OBSERVATIONS,
                            serde_json::to_vec(observation)?,
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
                        Backend::Simulator(_) => "simulator",
                    };
                    json!({
                        "status": "healthy",
                        "schema_version": STREAMCASTER_CONTROL_SCHEMA_VERSION,
                        "backend": backend,
                        "live_write_adapter_available": false,
                        "hardware_configuration_authority": "external_radio_management_api",
                        "mesh_enabled": state.mesh.is_some(),
                        "fleet_generation": state.latest_plan_generation,
                        "latest_status": state.latest_status,
                        "latest_mesh_observation": state.latest_mesh_observation,
                    })
                };
                let _ = query
                    .reply(query.key_expr(), serde_json::to_vec(&health)?)
                    .await;
            }
            _ = replication_poll.tick(), if mesh.is_some() => {
                let mesh_node = Arc::clone(mesh.as_ref().expect("guarded AVIAN node"));
                reconnect_mesh_peers(Arc::clone(&mesh_node), &state).await;
                if let Some(plan) = newest_mesh_plan(mesh_node.as_ref()).await? {
                    let generation = plan.generation;
                    let should_publish = state.read().await.latest_plan_generation
                        .is_none_or(|current| generation > current);
                    if should_publish {
                        state.write().await.latest_plan_generation = Some(generation);
                        let _ = session
                            .put(TOPIC_FLEET_PLAN, serde_json::to_vec(&plan)?)
                            .await;
                    }
                }
                for observation in replicated_mesh_observations(mesh_node.as_ref()).await? {
                    let _ = session
                        .put(
                            TOPIC_MESH_OBSERVATIONS,
                            serde_json::to_vec(&observation)?,
                        )
                        .await;
                }
            }
            _ = observation_poll.tick() => {
                if let Some(observation) = observe_local_mesh(&state, now_unix_ms()).await {
                    state.write().await.latest_mesh_observation = serde_json::to_value(&observation).ok();
                    let _ = session
                        .put(
                            TOPIC_MESH_OBSERVATIONS,
                            serde_json::to_vec(&observation)?,
                        )
                        .await;
                    if observation.node.status == StreamCasterObservedStatus::Online {
                        let _ = session
                            .put(
                                TOPIC_EFFECTIVE_OBSERVATIONS,
                                serde_json::to_vec(&effective_observation(&observation))?,
                            )
                            .await;
                    }
                    persist_mesh_observation(&state, &observation).await;
                }
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }

    if let Some(mesh) = mesh {
        mesh.shutdown().await?;
    }
    Ok(())
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
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
        let peer_endpoint = mesh_transport::derive_endpoint_id(key, "peer-node").unwrap();
        let args = Args {
            command: None,
            input: None,
            output: None,
            serve: true,
            zenoh_endpoint: "unused".into(),
            source: "local-node".into(),
            simulate_radio: false,
            installation_evidence_dir: None,
            regulatory_evidence_file: None,
            mesh_formation_id: Some("arc-radio".into()),
            mesh_formation_key_file: Some(key_path),
            mesh_bind: Some("127.0.0.1:0".parse().unwrap()),
            mesh_storage: Some(storage.path().join("mesh")),
            mesh_peer: vec![format!("peer-node={peer_endpoint}@127.0.0.1:9")],
        };

        let (mesh, peers) = start_mesh(&args).await.unwrap();

        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].name, "peer-node");
        mesh.unwrap().shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn simulator_prepares_plans_but_blocks_all_hardware_intents() {
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
        request.assignment.management_address = "10.0.0.52".into();
        let simulator = simulator_for(&request.fleet_plan, &request.assignment).unwrap();
        let state = Arc::new(RwLock::new(ServiceState {
            source: NodeId::from("sim-node"),
            sequence: AtomicU64::new(1),
            backend: Backend::Simulator(simulator),
            installation_evidence_dir: Some(evidence.path().to_path_buf()),
            regulatory_evidence_file: Some(regulatory_path),
            latest_status: None,
            latest_plan_generation: None,
            latest_mesh_observation: None,
            mesh: None,
            mesh_peers: Vec::new(),
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

        let mut wrong_binding = request.clone();
        wrong_binding.assignment.management_address = "10.0.0.99".into();
        let blocked = process_operation(&state, wrong_binding, 9_500).await;
        assert_eq!(blocked.phase, StreamCasterApplyPhase::Blocked);
        assert_eq!(
            blocked.error_code.as_deref(),
            Some("management_binding_mismatch")
        );

        let prepared = process_operation(&state, request.clone(), 10_000).await;
        assert_eq!(prepared.phase, StreamCasterApplyPhase::Prepared);

        for intent in [
            StreamCasterOperationIntent::Activate,
            StreamCasterOperationIntent::Confirm,
            StreamCasterOperationIntent::Rollback,
        ] {
            let mut hardware_request = request.clone();
            hardware_request.intent = intent;
            let blocked = process_operation(&state, hardware_request, 11_000).await;
            assert_eq!(blocked.phase, StreamCasterApplyPhase::Blocked);
            assert_eq!(
                blocked.error_code.as_deref(),
                Some("external_management_required")
            );
        }
    }
}
