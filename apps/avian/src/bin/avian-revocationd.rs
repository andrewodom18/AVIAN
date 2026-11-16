use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, bail};
use avian_membership::{MembershipStore, TrustRoot};
use avian_revocations::{apply_batch_and_publish, export_batch, fetch_batch, refresh_engine_cache};
use axum::{
    Json, Router,
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use serde::Deserialize;
use tokio::sync::{Mutex, Semaphore};

#[derive(Clone, Parser)]
#[command(about = "Exchange signed revocation records over an isolated private network")]
struct Arguments {
    #[arg(long)]
    trust_root: PathBuf,
    #[arg(long)]
    membership: PathBuf,
    #[arg(long)]
    engine_trust: PathBuf,
    #[arg(long)]
    listen: SocketAddr,
    #[arg(long)]
    peer: Vec<String>,
    #[arg(long, default_value_t = 500)]
    poll_interval_ms: u64,
    #[arg(long, default_value_t = 128)]
    batch_size: usize,
}

struct ServiceState {
    arguments: Arguments,
    root: TrustRoot,
    last_verified: Mutex<MembershipStore>,
    response_slots: Arc<Semaphore>,
}

impl ServiceState {
    fn load_verified(&self, previous: &MembershipStore) -> Result<MembershipStore> {
        let current = MembershipStore::load(&self.arguments.membership, self.root.clone())?;
        current.validate_successor(previous)?;
        Ok(current)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchQuery {
    #[serde(default)]
    after_revision: u64,
    limit: Option<usize>,
    network_id: Option<String>,
}

async fn serve_batch(
    State(state): State<Arc<ServiceState>>,
    Query(query): Query<BatchQuery>,
) -> Response {
    if query
        .network_id
        .as_ref()
        .is_some_and(|network| network != &state.root.network_id)
    {
        return (StatusCode::BAD_REQUEST, "wrong network").into_response();
    }
    let limit = query.limit.unwrap_or(state.arguments.batch_size);
    if limit == 0 || limit > state.arguments.batch_size {
        return (StatusCode::BAD_REQUEST, "invalid batch limit").into_response();
    }
    let Ok(permit) = state.response_slots.clone().try_acquire_owned() else {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "revocation response workers are busy",
        )
            .into_response();
    };
    let state_for_work = state.clone();
    let result = tokio::task::spawn_blocking(move || -> Result<_> {
        let _permit = permit;
        let mut previous = state_for_work.last_verified.blocking_lock();
        let current = state_for_work.load_verified(&previous)?;
        let batch = export_batch(&current, query.after_revision, limit, 1024 * 1024)?;
        *previous = current;
        Ok(batch)
    })
    .await;
    match result {
        Ok(Ok(batch)) => Json(batch).into_response(),
        _ => (
            StatusCode::SERVICE_UNAVAILABLE,
            "verified revocation records unavailable",
        )
            .into_response(),
    }
}

async fn synchronize(state: Arc<ServiceState>) {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(3))
        .no_proxy()
        .build()
        .expect("build revocation HTTP client");
    let mut interval =
        tokio::time::interval(Duration::from_millis(state.arguments.poll_interval_ms));
    loop {
        interval.tick().await;
        for peer in &state.arguments.peer {
            let revision = state.last_verified.lock().await.revocation_revision();
            let batch = match fetch_batch(
                &client,
                peer,
                &state.root.network_id,
                revision,
                state.arguments.batch_size,
            )
            .await
            {
                Ok(batch) => batch,
                Err(_) => {
                    tracing::debug!("revocation peer unavailable or update rejected");
                    continue;
                }
            };
            let work_state = state.clone();
            let result = tokio::task::spawn_blocking(move || -> Result<()> {
                let mut previous = work_state.last_verified.blocking_lock();
                work_state.load_verified(&previous)?;
                apply_batch_and_publish(
                    work_state.root.clone(),
                    &work_state.arguments.membership,
                    &work_state.arguments.engine_trust,
                    &batch,
                    SystemTime::now(),
                )?;
                *previous = work_state.load_verified(&previous)?;
                Ok(())
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                tracing::warn!("revocation update could not be persisted");
            }
        }
    }
}

async fn renew_leases(state: Arc<ServiceState>) {
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    let mut last_published_revision = u64::MAX;
    let mut last_publication = tokio::time::Instant::now();
    loop {
        interval.tick().await;
        let work_state = state.clone();
        let revision_before = last_published_revision;
        let refresh_due = last_publication.elapsed() >= Duration::from_secs(30);
        let refreshed = tokio::task::spawn_blocking(move || -> Result<(u64, bool)> {
            let mut previous = work_state.last_verified.blocking_lock();
            let current = work_state.load_verified(&previous)?;
            if !refresh_due && current.revocation_revision() == revision_before {
                return Ok((revision_before, false));
            }
            refresh_engine_cache(
                work_state.root.clone(),
                &work_state.arguments.membership,
                &work_state.arguments.engine_trust,
                SystemTime::now(),
            )?;
            *previous = work_state.load_verified(&previous)?;
            Ok((previous.revocation_revision(), true))
        })
        .await;
        match refreshed {
            Ok(Ok((revision, published))) => {
                last_published_revision = revision;
                if published {
                    last_publication = tokio::time::Instant::now();
                }
            }
            _ => tracing::warn!("trust cache refresh failed; existing leases will expire"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("avian=info")
        .init();
    let arguments = Arguments::parse();
    let private = match arguments.listen.ip() {
        std::net::IpAddr::V4(address) => address.is_loopback() || address.is_private(),
        std::net::IpAddr::V6(address) => {
            address.is_loopback() || address.segments()[0] & 0xfe00 == 0xfc00
        }
    };
    if !private {
        bail!("revocation listener requires an explicit private or loopback address");
    }
    if arguments.poll_interval_ms < 100 || arguments.batch_size == 0 || arguments.batch_size > 256 {
        bail!("poll interval must be at least 100 ms and batch size between 1 and 256");
    }
    let root: TrustRoot = serde_json::from_slice(&std::fs::read(&arguments.trust_root)?)?;
    let membership = MembershipStore::load(&arguments.membership, root.clone())?;
    refresh_engine_cache(
        root.clone(),
        &arguments.membership,
        &arguments.engine_trust,
        SystemTime::now(),
    )?;
    let listener = tokio::net::TcpListener::bind(arguments.listen)
        .await
        .context("bind revocation listener")?;
    let state = Arc::new(ServiceState {
        arguments,
        root,
        last_verified: Mutex::new(membership),
        response_slots: Arc::new(Semaphore::new(
            std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
        )),
    });
    let worker = tokio::spawn(synchronize(state.clone()));
    let renewal = tokio::spawn(renew_leases(state.clone()));
    let router = Router::new()
        .route("/v1/revocations", get(serve_batch))
        .with_state(state);
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    worker.abort();
    renewal.abort();
    result?;
    Ok(())
}
