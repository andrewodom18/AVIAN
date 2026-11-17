use std::{
    fs,
    sync::Arc,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result};
use avian_membership::{MembershipStore, TrustRoot};
use tokio::sync::{RwLock, Semaphore, broadcast};

use crate::{
    api::StateEvent,
    chud::ChudClient,
    configuration::{Configuration, read_secret},
};

pub struct ApplicationState {
    pub api_token: String,
    pub membership: RwLock<MembershipStore>,
    pub chud: Option<ChudClient>,
    pub events: broadcast::Sender<StateEvent>,
    pub reload_error: RwLock<Option<String>>,
    pub preview_slots: Arc<Semaphore>,
    pub runtime: Option<Arc<avian_runtime::RuntimeSupervisor>>,
    pub services: Option<Arc<crate::services::ServiceDirectory>>,
    pub service_limits: avian_services::Limits,
}

impl ApplicationState {
    pub fn load(configuration: &Configuration) -> Result<Arc<Self>> {
        let trust_root: TrustRoot = serde_json::from_slice(
            &fs::read(&configuration.trust_root_file).context("read trust root")?,
        )
        .context("parse trust root")?;
        let membership = MembershipStore::load(&configuration.membership_file, trust_root)?;
        let service_limits = configuration.service_limits.clone().unwrap_or_default();
        let services = configuration
            .services
            .clone()
            .map(|configuration| {
                crate::services::ServiceDirectory::load(
                    configuration,
                    &membership,
                    service_limits.clone(),
                    crate::service_exchange::now_unix()?,
                )
                .map(Arc::new)
            })
            .transpose()?;
        let (events, _) = broadcast::channel(128);
        let local_workers = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Ok(Arc::new(Self {
            api_token: read_secret(&configuration.token_file)?,
            services,
            service_limits,
            membership: RwLock::new(membership),
            chud: configuration
                .chud
                .as_ref()
                .map(ChudClient::new)
                .transpose()?,
            events,
            reload_error: RwLock::new(None),
            preview_slots: Arc::new(Semaphore::new(local_workers)),
            runtime: configuration
                .runtime
                .clone()
                .map(avian_runtime::RuntimeSupervisor::new)
                .map(Arc::new),
        }))
    }

    pub async fn watch_membership(self: Arc<Self>, configuration: Configuration) {
        let mut last_change: Option<(SystemTime, u64)> = None;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let metadata = match tokio::fs::metadata(&configuration.membership_file).await {
                Ok(metadata) => metadata,
                Err(_) => {
                    *self.reload_error.write().await = Some(
                        "Membership file is unavailable; retaining last verified state.".into(),
                    );
                    continue;
                }
            };
            let change = metadata
                .modified()
                .ok()
                .map(|modified| (modified, metadata.len()));
            if change.is_some() && change == last_change {
                continue;
            }
            let root = self.membership.read().await.trust_root().clone();
            let path = configuration.membership_file.clone();
            let loaded =
                tokio::task::spawn_blocking(move || MembershipStore::load(path, root)).await;
            match loaded {
                Ok(Ok(replacement)) => {
                    let mut membership = self.membership.write().await;
                    if replacement.validate_successor(&membership).is_err() {
                        *self.reload_error.write().await = Some(
                            "Rejected membership rollback; retaining last verified state.".into(),
                        );
                        last_change = change;
                        continue;
                    }
                    let revision = replacement.revocation_revision();
                    *membership = replacement;
                    *self.reload_error.write().await = None;
                    let _ = self.events.send(StateEvent {
                        kind: "membership_updated".into(),
                        revocation_revision: revision,
                    });
                }
                _ => {
                    *self.reload_error.write().await = Some(
                        "Membership verification failed; retaining last verified state.".into(),
                    );
                }
            }
            last_change = change;
        }
    }
}

impl ApplicationState {
    pub async fn watch_connectivity(self: Arc<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            return;
        };
        let mut previous_runtime = None;
        let mut previous_underlays = None;
        let mut previous_routes = None;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            let status = runtime.status().await;
            let fingerprint = serde_json::json!({"state":status.state,"process_id":status.process_id,"engine_ready":status.engine_ready,"last_error":status.last_error});
            if previous_runtime.as_ref() != Some(&fingerprint) {
                previous_runtime = Some(fingerprint);
                let _ = self.events.send(StateEvent {
                    kind: "runtime_updated".into(),
                    revocation_revision: self.membership.read().await.revocation_revision(),
                });
            }
            if runtime.configuration().measured_routing {
                let revision = if status.engine_ready {
                    runtime
                        .inspect_class_routes_page(None, 1, None)
                        .await
                        .ok()
                        .and_then(|page| page.get("revision").cloned())
                } else {
                    None
                };
                if previous_routes != revision {
                    previous_routes = revision;
                    let _ = self.events.send(StateEvent {
                        kind: "paths_updated".into(),
                        revocation_revision: self.membership.read().await.revocation_revision(),
                    });
                }
            }
            let interfaces = runtime.inspect_underlays().await.ok();
            if previous_underlays.as_ref() != Some(&interfaces) {
                previous_underlays = Some(interfaces);
                let _ = self.events.send(StateEvent {
                    kind: "connections_updated".into(),
                    revocation_revision: self.membership.read().await.revocation_revision(),
                });
            }
        }
    }
}
