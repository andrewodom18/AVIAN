//! Reconcile owned connectors without granting trust to discovery advertisements.
use super::*;
use avian_connectivity::{
    ConnectionInventory, DiscoveryMessage, Endpoint, Underlay,
    bootstrap::{BootstrapCoordinator, DiscoverySocket},
};
use std::collections::{BTreeMap, BTreeSet};

pub(super) async fn run(
    configuration: RuntimeConfiguration,
    lifecycle: Arc<Mutex<Lifecycle>>,
    network_id: String,
    probe_budget: Arc<Mutex<ProbeBudget>>,
    probe_slot: Arc<tokio::sync::Semaphore>,
) {
    let mut bootstrap = configuration.bootstrap.clone();
    for peer in &configuration.peers {
        if let Ok(endpoint) = Endpoint::parse(&format!("tcp://{peer}")) {
            bootstrap.owned_endpoints.push(endpoint);
        }
    }
    let Ok(mut coordinator) = BootstrapCoordinator::new(
        bootstrap,
        network_id.clone(),
        configuration.discovery_hint_budget,
        configuration.connection_budget,
    ) else {
        return;
    };
    let mut inventory = ConnectionInventory::default();
    let mut applied = BTreeSet::<String>::new();
    let mut sockets = BTreeMap::<(String, std::net::Ipv4Addr), DiscoverySocket>::new();
    let started = tokio::time::Instant::now();
    let mut probe_candidates = std::collections::VecDeque::<(String, u32)>::new();
    let mut link_cursor = None::<String>;
    let probe_start = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut probe_states = BTreeMap::<String, ProbeState>::new();
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let mut owner = lifecycle.lock().await;
        if owner.phase != RuntimePhase::Running
            || owner
                .child
                .as_mut()
                .is_none_or(|child| !matches!(child.try_wait(), Ok(None)))
        {
            break;
        }
        // The owner check spans connector mutations, preventing stop/restart from racing
        // an old coordinator into the replacement engine's RPC portal.
        if !inspect(&configuration, "node")
            .await
            .is_ok_and(|node| node_matches(&configuration, &node))
        {
            continue;
        }
        let observations = collect_underlays(&configuration).await;
        let Ok(observations) = observations else {
            let _ = fs::remove_file(configuration.policy_file());
            for url in applied.clone().iter().take(8) {
                if mutate_connector(&configuration, "remove", url)
                    .await
                    .is_ok()
                {
                    applied.remove(url);
                }
            }
            sockets.clear();
            if configuration.network_helper_socket.is_some() {
                let _ = reconcile_listeners(&configuration, &[]).await;
            }
            continue;
        };
        if inventory
            .update(observations, &configuration.interface_name)
            .is_err()
        {
            let _ = fs::remove_file(configuration.policy_file());
            continue;
        }
        let underlays: Vec<Underlay> = inventory.interfaces().cloned().collect();
        if publish_policy(&configuration, &underlays).is_err() {
            let _ = fs::remove_file(configuration.policy_file());
            continue;
        }
        let confirmed_listeners = if configuration.network_helper_socket.is_some() {
            reconcile_listeners(&configuration, &underlays)
                .await
                .unwrap_or_default()
        } else {
            BTreeSet::new()
        };
        let now_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
        if configuration.execution == ExecutionMode::Native
            && configuration.bootstrap.local_discovery
        {
            let active: BTreeSet<_> = underlays
                .iter()
                .filter(|underlay| underlay.available)
                .flat_map(|underlay| {
                    underlay
                        .addresses
                        .iter()
                        .filter_map(|address| match address {
                            IpAddr::V4(address) if address.is_private() => {
                                Some((underlay.interface_name.clone(), *address))
                            }
                            _ => None,
                        })
                })
                .collect();
            sockets.retain(|key, _| active.contains(key));
            for (name, address) in active {
                let key = (name.clone(), address);
                if !sockets.contains_key(&key)
                    && let Some(underlay) = underlays
                        .iter()
                        .find(|underlay| underlay.interface_name == name)
                    && let Ok(socket) = DiscoverySocket::bind(underlay.clone(), address)
                {
                    sockets.insert(key.clone(), socket);
                }
                if let Some(socket) = sockets.get_mut(&key) {
                    if let Ok(messages) = socket.poll() {
                        for (bytes, source) in messages {
                            let _ =
                                coordinator.accept_hint(&bytes, source, socket.underlay(), now_ms);
                        }
                    }
                    if configuration.listen.ip().is_unspecified()
                        || configuration.listen.ip() == IpAddr::V4(address)
                    {
                        let endpoints = if configuration.network_helper_socket.is_some() {
                            confirmed_listeners
                                .iter()
                                .filter_map(|url| {
                                    let public = url.split('?').next()?;
                                    let endpoint = Endpoint::parse(public).ok()?;
                                    let tcp =
                                        format!("tcp://{address}:{}", configuration.listen.port());
                                    let udp =
                                        format!("udp://{address}:{}", configuration.listen.port());
                                    (public == tcp || public == udp).then_some(endpoint)
                                })
                                .collect()
                        } else {
                            ["tcp", "udp"]
                                .into_iter()
                                .filter_map(|scheme| {
                                    Endpoint::parse(&format!(
                                        "{scheme}://{address}:{}",
                                        configuration.listen.port()
                                    ))
                                    .ok()
                                })
                                .collect()
                        };
                        let _ = socket
                            .advertise(&DiscoveryMessage {
                                version: 1,
                                network_id: network_id.clone(),
                                endpoints,
                            })
                            .await;
                    }
                }
            }
        }
        if coordinator
            .reconcile(
                &underlays,
                &[IpAddr::V4(configuration.overlay_address)],
                now_ms,
            )
            .is_err()
        {
            let _ = fs::remove_file(configuration.policy_file());
            continue;
        }
        let desired: BTreeSet<_> = coordinator
            .desired()
            .map(|connection| connection.engine_url.clone())
            .collect();
        let removed: Vec<_> = applied.difference(&desired).take(8).cloned().collect();
        for url in removed {
            if mutate_connector(&configuration, "remove", &url)
                .await
                .is_ok()
            {
                applied.remove(&url);
            }
        }
        // Keep failed removals ahead of additions. A stale connector is never forgotten.
        if applied.iter().any(|url| !desired.contains(url)) {
            continue;
        }
        let added: Vec<_> = desired.difference(&applied).take(8).cloned().collect();
        for url in added {
            if mutate_connector(&configuration, "add", &url).await.is_ok() {
                applied.insert(url);
            }
        }
        drop(owner);
        if configuration.automatic_capacity_probes && tokio::time::Instant::now() >= probe_start {
            let now = tokio::time::Instant::now();
            probe_states.retain(|_, state| now < state.next_attempt + Duration::from_secs(60));
            if probe_candidates.is_empty() {
                if let Ok(arguments) = page_arguments("links", link_cursor.as_deref(), 100)
                    && let Ok(page) =
                        inspect_arguments(&configuration, &arguments, Duration::from_secs(1)).await
                {
                    link_cursor = page["next_cursor"].as_str().map(str::to_owned);
                    for link in page["items"].as_array().into_iter().flatten() {
                        if let Some(candidate) = eligible_probe_candidate(link) {
                            probe_candidates.push_back(candidate);
                        }
                    }
                } else {
                    link_cursor = None;
                }
            }
            if let Some((connection, peer)) = probe_candidates.pop_front() {
                if !probe_states.contains_key(&connection)
                    && probe_states.len() >= configuration.connection_budget
                {
                    continue;
                }
                let state = probe_states
                    .entry(connection.clone())
                    .or_insert(ProbeState {
                        offered_bps: 64_000,
                        next_attempt: now,
                    });
                if now < state.next_attempt {
                    continue;
                }
                state.next_attempt = now + Duration::from_secs(30);
                let request = LinkProbeRequest {
                    peer_id: peer,
                    connection_id: Some(connection),
                    duration_ms: 50,
                    offered_bps: state.offered_bps,
                    allow_metered: false,
                };
                let duration_ms = request.duration_ms;
                let result = perform_probe(
                    &configuration,
                    &lifecycle,
                    &probe_budget,
                    &probe_slot,
                    request,
                )
                .await;
                state.offered_bps =
                    next_probe_rate(state.offered_bps, duration_ms, result.as_ref().ok());
            }
        }
    }
}

struct ProbeState {
    offered_bps: u64,
    next_attempt: tokio::time::Instant,
}

fn publish_policy(configuration: &RuntimeConfiguration, underlays: &[Underlay]) -> Result<()> {
    let addresses: BTreeSet<_> = configuration
        .metered_addresses
        .iter()
        .copied()
        .chain(
            underlays
                .iter()
                .filter(|underlay| underlay.metering.restrict_best_effort())
                .flat_map(|underlay| underlay.addresses.iter().copied()),
        )
        .collect();
    let policy = serde_json::json!({"metered_addresses":addresses,"control_tcp_ports":configuration.control_tcp_ports,
        "allow_metered_video":configuration.allow_metered_video,"allow_metered_bulk":configuration.allow_metered_bulk});
    write_private(&configuration.policy_file(), &serde_json::to_vec(&policy)?)
}

async fn mutate_connector(
    configuration: &RuntimeConfiguration,
    action: &str,
    url: &str,
) -> Result<()> {
    mutate_resource(configuration, "connector", action, url).await
}

async fn mutate_resource(
    configuration: &RuntimeConfiguration,
    resource: &str,
    action: &str,
    url: &str,
) -> Result<()> {
    let mut child = namespace_command(configuration, "easytier-cli")
        .args([
            "--rpc-portal",
            &configuration.rpc_listen.to_string(),
            resource,
            action,
            url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let status = timeout(Duration::from_secs(2), child.wait())
        .await
        .map_err(|_| RuntimeError::InspectionFailed)??;
    if !status.success() {
        return Err(RuntimeError::InspectionFailed);
    }
    Ok(())
}

async fn local_listeners(configuration: &RuntimeConfiguration) -> Result<BTreeSet<String>> {
    let value = inspect_arguments(
        configuration,
        &["listener".into(), "local-list".into()],
        Duration::from_secs(1),
    )
    .await?;
    fn strings(value: &serde_json::Value, found: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::String(value) => {
                found.insert(value.clone());
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    strings(value, found);
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values() {
                    strings(value, found);
                }
            }
            _ => {}
        }
    }
    let mut found = BTreeSet::new();
    strings(&value, &mut found);
    found.retain(|url| url.starts_with("tcp://") || url.starts_with("udp://"));
    Ok(found)
}

async fn reconcile_listeners(
    configuration: &RuntimeConfiguration,
    underlays: &[Underlay],
) -> Result<BTreeSet<String>> {
    let mut desired = BTreeSet::new();
    for underlay in underlays.iter().filter(|underlay| underlay.available) {
        for address in &underlay.addresses {
            if address.is_loopback()
                || address.is_unspecified()
                || address.is_multicast()
                || !(configuration.listen.ip().is_unspecified()
                    || configuration.listen.ip() == *address)
            {
                continue;
            }
            for scheme in ["tcp", "udp"] {
                if desired.len() >= configuration.connection_budget {
                    break;
                }
                let local = SocketAddr::new(*address, configuration.listen.port());
                desired.insert(format!(
                    "{scheme}://{local}?avian_bind={address}&avian_interface={}",
                    underlay.interface_name
                ));
            }
        }
    }
    let mut actual = local_listeners(configuration).await?;
    for url in actual
        .difference(&desired)
        .take(8)
        .cloned()
        .collect::<Vec<_>>()
    {
        if mutate_resource(configuration, "listener", "local-remove", &url)
            .await
            .is_ok()
        {
            actual.remove(&url);
        }
    }
    if actual.iter().any(|url| !desired.contains(url)) {
        return Ok(BTreeSet::new());
    }
    for url in desired.difference(&actual).take(8) {
        let _ = mutate_resource(configuration, "listener", "local-add", url).await;
    }
    // Advertise only listeners confirmed by the engine after the mutation completes.
    Ok(local_listeners(configuration)
        .await?
        .intersection(&desired)
        .cloned()
        .collect())
}
