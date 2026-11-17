//! Local service publication uses the configured owner identity, never an issuer key.

use std::{net::IpAddr, path::PathBuf, sync::Mutex};

use anyhow::{Context, Result};
use avian_membership::{MembershipStore, node_id_for_key, read_signing_key};
use avian_services::{
    AnnouncementPage, Limits, Registry, ServiceEndpoint, ServicePage, SignedAnnouncement,
    verify_overlay_peer,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfiguration {
    pub state_file: PathBuf,
    /// Omit on nodes that only receive advertisements.
    pub identity_key_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalPublication {
    pub ttl_seconds: i64,
    pub endpoints: Vec<ServiceEndpoint>,
}

/// Call filesystem-backed mutations from a blocking worker in an asynchronous server.
pub struct ServiceDirectory {
    configuration: ServiceConfiguration,
    registry: Mutex<Registry>,
}

impl ServiceDirectory {
    pub fn load(
        configuration: ServiceConfiguration,
        membership: &MembershipStore,
        limits: Limits,
        now_unix: i64,
    ) -> Result<Self> {
        let parent = configuration
            .state_file
            .parent()
            .context("service state requires a directory")?;
        avian_storage::validate_private_directory(parent)
            .context("validate private service state directory")?;
        let registry = if configuration.state_file.exists() {
            Registry::load(&configuration.state_file, membership, limits, now_unix)?
        } else {
            Registry::new(membership, limits)?
        };
        Ok(Self {
            configuration,
            registry: Mutex::new(registry),
        })
    }

    pub fn publish(
        &self,
        membership: &MembershipStore,
        publication: LocalPublication,
        now_unix: i64,
    ) -> Result<SignedAnnouncement> {
        let key_file = self
            .configuration
            .identity_key_file
            .as_ref()
            .context("local service publication has no configured owner identity")?;
        let identity =
            read_signing_key(key_file).context("read protected service owner identity")?;
        self.publish_with_identity(membership, &identity, publication, now_unix)
    }

    /// Mobile hosts unwrap identity material through their protected key store.
    pub fn publish_with_identity(
        &self,
        membership: &MembershipStore,
        identity: &ed25519_dalek::SigningKey,
        publication: LocalPublication,
        now_unix: i64,
    ) -> Result<SignedAnnouncement> {
        let owner_node_id = node_id_for_key(&identity.verifying_key());
        let mut current = self
            .registry
            .lock()
            .map_err(|_| anyhow::anyhow!("service state lock is unavailable"))?;
        let mut replacement = current.clone();
        let announcement = SignedAnnouncement::sign(
            membership,
            identity,
            replacement.next_revision(&owner_node_id)?,
            publication.endpoints,
            publication.ttl_seconds,
            now_unix,
            replacement.limits(),
        )?;
        replacement.accept(membership, announcement.clone(), now_unix)?;
        replacement.save(&self.configuration.state_file)?;
        *current = replacement;
        Ok(announcement)
    }

    pub fn receive(
        &self,
        membership: &MembershipStore,
        page: AnnouncementPage,
        now_unix: i64,
    ) -> Result<usize> {
        let mut current = self
            .registry
            .lock()
            .map_err(|_| anyhow::anyhow!("service state lock is unavailable"))?;
        let mut replacement = current.clone();
        let changed = replacement.accept_page(membership, page, now_unix)?;
        replacement.save(&self.configuration.state_file)?;
        *current = replacement;
        Ok(changed)
    }

    pub fn page(
        &self,
        membership: &MembershipStore,
        after: Option<&str>,
        limit: usize,
        now_unix: i64,
    ) -> Result<ServicePage> {
        Ok(self
            .registry
            .lock()
            .map_err(|_| anyhow::anyhow!("service state lock is unavailable"))?
            .page(membership, after, limit, now_unix)?)
    }

    /// Serve this only on the authenticated overlay, independently of the local bearer-token API.
    pub fn announcements_for_peer(
        &self,
        membership: &MembershipStore,
        source_address: IpAddr,
        after: Option<&str>,
        limit: usize,
        now_unix: i64,
    ) -> Result<AnnouncementPage> {
        let peer = membership
            .memberships()
            .find(|member| member.claims.overlay_addresses.contains(&source_address))
            .context("service peer address is not enrolled")?;
        verify_overlay_peer(membership, &peer.claims.node_id, source_address)?;
        Ok(self
            .registry
            .lock()
            .map_err(|_| anyhow::anyhow!("service state lock is unavailable"))?
            .announcements_page(membership, after, limit, now_unix)?)
    }
}
