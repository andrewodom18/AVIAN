use super::*;

pub(super) async fn persist_plan(
    state: &Arc<RwLock<ServiceState>>,
    plan: &ArcRadioConfiguration,
    now_ms: u64,
) {
    let (mesh, source, sequence) = {
        let state = state.read().await;
        (
            state.mesh.clone(),
            state.source.clone(),
            state.sequence.fetch_add(1, Ordering::Relaxed),
        )
    };
    let Some(mesh) = mesh else {
        return;
    };
    match AvianRecord::new(
        source,
        sequence,
        DeliveryClass::Mission,
        now_ms,
        MeshPayload::RadioConfiguration(plan.clone()),
    ) {
        Ok(record) => {
            if let Err(error) = mesh.put("streamcaster-plan-current", &record).await {
                tracing::warn!(%error, "failed to persist StreamCaster plan through AVIAN");
            } else if let Err(error) = mesh.sync_now().await {
                tracing::warn!(%error, "failed to request AVIAN plan sync");
            }
        }
        Err(error) => tracing::warn!(%error, "failed to create AVIAN StreamCaster record"),
    }
}

pub(super) async fn newest_mesh_plan(
    mesh: &MeshNode,
) -> anyhow::Result<Option<ArcRadioConfiguration>> {
    let mut plans = mesh
        .scan(DeliveryClass::Mission)
        .await?
        .into_iter()
        .filter_map(|(_, record)| match record.payload {
            MeshPayload::RadioConfiguration(plan) => Some(plan),
            _ => None,
        })
        .collect::<Vec<_>>();
    plans.sort_by_key(|plan| plan.generation);
    Ok(plans.pop())
}

pub(super) async fn persist_mesh_observation(
    state: &Arc<RwLock<ServiceState>>,
    observation: &StreamCasterMeshObservation,
) {
    let (mesh, sequence) = {
        let state = state.read().await;
        (
            state.mesh.clone(),
            state.sequence.fetch_add(1, Ordering::Relaxed),
        )
    };
    let Some(mesh) = mesh else {
        return;
    };
    let record = AvianRecord::new(
        observation.source.clone(),
        sequence,
        DeliveryClass::Telemetry,
        observation.observed_at_ms,
        MeshPayload::StreamCasterMeshObservation(observation.clone()),
    );
    let record_id = format!("streamcaster-mesh/{}", observation.source);
    match record {
        Ok(record) => {
            if let Err(error) = mesh.put(&record_id, &record).await {
                tracing::warn!(%error, "failed to persist StreamCaster mesh observation through AVIAN");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to create AVIAN StreamCaster mesh observation")
        }
    }
}

pub(super) async fn replicated_mesh_observations(
    mesh: &MeshNode,
) -> anyhow::Result<Vec<StreamCasterMeshObservation>> {
    Ok(mesh
        .scan(DeliveryClass::Telemetry)
        .await?
        .into_iter()
        .filter_map(|(_, record)| match record.payload {
            MeshPayload::StreamCasterMeshObservation(observation) => Some(observation),
            _ => None,
        })
        .collect())
}

pub(super) async fn start_mesh(
    args: &Args,
) -> anyhow::Result<(Option<Arc<MeshNode>>, Vec<PeerDescriptor>)> {
    let options_present = [
        args.mesh_formation_id.is_some(),
        args.mesh_formation_key_file.is_some(),
        args.mesh_bind.is_some(),
        args.mesh_storage.is_some(),
    ];
    if !options_present.iter().any(|present| *present) {
        return Ok((None, Vec::new()));
    }
    if !options_present.iter().all(|present| *present) {
        bail!("AVIAN formation ID, key file, bind address, and storage are required together");
    }
    let key_path = args.mesh_formation_key_file.as_ref().expect("checked");
    let key = std::fs::read_to_string(key_path)
        .with_context(|| format!("read AVIAN formation key {}", key_path.display()))?;
    let mesh = Arc::new(
        MeshNode::start(MeshNodeConfig {
            name: args.source.clone(),
            formation_id: args.mesh_formation_id.clone().expect("checked"),
            base64_shared_key: key.trim().to_owned(),
            bind_address: args.mesh_bind.expect("checked"),
            storage_path: args.mesh_storage.clone().expect("checked"),
        })
        .await?,
    );
    let mut peers = Vec::with_capacity(args.mesh_peer.len());
    for peer in &args.mesh_peer {
        let peer: PeerDescriptor = peer.parse()?;
        peers.push(peer);
    }
    Ok((Some(mesh), peers))
}

pub(super) async fn reconnect_mesh_peers(mesh: Arc<MeshNode>, state: &Arc<RwLock<ServiceState>>) {
    let peers = state.read().await.mesh_peers.clone();
    let mut attempts = tokio::task::JoinSet::new();
    for peer in peers {
        if mesh.is_peer_connected(&peer) {
            continue;
        }
        let mesh = Arc::clone(&mesh);
        attempts.spawn(async move {
            let name = peer.name.clone();
            let result = tokio::time::timeout(MESH_RECONNECT_TIMEOUT, mesh.connect(&peer)).await;
            (name, result)
        });
    }
    while let Some(attempt) = attempts.join_next().await {
        match attempt {
            Ok((_, Ok(Ok(_)))) => {}
            Ok((peer, Ok(Err(error)))) => {
                tracing::debug!(%peer, %error, "AVIAN peer remains unavailable");
            }
            Ok((peer, Err(_))) => {
                tracing::debug!(%peer, "AVIAN peer connection attempt timed out");
            }
            Err(error) => {
                tracing::warn!(%error, "AVIAN peer reconnect task failed");
            }
        }
    }
}
