use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::Parser;
use mesh_agent::control::spawn_control_server;
use mesh_core::{
    Capability, DeliveryClass, InFlightRelayDecision, InFlightRelayPlanner, LinkMonitorObservation,
    MeshPayload, NodeId, NodeProfile, NodeRole, RadioAttachmentAssertion, RelayBroadcastPair,
    RelayLinkObservation, RelayObservationPublication, RelayObservationTrafficGovernor,
    RelayRuntimeAction, RelayRuntimeConfiguration, RelayRuntimeSnapshot, SwarmStatusSummary,
    SwarmTrafficPolicy, Telemetry, TelemetryPublication, TelemetryTrafficGovernor, TransportKind,
};
use mesh_transport::{AvianRecord, MeshNode, MeshNodeConfig};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::time::{self, Duration, MissedTickBehavior};
use vehicle_adapters::{
    spawn_mavlink_runtime, MavlinkCommandOutcome, MavlinkCommandSender, MavlinkEventReceiver,
    MavlinkSourceConfig, MavlinkTelemetryEvent,
};

mod membership;
mod peer_management;
mod runtime;
use peer_management::{
    enforce_ground_peer_path_policy, peer_statuses, update_peer_connection_states,
    ConnectionAttempts, PeerRegistry,
};

use membership::load_membership;
use mesh_agent::commands::{AckOutcome, CommandEvaluation, CommandRuntime};
use mesh_agent::config::{
    validate_private_file_permissions, CliArgs, CommandMode, ConfiguredNodeRole, ResolvedConfig,
    Underlay,
};
use mesh_agent::link_monitor_protocol;
use mesh_agent::paired_peers;
use mesh_agent::payload_ingress::{self, PayloadEvent};
use mesh_agent::status::{AgentStatus, RadioDeviceStatus, UnderlayStatus};

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
    let command_runtime = CommandRuntime::load(args.commands.clone(), node_id.clone())?;
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
    let node = MeshNode::start(MeshNodeConfig {
        name: args.name.clone(),
        formation_id: args.formation_id.clone(),
        base64_shared_key: formation_key,
        bind_address: args.bind,
        storage_path: args.storage.clone(),
    })
    .await
    .context("starting AVIAN mesh transport node")?;
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
        .context("reading local mesh transport address")?;
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
                "Membership generation {} selected {} direct mesh transport neighbors",
                selection.generation,
                selection.peers.len()
            );
            (selection.peers, selection.members, selection.generation)
        } else {
            (args.peers.clone(), vec![node_id.clone()], 0)
        };
    let paired_peer_path = args.storage.join("paired-peers.json");
    let persisted_paired_peers = paired_peers::load(&paired_peer_path)?;
    anyhow::ensure!(
        args.role == ConfiguredNodeRole::Ground || persisted_paired_peers.is_empty(),
        "paired peers are permitted only on a ground node"
    );
    anyhow::ensure!(
        args.membership_file.is_none() || persisted_paired_peers.is_empty(),
        "paired peers cannot be combined with a managed membership file"
    );
    let dynamic_peer_count = persisted_paired_peers
        .iter()
        .filter(|paired| !args.peers.iter().any(|peer| peer.name == paired.name))
        .count();
    anyhow::ensure!(
        peers.len().saturating_add(dynamic_peer_count) <= args.max_mesh_peers,
        "configured peer limit is exceeded by persisted paired aircraft"
    );
    let mut tagged_peers = args.tagged_peers.clone();
    for paired_peer in persisted_paired_peers.iter().cloned() {
        peer_management::merge_persisted_peer(
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
    if let Some(attachment) = &args.radio.attachment {
        publish_radio_attachment(&node, &node_id, attachment).await?;
    }
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
    let mut paired_path_enforcement = time::interval(Duration::from_millis(250));
    paired_path_enforcement.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut telemetry_publish = time::interval(Duration::from_secs_f64(1.0 / args.telemetry_hz));
    telemetry_publish.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut operator_summary_publish = time::interval(Duration::from_millis(
        traffic_policy.operator_summary_interval_ms,
    ));
    operator_summary_publish.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut relay_evaluation = time::interval(Duration::from_millis(args.relay_evaluation_ms));
    relay_evaluation.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let (mut mavlink_receiver, mavlink_commands) = start_mavlink(&args, node_id.clone())?;
    let mut latest_telemetry: Option<Telemetry> = None;
    let mut telemetry_governor = TelemetryTrafficGovernor::default();
    let mut telemetry_sequence = 0_u64;
    let mut operator_summary_sequence = 0_u64;
    let mut relay_observation_sequence = 0_u64;
    let mut relay_observation_governor = RelayObservationTrafficGovernor::default();
    let mut relay_observation_buffer = vec![0_u8; 65_535];
    let mut payload_sequence = 0_u64;
    let mut payload_buffer = vec![0_u8; args.sockets.max_message_bytes.saturating_add(1)];
    let mut link_observation_buffer = vec![0_u8; args.sockets.max_message_bytes.saturating_add(1)];
    let registry = PeerRegistry {
        persisted: persisted_paired_peers,
        peers: peers.clone(),
        tagged: tagged_peers,
        path: paired_peer_path,
    };
    let mut control_runtime = runtime::ControlRuntime::start(
        node.clone(),
        args.clone(),
        registry,
        command_runtime,
        status.clone(),
        mavlink_commands,
    );
    let mut connection_tasks = ConnectionAttempts::default();
    let shutdown = runtime::shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            signal = &mut shutdown => {
                signal.context("waiting for shutdown signal")?;
                break;
            }
            _ = peer_retry.tick() => {
                connection_tasks.schedule(&node, &peers, &mut status);
            }
            _ = paired_path_enforcement.tick(), if args.role == ConfiguredNodeRole::Ground && args.membership_file.is_none() => {
                enforce_ground_peer_path_policy(&node, &mut status);
                update_peer_connection_states(&node, &mut status);
            }
            control = control_receiver.recv() => {
                if let Some(control) = control {
                    if args.role == ConfiguredNodeRole::Ground && args.membership_file.is_none() {
                        enforce_ground_peer_path_policy(&node, &mut status);
                    }
                    update_peer_connection_states(&node, &mut status);
                    runtime::dispatch_control(control, &status, &control_runtime.requests);
                }
            }
            update = control_runtime.updates.recv() => {
                let Some(update) = update else { anyhow::bail!("control worker stopped unexpectedly") };
                if let Some((tagged, configured)) = update.peer_registry {
                    for previous in &peers {
                        if !configured.iter().any(|peer| peer.endpoint_id_hex == previous.endpoint_id_hex) {
                            let _ = node.disconnect(previous);
                        }
                    }
                    peers = configured;
                    peer_management::refresh_peer_statuses(&node, &mut status, &tagged, &peers);
                    connection_tasks.schedule(&node, &peers, &mut status);
                }
                status.commands = update.commands;
                for error in update.errors {
                    status.record_error(&error.component, error.detail, error.at_ms);
                }
                // Acknowledge mutations only after their configuration is
                // installed in the reactor's admission and status view.
                if let Some((respond_to, response)) = update.response {
                    let _ = respond_to.send(response);
                }
            }
            _ = connection_tasks.reap(), if connection_tasks.has_tasks() => {}
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
                                status.telemetry.last_error = Some("mesh transport publication failed".into());
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
        control_runtime.update_mavlink(status.mavlink.clone());
    }
    control_task.abort();
    let _ = control_task.await;
    control_runtime
        .shutdown()
        .await
        .context("joining control worker")?;
    connection_tasks.shutdown().await;

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
) -> anyhow::Result<(Option<MavlinkEventReceiver>, Option<MavlinkCommandSender>)> {
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
    node: &MeshNode,
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
    node: &MeshNode,
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

async fn publish_radio_attachment(
    node: &MeshNode,
    node_id: &NodeId,
    configured: &mesh_agent::config::RadioAttachmentConfig,
) -> anyhow::Result<()> {
    let mut assertion = RadioAttachmentAssertion::new(
        unix_time_ms(),
        node_id.clone(),
        configured.drone_id.clone(),
        configured.mac_address.clone(),
        configured.radio_node_id.clone(),
    )?;
    if let Some(serial_number) = configured.radio_serial_number.as_deref() {
        assertion = assertion.with_serial_number(serial_number);
    }
    let record = AvianRecord::new(
        node_id.clone(),
        1,
        DeliveryClass::Mission,
        unix_time_ms(),
        MeshPayload::RadioAttachmentAssertion(assertion),
    )?;
    node.put(&format!("radio-attachment/{node_id}"), &record)
        .await
        .context("publishing local radio attachment assertion")?;
    Ok(())
}

async fn ingest_payload_event(
    node: &MeshNode,
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
    node: &MeshNode,
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
    node: &MeshNode,
    node_id: &NodeId,
    commands: &mut CommandRuntime,
    status: &mut AgentStatus,
    mavlink: Option<&MavlinkCommandSender>,
    ack_sequence: &mut u64,
    mavlink_status: &tokio::sync::watch::Receiver<mesh_agent::status::MavlinkStatus>,
) {
    let records = match node.scan(DeliveryClass::Emergency).await {
        Ok(records) => records,
        Err(error) => {
            status.record_error("commands", error.to_string(), unix_time_ms());
            return;
        }
    };
    for (_, record) in records {
        status.mavlink = mavlink_status.borrow().clone();
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
    node: &MeshNode,
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
    node: &MeshNode,
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
    node: &MeshNode,
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
    node: &MeshNode,
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
    fn transport_mapping_retains_the_supported_underlay_vocabulary() {
        for (transport, underlay, name) in [
            (TransportKind::Silvus, Underlay::Silvus, "silvus"),
            (TransportKind::Satellite, Underlay::Satellite, "satellite"),
            (TransportKind::Ethernet, Underlay::Ethernet, "ethernet"),
            (TransportKind::Wifi, Underlay::Wifi, "wifi"),
            (TransportKind::Other, Underlay::Other, "other"),
        ] {
            assert_eq!(underlay_for_transport(transport), Some(underlay));
            assert_eq!(underlay_name(underlay), name);
        }
    }

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
}
