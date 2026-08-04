use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::{Parser, ValueEnum};
use mesh_core::{DeliveryClass, FlightStack, MeshPayload, NodeId, Telemetry};
use mesh_peat::{AvianRecord, PeatNode, PeatNodeConfig, PeerDescriptor};
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};
use vehicle_adapters::{spawn_mavlink_source, MavlinkSourceConfig, MavlinkTelemetryEvent};

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

    /// Static peer as ENDPOINT_ID_HEX@IP:PORT. Repeat for multiple peers.
    #[arg(long)]
    peer: Vec<PeerDescriptor>,

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    if !args.telemetry_hz.is_finite() || !(0.1..=20.0).contains(&args.telemetry_hz) {
        anyhow::bail!("--telemetry-hz must be between 0.1 and 20.0");
    }
    let node_id = NodeId::from(args.name.clone());
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
    let local_peer = node
        .peer_descriptor()
        .context("reading local PEAT address")?;

    println!("AVIAN node '{}' is ready", node.name());
    println!("Endpoint: {}", node.endpoint_id_hex());
    println!(
        "Peer spec: {}@{}",
        local_peer.endpoint_id_hex, local_peer.address
    );

    println!("Mesh service running; press Ctrl-C to stop");
    let mut peer_retry = time::interval(Duration::from_secs(args.peer_retry_seconds.max(1)));
    peer_retry.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut telemetry_publish = time::interval(Duration::from_secs_f64(1.0 / args.telemetry_hz));
    telemetry_publish.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut mavlink_receiver = start_mavlink(&args, node_id.clone())?;
    let mut latest_telemetry: Option<Telemetry> = None;
    let mut telemetry_sequence = 0_u64;
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("waiting for shutdown signal")?;
                break;
            }
            _ = peer_retry.tick() => {
                connect_unavailable_peers(&node, &args.peer).await;
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
        }
    }
    node.shutdown().await.context("stopping AVIAN node")?;
    Ok(())
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
