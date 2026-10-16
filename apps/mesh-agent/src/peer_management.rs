//! Owns paired-peer state and stages changes before durable activation.
use crate::{underlay_name, unix_time_ms};
use anyhow::Context;
use mesh_agent::commands::CommandRuntime;
use mesh_agent::config::{ConfiguredNodeRole, ResolvedConfig, TaggedAddress, TaggedPeer, Underlay};
use mesh_agent::control::ControlEnvelope;
use mesh_agent::paired_peers;
use mesh_agent::protocol::{
    ControlRequest, ControlResponse, PairedPeerConnection, PeerConnectionAddress, RecordView,
};
use mesh_agent::status::{AgentStatus, PeerAddressStatus, PeerStatus};
use mesh_core::{DeliveryClass, MeshPayload, NodeId};
use mesh_transport::{AvianRecord, MeshNode, PeerDescriptor};
use std::collections::BTreeSet;
use std::path::PathBuf;
use tokio::time::{self, Duration};

/// One owner holds persisted intent and its runtime representations. Staging
/// validates every representation before a durable write makes it active.
pub(super) struct PeerRegistry {
    pub(super) persisted: Vec<TaggedPeer>,
    pub(super) peers: Vec<PeerDescriptor>,
    pub(super) tagged: Vec<TaggedPeer>,
    pub(super) path: PathBuf,
}

impl PeerRegistry {
    pub(super) async fn handle(
        &mut self,
        node: &MeshNode,
        status: &mut AgentStatus,
        commands: &mut CommandRuntime,
        config: &ResolvedConfig,
        envelope: ControlEnvelope,
    ) {
        handle_control_request(
            node,
            status,
            commands,
            config,
            &self.path,
            &mut self.persisted,
            &mut self.peers,
            &mut self.tagged,
            envelope,
        )
        .await;
    }
}

/// Owns retry futures so edits cannot leave an old path or removed peer
/// connecting in the background. At most one attempt runs per endpoint.
#[derive(Default)]
pub(super) struct ConnectionAttempts {
    tasks: tokio::task::JoinSet<(String, u64)>,
    active: std::collections::BTreeMap<String, (PeerDescriptor, u64, tokio::task::AbortHandle)>,
    generation: u64,
}

impl ConnectionAttempts {
    pub(super) fn schedule(
        &mut self,
        node: &MeshNode,
        peers: &[PeerDescriptor],
        status: &mut AgentStatus,
    ) {
        update_peer_connection_states(node, status);
        self.active.retain(|_, (peer, _, task)| {
            let retained = peers.contains(peer);
            if !retained || node.is_peer_connected(peer) {
                task.abort();
                if !retained {
                    let _ = node.disconnect(peer);
                }
                false
            } else {
                true
            }
        });
        for peer in peers {
            if node.is_peer_connected(peer) || self.active.contains_key(&peer.endpoint_id_hex) {
                continue;
            }
            self.generation = self.generation.saturating_add(1);
            let generation = self.generation;
            let node = node.clone();
            let candidate = peer.clone();
            let task = self.tasks.spawn(async move {
                let _ = time::timeout(Duration::from_secs(2), node.connect(&candidate)).await;
                (candidate.endpoint_id_hex, generation)
            });
            self.active.insert(
                peer.endpoint_id_hex.clone(),
                (peer.clone(), generation, task),
            );
        }
    }

    pub(super) fn has_tasks(&self) -> bool {
        !self.tasks.is_empty()
    }

    pub(super) async fn reap(&mut self) {
        match self.tasks.join_next().await {
            Some(Ok((endpoint, generation))) => {
                if self
                    .active
                    .get(&endpoint)
                    .is_some_and(|(_, current, _)| *current == generation)
                {
                    self.active.remove(&endpoint);
                }
            }
            Some(Err(error)) => self
                .active
                .retain(|_, (_, _, task)| task.id() != error.id()),
            None => {}
        }
    }

    pub(super) async fn shutdown(mut self) {
        self.tasks.abort_all();
        while self.tasks.join_next().await.is_some() {}
    }
}

pub(super) fn peer_statuses(
    tagged_peers: &[TaggedPeer],
    peers: &[PeerDescriptor],
    now_ms: u64,
) -> Vec<PeerStatus> {
    let mut statuses = peers
        .iter()
        .map(|peer| {
            let tagged = tagged_peers
                .iter()
                .find(|configured| configured.name == peer.name);
            let addresses = peer
                .addresses()
                .iter()
                .map(|address| PeerAddressStatus {
                    underlay: tagged.and_then(|configured| {
                        configured
                            .addresses
                            .iter()
                            .find(|candidate| candidate.address == *address)
                            .map(|candidate| candidate.underlay)
                    }),
                    address: address.to_string(),
                })
                .collect();
            PeerStatus {
                name: peer.name.clone(),
                endpoint_id: peer.endpoint_id_hex.clone(),
                addresses,
                connected: false,
                last_transition_at_ms: now_ms,
                selected_underlay: None,
            }
        })
        .collect::<Vec<_>>();
    statuses.extend(
        tagged_peers
            .iter()
            .filter(|tagged| !peers.iter().any(|peer| peer.name == tagged.name))
            .map(|tagged| PeerStatus {
                name: tagged.name.clone(),
                endpoint_id: tagged.endpoint_id.clone(),
                addresses: tagged
                    .addresses
                    .iter()
                    .map(|address| PeerAddressStatus {
                        underlay: Some(address.underlay),
                        address: address.address.to_string(),
                    })
                    .collect(),
                connected: false,
                last_transition_at_ms: now_ms,
                selected_underlay: None,
            }),
    );
    statuses.sort_by(|left, right| left.name.cmp(&right.name));
    statuses
}

#[allow(clippy::too_many_arguments)]
async fn handle_control_request(
    node: &MeshNode,
    status: &mut AgentStatus,
    commands: &mut CommandRuntime,
    config: &ResolvedConfig,
    paired_peer_path: &std::path::Path,
    persisted_paired_peers: &mut Vec<TaggedPeer>,
    peers: &mut Vec<PeerDescriptor>,
    tagged_peers: &mut Vec<TaggedPeer>,
    envelope: ControlEnvelope,
) {
    let response = match envelope.request {
        ControlRequest::Status { .. } => {
            if config.role == ConfiguredNodeRole::Ground && config.membership_file.is_none() {
                enforce_ground_peer_path_policy(node, status);
            }
            update_peer_connection_states(node, status);
            ControlResponse::Status {
                status: Box::new(status.snapshot(unix_time_ms())),
            }
        }
        ControlRequest::ListRecords { class, limit } if (1..=500).contains(&limit) => {
            match node.scan(class).await {
                Ok(mut records) => {
                    records.sort_by_key(|(_, record)| std::cmp::Reverse(record.published_at_ms));
                    records.truncate(usize::from(limit));
                    let records = records
                        .into_iter()
                        .filter_map(|(record_id, record)| {
                            serde_json::to_value(record)
                                .ok()
                                .map(|record| RecordView { record_id, record })
                        })
                        .collect();
                    ControlResponse::Records { records }
                }
                Err(error) => ControlResponse::Error {
                    code: "record_scan_failed".into(),
                    detail: error.to_string(),
                },
            }
        }
        ControlRequest::ListRecords { .. } => ControlResponse::Error {
            code: "invalid_limit".into(),
            detail: "record limit must be 1-500".into(),
        },
        ControlRequest::EmergencyRtl { target }
            if !target.is_empty()
                && target.len() <= 128
                && target.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                }) =>
        {
            match commands.issue_rtl(NodeId::from(target), unix_time_ms()) {
                Ok(command) => {
                    let command_id = command.command_id.to_string();
                    let record = AvianRecord::new(
                        NodeId::from(status.node.name.clone()),
                        command.nonce,
                        DeliveryClass::Emergency,
                        unix_time_ms(),
                        MeshPayload::EmergencyCommand(command),
                    );
                    match record {
                        Ok(record) => {
                            match node.put(&format!("command/{command_id}"), &record).await {
                                Ok(()) => {
                                    status.commands.last_command_at_ms = Some(unix_time_ms());
                                    status.commands.last_result = Some("issued".into());
                                    ControlResponse::CommandIssued { command_id }
                                }
                                Err(error) => ControlResponse::Error {
                                    code: "command_publication_failed".into(),
                                    detail: error.to_string(),
                                },
                            }
                        }
                        Err(error) => ControlResponse::Error {
                            code: "command_record_invalid".into(),
                            detail: error.to_string(),
                        },
                    }
                }
                Err(error) => ControlResponse::Error {
                    code: "command_rejected".into(),
                    detail: error.to_string(),
                },
            }
        }
        ControlRequest::EmergencyRtl { .. } => ControlResponse::Error {
            code: "invalid_target".into(),
            detail: "target must contain 1-128 ASCII letters, digits, dots, dashes, or underscores"
                .into(),
        },
        ControlRequest::ConfigurePeer {
            formation_id,
            name,
            endpoint_id,
            addresses,
        } => {
            match configure_peer(
                node,
                status,
                config,
                paired_peer_path,
                persisted_paired_peers,
                peers,
                tagged_peers,
                formation_id,
                name,
                endpoint_id,
                addresses,
            )
            .await
            {
                Ok((name, connected)) => ControlResponse::PeerConfigured { name, connected },
                Err(error) => ControlResponse::Error {
                    code: "peer_configuration_rejected".into(),
                    detail: error.to_string(),
                },
            }
        }
        ControlRequest::ListPairedPeers
            if config.role == ConfiguredNodeRole::Ground && config.membership_file.is_none() =>
        {
            let mut names = persisted_paired_peers
                .iter()
                .filter(|peer| {
                    !config
                        .peers
                        .iter()
                        .any(|configured| configured.name == peer.name)
                })
                .map(|peer| peer.name.clone())
                .collect::<Vec<_>>();
            names.sort();
            names.dedup();
            ControlResponse::PairedPeers { names }
        }
        ControlRequest::ListPairedPeers => ControlResponse::Error {
            code: "peer_management_rejected".into(),
            detail:
                "local paired-aircraft management is available only on an unmanaged ground node"
                    .into(),
        },
        ControlRequest::ListPairedPeerDetails
            if config.role == ConfiguredNodeRole::Ground && config.membership_file.is_none() =>
        {
            let mut peers = persisted_paired_peers
                .iter()
                .filter(|peer| {
                    !config
                        .peers
                        .iter()
                        .any(|configured| configured.name == peer.name)
                })
                .map(|peer| PairedPeerConnection {
                    name: peer.name.clone(),
                    addresses: peer
                        .addresses
                        .iter()
                        .map(|address| PeerConnectionAddress {
                            underlay: address.underlay,
                            address: address.address,
                        })
                        .collect(),
                })
                .collect::<Vec<_>>();
            peers.sort_by(|left, right| left.name.cmp(&right.name));
            ControlResponse::PairedPeerDetails { peers }
        }
        ControlRequest::ListPairedPeerDetails => ControlResponse::Error {
            code: "peer_management_rejected".into(),
            detail:
                "local paired-aircraft management is available only on an unmanaged ground node"
                    .into(),
        },
        ControlRequest::SetPeerPaths { name, addresses } => {
            match set_paired_peer_paths(
                node,
                status,
                config,
                paired_peer_path,
                persisted_paired_peers,
                peers,
                tagged_peers,
                name,
                addresses,
            )
            .await
            {
                Ok((name, addresses, connected)) => ControlResponse::PeerPathsConfigured {
                    name,
                    addresses,
                    connected,
                },
                Err(error) => ControlResponse::Error {
                    code: "peer_path_configuration_rejected".into(),
                    detail: error.to_string(),
                },
            }
        }
        ControlRequest::RemovePeer { name } => {
            match remove_paired_peer(
                node,
                status,
                config,
                paired_peer_path,
                persisted_paired_peers,
                peers,
                tagged_peers,
                name,
            ) {
                Ok(name) => ControlResponse::PeerRemoved { name },
                Err(error) => ControlResponse::Error {
                    code: "peer_removal_rejected".into(),
                    detail: error.to_string(),
                },
            }
        }
        ControlRequest::ConnectionInfo { mut addresses }
            if config.role == ConfiguredNodeRole::Aircraft
                && !addresses.is_empty()
                && addresses.len() <= 8
                && addresses.iter().all(valid_pairing_address) =>
        {
            for address in &mut addresses {
                address.address = canonical_socket_address(address.address);
            }
            addresses.sort_by_key(|value| underlay_priority(value.underlay));
            addresses.dedup_by_key(|value| value.address);
            ControlResponse::ConnectionInfo {
                formation_id: config.formation_id.clone(),
                name: config.name.clone(),
                endpoint_id: node.endpoint_id_hex(),
                addresses,
            }
        }
        ControlRequest::ConnectionInfo { .. } => ControlResponse::Error {
            code: "invalid_connection_addresses".into(),
            detail: "connection codes require an aircraft node and 1-8 routable unicast addresses"
                .into(),
        },
    };
    let _ = envelope.respond_to.send(response);
}

#[allow(clippy::too_many_arguments)]
async fn configure_peer(
    node: &MeshNode,
    status: &mut AgentStatus,
    config: &ResolvedConfig,
    paired_peer_path: &std::path::Path,
    persisted_paired_peers: &mut Vec<TaggedPeer>,
    peers: &mut Vec<PeerDescriptor>,
    tagged_peers: &mut Vec<TaggedPeer>,
    formation_id: String,
    name: String,
    endpoint_id: String,
    mut addresses: Vec<PeerConnectionAddress>,
) -> anyhow::Result<(String, bool)> {
    anyhow::ensure!(
        config.role == ConfiguredNodeRole::Ground,
        "aircraft pairing is available only on a ground node"
    );
    anyhow::ensure!(
        config.membership_file.is_none(),
        "managed-membership nodes cannot accept local pairing changes"
    );
    anyhow::ensure!(
        formation_id == config.formation_id,
        "connection code belongs to a different AVIAN formation"
    );

    for address in &mut addresses {
        address.address = canonical_socket_address(address.address);
    }
    let mut addresses = addresses
        .into_iter()
        .map(|value| TaggedAddress {
            underlay: value.underlay,
            address: value.address,
        })
        .collect::<Vec<_>>();
    addresses.sort_by_key(|value| underlay_priority(value.underlay));
    addresses.dedup_by_key(|value| value.address);
    let paired_peer = TaggedPeer {
        name: name.clone(),
        endpoint_id,
        addresses,
    };

    let staged = stage_paired_peer(
        &config.peers,
        persisted_paired_peers,
        peers,
        tagged_peers,
        paired_peer.clone(),
        config.max_mesh_peers,
    )?;
    let candidate_descriptor = staged
        .peers
        .iter()
        .find(|peer| peer.name == name)
        .cloned()
        .context("paired peer disappeared during validation")?;
    let replacing_connected_peer = staged
        .replaced_descriptor
        .as_ref()
        .is_some_and(|peer| node.is_peer_connected(peer));
    let candidate_was_connected = node.is_peer_connected(&candidate_descriptor);
    if replacing_connected_peer && !candidate_was_connected {
        let _ = time::timeout(Duration::from_secs(2), node.connect(&candidate_descriptor)).await;
        anyhow::ensure!(
            node.is_peer_connected(&candidate_descriptor),
            "replacement aircraft is unreachable; kept the existing live connection"
        );
    }
    if let Err(error) = paired_peers::persist(paired_peer_path, &staged.persisted) {
        if replacing_connected_peer && !candidate_was_connected {
            let _ = node.disconnect(&candidate_descriptor);
        }
        return Err(error);
    }

    *persisted_paired_peers = staged.persisted;
    *peers = staged.peers;
    *tagged_peers = staged.tagged_peers;
    if let Some(replaced_descriptor) = staged.replaced_descriptor {
        node.disconnect(&replaced_descriptor)?;
    }
    let descriptor = &candidate_descriptor;
    if !node.is_peer_connected(descriptor) {
        let _ = time::timeout(Duration::from_secs(2), node.connect(descriptor)).await;
    }
    let connected = node.is_peer_connected(descriptor);

    refresh_peer_statuses(node, status, tagged_peers, peers);
    println!("Paired ground node with aircraft '{name}'");
    Ok((name, connected))
}

#[allow(clippy::too_many_arguments)]
async fn set_paired_peer_paths(
    node: &MeshNode,
    status: &mut AgentStatus,
    config: &ResolvedConfig,
    paired_peer_path: &std::path::Path,
    persisted_paired_peers: &mut Vec<TaggedPeer>,
    peers: &mut Vec<PeerDescriptor>,
    tagged_peers: &mut Vec<TaggedPeer>,
    name: String,
    mut addresses: Vec<PeerConnectionAddress>,
) -> anyhow::Result<(String, Vec<PeerConnectionAddress>, bool)> {
    anyhow::ensure!(
        config.role == ConfiguredNodeRole::Ground,
        "aircraft path management is available only on a ground node"
    );
    anyhow::ensure!(
        config.membership_file.is_none(),
        "managed-membership nodes cannot accept local path changes"
    );
    anyhow::ensure!(valid_pairing_identifier(&name), "invalid peer name");
    anyhow::ensure!(
        addresses.len() <= 8,
        "a paired peer supports at most 8 paths"
    );
    for address in &mut addresses {
        address.address = canonical_socket_address(address.address);
    }
    anyhow::ensure!(
        addresses.iter().all(valid_pairing_address),
        "paired paths must be routable unicast endpoints with a nonzero port"
    );
    let unique_addresses = addresses
        .iter()
        .map(|path| path.address)
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        unique_addresses.len() == addresses.len(),
        "paired paths must use unique network addresses"
    );
    addresses.sort_by(|left, right| {
        underlay_priority(left.underlay)
            .cmp(&underlay_priority(right.underlay))
            .then_with(|| left.address.cmp(&right.address))
    });

    let staged = stage_paired_peer_paths(
        &config.peers,
        persisted_paired_peers,
        peers,
        tagged_peers,
        &name,
        &addresses,
        config.max_mesh_peers,
    )?;
    paired_peers::persist(paired_peer_path, &staged.persisted)?;

    *persisted_paired_peers = staged.persisted;
    *peers = staged.peers;
    *tagged_peers = staged.tagged_peers;
    if let Some(previous) = staged.previous_descriptor {
        let _ = node.disconnect(&previous);
    }
    // Reconnection is scheduled by the reactor after it receives the new
    // paths, so its admission check cannot reject a newly configured path.
    refresh_peer_statuses(node, status, tagged_peers, peers);
    let connected = staged
        .next_descriptor
        .as_ref()
        .is_some_and(|peer| node.is_peer_connected(peer));
    println!(
        "Configured {} path(s) for locally paired aircraft '{name}'",
        addresses.len()
    );
    Ok((name, addresses, connected))
}

#[allow(clippy::too_many_arguments)]
fn remove_paired_peer(
    node: &MeshNode,
    status: &mut AgentStatus,
    config: &ResolvedConfig,
    paired_peer_path: &std::path::Path,
    persisted_paired_peers: &mut Vec<TaggedPeer>,
    peers: &mut Vec<PeerDescriptor>,
    tagged_peers: &mut Vec<TaggedPeer>,
    name: String,
) -> anyhow::Result<String> {
    anyhow::ensure!(
        config.role == ConfiguredNodeRole::Ground,
        "aircraft removal is available only on a ground node"
    );
    anyhow::ensure!(
        config.membership_file.is_none(),
        "managed-membership nodes cannot accept local pairing changes"
    );

    let staged = stage_removed_paired_peer(
        &config.peers,
        persisted_paired_peers,
        peers,
        tagged_peers,
        &name,
    )?;
    paired_peers::persist(paired_peer_path, &staged.persisted)?;
    if let Some(removed) = staged.removed_descriptor.as_ref() {
        node.disconnect(removed)?;
    }

    *persisted_paired_peers = staged.persisted;
    *peers = staged.peers;
    *tagged_peers = staged.tagged_peers;
    refresh_peer_statuses(node, status, tagged_peers, peers);
    println!("Removed locally paired aircraft '{name}'");
    Ok(name)
}

pub(super) fn refresh_peer_statuses(
    node: &MeshNode,
    status: &mut AgentStatus,
    tagged_peers: &[TaggedPeer],
    peers: &[PeerDescriptor],
) {
    let previous = std::mem::take(&mut status.peers);
    status.peers = peer_statuses(tagged_peers, peers, unix_time_ms());
    for peer in &mut status.peers {
        if let Some(old) = previous
            .iter()
            .find(|candidate| candidate.endpoint_id == peer.endpoint_id)
        {
            peer.connected = old.connected;
            peer.last_transition_at_ms = old.last_transition_at_ms;
            peer.selected_underlay = old.selected_underlay.filter(|selected| {
                peer.addresses
                    .iter()
                    .any(|path| path.underlay == Some(*selected))
            });
        }
    }
    enforce_ground_peer_path_policy(node, status);
    update_peer_connection_states(node, status);
}

struct StagedPairedPeer {
    persisted: Vec<TaggedPeer>,
    peers: Vec<PeerDescriptor>,
    tagged_peers: Vec<TaggedPeer>,
    replaced_descriptor: Option<PeerDescriptor>,
}

struct StagedRemovedPeer {
    persisted: Vec<TaggedPeer>,
    peers: Vec<PeerDescriptor>,
    tagged_peers: Vec<TaggedPeer>,
    removed_descriptor: Option<PeerDescriptor>,
}

struct StagedPeerPaths {
    persisted: Vec<TaggedPeer>,
    peers: Vec<PeerDescriptor>,
    tagged_peers: Vec<TaggedPeer>,
    previous_descriptor: Option<PeerDescriptor>,
    next_descriptor: Option<PeerDescriptor>,
}

fn stage_paired_peer_paths(
    configured_peers: &[PeerDescriptor],
    persisted_paired_peers: &[TaggedPeer],
    peers: &[PeerDescriptor],
    tagged_peers: &[TaggedPeer],
    name: &str,
    addresses: &[PeerConnectionAddress],
    max_mesh_peers: usize,
) -> anyhow::Result<StagedPeerPaths> {
    anyhow::ensure!(valid_pairing_identifier(name), "invalid peer name");
    anyhow::ensure!(
        !configured_peers.iter().any(|peer| peer.name == name),
        "peer is managed by the agent configuration and its paths cannot be changed here"
    );
    let original = persisted_paired_peers
        .iter()
        .find(|peer| peer.name == name)
        .context("only locally paired aircraft paths can be changed")?;
    let mut normalized_addresses = addresses.to_vec();
    normalized_addresses.sort_by(|left, right| {
        underlay_priority(left.underlay)
            .cmp(&underlay_priority(right.underlay))
            .then_with(|| left.address.cmp(&right.address))
    });
    let updated = TaggedPeer {
        name: original.name.clone(),
        endpoint_id: original.endpoint_id.clone(),
        addresses: normalized_addresses
            .iter()
            .map(|path| TaggedAddress {
                underlay: path.underlay,
                address: path.address,
            })
            .collect(),
    };
    let mut next_persisted = persisted_paired_peers.to_vec();
    *next_persisted
        .iter_mut()
        .find(|peer| peer.name == name)
        .context("paired aircraft disappeared during path update")? = updated.clone();

    let previous_descriptor = peers
        .iter()
        .find(|peer| peer.name == name && peer.endpoint_id_hex == original.endpoint_id)
        .cloned();
    let mut next_peers = peers
        .iter()
        .filter(|peer| peer.name != name)
        .cloned()
        .collect::<Vec<_>>();
    let mut next_tagged_peers = tagged_peers
        .iter()
        .filter(|peer| peer.name != name)
        .cloned()
        .collect::<Vec<_>>();
    merge_persisted_peer(
        &mut next_peers,
        &mut next_tagged_peers,
        updated,
        max_mesh_peers,
    )?;
    let next_descriptor = next_peers.iter().find(|peer| peer.name == name).cloned();

    Ok(StagedPeerPaths {
        persisted: next_persisted,
        peers: next_peers,
        tagged_peers: next_tagged_peers,
        previous_descriptor,
        next_descriptor,
    })
}

fn stage_removed_paired_peer(
    configured_peers: &[PeerDescriptor],
    persisted_paired_peers: &[TaggedPeer],
    peers: &[PeerDescriptor],
    tagged_peers: &[TaggedPeer],
    name: &str,
) -> anyhow::Result<StagedRemovedPeer> {
    anyhow::ensure!(valid_pairing_identifier(name), "invalid peer name");
    anyhow::ensure!(
        !configured_peers.iter().any(|peer| peer.name == name),
        "peer is managed by the agent configuration and cannot be removed here"
    );
    let persisted = persisted_paired_peers
        .iter()
        .find(|peer| peer.name == name)
        .context("only locally paired aircraft can be removed")?;
    let removed_descriptor = peers
        .iter()
        .find(|peer| peer.name == name && peer.endpoint_id_hex == persisted.endpoint_id)
        .cloned();

    Ok(StagedRemovedPeer {
        persisted: persisted_paired_peers
            .iter()
            .filter(|peer| peer.name != name)
            .cloned()
            .collect(),
        peers: peers
            .iter()
            .filter(|peer| peer.name != name)
            .cloned()
            .collect(),
        tagged_peers: tagged_peers
            .iter()
            .filter(|peer| peer.name != name)
            .cloned()
            .collect(),
        removed_descriptor,
    })
}

fn stage_paired_peer(
    configured_peers: &[PeerDescriptor],
    persisted_paired_peers: &[TaggedPeer],
    peers: &[PeerDescriptor],
    tagged_peers: &[TaggedPeer],
    paired_peer: TaggedPeer,
    max_mesh_peers: usize,
) -> anyhow::Result<StagedPairedPeer> {
    let static_peer = configured_peers
        .iter()
        .find(|peer| peer.name == paired_peer.name);
    if let Some(static_peer) = static_peer {
        anyhow::ensure!(
            static_peer.endpoint_id_hex == paired_peer.endpoint_id,
            "peer name is managed by the agent configuration and cannot be replaced"
        );
    }
    let already_counted = static_peer.is_some()
        || persisted_paired_peers
            .iter()
            .any(|peer| peer.name == paired_peer.name);
    if !already_counted {
        let dynamic_peer_count = persisted_paired_peers
            .iter()
            .filter(|persisted| {
                !configured_peers
                    .iter()
                    .any(|configured| configured.name == persisted.name)
            })
            .count();
        anyhow::ensure!(
            configured_peers.len().saturating_add(dynamic_peer_count) < max_mesh_peers,
            "configured peer limit is reached"
        );
    }
    let replacing_dynamic = static_peer.is_none()
        && persisted_paired_peers.iter().any(|peer| {
            peer.name == paired_peer.name && peer.endpoint_id != paired_peer.endpoint_id
        });
    let replaced_descriptor = if replacing_dynamic {
        peers
            .iter()
            .find(|peer| peer.name == paired_peer.name)
            .cloned()
    } else {
        None
    };

    let mut next_persisted = persisted_paired_peers.to_vec();
    if let Some(existing) = next_persisted
        .iter_mut()
        .find(|peer| peer.name == paired_peer.name)
    {
        *existing = paired_peer.clone();
    } else {
        next_persisted.push(paired_peer.clone());
    }
    next_persisted.sort_by(|left, right| left.name.cmp(&right.name));

    let mut next_peers = peers.to_vec();
    let mut next_tagged_peers = tagged_peers.to_vec();
    if replacing_dynamic {
        next_peers.retain(|peer| peer.name != paired_peer.name);
        next_tagged_peers.retain(|peer| peer.name != paired_peer.name);
    }
    merge_runtime_peer(
        &mut next_peers,
        &mut next_tagged_peers,
        paired_peer,
        max_mesh_peers,
    )?;
    Ok(StagedPairedPeer {
        persisted: next_persisted,
        peers: next_peers,
        tagged_peers: next_tagged_peers,
        replaced_descriptor,
    })
}

pub(super) fn merge_persisted_peer(
    peers: &mut Vec<PeerDescriptor>,
    tagged_peers: &mut Vec<TaggedPeer>,
    paired_peer: TaggedPeer,
    max_mesh_peers: usize,
) -> anyhow::Result<()> {
    if !paired_peer.addresses.is_empty() {
        return merge_runtime_peer(peers, tagged_peers, paired_peer, max_mesh_peers);
    }
    anyhow::ensure!(
        valid_pairing_identifier(&paired_peer.name),
        "invalid peer name"
    );
    anyhow::ensure!(
        valid_endpoint_id(&paired_peer.endpoint_id),
        "invalid paired endpoint identity"
    );
    anyhow::ensure!(
        !peers.iter().any(|peer| {
            peer.name != paired_peer.name && peer.endpoint_id_hex == paired_peer.endpoint_id
        }),
        "endpoint identity is already assigned to another peer"
    );
    if let Some(existing) = tagged_peers
        .iter_mut()
        .find(|peer| peer.name == paired_peer.name)
    {
        anyhow::ensure!(
            existing.endpoint_id == paired_peer.endpoint_id,
            "peer name is already assigned to a different endpoint"
        );
        *existing = paired_peer;
    } else {
        tagged_peers.push(paired_peer);
    }
    Ok(())
}

fn merge_runtime_peer(
    peers: &mut Vec<PeerDescriptor>,
    tagged_peers: &mut Vec<TaggedPeer>,
    mut paired_peer: TaggedPeer,
    max_mesh_peers: usize,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        valid_pairing_identifier(&paired_peer.name),
        "invalid peer name"
    );
    anyhow::ensure!(
        !paired_peer.addresses.is_empty() && paired_peer.addresses.len() <= 8,
        "a paired peer must have 1-8 addresses"
    );
    anyhow::ensure!(
        paired_peer.addresses.iter().all(|value| {
            valid_pairing_address(&PeerConnectionAddress {
                underlay: value.underlay,
                address: value.address,
            })
        }),
        "paired addresses must be routable unicast endpoints with a nonzero port"
    );
    for address in &mut paired_peer.addresses {
        address.address = canonical_socket_address(address.address);
    }
    paired_peer
        .addresses
        .sort_by_key(|value| underlay_priority(value.underlay));
    paired_peer.addresses.dedup_by_key(|value| value.address);
    let descriptor = PeerDescriptor::with_addresses(
        paired_peer.name.clone(),
        paired_peer.endpoint_id.clone(),
        paired_peer
            .addresses
            .iter()
            .map(|value| value.address)
            .collect(),
    )?;

    anyhow::ensure!(
        !peers.iter().any(|peer| {
            peer.name != descriptor.name && peer.endpoint_id_hex == descriptor.endpoint_id_hex
        }),
        "endpoint identity is already assigned to another peer"
    );
    if let Some(existing) = peers.iter_mut().find(|peer| peer.name == descriptor.name) {
        anyhow::ensure!(
            existing.endpoint_id_hex == descriptor.endpoint_id_hex,
            "peer name is already assigned to a different endpoint"
        );
        *existing = descriptor;
    } else {
        anyhow::ensure!(
            peers.len() < max_mesh_peers,
            "configured peer limit is reached"
        );
        peers.push(descriptor);
    }
    if let Some(existing) = tagged_peers
        .iter_mut()
        .find(|peer| peer.name == paired_peer.name)
    {
        *existing = paired_peer;
    } else {
        tagged_peers.push(paired_peer);
    }
    Ok(())
}

fn valid_pairing_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_endpoint_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_pairing_address(value: &PeerConnectionAddress) -> bool {
    value.address.port() != 0 && valid_pairing_ip(value.address.ip())
}

fn canonical_socket_address(value: std::net::SocketAddr) -> std::net::SocketAddr {
    std::net::SocketAddr::new(value.ip().to_canonical(), value.port())
}

fn valid_pairing_ip(value: std::net::IpAddr) -> bool {
    match value {
        std::net::IpAddr::V4(value) => {
            !value.is_unspecified()
                && !value.is_multicast()
                && !value.is_loopback()
                && !value.is_broadcast()
        }
        std::net::IpAddr::V6(value) => value.to_ipv4_mapped().map_or_else(
            || !value.is_unspecified() && !value.is_multicast() && !value.is_loopback(),
            |mapped| valid_pairing_ip(mapped.into()),
        ),
    }
}

fn underlay_priority(value: Underlay) -> u8 {
    match value {
        Underlay::Silvus => 0,
        Underlay::Ethernet => 1,
        Underlay::Wifi => 2,
        Underlay::Satellite => 3,
        Underlay::Other => 4,
    }
}

pub(super) fn update_peer_connection_states(node: &MeshNode, status: &mut AgentStatus) {
    for peer_status in &mut status.peers {
        let was_connected = peer_status.connected;
        let previous_underlay = peer_status.selected_underlay;
        let connected = node.is_endpoint_connected(&peer_status.endpoint_id);
        let selected_underlay = connected
            .then(|| node.peer_remote_address(&peer_status.endpoint_id))
            .flatten()
            .and_then(|remote| {
                peer_status.addresses.iter().find_map(|path| {
                    (path.address.parse::<std::net::SocketAddr>().ok() == Some(remote))
                        .then_some(path.underlay)
                        .flatten()
                })
            });
        if connected != was_connected {
            if connected {
                println!("Peer {} recovered", peer_status.name);
            } else {
                eprintln!("Peer {} unavailable; reconnecting", peer_status.name);
            }
            peer_status.connected = connected;
            peer_status.last_transition_at_ms = unix_time_ms();
        }
        if selected_underlay != previous_underlay {
            if let Some(selected) = selected_underlay {
                println!(
                    "Peer {} active transport is {}",
                    peer_status.name,
                    underlay_name(selected)
                );
            }
            peer_status.selected_underlay = selected_underlay;
            peer_status.last_transition_at_ms = unix_time_ms();
        }
    }
}

pub(super) fn enforce_ground_peer_path_policy(node: &MeshNode, status: &mut AgentStatus) {
    for peer_status in &mut status.peers {
        if !node.is_endpoint_connected(&peer_status.endpoint_id) {
            continue;
        }
        let remote = node.peer_remote_address(&peer_status.endpoint_id);
        let allowed = configured_path_allows_remote(peer_status, remote);
        if allowed {
            continue;
        }
        let _ = node.disconnect_endpoint(&peer_status.endpoint_id);
        if peer_status.connected {
            eprintln!(
                "Peer {} connected through an unconfigured ground path; connection closed",
                peer_status.name
            );
        }
        peer_status.connected = false;
        peer_status.selected_underlay = None;
        peer_status.last_transition_at_ms = unix_time_ms();
    }
}

fn configured_path_allows_remote(
    peer_status: &mesh_agent::status::PeerStatus,
    remote: Option<std::net::SocketAddr>,
) -> bool {
    remote.is_some_and(|remote| {
        let remote = canonical_socket_address(remote);
        peer_status.addresses.iter().any(|path| {
            path.address
                .parse::<std::net::SocketAddr>()
                .ok()
                .map(canonical_socket_address)
                == Some(remote)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovered_and_saved_peer_statuses_keep_underlays_without_inventing_connections() {
        let endpoint = "11".repeat(32);
        let tagged = vec![
            TaggedPeer {
                name: "z-live".into(),
                endpoint_id: endpoint.clone(),
                addresses: vec![TaggedAddress {
                    underlay: Underlay::Ethernet,
                    address: "192.0.2.4:9000".parse().unwrap(),
                }],
            },
            TaggedPeer {
                name: "a-saved".into(),
                endpoint_id: "22".repeat(32),
                addresses: vec![TaggedAddress {
                    underlay: Underlay::Wifi,
                    address: "192.0.2.5:9000".parse().unwrap(),
                }],
            },
        ];
        let peers = vec![PeerDescriptor::with_addresses(
            "z-live",
            endpoint,
            vec![
                "192.0.2.4:9000".parse().unwrap(),
                "192.0.2.9:9000".parse().unwrap(),
            ],
        )
        .unwrap()];
        let states = peer_statuses(&tagged, &peers, 300);
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].name, "a-saved");
        assert_eq!(states[0].addresses[0].underlay, Some(Underlay::Wifi));
        assert_eq!(states[1].addresses[0].underlay, Some(Underlay::Ethernet));
        assert_eq!(states[1].addresses[1].underlay, None);
        assert!(states
            .iter()
            .all(|peer| !peer.connected && peer.last_transition_at_ms == 300));
        assert!(peer_statuses(&[], &[], 300).is_empty());
    }

    #[test]
    fn paired_peer_is_validated_ordered_and_bounded() {
        let mut peers = Vec::new();
        let mut tagged = Vec::new();
        let paired = TaggedPeer {
            name: "aircraft-001".into(),
            endpoint_id: "a".repeat(64),
            addresses: vec![
                TaggedAddress {
                    underlay: Underlay::Satellite,
                    address: "198.51.100.7:9000".parse().unwrap(),
                },
                TaggedAddress {
                    underlay: Underlay::Ethernet,
                    address: "192.0.2.4:9000".parse().unwrap(),
                },
            ],
        };
        merge_runtime_peer(&mut peers, &mut tagged, paired, 2).unwrap();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].addresses()[0], "192.0.2.4:9000".parse().unwrap());
        assert_eq!(tagged[0].addresses[0].underlay, Underlay::Ethernet);

        let name_conflict = TaggedPeer {
            endpoint_id: "b".repeat(64),
            ..tagged[0].clone()
        };
        assert!(merge_runtime_peer(&mut peers, &mut tagged, name_conflict, 2).is_err());
        let invalid_address = TaggedPeer {
            name: "aircraft-002".into(),
            endpoint_id: "c".repeat(64),
            addresses: vec![TaggedAddress {
                underlay: Underlay::Ethernet,
                address: "127.0.0.1:9000".parse().unwrap(),
            }],
        };
        assert!(merge_runtime_peer(&mut peers, &mut tagged, invalid_address, 2).is_err());
        for address in ["255.255.255.255:9000", "[::ffff:127.0.0.1]:9000"] {
            assert!(!valid_pairing_address(&PeerConnectionAddress {
                underlay: Underlay::Ethernet,
                address: address.parse().unwrap(),
            }));
        }
    }

    #[test]
    fn corrected_dynamic_pairing_replaces_identity_but_static_pairing_does_not() {
        let old = TaggedPeer {
            name: "aircraft-001".into(),
            endpoint_id: "a".repeat(64),
            addresses: vec![TaggedAddress {
                underlay: Underlay::Ethernet,
                address: "192.0.2.4:9000".parse().unwrap(),
            }],
        };
        let replacement = TaggedPeer {
            endpoint_id: "b".repeat(64),
            addresses: vec![TaggedAddress {
                underlay: Underlay::Satellite,
                address: "198.51.100.7:9000".parse().unwrap(),
            }],
            ..old.clone()
        };
        let mut peers = Vec::new();
        let mut tagged = Vec::new();
        merge_runtime_peer(&mut peers, &mut tagged, old.clone(), 2).unwrap();

        let staged =
            stage_paired_peer(&[], &tagged, &peers, &tagged, replacement.clone(), 2).unwrap();
        assert_eq!(staged.persisted, vec![replacement.clone()]);
        assert_eq!(staged.tagged_peers, vec![replacement]);
        assert_eq!(staged.peers[0].endpoint_id_hex, "b".repeat(64));
        assert_eq!(
            staged.replaced_descriptor.unwrap().endpoint_id_hex,
            "a".repeat(64)
        );

        assert!(stage_paired_peer(
            &peers,
            &tagged,
            &peers,
            &tagged,
            TaggedPeer {
                endpoint_id: "c".repeat(64),
                ..old
            },
            2,
        )
        .is_err());
    }

    #[test]
    fn only_dynamic_paired_aircraft_can_be_staged_for_removal() {
        let paired = TaggedPeer {
            name: "aircraft-001".into(),
            endpoint_id: "a".repeat(64),
            addresses: vec![TaggedAddress {
                underlay: Underlay::Ethernet,
                address: "192.0.2.4:9000".parse().unwrap(),
            }],
        };
        let mut peers = Vec::new();
        let mut tagged = Vec::new();
        merge_runtime_peer(&mut peers, &mut tagged, paired.clone(), 2).unwrap();

        let staged = stage_removed_paired_peer(
            &[],
            std::slice::from_ref(&paired),
            &peers,
            &tagged,
            &paired.name,
        )
        .unwrap();
        assert!(staged.persisted.is_empty());
        assert!(staged.peers.is_empty());
        assert!(staged.tagged_peers.is_empty());
        assert_eq!(
            staged.removed_descriptor.as_ref().unwrap().name,
            paired.name
        );

        assert!(stage_removed_paired_peer(
            &peers,
            std::slice::from_ref(&paired),
            &peers,
            &tagged,
            &paired.name
        )
        .is_err());
        assert!(stage_removed_paired_peer(&[], &[], &peers, &tagged, &paired.name).is_err());
        assert!(stage_removed_paired_peer(&[], &[paired], &peers, &tagged, "../aircraft").is_err());
    }

    #[test]
    fn paired_paths_can_be_removed_completely_and_restored_in_priority_order() {
        let paired = TaggedPeer {
            name: "aircraft-001".into(),
            endpoint_id: "a".repeat(64),
            addresses: vec![TaggedAddress {
                underlay: Underlay::Ethernet,
                address: "192.0.2.4:9000".parse().unwrap(),
            }],
        };
        let mut peers = Vec::new();
        let mut tagged = Vec::new();
        merge_persisted_peer(&mut peers, &mut tagged, paired.clone(), 2).unwrap();

        let disconnected = stage_paired_peer_paths(
            &[],
            std::slice::from_ref(&paired),
            &peers,
            &tagged,
            &paired.name,
            &[],
            2,
        )
        .unwrap();
        assert!(disconnected.persisted[0].addresses.is_empty());
        assert!(disconnected.peers.is_empty());
        assert_eq!(disconnected.tagged_peers.len(), 1);
        assert!(disconnected.next_descriptor.is_none());

        let restored = stage_paired_peer_paths(
            &[],
            &disconnected.persisted,
            &disconnected.peers,
            &disconnected.tagged_peers,
            &paired.name,
            &[
                PeerConnectionAddress {
                    underlay: Underlay::Satellite,
                    address: "198.51.100.7:9000".parse().unwrap(),
                },
                PeerConnectionAddress {
                    underlay: Underlay::Wifi,
                    address: "192.0.2.5:9000".parse().unwrap(),
                },
            ],
            2,
        )
        .unwrap();
        assert_eq!(restored.persisted[0].addresses.len(), 2);
        assert_eq!(restored.persisted[0].addresses[0].underlay, Underlay::Wifi);
        assert_eq!(
            restored.next_descriptor.unwrap().addresses()[0],
            "192.0.2.5:9000".parse().unwrap()
        );
    }

    #[test]
    fn zero_path_pairings_still_count_toward_the_peer_limit() {
        let paired = TaggedPeer {
            name: "aircraft-001".into(),
            endpoint_id: "a".repeat(64),
            addresses: Vec::new(),
        };
        let candidate = TaggedPeer {
            name: "aircraft-002".into(),
            endpoint_id: "b".repeat(64),
            addresses: vec![TaggedAddress {
                underlay: Underlay::Ethernet,
                address: "192.0.2.5:9000".parse().unwrap(),
            }],
        };

        assert!(stage_paired_peer(&[], &[paired], &[], &[], candidate, 1,).is_err());
    }

    #[test]
    fn ground_path_policy_matches_only_configured_canonical_addresses() {
        let status = mesh_agent::status::PeerStatus {
            name: "aircraft-001".into(),
            endpoint_id: "a".repeat(64),
            addresses: vec![mesh_agent::status::PeerAddressStatus {
                underlay: Some(Underlay::Ethernet),
                address: "[::ffff:192.0.2.4]:9000".into(),
            }],
            connected: true,
            last_transition_at_ms: 0,
            selected_underlay: None,
        };

        assert!(configured_path_allows_remote(
            &status,
            Some("192.0.2.4:9000".parse().unwrap())
        ));
        assert!(!configured_path_allows_remote(
            &status,
            Some("192.0.2.5:9000".parse().unwrap())
        ));
        assert!(!configured_path_allows_remote(&status, None));
        let disabled = mesh_agent::status::PeerStatus {
            addresses: Vec::new(),
            ..status
        };
        assert!(!configured_path_allows_remote(
            &disabled,
            Some("192.0.2.4:9000".parse().unwrap())
        ));
    }
}
