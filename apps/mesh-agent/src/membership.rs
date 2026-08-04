use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::Path;

use anyhow::Context;
use mesh_core::{NodeId, TopologyPlanner};
use mesh_peat::PeerDescriptor;
use serde::Deserialize;

const MEMBERSHIP_SCHEMA_VERSION: u16 = 1;

#[derive(Debug)]
pub struct MembershipSelection {
    pub generation: u64,
    pub members: Vec<NodeId>,
    pub peers: Vec<PeerDescriptor>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipManifest {
    schema_version: u16,
    formation_id: String,
    generation: u64,
    members: Vec<MemberEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberEntry {
    name: String,
    endpoint_id_hex: String,
    addresses: Vec<SocketAddr>,
}

pub fn load_membership(
    path: &Path,
    expected_formation_id: &str,
    local_name: &str,
    local_endpoint_id_hex: &str,
    max_neighbors: usize,
) -> anyhow::Result<MembershipSelection> {
    let encoded = std::fs::read_to_string(path)
        .with_context(|| format!("reading membership manifest from {}", path.display()))?;
    select_membership(
        &encoded,
        expected_formation_id,
        local_name,
        local_endpoint_id_hex,
        max_neighbors,
    )
    .with_context(|| format!("validating membership manifest {}", path.display()))
}

fn select_membership(
    encoded: &str,
    expected_formation_id: &str,
    local_name: &str,
    local_endpoint_id_hex: &str,
    max_neighbors: usize,
) -> anyhow::Result<MembershipSelection> {
    let manifest: MembershipManifest =
        serde_json::from_str(encoded).context("decoding membership JSON")?;
    anyhow::ensure!(
        manifest.schema_version == MEMBERSHIP_SCHEMA_VERSION,
        "unsupported membership schema version {}",
        manifest.schema_version
    );
    anyhow::ensure!(
        manifest.formation_id == expected_formation_id,
        "membership formation {:?} does not match configured formation {:?}",
        manifest.formation_id,
        expected_formation_id
    );
    anyhow::ensure!(
        manifest.generation > 0,
        "membership generation must be positive"
    );

    let node_ids: Vec<NodeId> = manifest
        .members
        .iter()
        .map(|member| NodeId::from(member.name.clone()))
        .collect();
    anyhow::ensure!(
        manifest
            .members
            .iter()
            .all(|member| !member.name.trim().is_empty()),
        "membership contains an empty node name"
    );
    let topology = TopologyPlanner { max_neighbors }
        .plan(&node_ids)
        .context("planning membership overlay")?;

    let mut endpoint_ids = BTreeSet::new();
    let mut advertised_addresses = BTreeSet::new();
    let mut descriptors = BTreeMap::new();
    for member in manifest.members {
        anyhow::ensure!(
            endpoint_ids.insert(member.endpoint_id_hex.clone()),
            "membership contains a duplicate PEAT endpoint ID"
        );
        let node_id = NodeId::from(member.name.clone());
        let descriptor =
            PeerDescriptor::with_addresses(member.name, member.endpoint_id_hex, member.addresses)
                .with_context(|| format!("validating addresses for {node_id}"))?;
        for address in descriptor.addresses() {
            anyhow::ensure!(
                advertised_addresses.insert(*address),
                "membership advertises address {address} for more than one node"
            );
        }
        descriptors.insert(node_id, descriptor);
    }

    let local_node_id = NodeId::from(local_name);
    let local_descriptor = descriptors
        .get(&local_node_id)
        .with_context(|| format!("local node {local_name:?} is absent from membership"))?;
    anyhow::ensure!(
        local_descriptor.endpoint_id_hex == local_endpoint_id_hex,
        "local PEAT endpoint ID does not match membership entry for {local_name:?}"
    );
    let planned_neighbors = topology
        .neighbors(&local_node_id)
        .context("local node is absent from planned topology")?;
    let peers = planned_neighbors
        .iter()
        .map(|node_id| {
            descriptors
                .get(node_id)
                .cloned()
                .with_context(|| format!("planned peer {node_id} is absent from membership"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(MembershipSelection {
        generation: manifest.generation,
        members: node_ids,
        peers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(count: usize, formation_id: &str) -> String {
        let members = (0..count)
            .map(|index| {
                serde_json::json!({
                    "name": format!("aircraft-{index:03}"),
                    "endpoint_id_hex": format!("{:064x}", index + 1),
                    "addresses": [format!("10.40.0.{}:9000", index + 1)]
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "schema_version": 1,
            "formation_id": formation_id,
            "generation": 7,
            "members": members
        })
        .to_string()
    }

    #[test]
    fn scale_profiles_select_only_bounded_neighbors() {
        for count in [5, 25, 100, 200] {
            let encoded = manifest(count, "avian-test");
            let local_index = count / 2;
            let local_name = format!("aircraft-{local_index:03}");
            let selection = select_membership(
                &encoded,
                "avian-test",
                &local_name,
                &format!("{:064x}", local_index + 1),
                8,
            )
            .unwrap();

            assert_eq!(selection.generation, 7);
            assert_eq!(selection.members.len(), count);
            assert!(!selection.peers.is_empty());
            assert!(selection.peers.len() <= 8);
            assert!(selection.peers.iter().all(|peer| peer.name != local_name));
        }
    }

    #[test]
    fn rejects_wrong_formation_and_local_identity() {
        let encoded = manifest(5, "avian-test");
        assert!(
            select_membership(&encoded, "other", "aircraft-000", &format!("{:064x}", 1), 4)
                .is_err()
        );
        assert!(select_membership(
            &encoded,
            "avian-test",
            "aircraft-000",
            &format!("{:064x}", 99),
            4
        )
        .is_err());
    }
}
