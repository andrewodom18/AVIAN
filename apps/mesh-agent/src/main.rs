use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::{Parser, ValueEnum};
use mesh_core::DEFAULT_MAX_NEIGHBORS;
use mesh_core::{
    DeliveryClass, FlightStack, InFlightRelayDecision, InFlightRelayPlanner, MeshPayload, NodeId,
    RelayBroadcastPair, RelayLinkObservation, RelayRuntimeAction, RelayRuntimeConfiguration,
    RelayRuntimeSnapshot, Telemetry,
};
use mesh_peat::{AvianRecord, PeatNode, PeatNodeConfig, PeerDescriptor};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};
use vehicle_adapters::{spawn_mavlink_source, MavlinkSourceConfig, MavlinkTelemetryEvent};

mod membership;

use membership::load_membership;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum MavlinkStack {
    #[value(name = "ardupilot")]
    ArduPilot,
    #[value(name = "px4")]
    Px4,
}

impl From<MavlinkStack> for FlightStack {
    fn from(value: MavlinkStack) -> Self {
        match value {
            MavlinkStack::ArduPilot => Self::ArduPilot,
            MavlinkStack::Px4 => Self::Px4,
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "mesh-agent",
    about = "AVIAN onboard PEAT mesh service",
    version
)]
struct Args {
    /// Stable AVIAN node name used to derive the PEAT identity.
    #[arg(long)]
    name: String,

    /// Local IP and UDP port for the Iroh QUIC transport.
    #[arg(long, default_value = "0.0.0.0:9000")]
    bind: SocketAddr,

    /// Directory for persistent Automerge state.
    #[arg(long, default_value = "./avian-data")]
    storage: PathBuf,

    /// PEAT formation identifier shared by authorized AVIAN nodes.
    #[arg(long, default_value = "avian")]
    formation_id: String,

    /// File containing the shared base64 PEAT formation secret.
    #[arg(long)]
    formation_key_file: PathBuf,

    /// Static peer as ENDPOINT_ID_HEX@IP:PORT[,IP:PORT...]. Repeat per peer.
    #[arg(long)]
    peer: Vec<PeerDescriptor>,

    /// Shared versioned aircraft membership manifest; replaces --peer.
    #[arg(long, conflicts_with = "peer")]
    membership_file: Option<PathBuf>,

    /// Hard limit on direct PEAT neighbors; prevents accidental full meshes.
    #[arg(long, default_value_t = DEFAULT_MAX_NEIGHBORS)]
    max_mesh_peers: usize,

    /// Seconds between attempts to reconnect unavailable static peers.
    #[arg(long, default_value_t = 5)]
    peer_retry_seconds: u64,

    /// MAVLink connection, such as udpin:0.0.0.0:14550 or tcpout:127.0.0.1:5760.
    #[arg(long, requires = "flight_stack")]
    mavlink_address: Option<String>,

    /// Expected flight controller for the MAVLink heartbeat.
    #[arg(long, value_enum, requires = "mavlink_address")]
    flight_stack: Option<MavlinkStack>,

    /// Maximum AVIAN telemetry publications per second from this aircraft.
    #[arg(long, default_value_t = 2.0)]
    telemetry_hz: f64,

    /// Seconds before reconnecting a lost MAVLink transport.
    #[arg(long, default_value_t = 2)]
    mavlink_retry_seconds: u64,

    /// Shared ARC runtime relay configuration. When set, this companion reads
    /// synchronized telemetry/link observations and publishes relay decisions.
    #[arg(long)]
    relay_runtime_config: Option<PathBuf>,

    /// Milliseconds between in-flight relay evaluations.
    #[arg(long, default_value_t = 1_000, requires = "relay_runtime_config")]
    relay_evaluation_ms: u64,

    /// Local UDP listener for normalized relay-link observation JSON from a
    /// radio collector. Bind this to loopback unless the collector is on a
    /// separately controlled local network namespace.
    #[arg(long)]
    relay_observation_listen: Option<SocketAddr>,
}

#[derive(Debug)]
struct RelayRuntimeState {
    configuration: RelayRuntimeConfiguration,
    current_generation: u64,
    current_relay_members: Vec<NodeId>,
    current_broadcast_pairs: Vec<RelayBroadcastPair>,
    sequence: u64,
    last_published: Option<RelayDecisionKey>,
    last_error: Option<String>,
}

impl RelayRuntimeState {
    fn new(configuration: RelayRuntimeConfiguration) -> Self {
        let current_relay_members = configuration.current_relay_members.clone();
        let current_broadcast_pairs = configuration.current_broadcast_pairs.clone();
        Self {
            current_generation: configuration.generation,
            configuration,
            current_relay_members,
            current_broadcast_pairs,
            sequence: 0,
            last_published: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RelayDecisionKey {
    action: RelayRuntimeAction,
    proposed_generation: u64,
    relay_members: Vec<NodeId>,
    broadcast_pairs: Vec<RelayBroadcastPair>,
    disconnected_mission_members: Vec<NodeId>,
}

impl From<&InFlightRelayDecision> for RelayDecisionKey {
    fn from(decision: &InFlightRelayDecision) -> Self {
        Self {
            action: decision.action,
            proposed_generation: decision.proposed_generation,
            relay_members: decision
                .relay_group
                .as_ref()
                .map(|group| group.members.clone())
                .unwrap_or_default(),
            broadcast_pairs: decision
                .relay_group
                .as_ref()
                .map(|group| group.broadcast_pairs.clone())
                .unwrap_or_default(),
            disconnected_mission_members: decision.disconnected_mission_members.clone(),
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if !args.telemetry_hz.is_finite() || !(0.1..=20.0).contains(&args.telemetry_hz) {
        anyhow::bail!("--telemetry-hz must be between 0.1 and 20.0");
    }
    if args.relay_runtime_config.is_some() && args.relay_evaluation_ms == 0 {
        anyhow::bail!("--relay-evaluation-ms must be greater than zero");
    }
    if !(2..=DEFAULT_MAX_NEIGHBORS).contains(&args.max_mesh_peers)
        || !args.max_mesh_peers.is_multiple_of(2)
    {
        anyhow::bail!("--max-mesh-peers must be one of 2, 4, 6, or {DEFAULT_MAX_NEIGHBORS}");
    }
    if args.peer.len() > args.max_mesh_peers {
        anyhow::bail!(
            "{} configured peers exceeds --max-mesh-peers {}",
            args.peer.len(),
            args.max_mesh_peers
        );
    }
    let node_id = NodeId::from(args.name.clone());
    let mut relay_runtime = args
        .relay_runtime_config
        .as_deref()
        .map(load_relay_runtime_configuration)
        .transpose()?
        .map(RelayRuntimeState::new);
    let formation_key = std::fs::read_to_string(&args.formation_key_file).with_context(|| {
        format!(
            "reading formation key from {}",
            args.formation_key_file.display()
        )
    })?;
    let node = PeatNode::start(PeatNodeConfig {
        name: args.name.clone(),
        formation_id: args.formation_id.clone(),
        base64_shared_key: formation_key,
        bind_address: args.bind,
        storage_path: args.storage.clone(),
    })
    .await
    .context("starting AVIAN PEAT node")?;
    let relay_observation_socket = if let Some(address) = args.relay_observation_listen {
        Some(
            UdpSocket::bind(address)
                .await
                .with_context(|| format!("binding relay observation listener on {address}"))?,
        )
    } else {
        None
    };
    let local_peer = node
        .peer_descriptor()
        .context("reading local PEAT address")?;
    let peers = if let Some(path) = &args.membership_file {
        let selection = load_membership(
            path,
            &args.formation_id,
            &args.name,
            &node.endpoint_id_hex(),
            args.max_mesh_peers,
        )?;
        println!(
            "Membership generation {} selected {} direct PEAT neighbors",
            selection.generation,
            selection.peers.len()
        );
        selection.peers
    } else {
        args.peer.clone()
    };

    println!("AVIAN node '{}' is ready", node.name());
    println!("Endpoint: {}", node.endpoint_id_hex());
    let local_addresses = local_peer
        .addresses()
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    println!(
        "Peer spec: {}@{local_addresses}",
        local_peer.endpoint_id_hex
    );
    if let Some(socket) = &relay_observation_socket {
        println!(
            "Relay observation listener: {}",
            socket
                .local_addr()
                .context("reading relay observation listener address")?
        );
    }

    println!("Mesh service running; press Ctrl-C to stop");
    let mut peer_retry = time::interval(Duration::from_secs(args.peer_retry_seconds.max(1)));
    peer_retry.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut telemetry_publish = time::interval(Duration::from_secs_f64(1.0 / args.telemetry_hz));
    telemetry_publish.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut relay_evaluation = time::interval(Duration::from_millis(args.relay_evaluation_ms));
    relay_evaluation.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut mavlink_receiver = start_mavlink(&args, node_id.clone())?;
    let mut latest_telemetry: Option<Telemetry> = None;
    let mut telemetry_sequence = 0_u64;
    let mut relay_observation_sequence = 0_u64;
    let mut relay_observation_buffer = vec![0_u8; 65_535];
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("waiting for shutdown signal")?;
                break;
            }
            _ = peer_retry.tick() => {
                connect_unavailable_peers(&node, &peers).await;
            }
            event = async {
                mavlink_receiver
                    .as_mut()
                    .expect("guarded MAVLink receiver")
                    .recv()
                    .await
            }, if mavlink_receiver.is_some() => {
                match event {
                    Some(MavlinkTelemetryEvent::Connected) => {
                        println!("MAVLink flight controller connected");
                    }
                    Some(MavlinkTelemetryEvent::Telemetry(telemetry)) => {
                        latest_telemetry = Some(telemetry);
                    }
                    Some(MavlinkTelemetryEvent::ConnectionLost(error)) => {
                        eprintln!("MAVLink unavailable; will retry: {error}");
                    }
                    Some(MavlinkTelemetryEvent::Rejected(error)) => {
                        eprintln!("MAVLink telemetry rejected: {error}");
                    }
                    None => {
                        eprintln!("MAVLink source stopped");
                        mavlink_receiver = None;
                    }
                }
            }
            _ = telemetry_publish.tick() => {
                if let Some(telemetry) = latest_telemetry.take() {
                    telemetry_sequence = telemetry_sequence.saturating_add(1);
                    publish_telemetry(
                        &node,
                        &node_id,
                        telemetry_sequence,
                        telemetry,
                    ).await;
                }
            }
            _ = relay_evaluation.tick(), if relay_runtime.is_some() => {
                if let Some(runtime) = relay_runtime.as_mut() {
                    evaluate_relay_runtime(&node, &node_id, runtime).await;
                }
            }
            received = async {
                relay_observation_socket
                    .as_ref()
                    .expect("guarded relay observation listener")
                    .recv_from(&mut relay_observation_buffer)
                    .await
            }, if relay_observation_socket.is_some() => {
                match received {
                    Ok((length, _)) => {
                        relay_observation_sequence = relay_observation_sequence.saturating_add(1);
                        ingest_relay_observation(
                            &node,
                            &node_id,
                            relay_observation_sequence,
                            &relay_observation_buffer[..length],
                        ).await;
                    }
                    Err(error) => eprintln!("Relay observation listener failed: {error}"),
                }
            }
        }
    }
    node.shutdown().await.context("stopping AVIAN node")?;
    Ok(())
}

fn load_relay_runtime_configuration(
    path: &std::path::Path,
) -> anyhow::Result<RelayRuntimeConfiguration> {
    let encoded = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading relay runtime configuration from {}",
            path.display()
        )
    })?;
    let configuration: RelayRuntimeConfiguration = serde_json::from_str(&encoded)
        .with_context(|| format!("decoding relay runtime configuration {}", path.display()))?;
    anyhow::ensure!(
        configuration.generation > 0,
        "relay runtime configuration generation must be positive"
    );
    Ok(configuration)
}

fn start_mavlink(
    args: &Args,
    node_id: NodeId,
) -> anyhow::Result<Option<mpsc::Receiver<MavlinkTelemetryEvent>>> {
    let (Some(address), Some(stack)) = (&args.mavlink_address, args.flight_stack) else {
        return Ok(None);
    };
    let receiver = spawn_mavlink_source(
        MavlinkSourceConfig {
            address: address.clone(),
            source: node_id,
            expected_stack: stack.into(),
            reconnect_delay: Duration::from_secs(args.mavlink_retry_seconds.max(1)),
        },
        64,
    )
    .context("starting MAVLink telemetry source")?;
    Ok(Some(receiver))
}

async fn publish_telemetry(node: &PeatNode, node_id: &NodeId, sequence: u64, telemetry: Telemetry) {
    let record = match AvianRecord::new(
        node_id.clone(),
        sequence,
        DeliveryClass::Telemetry,
        unix_time_ms(),
        MeshPayload::Telemetry(telemetry),
    ) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("Telemetry record rejected: {error}");
            return;
        }
    };
    if let Err(error) = node.put(node_id.as_str(), &record).await {
        eprintln!("Telemetry publication failed: {error}");
    }
}

async fn ingest_relay_observation(
    node: &PeatNode,
    node_id: &NodeId,
    sequence: u64,
    encoded: &[u8],
) {
    let observation = match decode_relay_observation(encoded) {
        Ok(observation) => observation,
        Err(error) => {
            eprintln!("Relay observation rejected: {error}");
            return;
        }
    };
    let record = match AvianRecord::new(
        node_id.clone(),
        sequence,
        DeliveryClass::Telemetry,
        unix_time_ms(),
        MeshPayload::RelayLinkObservation(observation.clone()),
    ) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("Relay observation record rejected: {error}");
            return;
        }
    };
    let (first, second) = if observation.first <= observation.second {
        (&observation.first, &observation.second)
    } else {
        (&observation.second, &observation.first)
    };
    let record_id = format!("relay-link/{first}/{second}/{:?}", observation.transport);
    if let Err(error) = node.put(&record_id, &record).await {
        eprintln!("Relay observation publication failed: {error}");
    }
}

fn decode_relay_observation(encoded: &[u8]) -> Result<RelayLinkObservation, String> {
    let observation: RelayLinkObservation =
        serde_json::from_slice(encoded).map_err(|error| format!("invalid JSON: {error}"))?;
    if !observation.is_well_formed() {
        return Err("malformed metric or endpoint fields".to_owned());
    }
    Ok(observation)
}

async fn evaluate_relay_runtime(
    node: &PeatNode,
    node_id: &NodeId,
    runtime: &mut RelayRuntimeState,
) {
    let records = match node.scan(DeliveryClass::Telemetry).await {
        Ok(records) => records,
        Err(error) => {
            report_relay_runtime_error(runtime, format!("scanning live relay inputs: {error}"));
            return;
        }
    };
    let mut telemetry = Vec::new();
    let mut observations = Vec::new();
    for (_, record) in records {
        match record.payload {
            MeshPayload::Telemetry(value) => telemetry.push(value),
            MeshPayload::RelayLinkObservation(value) => observations.push(value),
            _ => {}
        }
    }
    let snapshot = RelayRuntimeSnapshot {
        observed_at_ms: unix_time_ms(),
        current_generation: runtime.current_generation,
        current_relay_members: runtime.current_relay_members.clone(),
        current_broadcast_pairs: runtime.current_broadcast_pairs.clone(),
        telemetry,
        observations,
    };
    let request = match runtime.configuration.build_request(&snapshot) {
        Ok(request) => request,
        Err(error) => {
            report_relay_runtime_error(runtime, format!("building live relay snapshot: {error}"));
            return;
        }
    };
    let decision = match InFlightRelayPlanner.decide(&request) {
        Ok(decision) => decision,
        Err(error) => {
            report_relay_runtime_error(runtime, format!("evaluating live relay snapshot: {error}"));
            return;
        }
    };
    runtime.last_error = None;
    let key = RelayDecisionKey::from(&decision);
    if matches!(
        decision.action,
        RelayRuntimeAction::MaintainDirect | RelayRuntimeAction::MaintainRelayChain
    ) || runtime.last_published.as_ref() == Some(&key)
    {
        return;
    }

    runtime.sequence = runtime.sequence.saturating_add(1);
    let record = match AvianRecord::new(
        node_id.clone(),
        runtime.sequence,
        DeliveryClass::Mission,
        unix_time_ms(),
        MeshPayload::RelayReconfiguration(decision.clone()),
    ) {
        Ok(record) => record,
        Err(error) => {
            report_relay_runtime_error(runtime, format!("creating relay decision record: {error}"));
            return;
        }
    };
    let record_id = format!(
        "relay/{}/{}/{}",
        runtime.configuration.mission_id, decision.proposed_generation, node_id
    );
    if let Err(error) = node.put(&record_id, &record).await {
        report_relay_runtime_error(runtime, format!("publishing relay decision: {error}"));
        return;
    }
    runtime.last_published = Some(key);
    match decision.action {
        RelayRuntimeAction::FormRelayChain => {
            runtime.current_generation = decision.proposed_generation;
            runtime.current_relay_members = decision
                .relay_group
                .as_ref()
                .map(|group| group.members.clone())
                .unwrap_or_default();
            runtime.current_broadcast_pairs = decision
                .relay_group
                .as_ref()
                .map(|group| group.broadcast_pairs.clone())
                .unwrap_or_default();
        }
        RelayRuntimeAction::ReleaseRelayChain => {
            runtime.current_generation = decision.proposed_generation;
            runtime.current_relay_members.clear();
            runtime.current_broadcast_pairs.clear();
        }
        RelayRuntimeAction::MaintainDirect
        | RelayRuntimeAction::MaintainRelayChain
        | RelayRuntimeAction::BeginRangeDiscovery
        | RelayRuntimeAction::OperatorActionRequired => {}
    }
}

fn report_relay_runtime_error(runtime: &mut RelayRuntimeState, detail: String) {
    if runtime.last_error.as_ref() != Some(&detail) {
        eprintln!("Relay runtime: {detail}");
    }
    runtime.last_error = Some(detail);
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn connect_unavailable_peers(node: &PeatNode, peers: &[PeerDescriptor]) {
    for peer in peers {
        if node.is_peer_connected(peer) {
            continue;
        }
        match node.connect(peer).await {
            Ok(true) => println!("Connected to {}", peer.name),
            Ok(false) => {
                println!("Connection to {} delegated by PEAT tie-breaking", peer.name);
            }
            Err(error) => eprintln!("Peer {} is unavailable; will retry: {error}", peer.name),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_runtime_configuration_sample_decodes_for_the_onboard_agent() {
        let configuration: RelayRuntimeConfiguration = serde_json::from_str(include_str!(
            "../../../examples/relay-runtime-config.sample.json"
        ))
        .unwrap();

        let runtime = RelayRuntimeState::new(configuration);
        assert_eq!(runtime.current_generation, 4);
        assert!(runtime.current_relay_members.is_empty());
    }

    #[test]
    fn relay_observation_ingress_rejects_invalid_radio_data() {
        let valid = r#"{
            "first":"aircraft-a",
            "second":"aircraft-b",
            "transport":"silvus",
            "observed_at_ms":1000,
            "sample_window_ms":500,
            "bidirectional":true,
            "available":true,
            "metrics":{
                "latency_ms":20.0,
                "loss_ratio":0.01,
                "goodput_bps":1000000.0,
                "signal_quality":0.9,
                "stability":0.9,
                "energy_cost":0.2
            },
            "geometry":{
                "distance_m":100.0,
                "line_of_sight":true,
                "fresnel_clearance_ratio":0.9
            },
            "received_power_dbm":-65.0,
            "link_margin_db":20.0
        }"#;
        assert!(decode_relay_observation(valid.as_bytes()).is_ok());

        let malformed = valid.replace("\"sample_window_ms\":500", "\"sample_window_ms\":0");
        assert!(decode_relay_observation(malformed.as_bytes()).is_err());
    }
}
