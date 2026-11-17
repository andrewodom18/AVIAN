use std::net::IpAddr;

use avian::services::{LocalPublication, ServiceConfiguration, ServiceDirectory};
use avian_membership::{
    MembershipClaims, MembershipStore, Permissions, TrustRoot, issue_membership, issue_revocations,
    node_id_for_key, write_signing_key,
};
use avian_services::{AnnouncementPage, Limits, ServiceEndpoint, ServiceKind, TransportProtocol};
use ed25519_dalek::SigningKey;

struct Fixture {
    directory: tempfile::TempDir,
    administrator: SigningKey,
    owner: SigningKey,
    membership: MembershipStore,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        avian_storage::create_private_directory(directory.path().join("private")).unwrap();
        let administrator = SigningKey::from_bytes(&[45; 32]);
        let owner = SigningKey::from_bytes(&[46; 32]);
        let observer = SigningKey::from_bytes(&[47; 32]);
        let root = TrustRoot::new("service-app-test", &administrator.verifying_key()).unwrap();
        let mut membership = MembershipStore::new(root.clone()).unwrap();
        for (index, identity) in [&owner, &observer].into_iter().enumerate() {
            let claims = MembershipClaims::new(
                "service-app-test",
                &identity.verifying_key(),
                [(index + 1) as u8; 32],
                vec![address(index)],
                1,
                Permissions::default(),
            );
            membership
                .import_membership(issue_membership(&root, &administrator, claims).unwrap())
                .unwrap();
        }
        write_signing_key(directory.path().join("private").join("owner.key"), &owner).unwrap();
        Self {
            directory,
            administrator,
            owner,
            membership,
        }
    }

    fn configuration(&self, name: &str, local_owner: bool) -> ServiceConfiguration {
        ServiceConfiguration {
            state_file: self
                .directory
                .path()
                .join("private")
                .join(format!("{name}.json")),
            identity_key_file: local_owner
                .then(|| self.directory.path().join("private").join("owner.key")),
        }
    }

    fn publication(&self) -> LocalPublication {
        LocalPublication {
            ttl_seconds: 30,
            endpoints: vec![ServiceEndpoint {
                kind: ServiceKind::ArcVideo,
                address: address(0),
                protocol: TransportProtocol::Udp,
                port: 5600,
            }],
        }
    }
}

fn address(index: usize) -> IpAddr {
    format!("10.91.0.{}", index + 1).parse().unwrap()
}

#[test]
fn protected_owner_publication_persists_and_restart_advances_revision() {
    let fixture = Fixture::new();
    let configuration = fixture.configuration("owner-services", true);
    let directory = ServiceDirectory::load(
        configuration.clone(),
        &fixture.membership,
        Limits::default(),
        1000,
    )
    .unwrap();
    let first = directory
        .publish(&fixture.membership, fixture.publication(), 1000)
        .unwrap();
    assert_eq!(
        first.claims.owner_node_id,
        node_id_for_key(&fixture.owner.verifying_key())
    );
    assert_eq!(first.claims.revision, 1);
    let restarted =
        ServiceDirectory::load(configuration, &fixture.membership, Limits::default(), 1001)
            .unwrap();
    let second = restarted
        .publish(&fixture.membership, fixture.publication(), 1001)
        .unwrap();
    assert_eq!(second.claims.revision, 2);
    let page = restarted.page(&fixture.membership, None, 10, 1002).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].expires_in_seconds, 29);
    assert_eq!(
        serde_json::to_value(page).unwrap()["items"][0]["reachability"],
        "unobserved"
    );
}

#[test]
fn relay_preserves_owner_signature_but_requires_enrolled_overlay_reader() {
    let fixture = Fixture::new();
    let owner = ServiceDirectory::load(
        fixture.configuration("owner", true),
        &fixture.membership,
        Limits::default(),
        1000,
    )
    .unwrap();
    let signed = owner
        .publish(&fixture.membership, fixture.publication(), 1000)
        .unwrap();
    let relay = ServiceDirectory::load(
        fixture.configuration("relay", false),
        &fixture.membership,
        Limits::default(),
        1000,
    )
    .unwrap();
    relay
        .receive(
            &fixture.membership,
            AnnouncementPage {
                version: 1,
                network_id: "service-app-test".into(),
                generated_at_unix: 1000,
                records: vec![signed.clone()],
                next_cursor: None,
            },
            1000,
        )
        .unwrap();
    let forwarded = relay
        .announcements_for_peer(&fixture.membership, address(1), None, 10, 1001)
        .unwrap();
    assert_eq!(forwarded.records[0], signed);
    assert!(
        relay
            .announcements_for_peer(
                &fixture.membership,
                "127.0.0.1".parse().unwrap(),
                None,
                10,
                1001
            )
            .is_err()
    );
    assert!(
        relay
            .announcements_for_peer(
                &fixture.membership,
                "10.91.0.99".parse().unwrap(),
                None,
                10,
                1001
            )
            .is_err()
    );
    assert!(
        relay
            .publish(&fixture.membership, fixture.publication(), 1001)
            .is_err()
    );
}

#[test]
fn local_callers_cannot_choose_a_foreign_owner_address_or_invent_a_class() {
    let fixture = Fixture::new();
    let owner = ServiceDirectory::load(
        fixture.configuration("owner", true),
        &fixture.membership,
        Limits::default(),
        1000,
    )
    .unwrap();
    let mut foreign = fixture.publication();
    foreign.endpoints[0].address = address(1);
    assert!(owner.publish(&fixture.membership, foreign, 1000).is_err());
    let body = serde_json::json!({"ttl_seconds":30,"endpoints":[{
        "kind":"arc_video","address":"10.91.0.1","protocol":"udp","port":5600,"traffic_class":"control"
    }]});
    assert!(serde_json::from_value::<LocalPublication>(body).is_err());
}

#[test]
fn revocation_and_expiry_are_reflected_without_reachability_claims() {
    let mut fixture = Fixture::new();
    let owner = ServiceDirectory::load(
        fixture.configuration("owner", true),
        &fixture.membership,
        Limits::default(),
        1000,
    )
    .unwrap();
    owner
        .publish(&fixture.membership, fixture.publication(), 1000)
        .unwrap();
    let revocation = issue_revocations(
        fixture.membership.trust_root(),
        &fixture.administrator,
        0,
        vec![node_id_for_key(&fixture.owner.verifying_key())],
    )
    .unwrap();
    fixture.membership.apply_revocations(revocation).unwrap();
    assert!(
        owner
            .page(&fixture.membership, None, 10, 1001)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        owner
            .publish(&fixture.membership, fixture.publication(), 1001)
            .is_err()
    );
}

#[cfg(unix)]
#[test]
fn unprotected_identity_never_publishes_an_announcement() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let configuration = fixture.configuration("owner", true);
    std::fs::set_permissions(
        configuration.identity_key_file.as_ref().unwrap(),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let owner = ServiceDirectory::load(
        configuration.clone(),
        &fixture.membership,
        Limits::default(),
        1000,
    )
    .unwrap();
    assert!(
        owner
            .publish(&fixture.membership, fixture.publication(), 1000)
            .is_err()
    );
    assert!(!configuration.state_file.exists());
}
