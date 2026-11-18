//! Offline onboarding keeps issuer authority separate from device credentials.
use std::{fs, net::SocketAddr, path::Path};

use anyhow::{Context, Result, ensure};
use avian_membership::{
    MembershipClaims, MembershipStore, Permissions, SignedMembership, SignedRevocations, TrustRoot,
    fingerprint, generate_signing_key, issue_membership, node_id_for_key, public_key_hex,
    read_signing_key, write_signing_key,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::{configuration::Configuration, services::ServiceConfiguration};

const REQUEST_CONTEXT: &[u8] = b"AVIAN device request v1\0";
const BUNDLE_CONTEXT: &[u8] = b"AVIAN provisioning bundle v1\0";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRequest {
    pub version: u32,
    pub identity_public_key: String,
    pub transport_public_key: String,
    pub signature: String,
}

impl DeviceRequest {
    pub fn new(identity: &SigningKey, transport_public_key: [u8; 32]) -> Result<Self> {
        let mut request = Self {
            version: 1,
            identity_public_key: public_key_hex(&identity.verifying_key()),
            transport_public_key: hex::encode(transport_public_key),
            signature: String::new(),
        };
        request.signature = hex::encode(
            identity
                .sign(&message(REQUEST_CONTEXT, &request.claims())?)
                .to_bytes(),
        );
        request.verify()?;
        Ok(request)
    }

    pub fn verify(&self) -> Result<VerifyingKey> {
        ensure!(self.version == 1, "unsupported device request version");
        let identity = VerifyingKey::from_bytes(&decode_key(&self.identity_public_key)?)?;
        ensure!(
            decode_key(&self.transport_public_key)? != [0; 32],
            "invalid transport key"
        );
        verify_signature(&identity, REQUEST_CONTEXT, &self.claims(), &self.signature)?;
        Ok(identity)
    }

    fn claims(&self) -> (u32, &str, &str) {
        (
            self.version,
            &self.identity_public_key,
            &self.transport_public_key,
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleClaims {
    pub version: u32,
    pub trust_root: TrustRoot,
    pub node_id: String,
    pub memberships: Vec<SignedMembership>,
    pub revocations: Vec<SignedRevocations>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceBundle {
    pub claims: BundleClaims,
    pub signature: String,
}

impl DeviceBundle {
    pub fn verify_for_device(
        &self,
        expected_fingerprint: &str,
        identity: &VerifyingKey,
        transport_public_key: [u8; 32],
    ) -> Result<MembershipStore> {
        let claims = &self.claims;
        ensure!(
            claims.version == 1,
            "unsupported provisioning bundle version"
        );
        claims.trust_root.validate()?;
        let administrator =
            VerifyingKey::from_bytes(&decode_key(&claims.trust_root.administrator_public_key)?)?;
        ensure!(
            fingerprint(&administrator) == expected_fingerprint,
            "administrator fingerprint does not match"
        );
        verify_signature(&administrator, BUNDLE_CONTEXT, claims, &self.signature)?;
        ensure!(
            claims.node_id == node_id_for_key(identity),
            "bundle belongs to another device"
        );
        let incoming = MembershipStore::from_records(
            claims.trust_root.clone(),
            claims.memberships.clone(),
            claims.revocations.clone(),
        )?;
        incoming.authorize(&claims.node_id, &hex::encode(transport_public_key))?;
        Ok(incoming)
    }
}

fn message(context: &[u8], claims: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = context.to_vec();
    bytes.extend(serde_json::to_vec(claims)?);
    Ok(bytes)
}

fn verify_signature(
    key: &VerifyingKey,
    context: &[u8],
    claims: &impl Serialize,
    signature: &str,
) -> Result<()> {
    let signature = Signature::from_slice(&hex::decode(signature)?)?;
    key.verify_strict(&message(context, claims)?, &signature)
        .context("invalid provisioning signature")
}

pub fn decode_key(encoded: &str) -> Result<[u8; 32]> {
    hex::decode(encoded)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected 32-byte hexadecimal key"))
}

/// Existing complete device keys are reused; partial initialization is never replaced.
pub fn initialize_device(directory: &Path) -> Result<DeviceRequest> {
    if !directory.exists() {
        avian_storage::create_private_directory(directory)?;
    }
    avian_storage::validate_private_directory(directory)?;
    let _lock = lock_device(directory)?;
    let identity_path = directory.join("identity.key");
    let transport_path = directory.join("transport.key");
    ensure!(
        !directory.join("administrator.key").exists(),
        "administrator state cannot be used as device state"
    );
    if !identity_path.exists() && !transport_path.exists() {
        write_signing_key(&identity_path, &generate_signing_key()?)?;
        let mut secret = Zeroizing::new([0u8; 32]);
        getrandom::fill(secret.as_mut())
            .map_err(|problem| anyhow::anyhow!("generate transport key: {problem}"))?;
        let encoded = Zeroizing::new(STANDARD.encode(secret.as_ref()));
        avian_storage::create_secret_file(&transport_path, encoded.as_bytes())?;
    }
    let identity = read_signing_key(&identity_path)?;
    let transport = transport_public_key(directory)?;
    DeviceRequest::new(&identity, transport)
}

fn transport_public_key(directory: &Path) -> Result<[u8; 32]> {
    let encoded = avian_storage::read_secret_file(directory.join("transport.key"), 4096)?;
    let bytes = Zeroizing::new(STANDARD.decode(std::str::from_utf8(&encoded)?.trim())?);
    let secret = Zeroizing::new(
        <[u8; 32]>::try_from(bytes.as_slice()).context("invalid transport credential")?,
    );
    Ok(*x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*secret)).as_bytes())
}

pub fn issue_bundle(
    request: &DeviceRequest,
    store: &mut MembershipStore,
    administrator: &SigningKey,
    addresses: Vec<std::net::IpAddr>,
    permissions: Permissions,
    revision: u64,
) -> Result<DeviceBundle> {
    let identity = request.verify()?;
    let root = store.trust_root().clone();
    let membership = issue_membership(
        &root,
        administrator,
        MembershipClaims::new(
            root.network_id.clone(),
            &identity,
            decode_key(&request.transport_public_key)?,
            addresses,
            revision,
            permissions,
        ),
    )?;
    store.import_membership(membership)?;
    let claims = BundleClaims {
        version: 1,
        trust_root: root,
        node_id: node_id_for_key(&identity),
        memberships: store.membership_history().cloned().collect(),
        revocations: store.revocations_after(0).cloned().collect(),
    };
    let signature = hex::encode(
        administrator
            .sign(&message(BUNDLE_CONTEXT, &claims)?)
            .to_bytes(),
    );
    Ok(DeviceBundle { claims, signature })
}

/// Imports public authority only after checking the out-of-band issuer fingerprint.
/// Reimport preserves existing credentials, configuration, and anti-rollback floors.
pub fn import_bundle(
    directory: &Path,
    bundle: &DeviceBundle,
    expected_fingerprint: &str,
    listen: SocketAddr,
) -> Result<Configuration> {
    ensure!(
        listen.ip().is_loopback() && listen.port() != 0,
        "the local API requires a loopback address and nonzero port"
    );
    avian_storage::validate_private_directory(directory)?;
    let _lock = lock_device(directory)?;
    ensure!(
        !directory.join("administrator.key").exists(),
        "administrator state cannot be used as device state"
    );
    let claims = &bundle.claims;
    let identity = read_signing_key(directory.join("identity.key"))?;
    let incoming = bundle.verify_for_device(
        expected_fingerprint,
        &identity.verifying_key(),
        transport_public_key(directory)?,
    )?;
    let membership_path = directory.join("membership.json");
    if membership_path.exists() {
        let previous = MembershipStore::load(&membership_path, claims.trust_root.clone())?;
        incoming.validate_successor(&previous)?;
    }
    let root_path = directory.join("trust-root.json");
    if root_path.exists() {
        let existing: TrustRoot =
            serde_json::from_slice(&avian_storage::read_private_file(&root_path, 16384)?)?;
        ensure!(
            existing == claims.trust_root,
            "device already trusts another network"
        );
    }
    let configuration_path = directory.join("avian.toml");
    let configuration = if configuration_path.exists() {
        Configuration::load(&configuration_path)?
    } else {
        Configuration {
            listen,
            token_file: "api-token".into(),
            membership_file: "membership.json".into(),
            trust_root_file: "trust-root.json".into(),
            chud: None,
            runtime: None,
            services: Some(ServiceConfiguration {
                state_file: "services.json".into(),
                identity_key_file: Some("identity.key".into()),
            }),
            service_exchange: None,
            service_limits: None,
        }
    };
    if !root_path.exists() {
        avian_storage::create_private_file(
            &root_path,
            &serde_json::to_vec_pretty(&claims.trust_root)?,
        )?;
    }
    incoming.save(&membership_path)?;
    let token_path = directory.join("api-token");
    if !token_path.exists() {
        let mut token = Zeroizing::new([0u8; 32]);
        getrandom::fill(token.as_mut())
            .map_err(|problem| anyhow::anyhow!("generate API credential: {problem}"))?;
        let encoded = Zeroizing::new(hex::encode(token.as_ref()));
        avian_storage::create_secret_file(token_path, encoded.as_bytes())?;
    }
    if !configuration_path.exists() {
        avian_storage::create_private_file(
            configuration_path,
            toml::to_string_pretty(&configuration)?.as_bytes(),
        )?;
    }
    Ok(configuration)
}

/// Device-owned fields are bound after enrollment verification, rather than
/// requiring meaningless key paths or a node identifier in an operator template.
pub fn parse_runtime_template(document: &str) -> Result<avian_runtime::RuntimeConfiguration> {
    let mut fields: toml::Table = toml::from_str(document).context("parse runtime template")?;
    for (name, value) in [
        ("node_id", ""),
        ("membership_file", "membership.json"),
        ("trust_root_file", "trust-root.json"),
        ("credential_file", "transport.key"),
    ] {
        fields.insert(name.into(), value.into());
    }
    toml::Value::Table(fields)
        .try_into()
        .context("parse runtime settings")
}

/// The operator supplies host-specific execution settings; identity and trust paths
/// always come from this device's protected enrollment state.
pub fn configure_runtime(
    directory: &Path,
    mut runtime: avian_runtime::RuntimeConfiguration,
) -> Result<()> {
    avian_storage::validate_private_directory(directory)?;
    let _lock = lock_device(directory)?;
    let directory = fs::canonicalize(directory)?;
    let config_path = directory.join("avian.toml");
    let mut configuration = Configuration::load(&config_path)?;
    let identity = read_signing_key(directory.join("identity.key"))?;
    let root: TrustRoot = serde_json::from_slice(&avian_storage::read_private_file(
        directory.join("trust-root.json"),
        16384,
    )?)?;
    let membership = MembershipStore::load(directory.join("membership.json"), root)?;
    let node_id = node_id_for_key(&identity.verifying_key());
    let local = membership.authorize(&node_id, &hex::encode(transport_public_key(&directory)?))?;
    ensure!(
        local
            .overlay_addresses
            .contains(&std::net::IpAddr::V4(runtime.overlay_address)),
        "runtime address is not assigned to this device"
    );
    runtime.node_id = node_id;
    runtime.credential_file = directory.join("transport.key");
    runtime.membership_file = directory.join("membership.json");
    runtime.trust_root_file = directory.join("trust-root.json");
    runtime.resolve_paths(&directory);
    runtime.validate()?;
    configuration.runtime = Some(runtime);
    avian_storage::atomic_write_private_file(
        &config_path,
        toml::to_string_pretty(&configuration)?.as_bytes(),
    )?;
    Ok(())
}

pub fn write_public_export(path: &Path, value: &impl Serialize) -> Result<()> {
    avian_storage::create_private_file(path, &serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

pub(crate) fn lock_device(directory: &Path) -> Result<fs::File> {
    let file = avian_storage::open_private_lock(directory.join("device-operation.lock"))?;
    fs2::FileExt::try_lock_exclusive(&file)
        .context("another device provisioning operation is in progress")?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use avian_membership::issue_revocations;

    fn issuer() -> (SigningKey, MembershipStore) {
        let key = generate_signing_key().unwrap();
        let root = TrustRoot::new("field-network", &key.verifying_key()).unwrap();
        (key, MembershipStore::new(root).unwrap())
    }

    fn enroll(
        request: &DeviceRequest,
        issuer: &SigningKey,
        store: &mut MembershipStore,
    ) -> DeviceBundle {
        issue_bundle(
            request,
            store,
            issuer,
            vec!["10.77.0.2".parse().unwrap()],
            Permissions {
                relay: true,
                administer: false,
            },
            1,
        )
        .unwrap()
    }

    #[test]
    fn repeated_initialization_preserves_both_credentials() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("device");
        let first = initialize_device(&directory).unwrap();
        let original_key = read_signing_key(directory.join("identity.key"))
            .unwrap()
            .to_bytes();
        let repeated = initialize_device(&directory).unwrap();
        assert_eq!(first.identity_public_key, repeated.identity_public_key);
        assert_eq!(first.transport_public_key, repeated.transport_public_key);
        assert_eq!(
            original_key,
            read_signing_key(directory.join("identity.key"))
                .unwrap()
                .to_bytes()
        );
        assert!(!directory.join("administrator.key").exists());
        assert!(first.verify().is_ok());
    }

    #[test]
    fn tampered_request_cannot_bind_another_transport() {
        let temporary = tempfile::tempdir().unwrap();
        let mut request = initialize_device(&temporary.path().join("device")).unwrap();
        request.transport_public_key = "11".repeat(32);
        assert!(request.verify().is_err());
    }

    #[test]
    fn import_requires_pinned_issuer_and_local_key_possession() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("device");
        let request = initialize_device(&directory).unwrap();
        let (issuer, mut store) = issuer();
        let bundle = enroll(&request, &issuer, &mut store);
        let listen = "127.0.0.1:4179".parse().unwrap();
        assert!(import_bundle(&directory, &bundle, "wrong", listen).is_err());
        assert!(!directory.join("trust-root.json").exists());
        let other = temporary.path().join("other");
        initialize_device(&other).unwrap();
        let pin = fingerprint(&issuer.verifying_key());
        assert!(import_bundle(&other, &bundle, &pin, listen).is_err());
        import_bundle(&directory, &bundle, &pin, listen).unwrap();
        let token = avian_storage::read_secret_file(directory.join("api-token"), 4096).unwrap();
        import_bundle(&directory, &bundle, &pin, listen).unwrap();
        assert_eq!(
            *token,
            *avian_storage::read_secret_file(directory.join("api-token"), 4096).unwrap()
        );
        let configuration = Configuration::load(&directory.join("avian.toml")).unwrap();
        assert!(configuration.runtime.is_none());
        assert!(
            configuration
                .services
                .unwrap()
                .identity_key_file
                .unwrap()
                .ends_with("identity.key")
        );
        assert!(!directory.join("administrator.key").exists());
    }

    #[test]
    fn bundle_signature_protects_revocation_snapshot_and_target() {
        let identity = generate_signing_key().unwrap();
        let request = DeviceRequest::new(&identity, [7; 32]).unwrap();
        let (issuer, mut store) = issuer();
        let mut bundle = enroll(&request, &issuer, &mut store);
        bundle.claims.node_id = "0".repeat(64);
        assert!(
            bundle
                .verify_for_device(
                    &fingerprint(&issuer.verifying_key()),
                    &identity.verifying_key(),
                    [7; 32]
                )
                .is_err()
        );
    }

    #[test]
    fn transport_mismatch_and_revoked_target_are_rejected() {
        let identity = generate_signing_key().unwrap();
        let request = DeviceRequest::new(&identity, [7; 32]).unwrap();
        let (issuer, mut store) = issuer();
        let bundle = enroll(&request, &issuer, &mut store);
        let pin = fingerprint(&issuer.verifying_key());
        assert!(
            bundle
                .verify_for_device(&pin, &identity.verifying_key(), [8; 32])
                .is_err()
        );
        let revocation = issue_revocations(
            store.trust_root(),
            &issuer,
            0,
            vec![bundle.claims.node_id.clone()],
        )
        .unwrap();
        store.apply_revocations(revocation).unwrap();
        let mut revoked = bundle;
        revoked.claims.revocations = store.revocations_after(0).cloned().collect();
        revoked.signature = hex::encode(
            issuer
                .sign(&message(BUNDLE_CONTEXT, &revoked.claims).unwrap())
                .to_bytes(),
        );
        assert!(
            revoked
                .verify_for_device(&pin, &identity.verifying_key(), [7; 32])
                .is_err()
        );
    }

    #[test]
    fn import_rejects_a_signed_bundle_that_rolls_back_membership() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("device");
        let request = initialize_device(&directory).unwrap();
        let (issuer, mut store) = issuer();
        let first = enroll(&request, &issuer, &mut store);
        let second = issue_bundle(
            &request,
            &mut store,
            &issuer,
            vec!["10.77.0.2".parse().unwrap()],
            Permissions {
                relay: false,
                administer: false,
            },
            2,
        )
        .unwrap();
        let pin = fingerprint(&issuer.verifying_key());
        let listen = "127.0.0.1:4179".parse().unwrap();
        import_bundle(&directory, &second, &pin, listen).unwrap();
        assert!(import_bundle(&directory, &first, &pin, listen).is_err());
        let persisted = MembershipStore::load(
            directory.join("membership.json"),
            store.trust_root().clone(),
        )
        .unwrap();
        assert_eq!(persisted.memberships().next().unwrap().claims.revision, 2);
    }
    #[test]
    fn snapshot_import_accepts_reassigned_revoked_addresses_without_admitting_old_owner() {
        let old = generate_signing_key().unwrap();
        let current = generate_signing_key().unwrap();
        let (issuer, mut store) = issuer();
        let previous = enroll(
            &DeviceRequest::new(&old, [7; 32]).unwrap(),
            &issuer,
            &mut store,
        );
        let revocation = issue_revocations(
            store.trust_root(),
            &issuer,
            0,
            vec![previous.claims.node_id.clone()],
        )
        .unwrap();
        store.apply_revocations(revocation).unwrap();
        let bundle = enroll(
            &DeviceRequest::new(&current, [8; 32]).unwrap(),
            &issuer,
            &mut store,
        );
        let imported = bundle
            .verify_for_device(
                &fingerprint(&issuer.verifying_key()),
                &current.verifying_key(),
                [8; 32],
            )
            .unwrap();
        assert!(
            imported
                .authorize(&previous.claims.node_id, &hex::encode([7; 32]))
                .is_err()
        );
        assert_eq!(imported.memberships().count(), 1);
        assert_eq!(imported.membership_history().count(), 2);
    }

    #[test]
    fn partial_device_state_never_replaces_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("device");
        avian_storage::create_private_directory(&directory).unwrap();
        let original = generate_signing_key().unwrap();
        write_signing_key(directory.join("identity.key"), &original).unwrap();
        assert!(initialize_device(&directory).is_err());
        assert_eq!(
            read_signing_key(directory.join("identity.key"))
                .unwrap()
                .to_bytes(),
            original.to_bytes()
        );
        assert!(!directory.join("transport.key").exists());
    }

    #[test]
    fn runtime_template_is_bound_to_enrolled_keys_and_address_before_install() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("device");
        let request = initialize_device(&directory).unwrap();
        let (issuer, mut store) = issuer();
        let bundle = enroll(&request, &issuer, &mut store);
        import_bundle(
            &directory,
            &bundle,
            &fingerprint(&issuer.verifying_key()),
            "127.0.0.1:4179".parse().unwrap(),
        )
        .unwrap();
        let template = parse_runtime_template(
            r#"
isolation_required = true
namespace_name = "provision-test"
engine_directory = "engine"
runtime_directory = "runtime"
overlay_address = "10.77.0.99"
listen = "172.30.0.2:11010"
rpc_listen = "127.0.0.1:15888"
"#,
        )
        .unwrap();
        let config_path = directory.join("avian.toml");
        let original = fs::read(&config_path).unwrap();
        assert!(configure_runtime(&directory, template.clone()).is_err());
        assert_eq!(fs::read(&config_path).unwrap(), original);
        let mut valid = template;
        valid.overlay_address = "10.77.0.2".parse().unwrap();
        configure_runtime(&directory, valid).unwrap();
        let runtime = Configuration::load(&config_path).unwrap().runtime.unwrap();
        assert_eq!(runtime.node_id, bundle.claims.node_id);
        assert_eq!(
            runtime.credential_file,
            fs::canonicalize(&directory).unwrap().join("transport.key")
        );
        assert_eq!(
            runtime.membership_file,
            fs::canonicalize(&directory)
                .unwrap()
                .join("membership.json")
        );
        assert_eq!(
            runtime.trust_root_file,
            fs::canonicalize(&directory)
                .unwrap()
                .join("trust-root.json")
        );
        assert!(!directory.join("runtime").exists());
    }
}
