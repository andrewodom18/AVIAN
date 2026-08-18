use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::Parser;
use mesh_agent::control::{spawn_control_server, ControlEnvelope};
use mesh_core::{
    Capability, DeliveryClass, InFlightRelayDecision, InFlightRelayPlanner, MeshPayload, NodeId,
    NodeProfile, NodeRole, RelayBroadcastPair, RelayLinkObservation, RelayObservationPublication,
    RelayObservationTrafficGovernor, RelayRuntimeAction, RelayRuntimeConfiguration,
    RelayRuntimeSnapshot, SwarmStatusSummary, SwarmTrafficPolicy, Telemetry, TelemetryPublication,
    TelemetryTrafficGovernor,
};
use mesh_peat::{AvianRecord, PeatNode, PeatNodeConfig, PeerDescriptor};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};
use vehicle_adapters::{spawn_mavlink_source, MavlinkSourceConfig, MavlinkTelemetryEvent};

mod membership;

use membership::load_membership;
use mesh_agent::config::{CliArgs, ResolvedConfig};
use mesh_agent::payload_ingress::{self, PayloadEvent};
use mesh_agent::protocol::{ControlRequest, ControlResponse, RecordView};
use mesh_agent::status::{AgentStatus, PeerAddressStatus, PeerStatus};

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
    let (peers, swarm_members, membership_generation) = if let Some(path) = &args.membership_file {
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
    let started_at_ms = unix_time_ms();
    let mut status = AgentStatus::new(
        args.name.clone(),
        args.role,
        started_at_ms,
        args.commands.mode,
        args.mavlink_address.is_some(),
        args.radio.enabled,
    );
    status.node.endpoint_id = Some(node.endpoint_id_hex());
    status.peers = peer_statuses(&args, &peers, started_at_ms);
    publish_node_advertisement(&node, &node_id, node_profile(&args, &node_id)?).await?;
    let (control_sender, mut control_receiver) = mpsc::channel(32);
    let control_task = spawn_control_server(
        args.sockets.control.clone(),
        args.sockets.max_message_bytes,
        control_sender,
    )
    .await?;
    let payload_socket = payload_ingress::bind(&args.sockets.payload)?;

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
    let mut mavlink_receiver = start_mavlink(&args, node_id.clone())?;
    let mut latest_telemetry: Option<Telemetry> = None;
    let mut telemetry_governor = TelemetryTrafficGovernor::default();
    let mut telemetry_sequence = 0_u64;
    let mut operator_summary_sequence = 0_u64;
    let mut relay_observation_sequence = 0_u64;
    let mut relay_observation_governor = RelayObservationTrafficGovernor::default();
    let mut relay_observation_buffer = vec![0_u8; 65_535];
    let mut payload_sequence = 0_u64;
    let mut payload_buffer = vec![0_u8; args.sockets.max_message_bytes.saturating_add(1)];
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
                    handle_control_request(&node, &status, control).await;
                }
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
    config: &ResolvedConfig,
    peers: &[PeerDescriptor],
    now_ms: u64,
) -> Vec<PeerStatus> {
    peers
        .iter()
        .map(|peer| {
            let tagged = config
                .tagged_peers
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

async fn handle_control_request(node: &PeatNode, status: &AgentStatus, envelope: ControlEnvelope) {
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
        ControlRequest::EmergencyRtl { .. } => ControlResponse::Error {
            code: "commands_unavailable".into(),
            detail: "emergency command handling is disabled until configured".into(),
        },
    };
    let _ = envelope.respond_to.send(response);
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
            if let Err(error) = node.connect(peer).await {
                if was_connected {
                    eprintln!("Peer {} lost: {error}", peer.name);
                }
            }
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
}
