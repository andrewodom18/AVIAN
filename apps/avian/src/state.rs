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
}

impl ApplicationState {
    pub fn load(configuration: &Configuration) -> Result<Arc<Self>> {
        let trust_root: TrustRoot = serde_json::from_slice(
            &fs::read(&configuration.trust_root_file).context("read trust root")?,
        )
        .context("parse trust root")?;
        let membership = MembershipStore::load(&configuration.membership_file, trust_root)?;
        let (events, _) = broadcast::channel(128);
        let local_workers = std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1);
        Ok(Arc::new(Self {
            api_token: read_secret(&configuration.token_file)?,
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
