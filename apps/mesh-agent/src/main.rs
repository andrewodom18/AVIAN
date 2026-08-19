use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::Parser;
use mesh_agent::control::{spawn_control_server, ControlEnvelope};
use mesh_core::{
    Capability, DeliveryClass, InFlightRelayDecision, InFlightRelayPlanner, LinkMonitorObservation,
    MeshPayload, NodeId, NodeProfile, NodeRole, RelayBroadcastPair, RelayLinkObservation,
    RelayObservationPublication, RelayObservationTrafficGovernor, RelayRuntimeAction,
    RelayRuntimeConfiguration, RelayRuntimeSnapshot, SwarmStatusSummary, SwarmTrafficPolicy,
    Telemetry, TelemetryPublication, TelemetryTrafficGovernor, TransportKind,
};
use mesh_peat::{AvianRecord, PeatNode, PeatNodeConfig, PeerDescriptor};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};
use vehicle_adapters::{
    spawn_mavlink_runtime, MavlinkCommandOutcome, MavlinkCommandSender, MavlinkSourceConfig,
    MavlinkTelemetryEvent,
};

mod membership;

use membership::load_membership;
use mesh_agent::commands::{AckOutcome, CommandEvaluation, CommandRuntime};
use mesh_agent::config::{
    validate_private_file_permissions, CliArgs, CommandMode, ConfiguredNodeRole, ResolvedConfig,
    TaggedAddress, TaggedPeer, Underlay,
};
use mesh_agent::link_monitor_protocol;
use mesh_agent::paired_peers;
use mesh_agent::payload_ingress::{self, PayloadEvent};
use mesh_agent::protocol::{ControlRequest, ControlResponse, PeerConnectionAddress, RecordView};
use mesh_agent::status::{
    AgentStatus, PeerAddressStatus, PeerStatus, RadioDeviceStatus, UnderlayStatus,
};

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
    let args = ResolvedConfig::load(CliArgs::parse())?;
    let node_id = NodeId::from(args.name.clone());
    let mut command_runtime = CommandRuntime::load(args.commands.clone(), node_id.clone())?;
    let traffic_policy = args
        .traffic_policy_file
        .as_deref()
        .map(load_traffic_policy)
        .transpose()?
        .unwrap_or_default();
    traffic_policy
        .validate()
        .map_err(|error| anyhow::anyhow!(error))?;
    let telemetry_tick_ms = 1_000.0 / args.telemetry_hz;
    if telemetry_tick_ms > traffic_policy.priority_telemetry_interval_ms as f64 {
        anyhow::bail!(
            "--telemetry-hz is too low for the traffic policy priority interval of {} ms",
            traffic_policy.priority_telemetry_interval_ms
        );
    }
    let mut relay_runtime = args
        .relay_runtime_config
        .as_deref()
        .map(load_relay_runtime_configuration)
        .transpose()?
        .map(RelayRuntimeState::new);
    validate_private_file_permissions(&args.formation_key_file, "formation key")?;
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
    let (mut peers, swarm_members, membership_generation) =
        if let Some(path) = &args.membership_file {
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
            (selection.peers, selection.members, selection.generation)
        } else {
            (args.peers.clone(), vec![node_id.clone()], 0)
        };
    let paired_peer_path = args.storage.join("paired-peers.json");
    let mut persisted_paired_peers = paired_peers::load(&paired_peer_path)?;
    anyhow::ensure!(
        args.role == ConfiguredNodeRole::Ground || persisted_paired_peers.is_empty(),
        "paired peers are permitted only on a ground node"
    );
    anyhow::ensure!(
        args.membership_file.is_none() || persisted_paired_peers.is_empty(),
        "paired peers cannot be combined with a managed membership file"
    );
    let mut tagged_peers = args.tagged_peers.clone();
    for paired_peer in persisted_paired_peers.iter().cloned() {
        merge_runtime_peer(
            &mut peers,
            &mut tagged_peers,
            paired_peer,
            args.max_mesh_peers,
        )?;
    }
    let started_at_ms = unix_time_ms();
    let mut status = AgentStatus::new(
        args.name.clone(),
        args.role,
        started_at_ms,
        args.commands.mode,
        args.mavlink_address.is_some(),
        args.radio.enabled,
        args.radio
            .observation_interval_seconds
            .saturating_mul(3_000),
    );
    status.node.endpoint_id = Some(node.endpoint_id_hex());
    status.peers = peer_statuses(&tagged_peers, &peers, started_at_ms);
    publish_node_advertisement(&node, &node_id, node_profile(&args, &node_id)?).await?;
    let (control_sender, mut control_receiver) = mpsc::channel(32);
    let control_task = spawn_control_server(
        args.sockets.control.clone(),
        args.sockets.max_message_bytes,
        control_sender,
    )
    .await?;
    let payload_socket = payload_ingress::bind(&args.sockets.payload)?;
    let link_observation_socket = link_monitor_protocol::bind(&args.sockets.link_observation)?;

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
    println!(
        "Traffic policy: routine {} ms, priority {} ms, {} rotating operator summary replicas every {} ms",
        traffic_policy.routine_telemetry_interval_ms,
        traffic_policy.priority_telemetry_interval_ms,
        traffic_policy.operator_summary_replicas,
        traffic_policy.operator_summary_interval_ms,
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
    let mut operator_summary_publish = time::interval(Duration::from_millis(
        traffic_policy.operator_summary_interval_ms,
    ));
    operator_summary_publish.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut relay_evaluation = time::interval(Duration::from_millis(args.relay_evaluation_ms));
    relay_evaluation.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut command_poll = time::interval(Duration::from_millis(command_runtime.poll_ms()));
    command_poll.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let (mut mavlink_receiver, mavlink_commands) = start_mavlink(&args, node_id.clone())?;
    let mut latest_telemetry: Option<Telemetry> = None;
    let mut telemetry_governor = TelemetryTrafficGovernor::default();
    let mut telemetry_sequence = 0_u64;
    let mut operator_summary_sequence = 0_u64;
    let mut relay_observation_sequence = 0_u64;
    let mut relay_observation_governor = RelayObservationTrafficGovernor::default();
    let mut relay_observation_buffer = vec![0_u8; 65_535];
    let mut payload_sequence = 0_u64;
    let mut command_ack_sequence = 0_u64;
    let mut payload_buffer = vec![0_u8; args.sockets.max_message_bytes.saturating_add(1)];
    let mut link_observation_buffer = vec![0_u8; args.sockets.max_message_bytes.saturating_add(1)];
    loop {
        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal.context("waiting for shutdown signal")?;
                break;
            }
            _ = peer_retry.tick() => {
                connect_unavailable_peers(&node, &peers, &mut status).await;
            }
            control = control_receiver.recv() => {
                if let Some(control) = control {
                    handle_control_request(
                        &node,
                        &mut status,
                        &mut command_runtime,
                        &args,
                        &paired_peer_path,
                        &mut persisted_paired_peers,
                        &mut peers,
                        &mut tagged_peers,
                        control,
                    ).await;
                }
            }
            _ = command_poll.tick(), if command_runtime.mode() != CommandMode::Disabled => {
                process_emergency_commands(
                    &node,
                    &node_id,
                    &mut command_runtime,
                    &mut status,
                    mavlink_commands.as_ref(),
                    &mut command_ack_sequence,
                ).await;
            }
            received = payload_socket.recv(&mut payload_buffer) => {
                match received {
                    Ok(length) => {
                        payload_sequence = payload_sequence.saturating_add(1);
                        match ingest_payload_event(
                            &node,
                            &node_id,
                            payload_sequence,
                            &payload_buffer[..length],
                            args.sockets.max_message_bytes,
                        ).await {
                            Ok(()) => {
                                status.payload.accepted = status.payload.accepted.saturating_add(1);
                                status.payload.last_event_at_ms = Some(unix_time_ms());
                                status.payload.last_error = None;
                            }
                            Err(error) => {
                                status.payload.rejected = status.payload.rejected.saturating_add(1);
                                status.payload.last_error = Some(error.to_string());
                                eprintln!("Payload event rejected: {error}");
                            }
                        }
                    }
                    Err(error) => {
                        status.payload.last_error = Some(error.to_string());
                        eprintln!("Payload event socket failed: {error}");
                    }
                }
            }
            received = link_observation_socket.recv(&mut link_observation_buffer) => {
                match received {
                    Ok(length) => {
                        match link_monitor_protocol::decode(
                            &link_observation_buffer[..length],
                            args.sockets.max_message_bytes,
                        ) {
                            Ok(observation) => {
                                ingest_link_monitor_observation(
                                    &node,
                                    &node_id,
                                    &mut relay_observation_sequence,
                                    observation,
                                    &mut status,
                                    traffic_policy,
                                    &mut relay_observation_governor,
                                ).await;
                            }
                            Err(error) => {
                                status.record_error("link-monitor", error.to_string(), unix_time_ms());
                                eprintln!("Link-monitor observation rejected: {error}");
                            }
                        }
                    }
                    Err(error) => {
                        status.record_error("link-monitor", error.to_string(), unix_time_ms());
                        eprintln!("Link-monitor socket failed: {error}");
                    }
                }
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
                        if !status.mavlink.connected {
                            println!("MAVLink flight controller connected");
                        }
                        status.mavlink.connected = true;
                        status.mavlink.last_error = None;
                    }
                    Some(MavlinkTelemetryEvent::SystemLocked(system_id)) => {
                        status.mavlink.target_system_id = Some(system_id);
                        status.mavlink.last_message_at_ms = Some(unix_time_ms());
                    }
                    Some(MavlinkTelemetryEvent::Telemetry(telemetry)) => {
                        status.mavlink.connected = true;
                        status.mavlink.last_message_at_ms = Some(unix_time_ms());
                        latest_telemetry = Some(telemetry);
                    }
                    Some(MavlinkTelemetryEvent::ConnectionLost(error)) => {
                        if status.mavlink.connected {
                            eprintln!("MAVLink unavailable; will retry: {error}");
                        }
                        status.mavlink.connected = false;
                        status.mavlink.target_system_id = None;
                        status.mavlink.last_error = Some(error);
                    }
                    Some(MavlinkTelemetryEvent::Rejected(error)) => {
                        eprintln!("MAVLink telemetry rejected: {error}");
                        status.mavlink.last_error = Some(error);
                    }
                    None => {
                        eprintln!("MAVLink source stopped");
                        status.mavlink.connected = false;
                        status.mavlink.target_system_id = None;
                        mavlink_receiver = None;
                    }
                }
            }
            _ = telemetry_publish.tick() => {
                if let Some(telemetry) = latest_telemetry.take() {
                    let publication = telemetry_governor.decide(
                        traffic_policy,
                        &telemetry,
                        unix_time_ms(),
                        relay_runtime
                            .as_ref()
                            .is_some_and(|runtime| telemetry_is_mission_critical(runtime, &node_id)),
                    );
                    match publication {
                        Ok(TelemetryPublication::Suppress) => {}
                        Ok(_) => {
                            telemetry_sequence = telemetry_sequence.saturating_add(1);
                            if publish_telemetry(
                                &node,
                                &node_id,
                                telemetry_sequence,
                                telemetry,
                            ).await {
                                status.telemetry.published = status.telemetry.published.saturating_add(1);
                                status.telemetry.last_published_at_ms = Some(unix_time_ms());
                                status.telemetry.last_error = None;
                            } else {
                                status.telemetry.last_error = Some("PEAT publication failed".into());
                            }
                        }
                        Err(error) => eprintln!("Telemetry traffic policy rejected publication: {error}"),
                    }
                }
            }
            _ = operator_summary_publish.tick() => {
                let now_ms = unix_time_ms();
                match traffic_policy.is_summary_publisher(&swarm_members, &node_id, now_ms) {
                    Ok(true) => {
                        operator_summary_sequence = operator_summary_sequence.saturating_add(1);
                        publish_operator_summary(
                            &node,
                            &node_id,
                            operator_summary_sequence,
                            &swarm_members,
                            membership_generation,
                            traffic_policy,
                            now_ms,
                        ).await;
                    }
                    Ok(false) => {}
                    Err(error) => eprintln!("Operator summary selection failed: {error}"),
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
                            traffic_policy,
                            &mut relay_observation_governor,
                        ).await;
                    }
                    Err(error) => eprintln!("Relay observation listener failed: {error}"),
                }
            }
        }
    }
    control_task.abort();
    let _ = std::fs::remove_file(&args.sockets.control);
    let _ = std::fs::remove_file(&args.sockets.payload);
    let _ = std::fs::remove_file(&args.sockets.link_observation);
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

fn load_traffic_policy(path: &std::path::Path) -> anyhow::Result<SwarmTrafficPolicy> {
    let encoded = std::fs::read_to_string(path)
        .with_context(|| format!("reading traffic policy from {}", path.display()))?;
    let policy: SwarmTrafficPolicy = serde_json::from_str(&encoded)
        .with_context(|| format!("decoding traffic policy {}", path.display()))?;
    policy.validate().map_err(|error| anyhow::anyhow!(error))?;
    Ok(policy)
}

fn telemetry_is_mission_critical(runtime: &RelayRuntimeState, node_id: &NodeId) -> bool {
    // A configured live relay candidate needs timely mesh state for dynamic
    // path and pairing decisions. Other aircraft use the routine source cap;
    // they still surface any failsafe/low-battery transition immediately.
    runtime
        .configuration
        .candidates
        .iter()
        .any(|candidate| candidate.node_id == *node_id)
}

fn start_mavlink(
    args: &ResolvedConfig,
    node_id: NodeId,
) -> anyhow::Result<(
    Option<mpsc::Receiver<MavlinkTelemetryEvent>>,
    Option<MavlinkCommandSender>,
)> {
    let (Some(address), Some(stack)) = (&args.mavlink_address, args.flight_stack) else {
        return Ok((None, None));
    };
    let runtime = spawn_mavlink_runtime(
        MavlinkSourceConfig {
            address: address.clone(),
            source: node_id,
            expected_stack: stack.into(),
            reconnect_delay: Duration::from_secs(args.mavlink_retry_seconds.max(1)),
        },
        64,
    )
    .context("starting MAVLink telemetry source")?;
    Ok((Some(runtime.events), Some(runtime.commands)))
}

async fn publish_telemetry(
    node: &PeatNode,
    node_id: &NodeId,
    sequence: u64,
    telemetry: Telemetry,
) -> bool {
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
            return false;
        }
    };
    if let Err(error) = node.put(node_id.as_str(), &record).await {
        eprintln!("Telemetry publication failed: {error}");
        return false;
    }
    true
}

fn node_profile(config: &ResolvedConfig, node_id: &NodeId) -> anyhow::Result<NodeProfile> {
    Ok(match config.role {
        mesh_agent::config::ConfiguredNodeRole::Aircraft => {
            if let Some(stack) = config.flight_stack {
                NodeProfile::aircraft(node_id.clone(), stack.into(), mesh_core::SYSTEM_MAX_MSL_M)?
            } else {
                NodeProfile {
                    node_id: node_id.clone(),
                    role: NodeRole::Aircraft,
                    flight_stack: None,
                    capabilities: std::collections::BTreeSet::from([Capability::MeshRelay]),
                    platform_max_msl_m: None,
                }
            }
        }
        mesh_agent::config::ConfiguredNodeRole::Ground => NodeProfile::ground(node_id.clone()),
        mesh_agent::config::ConfiguredNodeRole::Observer => NodeProfile {
            node_id: node_id.clone(),
            role: NodeRole::Cloud,
            flight_stack: None,
            capabilities: std::collections::BTreeSet::from([
                Capability::Telemetry,
                Capability::MeshRelay,
            ]),
            platform_max_msl_m: None,
        },
    })
}

async fn publish_node_advertisement(
    node: &PeatNode,
    node_id: &NodeId,
    profile: NodeProfile,
) -> anyhow::Result<()> {
    let record = AvianRecord::new(
        node_id.clone(),
        1,
        DeliveryClass::Mission,
        unix_time_ms(),
        MeshPayload::NodeAdvertisement(profile),
    )?;
    node.put(&format!("node-advertisement/{node_id}"), &record)
        .await?;
    Ok(())
}

fn peer_statuses(
    tagged_peers: &[TaggedPeer],
    peers: &[PeerDescriptor],
    now_ms: u64,
) -> Vec<PeerStatus> {
    peers
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
        .collect()
}

#[allow(clippy::too_many_arguments)]
async fn handle_control_request(
    node: &PeatNode,
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
        ControlRequest::Status { .. } => ControlResponse::Status {
            status: Box::new(status.snapshot(unix_time_ms())),
        },
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
    node: &PeatNode,
    status: &mut AgentStatus,
    config: &ResolvedConfig,
    paired_peer_path: &std::path::Path,
    persisted_paired_peers: &mut Vec<TaggedPeer>,
    peers: &mut Vec<PeerDescriptor>,
    tagged_peers: &mut Vec<TaggedPeer>,
    formation_id: String,
    name: String,
    endpoint_id: String,
    addresses: Vec<PeerConnectionAddress>,
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
fn remove_paired_peer(
    node: &PeatNode,
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
    node.disconnect(&staged.removed_descriptor)?;

    *persisted_paired_peers = staged.persisted;
    *peers = staged.peers;
    *tagged_peers = staged.tagged_peers;
    refresh_peer_statuses(node, status, tagged_peers, peers);
    println!("Removed locally paired aircraft '{name}'");
    Ok(name)
}

fn refresh_peer_statuses(
    node: &PeatNode,
    status: &mut AgentStatus,
    tagged_peers: &[TaggedPeer],
    peers: &[PeerDescriptor],
) {
    let previous = std::mem::take(&mut status.peers);
    status.peers = peer_statuses(tagged_peers, peers, unix_time_ms());
    for peer in &mut status.peers {
        peer.connected = peers
            .iter()
            .find(|candidate| candidate.endpoint_id_hex == peer.endpoint_id)
            .is_some_and(|candidate| node.is_peer_connected(candidate));
        if let Some(old) = previous
            .iter()
            .find(|candidate| candidate.endpoint_id == peer.endpoint_id)
        {
            if old.connected == peer.connected {
                peer.last_transition_at_ms = old.last_transition_at_ms;
            }
            peer.selected_underlay = old.selected_underlay;
        }
    }
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
    removed_descriptor: PeerDescriptor,
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
        .cloned()
        .context("locally paired aircraft is missing from the runtime configuration")?;

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

fn valid_pairing_address(value: &PeerConnectionAddress) -> bool {
    value.address.port() != 0 && valid_pairing_ip(value.address.ip())
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

async fn ingest_payload_event(
    node: &PeatNode,
    node_id: &NodeId,
    sequence: u64,
    encoded: &[u8],
    max_message_bytes: usize,
) -> anyhow::Result<()> {
    let event = payload_ingress::decode(encoded, max_message_bytes)?;
    let (class, record_id, payload) = match event {
        PayloadEvent::ImageManifest { manifest } => (
            DeliveryClass::Bulk,
            format!("image/{}", manifest.image_id),
            MeshPayload::ImageManifest(manifest),
        ),
        PayloadEvent::Detection { detection } => (
            DeliveryClass::Mission,
            format!("detection/{}", detection.detection_id),
            MeshPayload::Detection(detection),
        ),
    };
    let record = AvianRecord::new(node_id.clone(), sequence, class, unix_time_ms(), payload)?;
    node.put(&record_id, &record).await?;
    Ok(())
}

async fn ingest_link_monitor_observation(
    node: &PeatNode,
    node_id: &NodeId,
    sequence: &mut u64,
    observation: LinkMonitorObservation,
    status: &mut AgentStatus,
    traffic_policy: SwarmTrafficPolicy,
    governor: &mut RelayObservationTrafficGovernor,
) {
    status.radio.last_observation_at_ms = Some(observation.observed_at_ms);
    status.radio.api_healthy =
        !observation.radios.is_empty() && observation.radios.iter().all(|radio| radio.api_fresh);
    status.radio.devices = observation
        .radios
        .iter()
        .map(|radio| RadioDeviceStatus {
            name: radio.name.clone(),
            model: radio
                .capabilities
                .as_ref()
                .and_then(|value| value.model)
                .map(|value| format!("{value:?}")),
            firmware: radio
                .capabilities
                .as_ref()
                .and_then(|value| value.firmware_version.clone()),
            api_fresh: radio.api_fresh,
            neighbors: radio.rf_links.len(),
            error: (!radio.errors.is_empty()).then(|| radio.errors.join(",")),
        })
        .collect();
    status.radio.degradation_reasons = observation.degradation_reasons.clone();

    for probe in &observation.probes {
        let Some(underlay) = underlay_for_transport(probe.underlay) else {
            continue;
        };
        status.underlays.insert(
            underlay_name(underlay).into(),
            UnderlayStatus {
                reachable: probe.reachable,
                last_observed_at_ms: Some(probe.observed_at_ms),
                latency_ms: probe.latency_ms,
                loss_ratio: Some(probe.loss_ratio),
                goodput_bps: probe.goodput_bps,
                stability: probe.stability,
                error: probe.error.clone(),
            },
        );
    }
    update_selected_underlays(status, &observation);

    *sequence = sequence.saturating_add(1);
    let record = AvianRecord::new(
        node_id.clone(),
        *sequence,
        DeliveryClass::Telemetry,
        unix_time_ms(),
        MeshPayload::LinkMonitorObservation(observation.clone()),
    );
    match record {
        Ok(record) => {
            if let Err(error) = node.put(&format!("link-monitor/{node_id}"), &record).await {
                status.record_error("link-monitor", error.to_string(), unix_time_ms());
            }
        }
        Err(error) => status.record_error("link-monitor", error.to_string(), unix_time_ms()),
    }

    for relay in observation.relay_observations {
        *sequence = sequence.saturating_add(1);
        match serde_json::to_vec(&relay) {
            Ok(encoded) => {
                ingest_relay_observation(
                    node,
                    node_id,
                    *sequence,
                    &encoded,
                    traffic_policy,
                    governor,
                )
                .await;
            }
            Err(error) => status.record_error("link-monitor", error.to_string(), unix_time_ms()),
        }
    }
}

fn update_selected_underlays(status: &mut AgentStatus, observation: &LinkMonitorObservation) {
    for peer in &mut status.peers {
        let selected = peer.addresses.iter().find_map(|address| {
            let underlay = address.underlay?;
            observation
                .probes
                .iter()
                .any(|probe| {
                    probe.peer == peer.name
                        && probe.underlay == transport_for_underlay(underlay)
                        && probe.reachable
                })
                .then_some(underlay)
        });
        if selected == peer.selected_underlay {
            continue;
        }
        match (peer.selected_underlay, selected) {
            (Some(Underlay::Silvus), Some(fallback)) => eprintln!(
                "Peer {} Silvus path interrupted; selected reachable fallback {}",
                peer.name,
                underlay_name(fallback)
            ),
            (Some(_), Some(Underlay::Silvus)) => println!(
                "Peer {} Silvus path recovered; preferred underlay restored",
                peer.name
            ),
            (None, Some(selected)) => println!(
                "Peer {} reachable on {}",
                peer.name,
                underlay_name(selected)
            ),
            (Some(previous), None) => eprintln!(
                "Peer {} lost its last reachable {} path",
                peer.name,
                underlay_name(previous)
            ),
            _ => {}
        }
        peer.selected_underlay = selected;
        peer.last_transition_at_ms = unix_time_ms();
    }
}

fn transport_for_underlay(underlay: Underlay) -> TransportKind {
    match underlay {
        Underlay::Silvus => TransportKind::Silvus,
        Underlay::Satellite => TransportKind::Satellite,
        Underlay::Ethernet => TransportKind::Ethernet,
        Underlay::Wifi => TransportKind::Wifi,
        Underlay::Other => TransportKind::Other,
    }
}

fn underlay_for_transport(transport: TransportKind) -> Option<Underlay> {
    match transport {
        TransportKind::Silvus => Some(Underlay::Silvus),
        TransportKind::Satellite => Some(Underlay::Satellite),
        TransportKind::Ethernet => Some(Underlay::Ethernet),
        TransportKind::Wifi => Some(Underlay::Wifi),
        TransportKind::Other => Some(Underlay::Other),
        _ => None,
    }
}

fn underlay_name(underlay: Underlay) -> &'static str {
    match underlay {
        Underlay::Silvus => "silvus",
        Underlay::Satellite => "satellite",
        Underlay::Ethernet => "ethernet",
        Underlay::Wifi => "wifi",
        Underlay::Other => "other",
    }
}

async fn process_emergency_commands(
    node: &PeatNode,
    node_id: &NodeId,
    commands: &mut CommandRuntime,
    status: &mut AgentStatus,
    mavlink: Option<&MavlinkCommandSender>,
    ack_sequence: &mut u64,
) {
    let records = match node.scan(DeliveryClass::Emergency).await {
        Ok(records) => records,
        Err(error) => {
            status.record_error("commands", error.to_string(), unix_time_ms());
            return;
        }
    };
    for (_, record) in records {
        let now_ms = unix_time_ms();
        if record.is_expired_at(now_ms) {
            continue;
        }
        let MeshPayload::EmergencyCommand(command) = record.payload else {
            continue;
        };
        let system_locked = mavlink_system_lock_is_fresh(status, now_ms);
        let evaluation = match commands.evaluate(&command, now_ms, system_locked) {
            Ok(value) => value,
            Err(error) => {
                status.record_error("commands", error.to_string(), now_ms);
                continue;
            }
        };
        let ack = match evaluation {
            CommandEvaluation::AlreadyProcessed => match commands.pending_ack(command.command_id) {
                Some(ack) => Some(ack),
                None => match commands.recover_interrupted(&command, now_ms) {
                    Ok(ack) => ack,
                    Err(error) => {
                        status.record_error("commands", error.to_string(), now_ms);
                        None
                    }
                },
            },
            CommandEvaluation::Rejected(ack) => {
                status.commands.rejected = status.commands.rejected.saturating_add(1);
                Some(ack)
            }
            CommandEvaluation::Accepted if commands.mode() == CommandMode::DryRun => {
                status.commands.accepted = status.commands.accepted.saturating_add(1);
                let ack = commands.ack(
                    &command,
                    AckOutcome {
                        verified: true,
                        accepted: true,
                        executed: false,
                        mavlink_result: None,
                        detail: "verified dry run; no MAVLink command sent".into(),
                    },
                    now_ms,
                );
                match commands.queue_ack(ack.clone()) {
                    Ok(()) => Some(ack),
                    Err(error) => {
                        status.record_error("commands", error.to_string(), now_ms);
                        None
                    }
                }
            }
            CommandEvaluation::Accepted if commands.mode() == CommandMode::Execute => {
                status.commands.accepted = status.commands.accepted.saturating_add(1);
                let outcome = match (mavlink, status.mavlink.target_system_id) {
                    (Some(sender), Some(system_id)) => {
                        sender
                            .return_to_launch(
                                system_id,
                                Duration::from_millis(commands.ack_timeout_ms()),
                                commands.retries(),
                            )
                            .await
                    }
                    _ => MavlinkCommandOutcome::TransportUnavailable(
                        "MAVLink command channel is unavailable".into(),
                    ),
                };
                let (executed, result, detail) = match outcome {
                    MavlinkCommandOutcome::Accepted => (
                        true,
                        Some("accepted".into()),
                        "SITL acknowledged return-to-launch".to_owned(),
                    ),
                    MavlinkCommandOutcome::Rejected(result) => (
                        false,
                        Some(result.clone()),
                        format!("SITL rejected return-to-launch: {result}"),
                    ),
                    MavlinkCommandOutcome::TimedOut => (
                        false,
                        Some("timeout".into()),
                        "SITL command acknowledgement timed out".to_owned(),
                    ),
                    MavlinkCommandOutcome::TransportUnavailable(error) => {
                        (false, Some("transport_unavailable".into()), error)
                    }
                };
                let ack = commands.ack(
                    &command,
                    AckOutcome {
                        verified: true,
                        accepted: true,
                        executed,
                        mavlink_result: result,
                        detail,
                    },
                    unix_time_ms(),
                );
                match commands.queue_ack(ack.clone()) {
                    Ok(()) => Some(ack),
                    Err(error) => {
                        status.record_error("commands", error.to_string(), unix_time_ms());
                        None
                    }
                }
            }
            CommandEvaluation::Accepted => None,
        };
        if let Some(ack) = ack {
            *ack_sequence = ack_sequence.saturating_add(1);
            status.commands.last_command_at_ms = Some(now_ms);
            status.commands.last_result = Some(ack.detail.clone());
            if publish_command_ack(node, node_id, *ack_sequence, &ack).await {
                if let Err(error) = commands.mark_ack_published(ack.command_id) {
                    status.record_error("commands", error.to_string(), unix_time_ms());
                }
            } else {
                status.record_error(
                    "commands",
                    "durable command acknowledgement publication failed",
                    unix_time_ms(),
                );
            }
        }
    }
}

async fn publish_command_ack(
    node: &PeatNode,
    node_id: &NodeId,
    sequence: u64,
    ack: &mesh_core::EmergencyAck,
) -> bool {
    let record = match AvianRecord::new(
        node_id.clone(),
        sequence,
        DeliveryClass::Acknowledgement,
        unix_time_ms(),
        MeshPayload::EmergencyAck(ack.clone()),
    ) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("Command acknowledgement rejected: {error}");
            return false;
        }
    };
    let record_id = format!("ack/{node_id}/{}", ack.command_id);
    match node.put(&record_id, &record).await {
        Ok(()) => true,
        Err(error) => {
            eprintln!("Command acknowledgement publication failed: {error}");
            false
        }
    }
}

async fn publish_operator_summary(
    node: &PeatNode,
    node_id: &NodeId,
    sequence: u64,
    swarm_members: &[NodeId],
    membership_generation: u64,
    policy: SwarmTrafficPolicy,
    observed_at_ms: u64,
) {
    let records = match node.scan(DeliveryClass::Telemetry).await {
        Ok(records) => records,
        Err(error) => {
            eprintln!("Operator summary scan failed: {error}");
            return;
        }
    };
    let swarm: std::collections::BTreeSet<NodeId> = swarm_members.iter().cloned().collect();
    let mut latest = std::collections::BTreeMap::new();
    for (_, record) in records {
        let MeshPayload::Telemetry(telemetry) = record.payload else {
            continue;
        };
        if !swarm.contains(&telemetry.source) || telemetry.timestamp_ms > observed_at_ms {
            continue;
        }
        let replace = latest
            .get(&telemetry.source)
            .is_none_or(|current: &Telemetry| telemetry.timestamp_ms >= current.timestamp_ms);
        if replace {
            latest.insert(telemetry.source.clone(), telemetry);
        }
    }

    let mut fresh_members = 0_usize;
    let mut failsafe_members = Vec::new();
    let mut low_battery_members = Vec::new();
    for (member, telemetry) in latest {
        if observed_at_ms.saturating_sub(telemetry.timestamp_ms)
            > policy.operator_summary_max_age_ms
        {
            continue;
        }
        fresh_members = fresh_members.saturating_add(1);
        if telemetry.failsafe {
            failsafe_members.push(member.clone());
        }
        if policy.low_battery_threshold.is_some_and(|threshold| {
            telemetry
                .battery_remaining
                .is_some_and(|remaining| remaining <= threshold)
        }) {
            low_battery_members.push(member);
        }
    }
    failsafe_members.truncate(policy.max_attention_members);
    low_battery_members.truncate(policy.max_attention_members);
    let summary = SwarmStatusSummary {
        publisher: node_id.clone(),
        observed_at_ms,
        membership_generation,
        configured_members: swarm.len(),
        fresh_members,
        stale_members: swarm.len().saturating_sub(fresh_members),
        failsafe_members,
        low_battery_members,
    };
    let record = match AvianRecord::new(
        node_id.clone(),
        sequence,
        DeliveryClass::Telemetry,
        observed_at_ms,
        MeshPayload::SwarmStatusSummary(summary),
    ) {
        Ok(record) => record,
        Err(error) => {
            eprintln!("Operator summary record rejected: {error}");
            return;
        }
    };
    let record_id = format!("operator-summary/{node_id}");
    if let Err(error) = node.put(&record_id, &record).await {
        eprintln!("Operator summary publication failed: {error}");
    }
}

async fn ingest_relay_observation(
    node: &PeatNode,
    node_id: &NodeId,
    sequence: u64,
    encoded: &[u8],
    traffic_policy: SwarmTrafficPolicy,
    governor: &mut RelayObservationTrafficGovernor,
) {
    let observation = match decode_relay_observation(encoded) {
        Ok(observation) => observation,
        Err(error) => {
            eprintln!("Relay observation rejected: {error}");
            return;
        }
    };
    match governor.decide(traffic_policy, &observation, unix_time_ms()) {
        Ok(RelayObservationPublication::Suppress) => return,
        Ok(RelayObservationPublication::Updated | RelayObservationPublication::StateChange) => {}
        Err(error) => {
            eprintln!("Relay observation traffic policy rejected publication: {error}");
            return;
        }
    }
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

async fn connect_unavailable_peers(
    node: &PeatNode,
    peers: &[PeerDescriptor],
    status: &mut AgentStatus,
) {
    for peer in peers {
        let was_connected = status
            .peers
            .iter()
            .find(|value| value.endpoint_id == peer.endpoint_id_hex)
            .is_some_and(|value| value.connected);
        if !node.is_peer_connected(peer) {
            let _ = node.connect(peer).await;
        }
        let connected = node.is_peer_connected(peer);
        if connected != was_connected {
            if connected {
                println!("Peer {} recovered", peer.name);
            } else {
                eprintln!("Peer {} unavailable; reconnecting", peer.name);
            }
            if let Some(peer_status) = status
                .peers
                .iter_mut()
                .find(|value| value.endpoint_id == peer.endpoint_id_hex)
            {
                peer_status.connected = connected;
                peer_status.last_transition_at_ms = unix_time_ms();
            }
        }
    }
}

fn mavlink_system_lock_is_fresh(status: &AgentStatus, now_ms: u64) -> bool {
    status.mavlink.connected
        && status.mavlink.target_system_id.is_some()
        && status
            .mavlink
            .last_message_at_ms
            .is_some_and(|at| now_ms.saturating_sub(at) <= 5_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_agent::config::ConfiguredNodeRole;

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
    fn command_system_lock_must_be_fresh() {
        let mut status = AgentStatus::new(
            "air-1".into(),
            ConfiguredNodeRole::Aircraft,
            1_000,
            CommandMode::DryRun,
            true,
            false,
            0,
        );
        status.mavlink.connected = true;
        status.mavlink.target_system_id = Some(1);
        status.mavlink.last_message_at_ms = Some(1_000);
        assert!(mavlink_system_lock_is_fresh(&status, 6_000));
        assert!(!mavlink_system_lock_is_fresh(&status, 6_001));
    }

    #[test]
    fn swarm_traffic_policy_sample_is_valid_for_the_onboard_agent() {
        let policy: SwarmTrafficPolicy = serde_json::from_str(include_str!(
            "../../../examples/swarm-traffic-policy.sample.json"
        ))
        .unwrap();

        assert_eq!(policy.priority_telemetry_interval_ms, 500);
        assert_eq!(policy.operator_summary_replicas, 3);
        assert_eq!(policy.validate(), Ok(()));
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
        assert_eq!(staged.removed_descriptor.name, paired.name);

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
}
