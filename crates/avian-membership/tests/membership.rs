use std::net::IpAddr;

use avian_membership::{
    MembershipClaims, MembershipError, MembershipStore, Permissions, SignedMembership, TrustRoot,
    generate_signing_key, issue_membership, issue_revocations, node_id_for_key, public_key_hex,
    read_signing_key, write_signing_key,
};
use ed25519_dalek::SigningKey;
use tempfile::tempdir;

fn administrator() -> (SigningKey, TrustRoot) {
    let key = SigningKey::from_bytes(&[7; 32]);
    let root = TrustRoot::new("field-network", &key.verifying_key()).unwrap();
    (key, root)
}

fn membership(
    administrator: &SigningKey,
    root: &TrustRoot,
    identity_seed: u8,
    address: &str,
    revision: u64,
) -> SignedMembership {
    let identity = SigningKey::from_bytes(&[identity_seed; 32]);
    issue_membership(
        root,
        administrator,
        MembershipClaims::new(
            root.network_id.clone(),
            &identity.verifying_key(),
            [identity_seed; 32],
            vec![address.parse::<IpAddr>().unwrap()],
            revision,
            Permissions {
                relay: true,
                administer: false,
            },
        ),
    )
    .unwrap()
}

#[test]
fn enrolled_device_survives_offline_restart_without_an_administrator_key() {
    let (key, root) = administrator();
    let record = membership(&key, &root, 11, "fd12:3456::1", 1);
    let mut store = MembershipStore::new(root.clone()).unwrap();
    store.import_membership(record.clone()).unwrap();
    let directory = tempdir().unwrap();
    let path = directory.path().join("membership.json");
    store.save(&path).unwrap();
    drop(key);
    drop(store);

    let restored = MembershipStore::load(&path, root).unwrap();
    let claims = restored
        .authorize(&record.claims.node_id, &record.claims.transport_public_key)
        .unwrap();
    assert!(claims.permissions.relay);
    assert!(!claims.permissions.administer);
    assert!(matches!(
        restored.authorize(&record.claims.node_id, &hex::encode([91; 32])),
        Err(MembershipError::WrongTransportKey)
    ));
    assert!(matches!(
        restored.authorize("unknown", &record.claims.transport_public_key),
        Err(MembershipError::UnknownDevice)
    ));
}

#[test]
fn signed_record_cannot_be_modified_or_moved_between_networks() {
    let (key, root) = administrator();
    let record = membership(&key, &root, 11, "10.42.0.1", 1);
    let mut modified = record.clone();
    modified.claims.permissions.administer = true;
    assert!(matches!(
        modified.verify(&root),
        Err(MembershipError::InvalidSignature)
    ));

    let other_network = TrustRoot::new("other-network", &key.verifying_key()).unwrap();
    assert!(matches!(
        record.verify(&other_network),
        Err(MembershipError::WrongNetwork)
    ));

    let other_administrator = SigningKey::from_bytes(&[33; 32]);
    let substituted_root =
        TrustRoot::new("field-network", &other_administrator.verifying_key()).unwrap();
    assert!(matches!(
        record.verify(&substituted_root),
        Err(MembershipError::InvalidSignature)
    ));
    assert!(matches!(
        issue_membership(&root, &other_administrator, record.claims),
        Err(MembershipError::WrongAdministrator)
    ));
}

#[test]
fn enrollment_rejects_address_and_transport_key_conflicts() {
    let (key, root) = administrator();
    let first = membership(&key, &root, 11, "fd12:3456::1", 1);
    let mut store = MembershipStore::new(root.clone()).unwrap();
    store.import_membership(first.clone()).unwrap();
    assert!(matches!(
        store.import_membership(membership(&key, &root, 12, "fd12:3456::1", 1)),
        Err(MembershipError::AddressConflict(_))
    ));
    let mut second = membership(&key, &root, 12, "fd12:3456::2", 1).claims;
    second.transport_public_key = first.claims.transport_public_key;
    assert!(matches!(
        store.import_membership(issue_membership(&root, &key, second).unwrap()),
        Err(MembershipError::TransportKeyConflict)
    ));
}

#[test]
fn membership_revisions_allow_rotation_but_reject_replay() {
    let (key, root) = administrator();
    let original = membership(&key, &root, 11, "10.42.0.1", 1);
    let mut rotated = original.claims.clone();
    rotated.revision = 2;
    rotated.transport_public_key = hex::encode([99; 32]);
    let rotated = issue_membership(&root, &key, rotated).unwrap();
    let mut store = MembershipStore::new(root).unwrap();
    store.import_membership(original.clone()).unwrap();
    store.import_membership(original.clone()).unwrap();
    store.import_membership(rotated.clone()).unwrap();
    assert!(matches!(
        store.import_membership(original.clone()),
        Err(MembershipError::Rollback)
    ));
    assert!(matches!(
        store.authorize(
            &original.claims.node_id,
            &original.claims.transport_public_key
        ),
        Err(MembershipError::WrongTransportKey)
    ));
    store
        .authorize(
            &rotated.claims.node_id,
            &rotated.claims.transport_public_key,
        )
        .unwrap();
}

#[test]
fn revocations_are_incremental_and_cannot_be_skipped_or_replayed() {
    let (key, root) = administrator();
    let first = membership(&key, &root, 11, "10.42.0.1", 1);
    let second = membership(&key, &root, 12, "10.42.0.2", 1);
    let mut store = MembershipStore::new(root.clone()).unwrap();
    store.import_membership(first.clone()).unwrap();
    store.import_membership(second.clone()).unwrap();
    let first_revocation =
        issue_revocations(&root, &key, 0, vec![first.claims.node_id.clone()]).unwrap();
    let second_revocation =
        issue_revocations(&root, &key, 1, vec![second.claims.node_id.clone()]).unwrap();
    assert!(matches!(
        store.apply_revocations(second_revocation.clone()),
        Err(MembershipError::MissingRevocations {
            expected: 0,
            received: 1
        })
    ));
    store.apply_revocations(first_revocation.clone()).unwrap();
    store.apply_revocations(first_revocation.clone()).unwrap();
    store.apply_revocations(second_revocation).unwrap();
    assert_eq!(store.revocation_revision(), 2);
    assert_eq!(store.revocations_after(1).count(), 1);
    assert_eq!(store.memberships().count(), 0);
    assert!(matches!(
        store.apply_revocations(first_revocation),
        Err(MembershipError::Rollback)
    ));
    assert!(matches!(
        store.import_membership(first.clone()),
        Err(MembershipError::Revoked)
    ));
    assert!(matches!(
        store.authorize(&first.claims.node_id, &first.claims.transport_public_key),
        Err(MembershipError::Revoked)
    ));
}

#[test]
fn stale_snapshot_cannot_overwrite_newer_disk_state_or_replace_live_state() {
    let (key, root) = administrator();
    let record = membership(&key, &root, 11, "fd12:3456::1", 1);
    let directory = tempdir().unwrap();
    let path = directory.path().join("membership.json");
    let mut store = MembershipStore::new(root.clone()).unwrap();
    store.import_membership(record.clone()).unwrap();
    store.save(&path).unwrap();
    let old_snapshot = store.clone();
    let revocation =
        issue_revocations(&root, &key, 0, vec![record.claims.node_id.clone()]).unwrap();
    store.apply_revocations(revocation).unwrap();
    store.save(&path).unwrap();
    assert!(matches!(
        old_snapshot.save(&path),
        Err(MembershipError::Rollback)
    ));
    assert!(matches!(
        old_snapshot.validate_successor(&store),
        Err(MembershipError::Rollback)
    ));

    let mut restored = MembershipStore::load(&path, root).unwrap();
    assert!(matches!(
        restored.import_membership(record),
        Err(MembershipError::Revoked)
    ));
    assert_eq!(restored.revocation_revision(), 1);
}

#[test]
fn revoked_address_can_be_reassigned_and_reloaded_without_resurrection() {
    let (key, root) = administrator();
    let original = membership(&key, &root, 11, "10.42.0.1", 1);
    let mut store = MembershipStore::new(root.clone()).unwrap();
    store.import_membership(original.clone()).unwrap();
    store
        .apply_revocations(
            issue_revocations(&root, &key, 0, vec![original.claims.node_id.clone()]).unwrap(),
        )
        .unwrap();
    let replacement = membership(&key, &root, 12, "10.42.0.1", 1);
    store.import_membership(replacement.clone()).unwrap();
    let directory = tempdir().unwrap();
    let path = directory.path().join("membership.json");
    store.save(&path).unwrap();
    let restored = MembershipStore::load(path, root).unwrap();
    assert_eq!(restored.memberships().count(), 1);
    restored
        .authorize(
            &replacement.claims.node_id,
            &replacement.claims.transport_public_key,
        )
        .unwrap();
    assert!(matches!(
        restored.authorize(
            &original.claims.node_id,
            &original.claims.transport_public_key
        ),
        Err(MembershipError::Revoked)
    ));
}

#[test]
fn unknown_fields_and_unsupported_versions_are_rejected() {
    let (key, root) = administrator();
    let record = membership(&key, &root, 11, "10.42.0.1", 1);
    let mut document = serde_json::to_value(&record).unwrap();
    document["claims"]["allow_any_key"] = serde_json::json!(true);
    assert!(serde_json::from_value::<SignedMembership>(document).is_err());
    let mut future = record;
    future.claims.version = 2;
    assert!(matches!(
        future.verify(&root),
        Err(MembershipError::InvalidField("membership version"))
    ));
}

#[cfg(unix)]
#[test]
fn private_keys_are_owner_only_and_existing_identity_is_never_overwritten() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempdir().unwrap();
    let path = directory.path().join("identity.key");
    let key = generate_signing_key().unwrap();
    write_signing_key(&path, &key).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let restored = read_signing_key(&path).unwrap();
    assert_eq!(
        public_key_hex(&restored.verifying_key()),
        public_key_hex(&key.verifying_key())
    );
    assert_eq!(
        node_id_for_key(&restored.verifying_key()),
        node_id_for_key(&key.verifying_key())
    );
    assert!(write_signing_key(&path, &generate_signing_key().unwrap()).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        read_signing_key(&path),
        Err(MembershipError::InsecurePermissions)
    ));
}

#[cfg(unix)]
#[test]
fn private_key_symlinks_are_rejected() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("identity.key");
    let link = directory.path().join("linked.key");
    write_signing_key(&path, &generate_signing_key().unwrap()).unwrap();
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(matches!(
        read_signing_key(&link),
        Err(MembershipError::InsecurePermissions)
    ));
    assert!(write_signing_key(&link, &generate_signing_key().unwrap()).is_err());
}
