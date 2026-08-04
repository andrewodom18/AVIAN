use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::Parser;
use mesh_peat::{PeatNode, PeatNodeConfig, PeerDescriptor};
use tokio::time::{self, Duration};

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let formation_key = std::fs::read_to_string(&args.formation_key_file).with_context(|| {
        format!(
            "reading formation key from {}",
            args.formation_key_file.display()
        )
    })?;
    let node = PeatNode::start(PeatNodeConfig {
        name: args.name,
        formation_id: args.formation_id,
        base64_shared_key: formation_key,
        bind_address: args.bind,
        storage_path: args.storage,
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
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("waiting for shutdown signal")?;
                break;
            }
            _ = peer_retry.tick() => {
                connect_unavailable_peers(&node, &args.peer).await;
            }
        }
    }
    node.shutdown().await.context("stopping AVIAN node")?;
    Ok(())
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
