use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context};
use clap::Args;
use mesh_core::{
    NodeId, TopologyPlanner, DEFAULT_MAX_NEIGHBORS, MAX_SUPPORTED_SWARM_SIZE,
    MIN_SUPPORTED_SWARM_SIZE,
};
use mesh_peat::{derive_peat_endpoint_id, PeerDescriptor};
use serde::{Deserialize, Serialize};

const BOOTSTRAP_SCHEMA_VERSION: u16 = 1;
#[derive(Debug, Args)]
pub struct BootstrapArgs {
    /// JSON inventory containing stable node names, radio URLs, and dialable PEAT addresses.
    #[arg(long)]
    inventory: PathBuf,

    /// Shared PEAT formation identifier used by every generated node.
    #[arg(long, default_value = "arc-radio")]
    formation_id: String,

    /// File containing the shared base64 PEAT formation secret. The secret is never emitted.
    #[arg(long)]
    formation_key_file: PathBuf,

    /// Empty or absent output directory for membership, summary, and Ansible host variables.
    #[arg(long)]
    output_dir: PathBuf,

    /// Maximum direct PEAT neighbors for formations of five or more nodes.
    #[arg(long, default_value_t = DEFAULT_MAX_NEIGHBORS)]
    max_neighbors: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapInventory {
    schema_version: u16,
    generation: u64,
    peat_bind: SocketAddr,
    nodes: Vec<BootstrapNodeInput>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapNodeInput {
    name: String,
    radio_url: String,
    addresses: Vec<SocketAddr>,
}

#[derive(Debug)]
struct ResolvedNode {
    node_id: NodeId,
    name: String,
    endpoint_id_hex: String,
    radio_url: String,
    addresses: Vec<SocketAddr>,
}

#[derive(Debug, Serialize)]
struct BootstrapSummary {
    schema_version: u16,
    formation_id: String,
    generation: u64,
    topology_mode: &'static str,
    node_count: usize,
    edge_count: usize,
    max_neighbors: usize,
    membership_file: &'static str,
    host_vars_directory: &'static str,
    nodes: Vec<BootstrapNodeSummary>,
}

#[derive(Debug, Serialize)]
struct BootstrapNodeSummary {
    name: String,
    endpoint_id_hex: String,
    addresses: Vec<SocketAddr>,
    peer_count: usize,
    peers: Vec<String>,
    host_vars_file: String,
}

#[derive(Debug, Serialize)]
struct MembershipManifest {
    schema_version: u16,
    formation_id: String,
    generation: u64,
    members: Vec<MembershipMember>,
}

#[derive(Debug, Serialize)]
struct MembershipMember {
    name: String,
    endpoint_id_hex: String,
    addresses: Vec<SocketAddr>,
}

#[derive(Debug, Serialize)]
struct StreamCasterHostVars {
    arc_streamcaster_plugin_enabled: bool,
    arc_streamcaster_radio_url: String,
    arc_streamcaster_peat_formation_id: String,
    arc_streamcaster_peat_bind: String,
    arc_streamcaster_peat_peers: Vec<String>,
}

struct GeneratedBootstrap {
    summary: BootstrapSummary,
    membership: MembershipManifest,
    host_vars: BTreeMap<String, StreamCasterHostVars>,
}

pub fn run(args: &BootstrapArgs) -> anyhow::Result<()> {
    let encoded = std::fs::read_to_string(&args.inventory)
        .with_context(|| format!("reading bootstrap inventory {}", args.inventory.display()))?;
    let inventory: BootstrapInventory = serde_json::from_str(&encoded)
        .with_context(|| format!("decoding bootstrap inventory {}", args.inventory.display()))?;
    let formation_key = std::fs::read_to_string(&args.formation_key_file).with_context(|| {
        format!(
            "reading PEAT formation key {}",
            args.formation_key_file.display()
        )
    })?;
    let generated = generate(
        inventory,
        &args.formation_id,
        formation_key.trim(),
        args.max_neighbors,
    )?;
    write_bundle(&args.output_dir, &generated)?;

    println!(
        "Generated {} PEAT identities and {} bounded peer links in {}",
        generated.summary.node_count,
        generated.summary.edge_count,
        args.output_dir.display()
    );
    println!(
        "Review bootstrap-summary.json, then copy host_vars/*.json into ARC infra/ansible/host_vars/."
    );
    Ok(())
}

fn generate(
    inventory: BootstrapInventory,
    formation_id: &str,
    formation_key: &str,
    max_neighbors: usize,
) -> anyhow::Result<GeneratedBootstrap> {
    ensure!(
        inventory.schema_version == BOOTSTRAP_SCHEMA_VERSION,
        "unsupported bootstrap schema version {}",
        inventory.schema_version
    );
    ensure!(
        inventory.generation > 0,
        "bootstrap generation must be positive"
    );
    ensure!(
        !formation_id.trim().is_empty(),
        "formation ID cannot be empty"
    );
    ensure!(
        inventory.peat_bind.port() > 0,
        "PEAT bind port must be nonzero"
    );
    ensure!(
        max_neighbors >= 2 && max_neighbors.is_multiple_of(2),
        "maximum neighbors must be an even value of at least two"
    );
    ensure!(
        (1..=MAX_SUPPORTED_SWARM_SIZE).contains(&inventory.nodes.len()),
        "bootstrap inventory must contain 1-{MAX_SUPPORTED_SWARM_SIZE} nodes"
    );

    let mut names = BTreeSet::new();
    let mut endpoint_ids = BTreeSet::new();
    let mut advertised_addresses = BTreeSet::new();
    let mut resolved = Vec::with_capacity(inventory.nodes.len());
    for node in inventory.nodes {
        validate_node_name(&node.name)?;
        ensure!(
            names.insert(node.name.clone()),
            "duplicate node name {:?}",
            node.name
        );
        validate_radio_url(&node.radio_url, &node.name)?;
        ensure!(
            !node.addresses.is_empty() && node.addresses.len() <= 8,
            "node {:?} must advertise 1-8 PEAT addresses",
            node.name
        );
        for address in &node.addresses {
            ensure!(
                address.port() > 0,
                "node {:?} advertises port zero",
                node.name
            );
            ensure!(
                !address.ip().is_unspecified() && !address.ip().is_multicast(),
                "node {:?} advertises non-dialable address {address}",
                node.name
            );
            ensure!(
                advertised_addresses.insert(*address),
                "PEAT address {address} is advertised by more than one node"
            );
        }
        let endpoint_id_hex = derive_peat_endpoint_id(formation_key, &node.name)
            .with_context(|| format!("deriving PEAT identity for {:?}", node.name))?;
        ensure!(
            endpoint_ids.insert(endpoint_id_hex.clone()),
            "derived duplicate PEAT endpoint ID"
        );
        resolved.push(ResolvedNode {
            node_id: NodeId::from(node.name.clone()),
            name: node.name,
            endpoint_id_hex,
            radio_url: node.radio_url,
            addresses: node.addresses,
        });
    }
    resolved.sort_by(|left, right| left.node_id.cmp(&right.node_id));

    let node_ids = resolved
        .iter()
        .map(|node| node.node_id.clone())
        .collect::<Vec<_>>();
    let (neighbors, topology_mode) = plan_neighbors(&node_ids, max_neighbors)?;
    let edge_count = neighbors.values().map(BTreeSet::len).sum::<usize>() / 2;
    let by_id = resolved
        .iter()
        .map(|node| (node.node_id.clone(), node))
        .collect::<BTreeMap<_, _>>();

    let mut summaries = Vec::with_capacity(resolved.len());
    let mut host_vars = BTreeMap::new();
    for node in &resolved {
        let peers = neighbors
            .get(&node.node_id)
            .into_iter()
            .flatten()
            .map(|peer_id| {
                let peer = by_id
                    .get(peer_id)
                    .with_context(|| format!("planned peer {peer_id} is absent"))?;
                PeerDescriptor::with_addresses(
                    peer.name.clone(),
                    peer.endpoint_id_hex.clone(),
                    peer.addresses.clone(),
                )
                .map(|descriptor| descriptor.named_spec())
                .map_err(anyhow::Error::from)
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        let host_vars_file = format!("host_vars/{}.json", node.name);
        summaries.push(BootstrapNodeSummary {
            name: node.name.clone(),
            endpoint_id_hex: node.endpoint_id_hex.clone(),
            addresses: node.addresses.clone(),
            peer_count: peers.len(),
            peers: peers.clone(),
            host_vars_file,
        });
        host_vars.insert(
            node.name.clone(),
            StreamCasterHostVars {
                arc_streamcaster_plugin_enabled: true,
                arc_streamcaster_radio_url: node.radio_url.clone(),
                arc_streamcaster_peat_formation_id: formation_id.to_owned(),
                arc_streamcaster_peat_bind: inventory.peat_bind.to_string(),
                arc_streamcaster_peat_peers: peers,
            },
        );
    }

    let membership = MembershipManifest {
        schema_version: BOOTSTRAP_SCHEMA_VERSION,
        formation_id: formation_id.to_owned(),
        generation: inventory.generation,
        members: resolved
            .iter()
            .map(|node| MembershipMember {
                name: node.name.clone(),
                endpoint_id_hex: node.endpoint_id_hex.clone(),
                addresses: node.addresses.clone(),
            })
            .collect(),
    };
    let summary = BootstrapSummary {
        schema_version: BOOTSTRAP_SCHEMA_VERSION,
        formation_id: formation_id.to_owned(),
        generation: inventory.generation,
        topology_mode,
        node_count: resolved.len(),
        edge_count,
        max_neighbors,
        membership_file: "membership.json",
        host_vars_directory: "host_vars",
        nodes: summaries,
    };
    Ok(GeneratedBootstrap {
        summary,
        membership,
        host_vars,
    })
}

fn plan_neighbors(
    node_ids: &[NodeId],
    max_neighbors: usize,
) -> anyhow::Result<(BTreeMap<NodeId, BTreeSet<NodeId>>, &'static str)> {
    if node_ids.len() >= MIN_SUPPORTED_SWARM_SIZE {
        let plan = TopologyPlanner { max_neighbors }
            .plan(node_ids)
            .context("planning bounded PEAT topology")?;
        let neighbors = node_ids
            .iter()
            .cloned()
            .map(|node| {
                let peers = plan.neighbors(&node).cloned().unwrap_or_default();
                (node, peers)
            })
            .collect();
        return Ok((neighbors, "bounded_ring_chords"));
    }

    let mut ordered = node_ids.to_vec();
    ordered.sort();
    let mut neighbors = ordered
        .iter()
        .cloned()
        .map(|node| (node, BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
    if ordered.len() > 1 {
        for index in 0..ordered.len() {
            let node = &ordered[index];
            let peer = &ordered[(index + 1) % ordered.len()];
            neighbors
                .get_mut(node)
                .expect("node exists")
                .insert(peer.clone());
            neighbors
                .get_mut(peer)
                .expect("peer exists")
                .insert(node.clone());
        }
    }
    Ok((neighbors, "bench_ring"))
}

fn validate_node_name(name: &str) -> anyhow::Result<()> {
    ensure!(!name.is_empty(), "node name cannot be empty");
    ensure!(name.len() <= 128, "node name is longer than 128 bytes");
    ensure!(
        name.bytes()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_' | b'.')),
        "node name {name:?} is not safe for an Ansible host_vars filename"
    );
    ensure!(name != "." && name != "..", "invalid node name {name:?}");
    Ok(())
}

fn validate_radio_url(url: &str, node_name: &str) -> anyhow::Result<()> {
    let authority = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .with_context(|| format!("node {node_name:?} radio URL must use http:// or https://"))?
        .trim_end_matches('/');
    ensure!(
        !authority.is_empty(),
        "node {node_name:?} radio URL has no host"
    );
    ensure!(
        !authority.contains(['@', '/', '?', '#']),
        "node {node_name:?} radio URL must be a credential-free base URL"
    );
    Ok(())
}

fn write_bundle(output_dir: &Path, generated: &GeneratedBootstrap) -> anyhow::Result<()> {
    if output_dir.exists() {
        ensure!(
            output_dir.is_dir(),
            "bootstrap output path {} is not a directory",
            output_dir.display()
        );
        if output_dir.read_dir()?.next().is_some() {
            bail!(
                "refusing to overwrite non-empty bootstrap output directory {}",
                output_dir.display()
            );
        }
    }
    let host_vars_dir = output_dir.join("host_vars");
    std::fs::create_dir_all(&host_vars_dir)
        .with_context(|| format!("creating bootstrap output {}", output_dir.display()))?;
    write_json(
        &output_dir.join("bootstrap-summary.json"),
        &generated.summary,
    )?;
    write_json(&output_dir.join("membership.json"), &generated.membership)?;
    for (name, host_vars) in &generated.host_vars {
        write_json(&host_vars_dir.join(format!("{name}.json")), host_vars)?;
    }
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    let mut encoded = serde_json::to_string_pretty(value).context("encoding bootstrap JSON")?;
    encoded.push('\n');
    std::fs::write(path, encoded)
        .with_context(|| format!("writing bootstrap output {}", path.display()))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const FORMATION_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    fn inventory(count: usize) -> BootstrapInventory {
        BootstrapInventory {
            schema_version: BOOTSTRAP_SCHEMA_VERSION,
            generation: 1,
            peat_bind: "0.0.0.0:4747".parse().unwrap(),
            nodes: (0..count)
                .map(|index| BootstrapNodeInput {
                    name: format!("drone-{index:03}"),
                    radio_url: "http://192.168.169.11".to_owned(),
                    addresses: vec![format!("10.40.0.{}:4747", index + 1).parse().unwrap()],
                })
                .collect(),
        }
    }

    #[test]
    fn three_node_bench_generates_named_symmetric_peers_without_secret_output() {
        let generated = generate(inventory(3), "arc-radio", FORMATION_KEY, 8).unwrap();

        assert_eq!(generated.summary.topology_mode, "bench_ring");
        assert_eq!(generated.summary.edge_count, 3);
        assert!(generated
            .summary
            .nodes
            .iter()
            .all(|node| node.peer_count == 2));
        assert!(generated
            .summary
            .nodes
            .iter()
            .flat_map(|node| &node.peers)
            .all(|peer| peer.starts_with("drone-") && peer.contains('=')));
        let encoded = serde_json::to_string(&generated.summary).unwrap();
        assert!(!encoded.contains(FORMATION_KEY));
    }

    #[test]
    fn one_hundred_fifty_nodes_stay_connected_and_bounded() {
        let generated = generate(inventory(150), "arc-radio", FORMATION_KEY, 8).unwrap();

        assert_eq!(generated.summary.topology_mode, "bounded_ring_chords");
        assert_eq!(generated.summary.node_count, 150);
        assert!(generated
            .summary
            .nodes
            .iter()
            .all(|node| node.peer_count <= 8));
        assert!(generated.summary.edge_count <= 150 * 8 / 2);
    }

    #[test]
    fn documented_sample_inventory_remains_executable() {
        let sample: BootstrapInventory = serde_json::from_str(include_str!(
            "../../../examples/arc-radio-bootstrap-inventory.sample.json"
        ))
        .unwrap();

        let generated = generate(sample, "arc-radio", FORMATION_KEY, 8).unwrap();

        assert_eq!(generated.summary.node_count, 3);
        assert_eq!(generated.summary.edge_count, 3);
    }

    #[test]
    fn bootstrap_command_writes_an_ansible_ready_bundle() {
        let temporary = TempDir::new().unwrap();
        let key_path = temporary.path().join("formation.key");
        std::fs::write(&key_path, FORMATION_KEY).unwrap();
        let output_dir = temporary.path().join("bundle");
        let args = BootstrapArgs {
            inventory: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../examples/arc-radio-bootstrap-inventory.sample.json"),
            formation_id: "arc-radio".into(),
            formation_key_file: key_path,
            output_dir: output_dir.clone(),
            max_neighbors: 8,
        };

        run(&args).unwrap();

        assert!(output_dir.join("bootstrap-summary.json").is_file());
        assert!(output_dir.join("membership.json").is_file());
        let host_vars =
            std::fs::read_to_string(output_dir.join("host_vars/surrogate-a.json")).unwrap();
        assert!(host_vars.contains("arc_streamcaster_plugin_enabled"));
        assert!(host_vars.contains("surrogate-b="));
        assert!(!host_vars.contains(FORMATION_KEY));
    }

    #[test]
    fn output_refuses_to_overwrite_an_existing_bundle() {
        let generated = generate(inventory(2), "arc-radio", FORMATION_KEY, 8).unwrap();
        let output = TempDir::new().unwrap();
        std::fs::write(output.path().join("keep.txt"), "operator data").unwrap();

        let error = write_bundle(output.path(), &generated).unwrap_err();

        assert!(error.to_string().contains("refusing to overwrite"));
        assert_eq!(
            std::fs::read_to_string(output.path().join("keep.txt")).unwrap(),
            "operator data"
        );
    }
}
