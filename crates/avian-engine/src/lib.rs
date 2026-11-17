//! Native trust-file export for the pinned engine. Export does not qualify runtime admission.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use avian_membership::MembershipStore;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const ENGINE_VERSION: &str = "2.6.4";
pub const REQUIRED_ADMISSION_EXTENSION: &str = "overlay-address-claims-v1";
pub const TRUST_FILE_FORMAT: &str = "avian-verified-membership-v1";
pub const TRUST_LEASE_DURATION: Duration = Duration::from_secs(300);
pub const TRUST_LEASE_REFRESH_MARGIN: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("the export clock is outside the supported Unix timestamp range")]
    InvalidClock,
    #[error("the transport public key is not a canonical 32-byte X25519 key")]
    InvalidTransportKey,
    #[error("the trust file destination must be a regular file, not a symbolic link")]
    InvalidDestination,
    #[error("a protected platform storage adapter is required to write this file")]
    ProtectedStorageRequired,
    #[error("randomness is unavailable: {0}")]
    Randomness(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, ExportError>;

/// Public-only entry for the pinned engine with the AVIAN admission extension.
/// There is deliberately no field that can serialize a private credential.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeCredential {
    pub pubkey: String,
    pub groups: Vec<String>,
    pub allow_relay: bool,
    pub allowed_proxy_cidrs: Vec<String>,
    pub overlay_addresses: Vec<String>,
    pub reusable: bool,
    pub expiry_unix: i64,
    pub created_at_unix: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TrustFileFormat {
    #[serde(rename = "avian-verified-membership-v1")]
    VerifiedMembershipV1,
}

/// An explicit format marker keeps an empty membership cache authoritative.
/// A flat native credential map cannot distinguish that state from absent local authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeTrustFile {
    pub format: TrustFileFormat,
    pub credentials: BTreeMap<String, NativeCredential>,
}

#[derive(Serialize)]
struct NativeTrustFileView<'a> {
    format: TrustFileFormat,
    credentials: &'a BTreeMap<String, NativeCredential>,
}

#[derive(Clone, Debug)]
pub struct TrustFileExport {
    credentials: BTreeMap<String, NativeCredential>,
    pub network_id: String,
    pub revocation_revision: u64,
    pub created_at_unix: i64,
    pub expires_at_unix: i64,
    pub refresh_at_unix: i64,
}

impl TrustFileExport {
    /// Derive temporary engine leases from durable, already verified membership.
    /// This never extends membership, distributes an administrator key, or imports remote trust.
    pub fn from_membership(store: &MembershipStore, now: SystemTime) -> Result<Self> {
        let created_at_unix = unix_timestamp(now)?;
        let expires_at_unix = created_at_unix
            .checked_add(TRUST_LEASE_DURATION.as_secs() as i64)
            .ok_or(ExportError::InvalidClock)?;
        let refresh_at_unix = expires_at_unix - TRUST_LEASE_REFRESH_MARGIN.as_secs() as i64;
        let mut credentials = BTreeMap::new();
        for membership in store.memberships() {
            let claims = &membership.claims;
            let mut public_key = [0u8; 32];
            hex::decode_to_slice(&claims.transport_public_key, &mut public_key)
                .map_err(|_| ExportError::InvalidTransportKey)?;
            if public_key == [0; 32] || hex::encode(public_key) != claims.transport_public_key {
                return Err(ExportError::InvalidTransportKey);
            }
            credentials.insert(
                claims.node_id.clone(),
                NativeCredential {
                    pubkey: STANDARD.encode(public_key),
                    groups: Vec::new(),
                    allow_relay: claims.permissions.relay,
                    allowed_proxy_cidrs: Vec::new(),
                    overlay_addresses: claims
                        .overlay_addresses
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                    reusable: false,
                    expiry_unix: expires_at_unix,
                    created_at_unix,
                },
            );
        }
        Ok(Self {
            credentials,
            network_id: store.trust_root().network_id.clone(),
            revocation_revision: store.revocation_revision(),
            created_at_unix,
            expires_at_unix,
            refresh_at_unix,
        })
    }

    pub fn credentials(&self) -> &BTreeMap<String, NativeCredential> {
        &self.credentials
    }

    pub fn refresh_due(&self, now: SystemTime) -> Result<bool> {
        let now = unix_timestamp(now)?;
        // Clock rollback must regenerate leases rather than prolong the old local window.
        Ok(now < self.created_at_unix || now >= self.refresh_at_unix)
    }

    pub fn is_expired(&self, now: SystemTime) -> Result<bool> {
        let now = unix_timestamp(now)?;
        Ok(now < self.created_at_unix || now >= self.expires_at_unix)
    }

    /// Write the authoritative local cache consumed by the required admission extension.
    /// The unpatched upstream engine cannot consume this format. Runtime reload and session
    /// revocation must be qualified separately from writing the file successfully.
    pub fn write_startup_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let document = NativeTrustFileView {
            format: TrustFileFormat::VerifiedMembershipV1,
            credentials: &self.credentials,
        };
        let mut encoded = serde_json::to_vec_pretty(&document)?;
        encoded.push(b'\n');
        avian_storage::atomic_write_private_file(path, &encoded).map_err(|error| match error {
            avian_storage::StorageError::InsecurePermissions => ExportError::InvalidDestination,
            avian_storage::StorageError::BackendUnavailable => {
                ExportError::ProtectedStorageRequired
            }
            avian_storage::StorageError::Io(error) => ExportError::Io(error),
            error => ExportError::Io(std::io::Error::other(error)),
        })
    }
}

fn unix_timestamp(time: SystemTime) -> Result<i64> {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ExportError::InvalidClock)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| ExportError::InvalidClock)
}
