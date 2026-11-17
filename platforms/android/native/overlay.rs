//! Overlay HTTP is intentionally not protected or bound to a physical Android Network.
#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, VecDeque},
    net::SocketAddr,
    sync::{Arc, Mutex, OnceLock, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, bail};
use avian::services::{LocalPublication, ServiceConfiguration, ServiceDirectory};
use avian_membership::MembershipStore;
use avian_services::{AnnouncementPage, FetchRequest, ServicePage};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Query, State},
    http::{HeaderMap, StatusCode, header},
    routing::get,
};
use serde::{Deserialize, Serialize};

use super::Configuration;
use std::sync::atomic::{AtomicU8, Ordering};
static TUN_STATE: AtomicU8 = AtomicU8::new(0);
pub fn tun_state() -> &'static str {
    match TUN_STATE.load(Ordering::Acquire) {
        1 => "ready",
        2 => "rejected",
        _ => "waiting",
    }
}
async fn observe_engine(
    mut receiver: tokio::sync::broadcast::Receiver<easytier::common::global_ctx::GlobalCtxEvent>,
    state: Arc<OverlayState>,
) {
    use easytier::common::global_ctx::GlobalCtxEvent;
    loop {
        match receiver.recv().await {
            Ok(GlobalCtxEvent::TunDeviceReady(_)) => {
                TUN_STATE.store(1, Ordering::Release);
                event(
                    "tun_ready",
                    state
                        .snapshot()
                        .map_or(0, |membership| membership.revocation_revision()),
                );
            }
            Ok(GlobalCtxEvent::TunDeviceError(error)) => {
                TUN_STATE.store(2, Ordering::Release);
                event_with_detail(
                    "tun_rejected",
                    state
                        .snapshot()
                        .map_or(0, |membership| membership.revocation_revision()),
                    Some(
                        serde_json::json!({"operation":"mobile_tun_setup","error":error.chars().take(256).collect::<String>()}),
                    ),
                );
            }
            Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayPeer {
    pub node_id: String,
    pub service_endpoint: SocketAddr,
    pub revocation_endpoint: SocketAddr,
}

#[derive(Clone, Serialize)]
pub struct RuntimeEvent {
    pub sequence: u64,
    pub kind: String,
    pub at_unix: i64,
    pub revocation_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}

#[derive(Default)]
struct EventBuffer {
    sequence: u64,
    entries: VecDeque<RuntimeEvent>,
}
static EVENT_EPOCH: OnceLock<String> = OnceLock::new();
static EVENTS: Mutex<EventBuffer> = Mutex::new(EventBuffer {
    sequence: 0,
    entries: VecDeque::new(),
});

pub fn now_unix() -> Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .try_into()?)
}

pub fn event(kind: &str, revision: u64) {
    event_with_detail(kind, revision, None);
}

pub fn event_with_detail(kind: &str, revision: u64, detail: Option<serde_json::Value>) {
    if let Ok(mut events) = EVENTS.lock() {
        events.sequence = events.sequence.saturating_add(1);
        let sequence = events.sequence;
        events.entries.push_back(RuntimeEvent {
            sequence,
            kind: kind.to_owned(),
            at_unix: now_unix().unwrap_or(0),
            revocation_revision: revision,
            detail,
        });
        while events.entries.len() > 256 {
            events.entries.pop_front();
        }
    }
}

pub fn events(after: u64, limit: usize) -> Result<String> {
    if limit == 0 || limit > 100 {
        bail!("invalid event page size");
    }
    let events = EVENTS
        .lock()
        .map_err(|_| anyhow::anyhow!("Android event lock failed"))?;
    let effective_after = if after > events.sequence { 0 } else { after };
    let oldest = events
        .entries
        .front()
        .map_or(events.sequence.saturating_add(1), |item| item.sequence);
    let items: Vec<_> = events
        .entries
        .iter()
        .filter(|item| item.sequence > effective_after)
        .take(limit)
        .collect();
    Ok(serde_json::to_string(&serde_json::json!({
        "epoch": EVENT_EPOCH.get_or_init(|| uuid::Uuid::new_v4().to_string()),
        "oldest_sequence": oldest, "latest_sequence": events.sequence,
        "gap": after.saturating_add(1) < oldest || after > events.sequence,
        "next_cursor": items.last().map_or(effective_after, |item| item.sequence), "items": items,
    }))?)
}

pub struct OverlayState {
    pub configuration: Configuration,
    pub membership: RwLock<MembershipStore>,
    pub directory: Arc<ServiceDirectory>,
    pub operations: Mutex<()>,
}

impl OverlayState {
    pub fn new(configuration: Configuration, membership: MembershipStore) -> Result<Arc<Self>> {
        let directory = ServiceDirectory::load(
            ServiceConfiguration {
                state_file: configuration.working_directory.join("services.json"),
                identity_key_file: None,
            },
            &membership,
            configuration.service_limits.clone(),
            now_unix()?,
        )?;
        for peer in &configuration.exchange_peers {
            avian_services::verify_overlay_peer(
                &membership,
                &peer.node_id,
                peer.service_endpoint.ip(),
            )?;
            avian_services::verify_overlay_peer(
                &membership,
                &peer.node_id,
                peer.revocation_endpoint.ip(),
            )?;
            if peer.service_endpoint.port() == 0 || peer.revocation_endpoint.port() == 0 {
                bail!("invalid overlay peer port");
            }
        }
        Ok(Arc::new(Self {
            configuration,
            membership: RwLock::new(membership),
            directory: Arc::new(directory),
            operations: Mutex::new(()),
        }))
    }

    pub fn snapshot(&self) -> Result<MembershipStore> {
        Ok(self
            .membership
            .read()
            .map_err(|_| anyhow::anyhow!("Android membership lock failed"))?
            .clone())
    }

    pub fn replace_membership(&self, replacement: MembershipStore) -> Result<()> {
        let revision = replacement.revocation_revision();
        let previous = self.snapshot()?;
        replacement.validate_successor(&previous)?;
        *self
            .membership
            .write()
            .map_err(|_| anyhow::anyhow!("Android membership lock failed"))? = replacement.clone();
        // The shared directory rechecks owner revocation on every view, even if persistence fails.
        self.directory.receive(
            &replacement,
            AnnouncementPage {
                version: 1,
                network_id: replacement.trust_root().network_id.clone(),
                generated_at_unix: now_unix()?,
                records: Vec::new(),
                next_cursor: None,
            },
            now_unix()?,
        )?;
        if revision != previous.revocation_revision() {
            event("membership_updated", revision);
        }
        Ok(())
    }

    pub fn publish(
        &self,
        identity: &ed25519_dalek::SigningKey,
        publication: LocalPublication,
    ) -> Result<avian_services::SignedAnnouncement> {
        let _operation = self
            .operations
            .lock()
            .map_err(|_| anyhow::anyhow!("Android operation lock failed"))?;
        if avian_membership::node_id_for_key(&identity.verifying_key())
            != self.configuration.node_id
        {
            bail!("local service identity does not own this runtime");
        }
        let membership = self.snapshot()?;
        let announcement = self.directory.publish_with_identity(
            &membership,
            identity,
            publication,
            now_unix()?,
        )?;
        event("local_services_published", membership.revocation_revision());
        Ok(announcement)
    }

    pub fn page(&self, after: Option<&str>, limit: usize) -> Result<ServicePage> {
        self.directory
            .page(&self.snapshot()?, after, limit, now_unix()?)
    }

    fn apply_revocations(&self, batch: avian_revocations::RevocationBatch) -> Result<usize> {
        let _operation = self
            .operations
            .lock()
            .map_err(|_| anyhow::anyhow!("Android operation lock failed"))?;
        let previous = self.snapshot()?;
        avian_revocations::verify_batch(&previous, &batch)?;
        if batch.records.is_empty() {
            return Ok(0);
        }
        let outcome = avian_revocations::apply_batch_and_publish(
            previous.trust_root().clone(),
            &self.configuration.membership_file,
            &self.configuration.trust_file(),
            &batch,
            SystemTime::now(),
        )?;
        let current = MembershipStore::load(
            &self.configuration.membership_file,
            previous.trust_root().clone(),
        )?;
        self.replace_membership(current)?;
        if outcome.applied_records > 0 {
            event("revocations_applied", outcome.current_revision);
        }
        Ok(outcome.applied_records)
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceQuery {
    after: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevocationQuery {
    network_id: String,
    after_revision: u64,
    limit: Option<usize>,
}

fn source_allowed(
    state: &OverlayState,
    source: SocketAddr,
    headers: &HeaderMap,
) -> Result<MembershipStore, StatusCode> {
    if headers.contains_key(header::ORIGIN) {
        return Err(StatusCode::FORBIDDEN);
    }
    let membership = state
        .snapshot()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let owner = membership
        .memberships()
        .find(|owner| owner.claims.overlay_addresses.contains(&source.ip()))
        .ok_or(StatusCode::FORBIDDEN)?;
    avian_services::verify_overlay_peer(&membership, &owner.claims.node_id, source.ip())
        .map_err(|_| StatusCode::FORBIDDEN)?;
    Ok(membership)
}

async fn service_records(
    State(state): State<Arc<OverlayState>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<ServiceQuery>,
) -> Result<Json<AnnouncementPage>, StatusCode> {
    let membership = source_allowed(&state, source, &headers)?;
    tokio::task::spawn_blocking(move || {
        state.directory.announcements_for_peer(
            &membership,
            source.ip(),
            query.after.as_deref(),
            query
                .limit
                .unwrap_or(state.configuration.service_limits.maximum_page_size),
            now_unix()?,
        )
    })
    .await
    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
    .map(Json)
    .map_err(|_| StatusCode::BAD_REQUEST)
}

async fn revocation_records(
    State(state): State<Arc<OverlayState>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(query): Query<RevocationQuery>,
) -> Result<Json<avian_revocations::RevocationBatch>, StatusCode> {
    let membership = source_allowed(&state, source, &headers)?;
    if query.network_id != membership.trust_root().network_id {
        return Err(StatusCode::FORBIDDEN);
    }
    avian_revocations::export_batch(
        &membership,
        query.after_revision,
        query.limit.unwrap_or(64),
        avian_revocations::MAXIMUM_RESPONSE_BYTES,
    )
    .map(Json)
    .map_err(|_| StatusCode::BAD_REQUEST)
}

async fn serve(state: Arc<OverlayState>) {
    let address = SocketAddr::new(
        state.configuration.overlay_address.into(),
        state.configuration.exchange_port,
    );
    loop {
        if let Ok(listener) = tokio::net::TcpListener::bind(address).await {
            event(
                "exchange_listening",
                state
                    .snapshot()
                    .map_or(0, |membership| membership.revocation_revision()),
            );
            let router = Router::new()
                .route("/v1/service-announcements", get(service_records))
                .route("/v1/revocations", get(revocation_records))
                .with_state(state.clone());
            let _ = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn synchronize_peer(
    state: Arc<OverlayState>,
    client: reqwest::Client,
    peer: OverlayPeer,
    cursor: Option<String>,
) -> Result<Option<String>> {
    let membership = state.snapshot()?;
    avian_services::verify_overlay_peer(&membership, &peer.node_id, peer.revocation_endpoint.ip())?;
    let batch = avian_revocations::fetch_batch(
        &client,
        &format!("http://{}/", peer.revocation_endpoint),
        &membership.trust_root().network_id,
        membership.revocation_revision(),
        64,
    )
    .await?;
    let state_for_apply = state.clone();
    tokio::task::spawn_blocking(move || state_for_apply.apply_revocations(batch)).await??;
    let membership = state.snapshot()?;
    let page = avian_services::fetch_announcements(
        &client,
        &membership,
        FetchRequest {
            peer_node_id: &peer.node_id,
            endpoint: peer.service_endpoint,
            after: cursor.as_deref(),
            limit: state.configuration.service_limits.maximum_page_size,
            now_unix: now_unix()?,
        },
        &state.configuration.service_limits,
    )
    .await?;
    let next = page.next_cursor.clone();
    let receiver = state.clone();
    let changed = tokio::task::spawn_blocking(move || {
        let _operation = receiver
            .operations
            .lock()
            .map_err(|_| anyhow::anyhow!("Android operation lock failed"))?;
        let membership = receiver.snapshot()?;
        receiver.directory.receive(&membership, page, now_unix()?)
    })
    .await??;
    if changed > 0 {
        event(
            "service_records_received",
            state.snapshot()?.revocation_revision(),
        );
    }
    Ok(next)
}

async fn synchronize(state: Arc<OverlayState>) {
    let Ok(client) = avian_services::synchronization_client() else {
        return;
    };
    let mut cursors = BTreeMap::<String, Option<String>>::new();
    loop {
        if tun_state() != "ready" {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        }
        let mut pending = state.configuration.exchange_peers.iter().cloned();
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            while tasks.len() < 4 {
                let Some(peer) = pending.next() else {
                    break;
                };
                let owner = peer.node_id.clone();
                let cursor = cursors.get(&owner).cloned().flatten();
                let state = state.clone();
                let client = client.clone();
                tasks.spawn(
                    async move { (owner, synchronize_peer(state, client, peer, cursor).await) },
                );
            }
            match tasks.join_next().await {
                Some(Ok((owner, Ok(cursor)))) => {
                    cursors.insert(owner, cursor);
                }
                Some(Ok((_, Err(error)))) => {
                    let revision = state
                        .snapshot()
                        .map_or(0, |membership| membership.revocation_revision());
                    let kind = if error
                        .downcast_ref::<avian_services::ServiceError>()
                        .is_some()
                    {
                        "service_update_rejected"
                    } else if error
                        .downcast_ref::<avian_revocations::DistributionError>()
                        .is_some()
                    {
                        "revocation_update_rejected"
                    } else {
                        "exchange_unavailable"
                    };
                    event(kind, revision);
                }
                Some(Err(_)) => event("exchange_worker_failed", 0),
                None => break,
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub fn start(
    state: Arc<OverlayState>,
    engine_events: Option<
        tokio::sync::broadcast::Receiver<easytier::common::global_ctx::GlobalCtxEvent>,
    >,
) -> Result<tokio::runtime::Runtime> {
    let Some(engine_events) = engine_events else {
        bail!("engine event observer is unavailable");
    };
    TUN_STATE.store(0, Ordering::Release);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.spawn(observe_engine(engine_events, state.clone()));
    super::probes::reset(state.configuration.automatic_capacity_probes);
    if state.configuration.automatic_capacity_probes {
        runtime.spawn(super::probes::run(state.clone()));
    }
    runtime.spawn(serve(state.clone()));
    runtime.spawn(synchronize(state));
    Ok(runtime)
}
