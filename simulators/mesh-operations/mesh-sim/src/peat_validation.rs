//! Bounded real-PEAT loopback validation.
//!
//! Nodes use PEAT's Automerge/Iroh implementation and persistent stores. They
//! run as independent node instances in one test process; this is stronger
//! evidence than the deterministic model but remains below OS-process and
//! hardware validation tiers.

use std::time::{Duration, Instant};

use mesh_core::{DeliveryClass, MeshPayload, MissionState, MissionStatus, NodeId};
use mesh_peat::{AvianRecord, PeatNode, PeatNodeConfig};
use serde::Serialize;
use tempfile::TempDir;
use uuid::Uuid;

const LOOPBACK_FORMATION_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeatClusterEvidence {
    pub nodes: usize,
    pub authenticated_connections: usize,
    pub converged_nodes: usize,
    pub convergence_ms: u64,
    pub passed: bool,
    pub process_model: String,
    pub hardware_validated: bool,
}

pub async fn run_bounded_peat_validation() -> Result<Vec<PeatClusterEvidence>, String> {
    let mut evidence = Vec::new();
    for size in [3, 5, 10] {
        evidence.push(run_cluster(size).await?);
    }
    Ok(evidence)
}

async fn run_cluster(size: usize) -> Result<PeatClusterEvidence, String> {
    let stores: Vec<TempDir> = (0..size)
        .map(|_| TempDir::new().map_err(|error| error.to_string()))
        .collect::<Result<_, _>>()?;
    let mut nodes = Vec::new();
    for (index, store) in stores.iter().enumerate() {
        let node = PeatNode::start(PeatNodeConfig {
            name: format!("avian-validation/node-{index:02}"),
            formation_id: "avian-simulator-validation".to_owned(),
            base64_shared_key: LOOPBACK_FORMATION_KEY.to_owned(),
            bind_address: "127.0.0.1:0".parse().expect("loopback address"),
            storage_path: store.path().to_path_buf(),
        })
        .await
        .map_err(|error| format!("starting PEAT node {index}: {error}"))?;
        nodes.push(node);
    }

    for index in 1..nodes.len() {
        let peer = nodes[index]
            .peer_descriptor()
            .map_err(|error| format!("reading PEAT peer {index}: {error}"))?;
        nodes[index - 1]
            .connect(&peer)
            .await
            .map_err(|error| format!("connecting PEAT peer {index}: {error}"))?;
    }

    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if nodes.iter().map(PeatNode::peer_count).sum::<usize>() >= (size - 1) * 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| format!("{size}-node PEAT cluster did not connect"))?;

    let record = AvianRecord::new(
        NodeId::from("avian-validation/node-00"),
        1,
        DeliveryClass::Mission,
        1,
        MeshPayload::Mission(MissionState {
            mission_id: Uuid::from_u128(size as u128),
            objective: format!("validate {size}-node PEAT convergence"),
            generation: 1,
            status: MissionStatus::Active,
        }),
    )
    .map_err(|error| error.to_string())?;
    let started = Instant::now();
    nodes[0]
        .put("validation/current", &record)
        .await
        .map_err(|error| error.to_string())?;

    let converged_nodes = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            for node in &nodes {
                node.sync_now().await.map_err(|error| error.to_string())?;
            }
            let mut count = 0;
            for node in &nodes {
                if node
                    .get(DeliveryClass::Mission, "validation/current")
                    .await
                    .map_err(|error| error.to_string())?
                    .as_ref()
                    == Some(&record)
                {
                    count += 1;
                }
            }
            if count == size {
                break Ok::<usize, String>(count);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| format!("{size}-node PEAT cluster did not converge"))??;
    let convergence_ms = started.elapsed().as_millis() as u64;
    let authenticated_connections = nodes.iter().map(PeatNode::peer_count).sum::<usize>() / 2;
    for node in &nodes {
        node.shutdown().await.map_err(|error| error.to_string())?;
    }

    Ok(PeatClusterEvidence {
        nodes: size,
        authenticated_connections,
        converged_nodes,
        convergence_ms,
        passed: converged_nodes == size && authenticated_connections >= size - 1,
        process_model: "real PEAT nodes in one OS process over loopback".to_owned(),
        hardware_validated: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn three_real_peat_nodes_converge() {
        let evidence = run_cluster(3).await.unwrap();
        assert!(evidence.passed, "{evidence:#?}");
    }
}
