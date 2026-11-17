use std::{net::IpAddr, path::Path};

use avian_membership::{
    MembershipClaims, MembershipStore, Permissions, TrustRoot, issue_membership, issue_revocations,
    node_id_for_key,
};
use avian_services::{
    AnnouncementPage, Limits, Registry, ServiceEndpoint, ServiceError, ServiceKind,
    SignedAnnouncement, TransportProtocol, verify_overlay_peer,
};
use ed25519_dalek::SigningKey;

struct Fixture {
    administrator: SigningKey,
    owners: Vec<SigningKey>,
    membership: MembershipStore,
}

impl Fixture {
    fn new(network: &str) -> Self {
        let administrator = SigningKey::from_bytes(&[90; 32]);
        let root = TrustRoot::new(network, &administrator.verifying_key()).unwrap();
        let mut membership = MembershipStore::new(root.clone()).unwrap();
        let owners: Vec<_> = (1..=3)
            .map(|seed| SigningKey::from_bytes(&[seed; 32]))
            .collect();
        for (index, owner) in owners.iter().enumerate() {
            let claims = MembershipClaims::new(
                network,
                &owner.verifying_key(),
                [(index + 1) as u8; 32],
                vec![address(index)],
                1,
                Permissions::default(),
            );
            membership
                .import_membership(issue_membership(&root, &administrator, claims).unwrap())
                .unwrap();
        }
        Self {
            administrator,
            owners,
            membership,
        }
    }

    fn announcement(
        &self,
        owner: usize,
        revision: u64,
        issued: i64,
        ttl: i64,
    ) -> SignedAnnouncement {
        SignedAnnouncement::sign(
            &self.membership,
            &self.owners[owner],
            revision,
            vec![endpoint(owner)],
            ttl,
            issued,
            &Limits::default(),
        )
        .unwrap()
    }

    fn revoke(&mut self, owner: usize) {
        let record = issue_revocations(
            self.membership.trust_root(),
            &self.administrator,
            self.membership.revocation_revision(),
            vec![node_id_for_key(&self.owners[owner].verifying_key())],
        )
        .unwrap();
        self.membership.apply_revocations(record).unwrap();
    }
}

fn address(owner: usize) -> IpAddr {
    format!("10.90.0.{}", owner + 1).parse().unwrap()
}

fn endpoint(owner: usize) -> ServiceEndpoint {
    ServiceEndpoint {
        kind: ServiceKind::ArcTelemetry,
        address: address(owner),
        protocol: TransportProtocol::Udp,
        port: 14550,
    }
}

#[test]
fn independently_signed_owner_records_cross_a_relay_and_paginate() {
    let fixture = Fixture::new("service-test");
    let mut relay = Registry::new(&fixture.membership, Limits::default()).unwrap();
    for owner in 0..3 {
        relay
            .accept(
                &fixture.membership,
                fixture.announcement(owner, 1, 1000, 60),
                1000,
            )
            .unwrap();
    }
    let relay_id = node_id_for_key(&fixture.owners[1].verifying_key());
    verify_overlay_peer(&fixture.membership, &relay_id, address(1)).unwrap();
    let mut receiver = Registry::new(&fixture.membership, Limits::default()).unwrap();
    let mut cursor = None;
    let mut received = 0;
    loop {
        let page = relay
            .announcements_page(&fixture.membership, cursor.as_deref(), 1, 1001)
            .unwrap();
        assert_eq!(page.records.len(), 1);
        cursor = page.next_cursor.clone();
        received += receiver
            .accept_page(&fixture.membership, page, 1001)
            .unwrap();
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(received, 3);
    let page = receiver.page(&fixture.membership, None, 10, 1002).unwrap();
    assert_eq!(page.items.len(), 3);
    assert!(
        page.items
            .iter()
            .all(|item| item.age_seconds == 2 && item.expires_in_seconds == 58)
    );
    let encoded = serde_json::to_value(page).unwrap();
    assert_eq!(encoded["items"][0]["reachability"], "unobserved");
    assert_eq!(
        ServiceKind::ArcTelemetry.traffic_class(),
        avian_routing::TrafficClass::Telemetry
    );
}

#[test]
fn tampering_wrong_network_and_unassigned_endpoints_are_rejected() {
    let fixture = Fixture::new("service-test");
    let mut changed = fixture.announcement(0, 1, 1000, 60);
    changed.claims.endpoints[0].port += 1;
    assert!(matches!(
        changed.verify(&fixture.membership, 1001, &Limits::default()),
        Err(ServiceError::InvalidSignature)
    ));
    let other_network = Fixture::new("another-network");
    assert!(matches!(
        fixture.announcement(0, 1, 1000, 60).verify(
            &other_network.membership,
            1001,
            &Limits::default()
        ),
        Err(ServiceError::WrongNetwork)
    ));
    let result = SignedAnnouncement::sign(
        &fixture.membership,
        &fixture.owners[0],
        1,
        vec![endpoint(1)],
        60,
        1000,
        &Limits::default(),
    );
    assert!(matches!(result, Err(ServiceError::InvalidField(_))));
    assert!(matches!(
        verify_overlay_peer(
            &fixture.membership,
            &node_id_for_key(&fixture.owners[0].verifying_key()),
            address(1)
        ),
        Err(ServiceError::Unauthorized)
    ));
    assert!(
        verify_overlay_peer(
            &fixture.membership,
            &node_id_for_key(&fixture.owners[0].verifying_key()),
            "127.0.0.1".parse().unwrap()
        )
        .is_err()
    );
}

#[test]
fn expiry_and_restart_keep_watermarks_against_still_valid_older_records() {
    let fixture = Fixture::new("service-test");
    let mut registry = Registry::new(&fixture.membership, Limits::default()).unwrap();
    let old = fixture.announcement(0, 1, 1000, 300);
    registry
        .accept(&fixture.membership, old.clone(), 1000)
        .unwrap();
    assert!(
        !registry
            .accept(&fixture.membership, old.clone(), 1001)
            .unwrap()
    );
    registry
        .accept(
            &fixture.membership,
            fixture.announcement(0, 2, 1001, 1),
            1001,
        )
        .unwrap();
    assert!(
        registry
            .page(&fixture.membership, None, 10, 1003)
            .unwrap()
            .items
            .is_empty()
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("services.json");
    registry.save(&path).unwrap();
    let mut restarted =
        Registry::load(&path, &fixture.membership, Limits::default(), 1004).unwrap();
    assert!(matches!(
        restarted.accept(&fixture.membership, old, 1004),
        Err(ServiceError::Rollback)
    ));
    let owner_id = node_id_for_key(&fixture.owners[0].verifying_key());
    assert_eq!(restarted.next_revision(&owner_id).unwrap(), 3);
    assert!(matches!(
        restarted.page(&fixture.membership, None, 10, 1002),
        Err(ServiceError::Rollback)
    ));
}

#[test]
fn revocation_hides_services_and_rejects_peer_owner_and_membership_rollback() {
    let mut fixture = Fixture::new("service-test");
    let previous_membership = fixture.membership.clone();
    let announcement = fixture.announcement(0, 1, 1000, 60);
    let mut registry = Registry::new(&fixture.membership, Limits::default()).unwrap();
    registry
        .accept(&fixture.membership, announcement.clone(), 1000)
        .unwrap();
    fixture.revoke(0);
    assert!(
        registry
            .page(&fixture.membership, None, 10, 1001)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(matches!(
        registry.accept(&fixture.membership, announcement, 1001),
        Err(ServiceError::Unauthorized)
    ));
    assert!(matches!(
        registry.page(&previous_membership, None, 10, 1001),
        Err(ServiceError::Rollback)
    ));
    let directory = tempfile::tempdir().unwrap();
    registry
        .save(directory.path().join("services.json"))
        .unwrap();
    assert!(
        Registry::load(
            directory.path().join("services.json"),
            &previous_membership,
            Limits::default(),
            1002
        )
        .is_err()
    );
}

#[test]
fn withdrawals_relay_and_do_not_allow_reappearance_by_replay() {
    let fixture = Fixture::new("service-test");
    let mut registry = Registry::new(&fixture.membership, Limits::default()).unwrap();
    let old = fixture.announcement(0, 1, 1000, 60);
    registry
        .accept(&fixture.membership, old.clone(), 1000)
        .unwrap();
    let withdrawal = SignedAnnouncement::sign(
        &fixture.membership,
        &fixture.owners[0],
        2,
        vec![],
        60,
        1001,
        &Limits::default(),
    )
    .unwrap();
    registry
        .accept(&fixture.membership, withdrawal, 1001)
        .unwrap();
    let page = registry
        .announcements_page(&fixture.membership, None, 10, 1001)
        .unwrap();
    assert!(page.records[0].claims.endpoints.is_empty());
    let mut receiver = Registry::new(&fixture.membership, Limits::default()).unwrap();
    receiver
        .accept_page(&fixture.membership, page, 1001)
        .unwrap();
    assert!(matches!(
        receiver.accept(&fixture.membership, old, 1002),
        Err(ServiceError::Rollback)
    ));
}

#[test]
fn invalid_page_is_atomic_and_resource_budgets_do_not_limit_mesh_membership() {
    let fixture = Fixture::new("service-test");
    let mut invalid = fixture.announcement(1, 1, 1000, 60);
    invalid.signature = "00".repeat(64);
    let page = AnnouncementPage {
        version: 1,
        network_id: "service-test".into(),
        generated_at_unix: 1000,
        records: vec![fixture.announcement(0, 1, 1000, 60), invalid],
        next_cursor: None,
    };
    let mut registry = Registry::new(&fixture.membership, Limits::default()).unwrap();
    assert!(
        registry
            .accept_page(&fixture.membership, page, 1000)
            .is_err()
    );
    assert!(
        registry
            .page(&fixture.membership, None, 10, 1000)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        SignedAnnouncement::sign(
            &fixture.membership,
            &fixture.owners[0],
            1,
            vec![endpoint(0)],
            301,
            1000,
            &Limits::default()
        )
        .is_err()
    );
    let limits = Limits {
        maximum_announcement_bytes: 1024,
        maximum_registry_bytes: 1500,
        ..Limits::default()
    };
    let mut bounded = Registry::new(&fixture.membership, limits).unwrap();
    bounded
        .accept(
            &fixture.membership,
            fixture.announcement(0, 1, 1000, 60),
            1000,
        )
        .unwrap();
    assert!(matches!(
        bounded.accept(
            &fixture.membership,
            fixture.announcement(1, 1, 1000, 60),
            1000
        ),
        Err(ServiceError::ResourceBudget)
    ));
    assert_eq!(
        bounded
            .page(&fixture.membership, None, 10, 1001)
            .unwrap()
            .items
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn protected_persistence_rejects_stale_writers_and_public_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new("service-test");
    let mut first = Registry::new(&fixture.membership, Limits::default()).unwrap();
    first
        .accept(
            &fixture.membership,
            fixture.announcement(0, 1, 1000, 60),
            1000,
        )
        .unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("services.json");
    first.save(&path).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let mut second = first.clone();
    second
        .accept(
            &fixture.membership,
            fixture.announcement(0, 2, 1001, 60),
            1001,
        )
        .unwrap();
    second.save(&path).unwrap();
    assert!(matches!(first.save(&path), Err(ServiceError::Rollback)));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        Registry::load(&path, &fixture.membership, Limits::default(), 1002),
        Err(ServiceError::Storage(
            avian_storage::StorageError::InsecurePermissions
        ))
    ));
    check_symlink(directory.path(), &fixture.membership);
}

#[cfg(unix)]
fn check_symlink(directory: &Path, membership: &MembershipStore) {
    let target = directory.join("target.json");
    let registry = Registry::new(membership, Limits::default()).unwrap();
    registry.save(&target).unwrap();
    let link = directory.join("link.json");
    std::os::unix::fs::symlink(target, &link).unwrap();
    assert!(matches!(
        Registry::load(link, membership, Limits::default(), 1000),
        Err(ServiceError::Storage(
            avian_storage::StorageError::InsecurePermissions
        ))
    ));
}

#[test]
fn membership_rotation_invalidates_old_endpoints_and_pins_its_revision() {
    let mut fixture = Fixture::new("service-test");
    let previous_membership = fixture.membership.clone();
    let mut registry = Registry::new(&fixture.membership, Limits::default()).unwrap();
    registry
        .accept(
            &fixture.membership,
            fixture.announcement(0, 1, 1000, 60),
            1000,
        )
        .unwrap();
    let replacement = MembershipClaims::new(
        "service-test",
        &fixture.owners[0].verifying_key(),
        [22; 32],
        vec!["10.90.0.99".parse().unwrap()],
        2,
        Permissions::default(),
    );
    fixture
        .membership
        .import_membership(
            issue_membership(
                fixture.membership.trust_root(),
                &fixture.administrator,
                replacement,
            )
            .unwrap(),
        )
        .unwrap();
    assert!(
        registry
            .page(&fixture.membership, None, 10, 1001)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(matches!(
        registry.page(&previous_membership, None, 10, 1002),
        Err(ServiceError::Rollback)
    ));
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("services.json");
    registry.save(&path).unwrap();
    assert!(matches!(
        Registry::load(path, &previous_membership, Limits::default(), 1002),
        Err(ServiceError::Rollback)
    ));
}

#[test]
fn future_dates_reordered_publications_and_invalid_page_cursors_fail_closed() {
    let fixture = Fixture::new("service-test");
    let mut registry = Registry::new(&fixture.membership, Limits::default()).unwrap();
    let announcement = fixture.announcement(0, 1, 1000, 60);
    assert!(matches!(
        announcement.verify(&fixture.membership, 990, &Limits::default()),
        Err(ServiceError::Expired)
    ));
    registry
        .accept(&fixture.membership, announcement, 1000)
        .unwrap();
    let backwards = fixture.announcement(0, 2, 999, 60);
    assert!(matches!(
        registry.accept(&fixture.membership, backwards, 1001),
        Err(ServiceError::Rollback)
    ));
    assert!(
        registry
            .page(&fixture.membership, Some("invalid-cursor"), 1, 1001)
            .is_err()
    );
    assert!(registry.page(&fixture.membership, None, 0, 1001).is_err());
    let key = SigningKey::from_bytes(&[50; 32]);
    assert!(matches!(
        SignedAnnouncement::sign(
            &fixture.membership,
            &key,
            1,
            vec![endpoint(0)],
            60,
            1001,
            &Limits::default()
        ),
        Err(ServiceError::Unauthorized)
    ));
}
