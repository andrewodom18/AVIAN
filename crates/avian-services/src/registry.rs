use std::{collections::BTreeMap, path::Path};

use avian_membership::{MembershipStore, TrustRoot};
use serde::{Deserialize, Serialize};

use crate::{Limits, Result, ServiceError, SignedAnnouncement, active_member};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptedRecord {
    announcement: SignedAnnouncement,
    accepted_at_unix: i64,
}

/// Watermarks outlive announcements, including withdrawals and revoked owners.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerState {
    revision: u64,
    membership_revision: u64,
    issued_at_unix: i64,
    digest: String,
    record: Option<AcceptedRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredRegistry {
    version: u32,
    trust_root: TrustRoot,
    revocation_revision: u64,
    last_observed_unix: i64,
    owners: BTreeMap<String, OwnerState>,
}

#[derive(Clone, Debug)]
pub struct Registry {
    stored: StoredRegistry,
    limits: Limits,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Advertisement {
    pub announcement: SignedAnnouncement,
    pub accepted_at_unix: i64,
    pub age_seconds: i64,
    pub expires_in_seconds: i64,
    pub reachability: Reachability,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    Unobserved,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServicePage {
    pub network_id: String,
    pub generated_at_unix: i64,
    pub revocation_revision: u64,
    pub items: Vec<Advertisement>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnnouncementPage {
    pub version: u32,
    pub network_id: String,
    pub generated_at_unix: i64,
    pub records: Vec<SignedAnnouncement>,
    pub next_cursor: Option<String>,
}

impl Registry {
    pub fn new(membership: &MembershipStore, limits: Limits) -> Result<Self> {
        limits.validate()?;
        Ok(Self {
            stored: StoredRegistry {
                version: 1,
                trust_root: membership.trust_root().clone(),
                revocation_revision: membership.revocation_revision(),
                last_observed_unix: 0,
                owners: BTreeMap::new(),
            },
            limits,
        })
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub fn next_revision(&self, owner_node_id: &str) -> Result<u64> {
        self.stored
            .owners
            .get(owner_node_id)
            .map_or(Ok(1), |owner| {
                owner.revision.checked_add(1).ok_or(ServiceError::Rollback)
            })
    }

    fn check_membership(&self, membership: &MembershipStore) -> Result<()> {
        if membership.trust_root() != &self.stored.trust_root {
            return Err(ServiceError::WrongNetwork);
        }
        if membership.revocation_revision() < self.stored.revocation_revision {
            return Err(ServiceError::Rollback);
        }
        for (node_id, state) in &self.stored.owners {
            if let Ok(owner) = active_member(membership, node_id)
                && owner.revision < state.membership_revision
            {
                return Err(ServiceError::Rollback);
            }
        }
        Ok(())
    }

    /// Returns false for an exact duplicate. Duplicate delivery never renews the TTL.
    pub fn accept(
        &mut self,
        membership: &MembershipStore,
        announcement: SignedAnnouncement,
        now_unix: i64,
    ) -> Result<bool> {
        self.check_membership(membership)?;
        if now_unix < self.stored.last_observed_unix {
            return Err(ServiceError::Rollback);
        }
        announcement.verify(membership, now_unix, &self.limits)?;
        let claims = &announcement.claims;
        let digest = announcement.digest()?;
        if let Some(previous) = self.stored.owners.get(&claims.owner_node_id) {
            if claims.revision == previous.revision && digest == previous.digest {
                return Ok(false);
            }
            if claims.revision <= previous.revision
                || claims.issued_at_unix < previous.issued_at_unix
            {
                return Err(ServiceError::Rollback);
            }
        }
        let mut replacement = self.clone();
        replacement.prune(membership, now_unix)?;
        replacement.stored.owners.insert(
            claims.owner_node_id.clone(),
            OwnerState {
                revision: claims.revision,
                membership_revision: claims.membership_revision,
                issued_at_unix: claims.issued_at_unix,
                digest,
                record: Some(AcceptedRecord {
                    announcement,
                    accepted_at_unix: now_unix,
                }),
            },
        );
        replacement.check_budget()?;
        *self = replacement;
        Ok(true)
    }

    /// Verify a whole received page before changing live state.
    pub fn accept_page(
        &mut self,
        membership: &MembershipStore,
        page: AnnouncementPage,
        now_unix: i64,
    ) -> Result<usize> {
        if page.version != 1 || page.network_id != self.stored.trust_root.network_id {
            return Err(ServiceError::WrongNetwork);
        }
        if page.records.len() > self.limits.maximum_page_size {
            return Err(ServiceError::ResourceBudget);
        }
        let mut replacement = self.clone();
        replacement.prune(membership, now_unix)?;
        let mut changed = 0;
        for record in page.records {
            changed += usize::from(replacement.accept(membership, record, now_unix)?);
        }
        *self = replacement;
        Ok(changed)
    }

    /// Expiry and revocation hide endpoints but never erase replay protection.
    pub fn prune(&mut self, membership: &MembershipStore, now_unix: i64) -> Result<()> {
        self.check_membership(membership)?;
        if now_unix < self.stored.last_observed_unix {
            return Err(ServiceError::Rollback);
        }
        self.stored.last_observed_unix = now_unix;
        self.stored.revocation_revision = membership.revocation_revision();
        for (node_id, owner) in &mut self.stored.owners {
            let remove = owner.record.as_ref().is_some_and(|record| {
                record.announcement.claims.expires_at_unix <= now_unix
                    || active_member(membership, node_id).is_err()
                    || active_member(membership, node_id).is_ok_and(|membership| {
                        membership.revision != record.announcement.claims.membership_revision
                    })
            });
            if remove {
                owner.record = None;
            }
            if let Ok(current_membership) = active_member(membership, node_id) {
                owner.membership_revision = current_membership.revision;
            }
        }
        Ok(())
    }

    pub fn page(
        &mut self,
        membership: &MembershipStore,
        after: Option<&str>,
        limit: usize,
        now_unix: i64,
    ) -> Result<ServicePage> {
        if limit == 0 || limit > self.limits.maximum_page_size {
            return Err(ServiceError::InvalidField("page size"));
        }
        if let Some(cursor) = after {
            validate_hex(cursor)?;
        }
        self.prune(membership, now_unix)?;
        let mut records = self
            .stored
            .owners
            .iter()
            .filter(|(node_id, _)| after.is_none_or(|cursor| node_id.as_str() > cursor))
            .filter_map(|(_, owner)| owner.record.as_ref());
        let selected: Vec<_> = records.by_ref().take(limit).collect();
        let next_cursor = if records.next().is_some() {
            selected
                .last()
                .map(|record| record.announcement.claims.owner_node_id.clone())
        } else {
            None
        };
        Ok(ServicePage {
            network_id: self.stored.trust_root.network_id.clone(),
            generated_at_unix: now_unix,
            revocation_revision: membership.revocation_revision(),
            items: selected
                .into_iter()
                .map(|record| Advertisement {
                    announcement: record.announcement.clone(),
                    accepted_at_unix: record.accepted_at_unix,
                    age_seconds: now_unix
                        .saturating_sub(record.announcement.claims.issued_at_unix)
                        .max(0),
                    expires_in_seconds: record.announcement.claims.expires_at_unix - now_unix,
                    reachability: Reachability::Unobserved,
                })
                .collect(),
            next_cursor,
        })
    }

    pub fn announcements_page(
        &mut self,
        membership: &MembershipStore,
        after: Option<&str>,
        limit: usize,
        now_unix: i64,
    ) -> Result<AnnouncementPage> {
        let page = self.page(membership, after, limit, now_unix)?;
        Ok(AnnouncementPage {
            version: 1,
            network_id: page.network_id,
            generated_at_unix: page.generated_at_unix,
            records: page
                .items
                .into_iter()
                .map(|item| item.announcement)
                .collect(),
            next_cursor: page.next_cursor,
        })
    }

    fn check_budget(&self) -> Result<()> {
        if serde_json::to_vec(&self.stored)?.len().saturating_add(1)
            > self.limits.maximum_registry_bytes
        {
            return Err(ServiceError::ResourceBudget);
        }
        Ok(())
    }

    pub fn load(
        path: impl AsRef<Path>,
        membership: &MembershipStore,
        limits: Limits,
        now_unix: i64,
    ) -> Result<Self> {
        limits.validate()?;
        let path = path.as_ref();
        let bytes = avian_storage::read_private_file(path, limits.maximum_registry_bytes)?;
        let stored: StoredRegistry = serde_json::from_slice(&bytes)?;
        if stored.version != 1 {
            return Err(ServiceError::InvalidField("registry version"));
        }
        let mut registry = Self { stored, limits };
        registry.check_membership(membership)?;
        for (node_id, owner) in &registry.stored.owners {
            validate_hex(node_id)?;
            validate_hex(&owner.digest)?;
            if owner.revision == 0 || owner.membership_revision == 0 || owner.issued_at_unix < 0 {
                return Err(ServiceError::InvalidField("stored watermark"));
            }
            if let Some(record) = &owner.record {
                let claims = &record.announcement.claims;
                if claims.owner_node_id != *node_id
                    || claims.revision != owner.revision
                    || claims.membership_revision != owner.membership_revision
                    || claims.issued_at_unix != owner.issued_at_unix
                    || record.announcement.digest()? != owner.digest
                    || record.accepted_at_unix < 0
                {
                    return Err(ServiceError::Rollback);
                }
                if active_member(membership, node_id)
                    .is_ok_and(|member| member.revision == claims.membership_revision)
                {
                    record
                        .announcement
                        .verify_identity(membership, &registry.limits)?;
                }
            }
        }
        registry.prune(membership, now_unix)?;
        Ok(registry)
    }

    /// Persist accepted state before exposing it to callers or peers.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        self.check_budget()?;
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let filename = path
            .file_name()
            .ok_or(ServiceError::InvalidField("state path"))?
            .to_string_lossy();
        let lock = avian_storage::open_private_lock(parent.join(format!("{filename}.lock")))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| ServiceError::StoreBusy)?;
        if std::fs::symlink_metadata(path).is_ok() {
            let bytes = avian_storage::read_private_file(path, self.limits.maximum_registry_bytes)?;
            let previous: StoredRegistry = serde_json::from_slice(&bytes)?;
            if previous.trust_root != self.stored.trust_root
                || previous.revocation_revision > self.stored.revocation_revision
                || previous.last_observed_unix > self.stored.last_observed_unix
            {
                return Err(ServiceError::Rollback);
            }
            for (node_id, old) in previous.owners {
                let current = self
                    .stored
                    .owners
                    .get(&node_id)
                    .ok_or(ServiceError::Rollback)?;
                if current.revision < old.revision
                    || current.membership_revision < old.membership_revision
                    || current.issued_at_unix < old.issued_at_unix
                    || (current.revision == old.revision && current.digest != old.digest)
                {
                    return Err(ServiceError::Rollback);
                }
            }
        }
        let mut bytes = serde_json::to_vec(&self.stored)?;
        bytes.push(b'\n');
        avian_storage::atomic_write_private_file(path, &bytes)?;
        Ok(())
    }
}

fn validate_hex(value: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ServiceError::InvalidField("node identifier or digest"));
    }
    Ok(())
}
