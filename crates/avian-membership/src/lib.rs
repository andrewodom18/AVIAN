//! Signed offline network membership. Transport admission must consume these decisions explicitly.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::net::IpAddr;
use std::path::Path;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

const FORMAT_VERSION: u32 = 1;
const MEMBERSHIP_CONTEXT: &[u8] = b"AVIAN membership v1\0";
const REVOCATION_CONTEXT: &[u8] = b"AVIAN revocation v1\0";

#[derive(Debug, Error)]
pub enum MembershipError {
    #[error("invalid {0}")]
    InvalidField(&'static str),
    #[error("the record belongs to another network")]
    WrongNetwork,
    #[error("signature verification failed")]
    InvalidSignature,
    #[error("the signing key does not match the trusted administrator")]
    WrongAdministrator,
    #[error("the device is revoked")]
    Revoked,
    #[error("the device is not enrolled")]
    UnknownDevice,
    #[error("the transport key does not match the enrolled device")]
    WrongTransportKey,
    #[error("the record would roll back previously accepted state")]
    Rollback,
    #[error(
        "revocation updates are missing; expected previous revision {expected}, received {received}"
    )]
    MissingRevocations { expected: u64, received: u64 },
    #[error("overlay address {0} is already assigned to another device")]
    AddressConflict(IpAddr),
    #[error("the transport key is already assigned to another device")]
    TransportKeyConflict,
    #[error("another process is saving this membership store")]
    StoreBusy,
    #[error("private files require owner-only permissions")]
    InsecurePermissions,
    #[error("use the platform credential store to protect private keys on this platform")]
    CredentialStoreRequired,
    #[error("randomness is unavailable: {0}")]
    Randomness(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, MembershipError>;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustRoot {
    pub network_id: String,
    pub administrator_public_key: String,
}

impl TrustRoot {
    pub fn new(network_id: impl Into<String>, administrator: &VerifyingKey) -> Result<Self> {
        let root = Self {
            network_id: network_id.into(),
            administrator_public_key: public_key_hex(administrator),
        };
        root.validate()?;
        Ok(root)
    }

    pub fn validate(&self) -> Result<()> {
        if self.network_id.is_empty()
            || self.network_id.len() > 128
            || !self
                .network_id
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_'))
        {
            return Err(MembershipError::InvalidField("network_id"));
        }
        verifying_key(&self.administrator_public_key)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Permissions {
    pub relay: bool,
    pub administer: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipClaims {
    pub version: u32,
    pub network_id: String,
    pub node_id: String,
    pub identity_public_key: String,
    pub transport_public_key: String,
    pub overlay_addresses: Vec<IpAddr>,
    pub revision: u64,
    pub permissions: Permissions,
}

impl MembershipClaims {
    pub fn new(
        network_id: impl Into<String>,
        identity_public_key: &VerifyingKey,
        transport_public_key: [u8; 32],
        overlay_addresses: Vec<IpAddr>,
        revision: u64,
        permissions: Permissions,
    ) -> Self {
        Self {
            version: FORMAT_VERSION,
            network_id: network_id.into(),
            node_id: node_id_for_key(identity_public_key),
            identity_public_key: public_key_hex(identity_public_key),
            transport_public_key: hex::encode(transport_public_key),
            overlay_addresses,
            revision,
            permissions,
        }
    }

    fn validate(&self, root: &TrustRoot) -> Result<()> {
        if self.version != FORMAT_VERSION {
            return Err(MembershipError::InvalidField("membership version"));
        }
        if self.network_id != root.network_id {
            return Err(MembershipError::WrongNetwork);
        }
        let identity = verifying_key(&self.identity_public_key)?;
        if self.node_id != node_id_for_key(&identity) {
            return Err(MembershipError::InvalidField("node_id"));
        }
        let transport_key = decode_hex::<32>(&self.transport_public_key, "transport_public_key")?;
        if transport_key == [0; 32] {
            return Err(MembershipError::InvalidField("transport_public_key"));
        }
        if self.revision == 0 {
            return Err(MembershipError::InvalidField("membership revision"));
        }
        let mut unique_addresses = BTreeSet::new();
        for address in &self.overlay_addresses {
            let is_private = match address {
                IpAddr::V4(address) => address.is_private(),
                IpAddr::V6(address) => address.segments()[0] & 0xfe00 == 0xfc00,
            };
            if !is_private || !unique_addresses.insert(*address) {
                return Err(MembershipError::InvalidField("overlay_addresses"));
            }
        }
        if unique_addresses.is_empty() {
            return Err(MembershipError::InvalidField("overlay_addresses"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedMembership {
    pub claims: MembershipClaims,
    pub signature: String,
}

impl SignedMembership {
    pub fn verify(&self, root: &TrustRoot) -> Result<()> {
        root.validate()?;
        self.claims.validate(root)?;
        verify_signature(root, MEMBERSHIP_CONTEXT, &self.claims, &self.signature)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevocationClaims {
    pub version: u32,
    pub network_id: String,
    pub previous_revision: u64,
    pub revision: u64,
    pub revoked_node_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRevocations {
    pub claims: RevocationClaims,
    pub signature: String,
}

impl SignedRevocations {
    pub fn verify(&self, root: &TrustRoot) -> Result<()> {
        root.validate()?;
        let claims = &self.claims;
        if claims.network_id != root.network_id {
            return Err(MembershipError::WrongNetwork);
        }
        if claims.version != FORMAT_VERSION
            || claims.previous_revision.checked_add(1) != Some(claims.revision)
            || claims.revoked_node_ids.is_empty()
        {
            return Err(MembershipError::InvalidField(
                "revocation revision or records",
            ));
        }
        let mut unique_nodes = BTreeSet::new();
        for node_id in &claims.revoked_node_ids {
            decode_hex::<32>(node_id, "revoked node_id")?;
            if !unique_nodes.insert(node_id) {
                return Err(MembershipError::InvalidField("duplicate revoked node_id"));
            }
        }
        verify_signature(root, REVOCATION_CONTEXT, claims, &self.signature)
    }
}

pub fn generate_signing_key() -> Result<SigningKey> {
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::fill(seed.as_mut())
        .map_err(|error| MembershipError::Randomness(error.to_string()))?;
    Ok(SigningKey::from_bytes(&seed))
}

pub fn public_key_hex(key: &VerifyingKey) -> String {
    hex::encode(key.as_bytes())
}

pub fn node_id_for_key(key: &VerifyingKey) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

pub fn fingerprint(key: &VerifyingKey) -> String {
    node_id_for_key(key)
        .as_bytes()
        .chunks(4)
        .map(|chunk| std::str::from_utf8(chunk).expect("hex is ASCII"))
        .collect::<Vec<_>>()
        .join("-")
}

pub fn issue_membership(
    root: &TrustRoot,
    administrator: &SigningKey,
    claims: MembershipClaims,
) -> Result<SignedMembership> {
    check_administrator(root, administrator)?;
    claims.validate(root)?;
    let signature = sign(administrator, MEMBERSHIP_CONTEXT, &claims)?;
    Ok(SignedMembership { claims, signature })
}

pub fn issue_revocations(
    root: &TrustRoot,
    administrator: &SigningKey,
    previous_revision: u64,
    revoked_node_ids: Vec<String>,
) -> Result<SignedRevocations> {
    check_administrator(root, administrator)?;
    let claims = RevocationClaims {
        version: FORMAT_VERSION,
        network_id: root.network_id.clone(),
        previous_revision,
        revision: previous_revision
            .checked_add(1)
            .ok_or(MembershipError::InvalidField("revocation revision"))?,
        revoked_node_ids,
    };
    let signed = SignedRevocations {
        signature: sign(administrator, REVOCATION_CONTEXT, &claims)?,
        claims,
    };
    signed.verify(root)?;
    Ok(signed)
}

/// The store contains memberships this device needs, not a mandatory global directory.
#[derive(Clone, Debug)]
pub struct MembershipStore {
    root: TrustRoot,
    memberships: BTreeMap<String, SignedMembership>,
    address_owners: BTreeMap<IpAddr, String>,
    transport_key_owners: BTreeMap<String, String>,
    revocations: Vec<SignedRevocations>,
    revoked_nodes: BTreeSet<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredMemberships {
    version: u32,
    network_id: String,
    memberships: Vec<SignedMembership>,
    revocations: Vec<SignedRevocations>,
}

impl MembershipStore {
    pub fn new(root: TrustRoot) -> Result<Self> {
        root.validate()?;
        Ok(Self {
            root,
            memberships: BTreeMap::new(),
            address_owners: BTreeMap::new(),
            transport_key_owners: BTreeMap::new(),
            revocations: Vec::new(),
            revoked_nodes: BTreeSet::new(),
        })
    }

    pub fn trust_root(&self) -> &TrustRoot {
        &self.root
    }

    pub fn revocation_revision(&self) -> u64 {
        self.revocations
            .last()
            .map_or(0, |record| record.claims.revision)
    }

    pub fn memberships(&self) -> impl Iterator<Item = &SignedMembership> {
        self.memberships
            .values()
            .filter(|record| !self.revoked_nodes.contains(&record.claims.node_id))
    }

    /// Includes revoked records for complete offline snapshots and rollback checks.
    /// Admission and discovery must use `memberships` or `authorize` instead.
    pub fn membership_history(&self) -> impl Iterator<Item = &SignedMembership> {
        self.memberships.values()
    }

    pub fn revocations_after(&self, revision: u64) -> impl Iterator<Item = &SignedRevocations> {
        self.revocations
            .iter()
            .filter(move |record| record.claims.revision > revision)
    }

    pub fn import_membership(&mut self, membership: SignedMembership) -> Result<()> {
        membership.verify(&self.root)?;
        let claims = &membership.claims;
        if self.revoked_nodes.contains(&claims.node_id) {
            return Err(MembershipError::Revoked);
        }
        if let Some(previous) = self.memberships.get(&claims.node_id) {
            if previous == &membership {
                return Ok(());
            }
            if claims.revision <= previous.claims.revision {
                return Err(MembershipError::Rollback);
            }
        }
        if self
            .transport_key_owners
            .get(&claims.transport_public_key)
            .is_some_and(|owner| owner != &claims.node_id)
        {
            return Err(MembershipError::TransportKeyConflict);
        }
        for address in &claims.overlay_addresses {
            if self
                .address_owners
                .get(address)
                .is_some_and(|owner| owner != &claims.node_id)
            {
                return Err(MembershipError::AddressConflict(*address));
            }
        }
        if let Some(previous) = self.memberships.get(&claims.node_id) {
            self.transport_key_owners
                .remove(&previous.claims.transport_public_key);
            for address in &previous.claims.overlay_addresses {
                self.address_owners.remove(address);
            }
        }
        self.transport_key_owners
            .insert(claims.transport_public_key.clone(), claims.node_id.clone());
        for address in &claims.overlay_addresses {
            self.address_owners.insert(*address, claims.node_id.clone());
        }
        self.memberships.insert(claims.node_id.clone(), membership);
        Ok(())
    }

    pub fn apply_revocations(&mut self, record: SignedRevocations) -> Result<()> {
        record.verify(&self.root)?;
        if self.revocations.last() == Some(&record) {
            return Ok(());
        }
        let current_revision = self.revocation_revision();
        if record.claims.revision <= current_revision {
            return Err(MembershipError::Rollback);
        }
        if record.claims.previous_revision != current_revision {
            return Err(MembershipError::MissingRevocations {
                expected: current_revision,
                received: record.claims.previous_revision,
            });
        }
        self.revoked_nodes
            .extend(record.claims.revoked_node_ids.iter().cloned());
        for node_id in &record.claims.revoked_node_ids {
            if let Some(membership) = self.memberships.get(node_id) {
                if self
                    .transport_key_owners
                    .get(&membership.claims.transport_public_key)
                    == Some(node_id)
                {
                    self.transport_key_owners
                        .remove(&membership.claims.transport_public_key);
                }
                for address in &membership.claims.overlay_addresses {
                    if self.address_owners.get(address) == Some(node_id) {
                        self.address_owners.remove(address);
                    }
                }
            }
        }
        self.revocations.push(record);
        Ok(())
    }

    /// The caller must authenticate possession of this transport key before admission.
    pub fn authorize(
        &self,
        node_id: &str,
        transport_public_key: &str,
    ) -> Result<&MembershipClaims> {
        if self.revoked_nodes.contains(node_id) {
            return Err(MembershipError::Revoked);
        }
        let record = self
            .memberships
            .get(node_id)
            .ok_or(MembershipError::UnknownDevice)?;
        if record.claims.transport_public_key != transport_public_key {
            return Err(MembershipError::WrongTransportKey);
        }
        Ok(&record.claims)
    }

    /// Load from a protected directory using a separately pinned administrator key.
    /// A running process should also call `validate_successor` before replacing live state.
    pub fn load(path: impl AsRef<Path>, expected_root: TrustRoot) -> Result<Self> {
        let stored: StoredMemberships = serde_json::from_reader(File::open(path)?)?;
        if stored.version != FORMAT_VERSION {
            return Err(MembershipError::InvalidField("store version"));
        }
        if stored.network_id != expected_root.network_id {
            return Err(MembershipError::WrongNetwork);
        }
        Self::from_records(expected_root, stored.memberships, stored.revocations)
    }

    /// Verify a complete signed snapshot, retaining revoked history without reserving
    /// addresses or transport keys that may have been reassigned after revocation.
    pub fn from_records(
        expected_root: TrustRoot,
        memberships: Vec<SignedMembership>,
        revocations: Vec<SignedRevocations>,
    ) -> Result<Self> {
        let mut store = Self::new(expected_root)?;
        // Verify the revocation chain first so old address assignments can be reused safely.
        for record in revocations {
            store.apply_revocations(record)?;
        }
        for record in memberships {
            record.verify(&store.root)?;
            if store.memberships.contains_key(&record.claims.node_id) {
                return Err(MembershipError::InvalidField("duplicate stored membership"));
            }
            if store.revoked_nodes.contains(&record.claims.node_id) {
                store
                    .memberships
                    .insert(record.claims.node_id.clone(), record);
            } else {
                store.import_membership(record)?;
            }
        }
        Ok(store)
    }

    /// Serializes writers and refuses to replace a newer store with an older snapshot.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let filename = path
            .file_name()
            .ok_or(MembershipError::InvalidField("store path"))?;
        let lock_path = parent.join(format!("{}.lock", filename.to_string_lossy()));
        let lock = avian_storage::open_private_lock(lock_path).map_err(storage_error)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| MembershipError::StoreBusy)?;

        if path.exists() {
            let previous = Self::load(path, self.root.clone())?;
            self.validate_successor(&previous)?;
        }
        let stored = StoredMemberships {
            version: FORMAT_VERSION,
            network_id: self.root.network_id.clone(),
            memberships: self.memberships.values().cloned().collect(),
            revocations: self.revocations.clone(),
        };
        let mut encoded = serde_json::to_vec_pretty(&stored)?;
        encoded.push(b'\n');
        avian_storage::atomic_write_private_file(path, &encoded).map_err(storage_error)
    }

    pub fn validate_successor(&self, previous: &Self) -> Result<()> {
        if self.root != previous.root {
            return Err(MembershipError::WrongNetwork);
        }
        if !self.revocations.starts_with(&previous.revocations) {
            return Err(MembershipError::Rollback);
        }
        for (node_id, old_record) in &previous.memberships {
            let current = self
                .memberships
                .get(node_id)
                .ok_or(MembershipError::Rollback)?;
            if current.claims.revision < old_record.claims.revision
                || (current.claims.revision == old_record.claims.revision && current != old_record)
            {
                return Err(MembershipError::Rollback);
            }
        }
        Ok(())
    }
}

/// Creates a new private key file without overwriting an existing identity.
pub fn write_signing_key(path: impl AsRef<Path>, key: &SigningKey) -> Result<()> {
    let mut encoded = Zeroizing::new(hex::encode(key.to_bytes()));
    encoded.push('\n');
    avian_storage::create_secret_file(path, encoded.as_bytes()).map_err(storage_error)
}

/// Uses a host-provided wrapping adapter, including Android protected storage.
pub fn write_signing_key_with_protector(
    path: impl AsRef<Path>,
    key: &SigningKey,
    protector: &dyn avian_storage::SecretProtector,
) -> Result<()> {
    let mut encoded = Zeroizing::new(hex::encode(key.to_bytes()));
    encoded.push('\n');
    avian_storage::create_secret_file_with_protector(path, encoded.as_bytes(), protector)
        .map_err(storage_error)
}

pub fn read_signing_key(path: impl AsRef<Path>) -> Result<SigningKey> {
    decode_signing_key(avian_storage::read_secret_file(path, 65))
}

pub fn read_signing_key_with_protector(
    path: impl AsRef<Path>,
    protector: &dyn avian_storage::SecretProtector,
) -> Result<SigningKey> {
    decode_signing_key(avian_storage::read_secret_file_with_protector(
        path, 65, protector,
    ))
}

fn decode_signing_key(encoded: avian_storage::Result<Zeroizing<Vec<u8>>>) -> Result<SigningKey> {
    let encoded = encoded.map_err(|error| match error {
        avian_storage::StorageError::TooLarge => MembershipError::InvalidField("private key"),
        error => storage_error(error),
    })?;
    let text =
        std::str::from_utf8(&encoded).map_err(|_| MembershipError::InvalidField("private key"))?;
    let seed = Zeroizing::new(decode_hex::<32>(text.trim(), "private key")?);
    Ok(SigningKey::from_bytes(&seed))
}

fn storage_error(error: avian_storage::StorageError) -> MembershipError {
    match error {
        avian_storage::StorageError::InsecurePermissions => MembershipError::InsecurePermissions,
        avian_storage::StorageError::BackendUnavailable => MembershipError::CredentialStoreRequired,
        avian_storage::StorageError::Io(error) => MembershipError::Io(error),
        error => MembershipError::Io(std::io::Error::other(error)),
    }
}

fn check_administrator(root: &TrustRoot, administrator: &SigningKey) -> Result<()> {
    root.validate()?;
    if public_key_hex(&administrator.verifying_key()) != root.administrator_public_key {
        return Err(MembershipError::WrongAdministrator);
    }
    Ok(())
}

fn signing_bytes(context: &[u8], claims: &impl Serialize) -> Result<Vec<u8>> {
    let mut message = context.to_vec();
    serde_json::to_writer(&mut message, claims)?;
    Ok(message)
}

fn sign(key: &SigningKey, context: &[u8], claims: &impl Serialize) -> Result<String> {
    Ok(hex::encode(
        key.sign(&signing_bytes(context, claims)?).to_bytes(),
    ))
}

fn verify_signature(
    root: &TrustRoot,
    context: &[u8],
    claims: &impl Serialize,
    signature: &str,
) -> Result<()> {
    let signature = Signature::from_bytes(&decode_hex::<64>(signature, "signature")?);
    verifying_key(&root.administrator_public_key)?
        .verify_strict(&signing_bytes(context, claims)?, &signature)
        .map_err(|_| MembershipError::InvalidSignature)
}

fn verifying_key(value: &str) -> Result<VerifyingKey> {
    let key = VerifyingKey::from_bytes(&decode_hex::<32>(value, "public key")?)
        .map_err(|_| MembershipError::InvalidField("public key"))?;
    if key.is_weak() {
        return Err(MembershipError::InvalidField("weak public key"));
    }
    Ok(key)
}

fn decode_hex<const LENGTH: usize>(value: &str, field: &'static str) -> Result<[u8; LENGTH]> {
    if value.len() != LENGTH * 2 {
        return Err(MembershipError::InvalidField(field));
    }
    let mut bytes = [0u8; LENGTH];
    hex::decode_to_slice(value, &mut bytes).map_err(|_| MembershipError::InvalidField(field))?;
    if hex::encode(bytes) != value {
        return Err(MembershipError::InvalidField(field));
    }
    Ok(bytes)
}
