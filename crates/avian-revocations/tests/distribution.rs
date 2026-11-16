use avian_engine::NativeTrustFile;
use avian_membership::{
    MembershipClaims, MembershipStore, Permissions, TrustRoot, generate_signing_key,
    issue_membership, issue_revocations,
};
use avian_revocations::*;
use ed25519_dalek::SigningKey;
use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    time::{Duration, UNIX_EPOCH},
};
use tempfile::TempDir;

struct Fixture {
    directory: TempDir,
    administrator: SigningKey,
    root: TrustRoot,
    store: MembershipStore,
    node_id: String,
}
impl Fixture {
    fn new(network: &str) -> Self {
        let administrator = generate_signing_key().unwrap();
        let root = TrustRoot::new(network, &administrator.verifying_key()).unwrap();
        let identity = generate_signing_key().unwrap();
        let claims = MembershipClaims::new(
            network,
            &identity.verifying_key(),
            [7; 32],
            vec![IpAddr::V4(Ipv4Addr::new(10, 1, 0, 1))],
            1,
            Permissions {
                relay: true,
                administer: false,
            },
        );
        let node_id = claims.node_id.clone();
        let mut store = MembershipStore::new(root.clone()).unwrap();
        store
            .import_membership(issue_membership(&root, &administrator, claims).unwrap())
            .unwrap();
        Self {
            directory: TempDir::new().unwrap(),
            administrator,
            root,
            store,
            node_id,
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.directory.path().join(name)
    }
    fn revoke(&mut self) {
        let record = issue_revocations(
            &self.root,
            &self.administrator,
            self.store.revocation_revision(),
            vec![self.node_id.clone()],
        )
        .unwrap();
        self.store.apply_revocations(record).unwrap();
    }
}

#[test]
fn durable_recipient_catches_up_offline_and_publishes_no_revoked_leases() {
    let mut sender = Fixture::new("mesh");
    let recipient = sender.store.clone();
    let recipient_path = sender.path("recipient.json");
    let cache_path = sender.path("engine.json");
    recipient.save(&recipient_path).unwrap();
    refresh_engine_cache(
        sender.root.clone(),
        &recipient_path,
        &cache_path,
        UNIX_EPOCH + Duration::from_secs(100),
    )
    .unwrap();
    let before: NativeTrustFile =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert_eq!(before.credentials.len(), 1);
    sender.revoke();
    let batch = export_batch(&sender.store, 0, 128, MAXIMUM_RESPONSE_BYTES).unwrap();
    let outcome = apply_batch_and_publish(
        sender.root.clone(),
        &recipient_path,
        &cache_path,
        &batch,
        UNIX_EPOCH + Duration::from_secs(110),
    )
    .unwrap();
    assert_eq!(outcome.current_revision, 1);
    let restarted = MembershipStore::load(&recipient_path, sender.root.clone()).unwrap();
    assert_eq!(restarted.revocation_revision(), 1);
    assert!(restarted.memberships().next().is_none());
    let cache: NativeTrustFile =
        serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
    assert!(cache.credentials.is_empty());
    assert!(
        apply_batch_and_publish(
            sender.root.clone(),
            &recipient_path,
            &cache_path,
            &batch,
            UNIX_EPOCH + Duration::from_secs(120)
        )
        .is_err()
    );
}

#[test]
fn tampering_wrong_network_order_and_cursor_replay_are_atomic() {
    let mut sender = Fixture::new("mesh");
    let recipient = sender.store.clone();
    sender.revoke();
    let valid = export_batch(&sender.store, 0, 128, MAXIMUM_RESPONSE_BYTES).unwrap();
    let mut tampered = valid.clone();
    tampered.records[0].claims.revoked_node_ids = vec!["00".repeat(32)];
    assert!(verify_batch(&recipient, &tampered).is_err());
    let mut wrong_network = valid.clone();
    wrong_network.network_id = "other".into();
    assert!(verify_batch(&recipient, &wrong_network).is_err());
    let mut missing = valid.clone();
    missing.records.clear();
    assert!(verify_batch(&recipient, &missing).is_err());
    let mut replay = valid.clone();
    replay.records.push(replay.records[0].clone());
    assert!(verify_batch(&recipient, &replay).is_err());
    assert_eq!(recipient.revocation_revision(), 0);
    assert_eq!(recipient.memberships().count(), 1);
}

#[test]
fn pagination_converges_and_gaps_do_not_skip_signed_records() {
    let mut sender = Fixture::new("mesh");
    let mut recipient = sender.store.clone();
    for index in 0..4u8 {
        let record = issue_revocations(
            &sender.root,
            &sender.administrator,
            sender.store.revocation_revision(),
            vec![format!("{index:02x}").repeat(32)],
        )
        .unwrap();
        sender.store.apply_revocations(record).unwrap();
    }
    let second_page = export_batch(&sender.store, 2, 2, MAXIMUM_RESPONSE_BYTES).unwrap();
    assert!(verify_batch(&recipient, &second_page).is_err());
    for _ in 0..2 {
        let batch = export_batch(
            &sender.store,
            recipient.revocation_revision(),
            2,
            MAXIMUM_RESPONSE_BYTES,
        )
        .unwrap();
        assert_eq!(batch.records.len(), 2);
        recipient = verify_batch(&recipient, &batch).unwrap();
    }
    assert_eq!(recipient.revocation_revision(), 4);
    assert!(export_batch(&sender.store, 5, 128, MAXIMUM_RESPONSE_BYTES).is_err());
    assert!(export_batch(&sender.store, 0, 128, 1).is_err());
}

#[test]
fn publication_failure_keeps_durable_revocation_for_retry() {
    let mut sender = Fixture::new("mesh");
    let path = sender.path("recipient.json");
    sender.store.save(&path).unwrap();
    sender.revoke();
    let batch = export_batch(&sender.store, 0, 128, MAXIMUM_RESPONSE_BYTES).unwrap();
    let missing_parent = sender.path("missing/engine.json");
    assert!(
        apply_batch_and_publish(
            sender.root.clone(),
            &path,
            &missing_parent,
            &batch,
            UNIX_EPOCH + Duration::from_secs(100)
        )
        .is_err()
    );
    let restarted = MembershipStore::load(&path, sender.root.clone()).unwrap();
    assert_eq!(restarted.revocation_revision(), 1);
    assert!(restarted.memberships().next().is_none());
    refresh_engine_cache(
        sender.root.clone(),
        &path,
        &sender.path("engine.json"),
        UNIX_EPOCH + Duration::from_secs(110),
    )
    .unwrap();
    let cache: NativeTrustFile =
        serde_json::from_slice(&std::fs::read(sender.path("engine.json")).unwrap()).unwrap();
    assert!(cache.credentials.is_empty());
}

#[tokio::test]
async fn http_transport_bounds_response_and_refuses_redirects() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let count = stream.read(&mut request).await.unwrap();
        let request = String::from_utf8_lossy(&request[..count]);
        assert!(request.contains("after_revision=0"));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
    });
    let client = synchronization_client().unwrap();
    assert!(matches!(
        fetch_batch(&client, &format!("http://{address}"), "mesh", 0, 128).await,
        Err(DistributionError::ResponseTooLarge)
    ));
    task.await.unwrap();
    assert!(
        fetch_batch(&client, "http://203.0.113.1", "mesh", 0, 128)
            .await
            .is_err()
    );
    assert!(
        fetch_batch(&client, "http://127.0.0.1/?token=secret", "mesh", 0, 128)
            .await
            .is_err()
    );
}

#[test]
fn stale_received_page_does_not_overwrite_a_concurrent_local_revocation() {
    let mut sender = Fixture::new("mesh");
    let path = sender.path("recipient.json");
    sender.store.save(&path).unwrap();
    sender.revoke();
    let batch = export_batch(&sender.store, 0, 128, MAXIMUM_RESPONSE_BYTES).unwrap();
    // A local administrative operation committed the same revision while fetch was in flight.
    sender.store.save(&path).unwrap();
    assert!(
        apply_batch_and_publish(
            sender.root.clone(),
            &path,
            &sender.path("engine.json"),
            &batch,
            UNIX_EPOCH + Duration::from_secs(100)
        )
        .is_err()
    );
    assert_eq!(
        MembershipStore::load(&path, sender.root.clone())
            .unwrap()
            .revocation_revision(),
        1
    );
}

#[test]
fn empty_page_does_not_rewrite_state_or_renew_engine_leases() {
    let fixture = Fixture::new("mesh");
    let path = fixture.path("recipient.json");
    let cache = fixture.path("engine.json");
    fixture.store.save(&path).unwrap();
    refresh_engine_cache(
        fixture.root.clone(),
        &path,
        &cache,
        UNIX_EPOCH + Duration::from_secs(100),
    )
    .unwrap();
    let before_store = std::fs::read(&path).unwrap();
    let before_cache = std::fs::read(&cache).unwrap();
    let before_modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let batch = export_batch(&fixture.store, 0, 128, MAXIMUM_RESPONSE_BYTES).unwrap();
    let outcome = apply_batch_and_publish(
        fixture.root.clone(),
        &path,
        &cache,
        &batch,
        UNIX_EPOCH + Duration::from_secs(200),
    )
    .unwrap();
    assert_eq!(outcome.applied_records, 0);
    assert_eq!(std::fs::read(&path).unwrap(), before_store);
    assert_eq!(std::fs::read(&cache).unwrap(), before_cache);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before_modified
    );
}

#[test]
fn unsigned_head_cannot_advance_state_without_signed_records() {
    let fixture = Fixture::new("mesh");
    let mut page = export_batch(&fixture.store, 0, 128, MAXIMUM_RESPONSE_BYTES).unwrap();
    page.current_revision = 100;
    assert!(verify_batch(&fixture.store, &page).is_err());
    page.next_revision = 100;
    assert!(verify_batch(&fixture.store, &page).is_err());
    assert_eq!(fixture.store.revocation_revision(), 0);
}

#[tokio::test]
async fn redirect_does_not_fetch_an_unapproved_endpoint() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0u8; 4096];
        let count = stream.read(&mut request).await.unwrap();
        assert!(count > 0);
        stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://203.0.113.1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    });
    let result = fetch_batch(
        &synchronization_client().unwrap(),
        &format!("http://{address}"),
        "mesh",
        0,
        128,
    )
    .await;
    assert!(matches!(result, Err(DistributionError::PeerStatus(302))));
    task.await.unwrap();
}
