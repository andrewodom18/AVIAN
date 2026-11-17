//! Signed, expiring service advertisements. Advertised endpoints are not reachability observations.

mod registry;
mod transport;

pub use registry::{Advertisement, AnnouncementPage, Registry, ServicePage};
pub use transport::{
    FetchRequest, fetch_announcements, synchronization_client, verify_overlay_peer,
};

use std::net::IpAddr;

use avian_membership::{MembershipClaims, MembershipStore, node_id_for_key};
use avian_routing::TrafficClass;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const FORMAT_VERSION: u32 = 1;
const SIGNATURE_CONTEXT: &[u8] = b"AVIAN service announcement v1\0";
const CLOCK_SKEW_SECONDS: i64 = 5;

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("invalid {0}")]
    InvalidField(&'static str),
    #[error("the announcement belongs to another network")]
    WrongNetwork,
    #[error("the service owner or overlay peer is not actively enrolled")]
    Unauthorized,
    #[error("signature verification failed")]
    InvalidSignature,
    #[error("the announcement expired or has an invalid publication time")]
    Expired,
    #[error("the update would roll back accepted state")]
    Rollback,
    #[error("the configured service resource budget was exceeded")]
    ResourceBudget,
    #[error("another process is saving service state")]
    StoreBusy,
    #[error(transparent)]
    Storage(#[from] avian_storage::StorageError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, ServiceError>;

/// Message and storage budgets are independent of mesh node count.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Limits {
    pub maximum_announcement_bytes: usize,
    pub maximum_registry_bytes: usize,
    pub maximum_ttl_seconds: i64,
    pub maximum_page_size: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            maximum_announcement_bytes: 64 * 1024,
            maximum_registry_bytes: 8 * 1024 * 1024,
            maximum_ttl_seconds: 300,
            maximum_page_size: 100,
        }
    }
}

impl Limits {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.maximum_announcement_bytes == 0
            || self.maximum_registry_bytes < self.maximum_announcement_bytes
            || self.maximum_ttl_seconds <= 0
            || self.maximum_page_size == 0
        {
            return Err(ServiceError::InvalidField("resource budgets"));
        }
        Ok(())
    }
}

/// These labels do not change engine classification, route policy, or metered permissions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceKind {
    ArcControl,
    ArcTelemetry,
    ArcVideo,
    ArcTransfer,
}

impl ServiceKind {
    pub fn traffic_class(self) -> TrafficClass {
        match self {
            Self::ArcControl => TrafficClass::Control,
            Self::ArcTelemetry => TrafficClass::Telemetry,
            Self::ArcVideo => TrafficClass::Video,
            Self::ArcTransfer => TrafficClass::Bulk,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportProtocol {
    Tcp,
    Udp,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceEndpoint {
    pub kind: ServiceKind,
    pub address: IpAddr,
    pub protocol: TransportProtocol,
    pub port: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceClaims {
    pub version: u32,
    pub network_id: String,
    pub owner_node_id: String,
    pub membership_revision: u64,
    pub revision: u64,
    pub issued_at_unix: i64,
    pub expires_at_unix: i64,
    /// A complete owner snapshot. An empty snapshot withdraws all of this owner's services.
    pub endpoints: Vec<ServiceEndpoint>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedAnnouncement {
    pub claims: ServiceClaims,
    pub signature: String,
}

impl SignedAnnouncement {
    pub fn sign(
        membership: &MembershipStore,
        identity: &SigningKey,
        revision: u64,
        endpoints: Vec<ServiceEndpoint>,
        ttl_seconds: i64,
        now_unix: i64,
        limits: &Limits,
    ) -> Result<Self> {
        let owner_node_id = node_id_for_key(&identity.verifying_key());
        let owner = active_member(membership, &owner_node_id)?;
        let claims = ServiceClaims {
            version: FORMAT_VERSION,
            network_id: membership.trust_root().network_id.clone(),
            owner_node_id,
            membership_revision: owner.revision,
            revision,
            issued_at_unix: now_unix,
            expires_at_unix: now_unix
                .checked_add(ttl_seconds)
                .ok_or(ServiceError::Expired)?,
            endpoints,
        };
        let signature = hex::encode(identity.sign(&signature_message(&claims)?).to_bytes());
        let announcement = Self { claims, signature };
        announcement.verify(membership, now_unix, limits)?;
        Ok(announcement)
    }

    pub fn verify(
        &self,
        membership: &MembershipStore,
        now_unix: i64,
        limits: &Limits,
    ) -> Result<()> {
        self.verify_identity(membership, limits)?;
        if now_unix < 0
            || self.claims.issued_at_unix > now_unix.saturating_add(CLOCK_SKEW_SECONDS)
            || self.claims.expires_at_unix <= now_unix
        {
            return Err(ServiceError::Expired);
        }
        Ok(())
    }

    pub(crate) fn verify_identity(
        &self,
        membership: &MembershipStore,
        limits: &Limits,
    ) -> Result<()> {
        limits.validate()?;
        let claims = &self.claims;
        if claims.network_id != membership.trust_root().network_id {
            return Err(ServiceError::WrongNetwork);
        }
        if claims.version != FORMAT_VERSION || claims.revision == 0 || claims.issued_at_unix < 0 {
            return Err(ServiceError::InvalidField(
                "announcement version, revision, or time",
            ));
        }
        let ttl = claims
            .expires_at_unix
            .checked_sub(claims.issued_at_unix)
            .ok_or(ServiceError::Expired)?;
        if ttl <= 0 || ttl > limits.maximum_ttl_seconds {
            return Err(ServiceError::Expired);
        }
        if serde_json::to_vec(self)?.len() > limits.maximum_announcement_bytes {
            return Err(ServiceError::ResourceBudget);
        }
        let owner = active_member(membership, &claims.owner_node_id)?;
        if claims.membership_revision != owner.revision {
            return Err(ServiceError::Rollback);
        }
        let mut unique = std::collections::BTreeSet::new();
        for endpoint in &claims.endpoints {
            if endpoint.port == 0
                || !owner.overlay_addresses.contains(&endpoint.address)
                || !unique.insert(endpoint)
            {
                return Err(ServiceError::InvalidField(
                    "service endpoint ownership or duplicate",
                ));
            }
        }
        let public_key: [u8; 32] = hex::decode(&owner.identity_public_key)
            .map_err(|_| ServiceError::InvalidSignature)?
            .try_into()
            .map_err(|_| ServiceError::InvalidSignature)?;
        let key =
            VerifyingKey::from_bytes(&public_key).map_err(|_| ServiceError::InvalidSignature)?;
        let bytes: [u8; 64] = hex::decode(&self.signature)
            .map_err(|_| ServiceError::InvalidSignature)?
            .try_into()
            .map_err(|_| ServiceError::InvalidSignature)?;
        key.verify_strict(&signature_message(claims)?, &Signature::from_bytes(&bytes))
            .map_err(|_| ServiceError::InvalidSignature)
    }

    pub(crate) fn digest(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(self)?)))
    }
}

pub(crate) fn active_member<'a>(
    membership: &'a MembershipStore,
    node_id: &str,
) -> Result<&'a MembershipClaims> {
    membership
        .memberships()
        .find(|record| record.claims.node_id == node_id)
        .map(|record| &record.claims)
        .ok_or(ServiceError::Unauthorized)
}

fn signature_message(claims: &ServiceClaims) -> Result<Vec<u8>> {
    let mut message = SIGNATURE_CONTEXT.to_vec();
    message.extend(serde_json::to_vec(claims)?);
    Ok(message)
}
