//! Signed public service records travel separately from the local administration API.
use crate::state::ApplicationState;
use axum::{
    Json, Router,
    extract::{ConnectInfo, Query, State},
    http::StatusCode,
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServicePeer {
    pub node_id: String,
    pub address: SocketAddr,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceExchangeConfiguration {
    pub listen: SocketAddr,
    #[serde(default)]
    pub peers: Vec<ServicePeer>,
}
impl ServiceExchangeConfiguration {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !private(self.listen.ip()) || self.listen.port() == 0 {
            anyhow::bail!("service exchange requires an explicit private overlay listener");
        }
        for peer in &self.peers {
            if peer.node_id.len() != 64
                || hex::decode(&peer.node_id).is_err()
                || !private(peer.address.ip())
                || peer.address.port() == 0
            {
                anyhow::bail!("invalid service exchange peer");
            }
        }
        Ok(())
    }
}
fn private(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => address.is_private(),
        std::net::IpAddr::V6(address) => address.segments()[0] & 0xfe00 == 0xfc00,
    }
}
pub fn now_unix() -> anyhow::Result<i64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_secs()
        .try_into()?)
}
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PageQuery {
    after: Option<String>,
    limit: Option<usize>,
}
async fn records(
    State(state): State<Arc<ApplicationState>>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    Query(query): Query<PageQuery>,
) -> Result<Json<avian_services::AnnouncementPage>, StatusCode> {
    let directory = state
        .services
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let membership = state.membership.read().await;
    directory
        .announcements_for_peer(
            &membership,
            source.ip(),
            query.after.as_deref(),
            query
                .limit
                .unwrap_or(state.service_limits.maximum_page_size),
            now_unix().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
        )
        .map(Json)
        .map_err(|_| StatusCode::FORBIDDEN)
}

pub async fn run(state: Arc<ApplicationState>, configuration: ServiceExchangeConfiguration) {
    let serving = async {
        loop {
            match tokio::net::TcpListener::bind(configuration.listen).await {
                Ok(listener) => {
                    let router = Router::new()
                        .route("/v1/service-announcements", get(records))
                        .with_state(state.clone());
                    if axum::serve(
                        listener,
                        router.into_make_service_with_connect_info::<SocketAddr>(),
                    )
                    .await
                    .is_err()
                    {
                        tracing::warn!("service exchange listener stopped");
                    }
                }
                Err(_) => tracing::debug!("service exchange overlay listener is not available"),
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    let synchronizing = async {
        let Ok(client) = avian_services::synchronization_client() else {
            return;
        };
        let mut cursors = std::collections::BTreeMap::<String, Option<String>>::new();
        loop {
            let mut pending = configuration.peers.iter();
            let mut workers = tokio::task::JoinSet::new();
            loop {
                while workers.len() < 4 {
                    let Some(peer) = pending.next().cloned() else {
                        break;
                    };
                    let state = state.clone();
                    let client = client.clone();
                    let cursor = cursors.get(&peer.node_id).cloned().flatten();
                    workers.spawn(async move {
                        let membership = state.membership.read().await.clone();
                        let now = now_unix().map_err(|_| ())?;
                        let page = avian_services::fetch_announcements(
                            &client,
                            &membership,
                            avian_services::FetchRequest {
                                peer_node_id: &peer.node_id,
                                endpoint: peer.address,
                                after: cursor.as_deref(),
                                limit: state.service_limits.maximum_page_size,
                                now_unix: now,
                            },
                            &state.service_limits,
                        )
                        .await
                        .map_err(|_| ())?;
                        let next = page.next_cursor.clone();
                        let membership = state.membership.read().await.clone();
                        let directory = state.services.clone().ok_or(())?;
                        let changed = tokio::task::spawn_blocking(move || {
                            directory.receive(&membership, page, now)
                        })
                        .await
                        .map_err(|_| ())?
                        .map_err(|_| ())?;
                        if changed > 0 {
                            let revision = state.membership.read().await.revocation_revision();
                            let _ = state.events.send(crate::api::StateEvent {
                                kind: "services_updated".into(),
                                revocation_revision: revision,
                            });
                        }
                        Ok::<_, ()>((peer.node_id, next))
                    });
                }
                match workers.join_next().await {
                    Some(Ok(Ok((peer, cursor)))) => {
                        cursors.insert(peer, cursor);
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    };
    tokio::join!(serving, synchronizing);
}
