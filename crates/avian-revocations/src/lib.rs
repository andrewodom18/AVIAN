//! Bounded anti-entropy for administrator-signed revocations.
//! Peer responses are untrusted; only the pinned administrator can authorize updates.

use avian_engine::TrustFileExport;
use avian_membership::{MembershipStore, SignedRevocations, TrustRoot};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    net::IpAddr,
    path::Path,
    time::{Duration, SystemTime},
};
use thiserror::Error;

pub const MAXIMUM_RECORDS: usize = 256;
pub const MAXIMUM_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum DistributionError {
    #[error("invalid revocation batch: {0}")]
    InvalidBatch(&'static str),
    #[error("another membership operation is in progress")]
    StoreBusy,
    #[error("invalid synchronization endpoint")]
    InvalidEndpoint,
    #[error("peer returned HTTP {0}")]
    PeerStatus(u16),
    #[error("peer response exceeds the response budget")]
    ResponseTooLarge,
    #[error(transparent)]
    Membership(#[from] avian_membership::MembershipError),
    #[error(transparent)]
    Export(#[from] avian_engine::ExportError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}
pub type Result<T> = std::result::Result<T, DistributionError>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevocationBatch {
    pub version: u32,
    pub network_id: String,
    pub after_revision: u64,
    pub next_revision: u64,
    /// Informational peer head, never an authorization or trusted checkpoint.
    pub current_revision: u64,
    pub records: Vec<SignedRevocations>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ApplyOutcome {
    pub previous_revision: u64,
    pub current_revision: u64,
    pub applied_records: usize,
}

/// Limits apply to one response, never network membership.
pub fn export_batch(
    store: &MembershipStore,
    after: u64,
    maximum_records: usize,
    maximum_bytes: usize,
) -> Result<RevocationBatch> {
    if maximum_records == 0
        || maximum_records > MAXIMUM_RECORDS
        || maximum_bytes == 0
        || maximum_bytes > MAXIMUM_RESPONSE_BYTES
    {
        return Err(DistributionError::InvalidBatch("response budget"));
    }
    if after > store.revocation_revision() {
        return Err(DistributionError::InvalidBatch(
            "requested revision is ahead of local state",
        ));
    }
    let mut batch = RevocationBatch {
        version: 1,
        network_id: store.trust_root().network_id.clone(),
        after_revision: after,
        next_revision: after,
        current_revision: store.revocation_revision(),
        records: Vec::new(),
    };
    for record in store.revocations_after(after).take(maximum_records) {
        batch.records.push(record.clone());
        batch.next_revision = record.claims.revision;
        if serde_json::to_vec(&batch)?.len() > maximum_bytes {
            batch.records.pop();
            batch.next_revision = batch
                .records
                .last()
                .map_or(after, |record| record.claims.revision);
            if batch.records.is_empty() {
                return Err(DistributionError::ResponseTooLarge);
            }
            break;
        }
    }
    if serde_json::to_vec(&batch)?.len() > maximum_bytes {
        return Err(DistributionError::ResponseTooLarge);
    }
    Ok(batch)
}

/// Verify a complete page before mutating the caller's state.
pub fn verify_batch(store: &MembershipStore, batch: &RevocationBatch) -> Result<MembershipStore> {
    if batch.version != 1 || batch.network_id != store.trust_root().network_id {
        return Err(DistributionError::InvalidBatch("version or network"));
    }
    if batch.after_revision != store.revocation_revision() || batch.records.len() > MAXIMUM_RECORDS
    {
        return Err(DistributionError::InvalidBatch("cursor or record budget"));
    }
    if serde_json::to_vec(batch)?.len() > MAXIMUM_RESPONSE_BYTES {
        return Err(DistributionError::ResponseTooLarge);
    }
    let mut replacement = store.clone();
    for record in &batch.records {
        if record.claims.previous_revision != replacement.revocation_revision()
            || record.claims.revision <= replacement.revocation_revision()
        {
            return Err(DistributionError::InvalidBatch(
                "unordered or replayed record",
            ));
        }
        replacement.apply_revocations(record.clone())?;
    }
    if replacement.revocation_revision() != batch.next_revision
        || batch.current_revision < batch.next_revision
        || (batch.records.is_empty() && batch.current_revision != batch.after_revision)
    {
        return Err(DistributionError::InvalidBatch(
            "inconsistent revision metadata",
        ));
    }
    Ok(replacement)
}

/// The store is durable before native trust is replaced. A publication failure is
/// retried by refreshing from disk; it never rolls back an accepted revocation.
pub fn apply_batch_and_publish(
    root: TrustRoot,
    membership_path: &Path,
    engine_cache_path: &Path,
    batch: &RevocationBatch,
    now: SystemTime,
) -> Result<ApplyOutcome> {
    let _lock = lock_store(membership_path)?;
    let store = MembershipStore::load(membership_path, root)?;
    let replacement = verify_batch(&store, batch)?;
    if batch.records.is_empty() {
        return Ok(ApplyOutcome {
            previous_revision: store.revocation_revision(),
            current_revision: store.revocation_revision(),
            applied_records: 0,
        });
    }
    let export = TrustFileExport::from_membership(&replacement, now)?;
    replacement.save(membership_path)?;
    export.write_startup_file(engine_cache_path)?;
    Ok(ApplyOutcome {
        previous_revision: store.revocation_revision(),
        current_revision: replacement.revocation_revision(),
        applied_records: batch.records.len(),
    })
}

/// Renew short native leases from durable verified membership, including after restart.
pub fn refresh_engine_cache(
    root: TrustRoot,
    membership_path: &Path,
    engine_cache_path: &Path,
    now: SystemTime,
) -> Result<u64> {
    let _lock = lock_store(membership_path)?;
    let store = MembershipStore::load(membership_path, root)?;
    TrustFileExport::from_membership(&store, now)?.write_startup_file(engine_cache_path)?;
    Ok(store.revocation_revision())
}

fn lock_store(membership_path: &Path) -> Result<fs::File> {
    let directory = membership_path.parent().unwrap_or_else(|| Path::new("."));
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(directory.join("membership-operation.lock"))?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|_| DistributionError::StoreBusy)?;
    Ok(file)
}

pub fn synchronization_client() -> Result<Client> {
    Ok(Client::builder()
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()?)
}

/// Fetch public signed records. The caller applies them against its own pinned trust root.
pub async fn fetch_batch(
    client: &Client,
    peer: &str,
    network_id: &str,
    after: u64,
    limit: usize,
) -> Result<RevocationBatch> {
    if limit == 0 || limit > MAXIMUM_RECORDS {
        return Err(DistributionError::InvalidBatch("request budget"));
    }
    let mut endpoint = Url::parse(peer).map_err(|_| DistributionError::InvalidEndpoint)?;
    let private_address = endpoint
        .host_str()
        .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|address| match address {
            IpAddr::V4(address) => address.is_loopback() || address.is_private(),
            IpAddr::V6(address) => {
                address.is_loopback() || address.segments()[0] & 0xfe00 == 0xfc00
            }
        });
    if !(endpoint.scheme() == "https" || endpoint.scheme() == "http" && private_address)
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
        || endpoint.path() != "/"
    {
        return Err(DistributionError::InvalidEndpoint);
    }
    endpoint.set_path("/v1/revocations");
    endpoint
        .query_pairs_mut()
        .append_pair("network_id", network_id)
        .append_pair("after_revision", &after.to_string())
        .append_pair("limit", &limit.to_string());
    let mut response = client.get(endpoint).send().await?;
    if !response.status().is_success() {
        return Err(DistributionError::PeerStatus(response.status().as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAXIMUM_RESPONSE_BYTES as u64)
    {
        return Err(DistributionError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAXIMUM_RESPONSE_BYTES {
            return Err(DistributionError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}
