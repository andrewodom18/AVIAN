use super::*;

pub(super) fn management_host(url: &str) -> Option<String> {
    let authority = url
        .split_once("://")
        .map_or(url, |(_, authority)| authority)
        .split(['/', '?', '#'])
        .next()?
        .rsplit('@')
        .next()?
        .trim();
    if authority.is_empty() {
        return None;
    }
    if let Some(bracketed) = authority.strip_prefix('[') {
        return bracketed.split(']').next().map(str::to_owned);
    }
    Some(authority.split(':').next().unwrap_or(authority).to_owned())
}

pub(super) async fn observe_local_mesh(
    state: &Arc<RwLock<ServiceState>>,
    observed_at_ms: u64,
) -> Option<StreamCasterMeshObservation> {
    let (source, backend, management_address, mesh, peers, latest_position) = {
        let state = state.read().await;
        (
            state.source.clone(),
            state.backend.clone(),
            state.management_address.clone(),
            state.mesh.clone(),
            state.mesh_peers.clone(),
            state.latest_position,
        )
    };

    let (capabilities, effective, rf_links, error, simulated) = match backend {
        Backend::ContractOnly => (None, None, Vec::new(), None, false),
        Backend::Simulator(api) => {
            let capabilities = api.read_capabilities(observed_at_ms).await.ok();
            let effective = api.read_effective_settings(observed_at_ms).await.ok();
            (capabilities, effective, Vec::new(), None, true)
        }
    };

    let endpoint_id = mesh.as_ref().map(|node| node.endpoint_id_hex());
    let links = peers
        .iter()
        .map(|peer| {
            let connected = mesh
                .as_ref()
                .is_some_and(|node| node.is_peer_connected(peer));
            StreamCasterPeerLink {
                source: source.clone(),
                source_endpoint_id: endpoint_id.clone(),
                target: peer.name.clone(),
                target_endpoint_id: peer.endpoint_id_hex.clone(),
                target_addresses: peer.addresses().iter().map(ToString::to_string).collect(),
                transport: "mesh_over_streamcaster".into(),
                state: if connected {
                    "connected"
                } else {
                    "disconnected"
                }
                .into(),
                observed_at_ms,
            }
        })
        .collect::<Vec<_>>();
    let status = if effective.is_some() {
        StreamCasterObservedStatus::Online
    } else {
        StreamCasterObservedStatus::Unreachable
    };

    Some(StreamCasterMeshObservation {
        schema_version: STREAMCASTER_MESH_OBSERVATION_SCHEMA_VERSION,
        observed_at_ms,
        source: source.clone(),
        capacity_requirement_nodes: STREAMCASTER_CAPACITY_REQUIREMENT_NODES,
        simulated,
        node: StreamCasterObservedNode {
            node_key: source,
            management_ip: management_address,
            status,
            last_seen_ms: observed_at_ms,
            mesh_endpoint_id: endpoint_id,
            mesh_connected_peers: mesh.as_ref().map_or(0, |node| node.peer_count()),
            position: latest_position.filter(|position| {
                position.observed_at_ms <= observed_at_ms.saturating_add(POSITION_MAX_CLOCK_SKEW_MS)
                    && observed_at_ms.saturating_sub(position.observed_at_ms) <= POSITION_FRESH_MS
            }),
            radio: StreamCasterObservedRadio {
                node_id: effective.as_ref().and_then(|settings| settings.node_id),
                system_name: effective
                    .as_ref()
                    .and_then(|settings| settings.system_name.clone()),
                network_id: effective
                    .as_ref()
                    .map(|settings| settings.network_id.clone()),
                center_frequency_mhz: effective
                    .as_ref()
                    .map(|settings| settings.center_frequency_mhz),
                bandwidth_mhz: effective.as_ref().map(|settings| settings.bandwidth_mhz),
                link_distance_m: effective.as_ref().map(|settings| settings.link_distance_m),
                antenna_mask: effective.as_ref().map(|settings| settings.antenna_mask),
                transmit_power_dbm_per_port: effective
                    .as_ref()
                    .and_then(|settings| settings.transmit_power_dbm_per_port),
                model: capabilities.as_ref().and_then(|value| value.model),
                firmware_version: capabilities
                    .as_ref()
                    .and_then(|value| value.firmware_version.clone()),
            },
        },
        links,
        rf_links,
        error,
    })
}

pub(super) fn observed_position(
    payload: &[u8],
    received_at_ms: u64,
) -> Option<StreamCasterObservedPosition> {
    let value: serde_json::Value = serde_json::from_slice(payload).ok()?;
    let position = value.get("position")?;
    let latitude_deg = position.get("lat")?.as_f64()?;
    let longitude_deg = position.get("lon")?.as_f64()?;
    if !latitude_deg.is_finite()
        || !longitude_deg.is_finite()
        || !(-90.0..=90.0).contains(&latitude_deg)
        || !(-180.0..=180.0).contains(&longitude_deg)
    {
        return None;
    }
    let altitude_msl_m = position
        .get("alt_msl_m")
        .and_then(serde_json::Value::as_f64)
        .filter(|altitude| altitude.is_finite());
    Some(StreamCasterObservedPosition {
        latitude_deg,
        longitude_deg,
        altitude_msl_m,
        observed_at_ms: value
            .get("timestamp_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(received_at_ms),
    })
}

pub(super) fn effective_observation(
    observation: &StreamCasterMeshObservation,
) -> serde_json::Value {
    json!({
        "observed_at_ms": observation.observed_at_ms,
        "node_id": observation.node.radio.node_id,
        "system_name": observation.node.radio.system_name,
        "network_id": observation.node.radio.network_id,
        "center_frequency_mhz": observation.node.radio.center_frequency_mhz,
        "bandwidth_mhz": observation.node.radio.bandwidth_mhz,
        "link_distance_m": observation.node.radio.link_distance_m,
        "antenna_mask": observation.node.radio.antenna_mask,
        "transmit_power_dbm_per_port": observation.node.radio.transmit_power_dbm_per_port,
        "model": observation.node.radio.model,
        "firmware_version": observation.node.radio.firmware_version,
    })
}
