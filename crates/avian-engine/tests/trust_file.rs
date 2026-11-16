use std::time::{Duration, UNIX_EPOCH};

use avian_engine::{NativeTrustFile, TRUST_LEASE_DURATION, TrustFileExport, TrustFileFormat};
use avian_membership::{
    MembershipClaims, MembershipStore, Permissions, TrustRoot, issue_membership, issue_revocations,
};
use ed25519_dalek::SigningKey;
use tempfile::tempdir;

fn enrolled_store() -> (SigningKey, MembershipStore, String) {
    let administrator = SigningKey::from_bytes(&[7; 32]);
    let root = TrustRoot::new("field-network", &administrator.verifying_key()).unwrap();
    let identity = SigningKey::from_bytes(&[11; 32]);
    let membership = issue_membership(
        &root,
        &administrator,
        MembershipClaims::new(
            root.network_id.clone(),
            &identity.verifying_key(),
            [11; 32],
            vec!["fd12:3456::1".parse().unwrap()],
            1,
            Permissions {
                relay: true,
                administer: true,
            },
        ),
    )
    .unwrap();
    let node_id = membership.claims.node_id.clone();
    let mut store = MembershipStore::new(root).unwrap();
    store.import_membership(membership).unwrap();
    (administrator, store, node_id)
}

#[test]
fn export_matches_shared_native_schema_without_admin_or_private_credentials() {
    let (_, store, node_id) = enrolled_store();
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let export = TrustFileExport::from_membership(&store, now).unwrap();
    let fixture: NativeTrustFile =
        serde_json::from_str(include_str!("fixtures/credential-file.json")).unwrap();
    assert_eq!(fixture.format, TrustFileFormat::VerifiedMembershipV1);
    assert_eq!(
        export.credentials()[&node_id],
        fixture.credentials["fixture-device"]
    );
    let document = serde_json::to_value(export.credentials()).unwrap();
    assert!(document[&node_id].get("secret").is_none());
    assert!(document[&node_id].get("administer").is_none());
    assert!(document[&node_id].get("network_secret").is_none());
    assert_eq!(document[&node_id]["groups"], serde_json::json!([]));
    assert_eq!(
        document[&node_id]["allowed_proxy_cidrs"],
        serde_json::json!([])
    );
    assert_eq!(export.network_id, "field-network");
    assert_eq!(export.revocation_revision, 0);
}

#[test]
fn lease_refresh_is_local_and_never_extends_durable_membership() {
    let (_, store, node_id) = enrolled_store();
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let export = TrustFileExport::from_membership(&store, now).unwrap();
    assert!(!export.refresh_due(now + Duration::from_secs(239)).unwrap());
    assert!(export.refresh_due(now + Duration::from_secs(240)).unwrap());
    assert!(!export.is_expired(now + Duration::from_secs(299)).unwrap());
    assert!(export.is_expired(now + TRUST_LEASE_DURATION).unwrap());
    assert!(export.is_expired(now - Duration::from_secs(1)).unwrap());
    assert!(export.refresh_due(now - Duration::from_secs(1)).unwrap());

    let renewed_at = now + Duration::from_secs(240);
    let renewed = TrustFileExport::from_membership(&store, renewed_at).unwrap();
    assert_eq!(renewed.expires_at_unix - renewed.created_at_unix, 300);
    assert_eq!(
        renewed.credentials()[&node_id].pubkey,
        export.credentials()[&node_id].pubkey
    );
    assert_eq!(store.memberships().next().unwrap().claims.revision, 1);
}

#[test]
fn revoked_devices_disappear_and_relay_permissions_are_preserved() {
    let (administrator, mut store, node_id) = enrolled_store();
    let root = store.trust_root().clone();
    let identity = SigningKey::from_bytes(&[12; 32]);
    let record = issue_membership(
        &root,
        &administrator,
        MembershipClaims::new(
            root.network_id.clone(),
            &identity.verifying_key(),
            [12; 32],
            vec!["fd12:3456::2".parse().unwrap()],
            1,
            Permissions {
                relay: false,
                administer: false,
            },
        ),
    )
    .unwrap();
    let endpoint_id = record.claims.node_id.clone();
    store.import_membership(record).unwrap();
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let initial = TrustFileExport::from_membership(&store, now).unwrap();
    assert!(initial.credentials()[&node_id].allow_relay);
    assert!(!initial.credentials()[&endpoint_id].allow_relay);

    let revocation = issue_revocations(&root, &administrator, 0, vec![node_id.clone()]).unwrap();
    store.apply_revocations(revocation).unwrap();
    let export = TrustFileExport::from_membership(&store, now).unwrap();
    assert!(!export.credentials().contains_key(&node_id));
    assert_eq!(export.credentials().len(), 1);
    assert_eq!(export.revocation_revision, 1);
}

#[cfg(unix)]
#[test]
fn startup_file_is_owner_only_and_replaced_without_retaining_revoked_entries() {
    use std::os::unix::fs::PermissionsExt;
    let (administrator, mut store, node_id) = enrolled_store();
    let directory = tempdir().unwrap();
    let path = directory.path().join("credentials.json");
    let now = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    TrustFileExport::from_membership(&store, now)
        .unwrap()
        .write_startup_file(&path)
        .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let loaded: NativeTrustFile = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(loaded.format, TrustFileFormat::VerifiedMembershipV1);
    assert!(loaded.credentials.contains_key(&node_id));

    let revocations =
        issue_revocations(store.trust_root(), &administrator, 0, vec![node_id]).unwrap();
    store.apply_revocations(revocations).unwrap();
    TrustFileExport::from_membership(&store, now)
        .unwrap()
        .write_startup_file(&path)
        .unwrap();
    let replaced: NativeTrustFile = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(replaced.format, TrustFileFormat::VerifiedMembershipV1);
    assert!(replaced.credentials.is_empty());
}

#[test]
fn authoritative_cache_requires_recognized_format_even_when_empty() {
    let empty: NativeTrustFile = serde_json::from_value(serde_json::json!({
        "format": "avian-verified-membership-v1", "credentials": {}
    }))
    .unwrap();
    assert!(empty.credentials.is_empty());
    assert!(serde_json::from_value::<NativeTrustFile>(serde_json::json!({})).is_err());
    assert!(
        serde_json::from_value::<NativeTrustFile>(serde_json::json!({
            "format": "avian-verified-membership-v2", "credentials": {}
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<NativeTrustFile>(serde_json::json!({
            "format": "avian-verified-membership-v1", "credentials": {}, "trust_remote_admins": true
        }))
        .is_err()
    );
}

#[cfg(unix)]
#[test]
fn startup_file_rejects_symbolic_link_destinations() {
    let (_, store, _) = enrolled_store();
    let directory = tempdir().unwrap();
    let target = directory.path().join("target.json");
    let link = directory.path().join("credentials.json");
    std::fs::write(&target, "preserve").unwrap();
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let export = TrustFileExport::from_membership(&store, UNIX_EPOCH).unwrap();
    assert!(matches!(
        export.write_startup_file(&link),
        Err(avian_engine::ExportError::InvalidDestination)
    ));
    assert_eq!(std::fs::read_to_string(target).unwrap(), "preserve");
}
