use avian::{
    api,
    configuration::{ChudConfiguration, Configuration, create_private_file},
    state::ApplicationState,
};
use avian_membership::{
    MembershipClaims, MembershipStore, Permissions, TrustRoot, issue_membership, issue_revocations,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, sync::Arc};
use tempfile::TempDir;
use tower::ServiceExt;

const LOCAL_TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Fixture {
    directory: TempDir,
    configuration: Configuration,
    state: Arc<ApplicationState>,
    router: Router,
}

impl Fixture {
    fn new(member_count: usize) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let administrator = SigningKey::from_bytes(&[91; 32]);
        let root = TrustRoot::new("contract-test", &administrator.verifying_key()).unwrap();
        let mut membership = MembershipStore::new(root.clone()).unwrap();
        for number in 1..=member_count {
            let mut identity_seed = [17; 32];
            identity_seed[..8].copy_from_slice(&(number as u64).to_be_bytes());
            let identity = SigningKey::from_bytes(&identity_seed);
            let claims = MembershipClaims::new(
                root.network_id.clone(),
                &identity.verifying_key(),
                identity.verifying_key().to_bytes(),
                vec![format!("fd42::{number:x}").parse().unwrap()],
                1,
                Permissions {
                    relay: true,
                    administer: false,
                },
            );
            membership
                .import_membership(issue_membership(&root, &administrator, claims).unwrap())
                .unwrap();
        }
        let configuration = Configuration {
            listen: "127.0.0.1:4179".parse().unwrap(),
            token_file: directory.path().join("token"),
            membership_file: directory.path().join("membership.json"),
            trust_root_file: directory.path().join("root.json"),
            chud: None,
        };
        create_private_file(&configuration.token_file, LOCAL_TOKEN.as_bytes()).unwrap();
        fs::write(
            &configuration.trust_root_file,
            serde_json::to_vec(&root).unwrap(),
        )
        .unwrap();
        membership.save(&configuration.membership_file).unwrap();
        let state = ApplicationState::load(&configuration).unwrap();
        let router = api::router(state.clone());
        Self {
            directory,
            configuration,
            state,
            router,
        }
    }
}

async fn request(
    router: Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let response = router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {LOCAL_TOKEN}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn missing_wrong_and_browser_credentials_cannot_access_local_api() {
    let fixture = Fixture::new(0);
    for endpoint in ["/v1/status", "/v1/nodes", "/v1/events", "/v1/qualification"] {
        for token in [None, Some("Bearer wrong-token")] {
            let mut builder = Request::builder().uri(endpoint);
            if let Some(token) = token {
                builder = builder.header(header::AUTHORIZATION, token);
            }
            let response = fixture
                .router
                .clone()
                .oneshot(builder.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{endpoint}");
        }
    }
    let response = fixture
        .router
        .oneshot(
            Request::builder()
                .uri("/v1/status")
                .header(header::AUTHORIZATION, format!("Bearer {LOCAL_TOKEN}"))
                .header(header::ORIGIN, "https://unrelated.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn response_page_budget_does_not_cap_membership() {
    let fixture = Fixture::new(503);
    let mut cursor = None;
    let mut seen = BTreeSet::new();
    loop {
        let path = match &cursor {
            Some(cursor) => format!("/v1/nodes?limit=500&after={cursor}"),
            None => "/v1/nodes?limit=500".into(),
        };
        let (status, body) = request(fixture.router.clone(), "GET", &path, None).await;
        assert_eq!(status, StatusCode::OK);
        let items = body["items"].as_array().unwrap();
        assert!(items.len() <= 500);
        for item in items {
            assert_eq!(item["reachability"], "unknown");
            assert!(item["observed_at"].is_null());
            assert!(seen.insert(item["node_id"].as_str().unwrap().to_owned()));
        }
        cursor = body["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 503);
    let (status, body) = request(fixture.router, "GET", "/v1/nodes?limit=501", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_page_size");
}

#[tokio::test]
async fn unobserved_services_paths_and_members_do_not_claim_live_connectivity() {
    let fixture = Fixture::new(1);
    let (status, body) = request(fixture.router.clone(), "GET", "/v1/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "observation_only");
    assert_eq!(body["data_plane"], "not_integrated");
    assert_eq!(body["network_control_enabled"], false);
    for endpoint in ["/v1/services", "/v1/paths"] {
        let (status, body) = request(fixture.router.clone(), "GET", endpoint, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["availability"], "not_integrated");
        assert_eq!(body["items"], json!([]));
    }
}

fn preview(traffic_class: &str) -> Value {
    json!({
        "source": "ground",
        "destination": "drone",
        "traffic_class": traffic_class,
        "now_ms": 10_000,
        "snapshot": {
            "nodes": [
                {"node_id": "ground", "relay_allowed": true},
                {"node_id": "drone", "relay_allowed": true}
            ],
            "links": [
                {
                    "link_id": "cellular", "source": "ground", "destination": "drone",
                    "interface_name": "wwan0", "available": true, "metered": true,
                    "observed_at_ms": 10_000, "round_trip_time_ms": 1.0,
                    "loss_fraction": 0.0, "jitter_ms": null, "available_capacity_bps": 100_000_000
                },
                {
                    "link_id": "radio", "source": "ground", "destination": "drone",
                    "interface_name": "radio0", "available": true, "metered": false,
                    "observed_at_ms": 10_000, "round_trip_time_ms": 10.0,
                    "loss_fraction": 0.0, "jitter_ms": null, "available_capacity_bps": 10_000_000
                }
            ]
        }
    })
}

#[tokio::test]
async fn preview_respects_metered_policy_and_never_applies_caller_observations() {
    let fixture = Fixture::new(1);
    let before = fs::read(&fixture.configuration.membership_file).unwrap();
    let (status, control) = request(
        fixture.router.clone(),
        "POST",
        "/v1/route-preview",
        Some(preview("control")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(control["route"]["link_ids"], json!(["cellular"]));
    let (status, video) = request(
        fixture.router.clone(),
        "POST",
        "/v1/route-preview",
        Some(preview("video")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(video["route"]["link_ids"], json!(["radio"]));
    assert_eq!(video["source"], "caller_supplied");
    assert_eq!(video["applied"], false);
    assert!(video["route"]["estimated_jitter_ms"].is_null());
    let mut allowed = preview("video");
    allowed["policy"] = json!({"allow_metered_video": true});
    let (status, video) = request(
        fixture.router.clone(),
        "POST",
        "/v1/route-preview",
        Some(allowed),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(video["route"]["link_ids"], json!(["cellular"]));
    assert_eq!(
        fs::read(&fixture.configuration.membership_file).unwrap(),
        before
    );
    assert_eq!(
        fixture.state.membership.read().await.memberships().count(),
        1
    );
    let (_, paths) = request(fixture.router.clone(), "GET", "/v1/paths", None).await;
    assert_eq!(paths["items"], json!([]));
    let (status, denied) = request(
        fixture.router,
        "POST",
        "/v1/network-control",
        Some(json!({"enable": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(denied["error"]["code"], "qualification_required");
}

#[tokio::test]
async fn preview_overload_returns_backpressure_without_blocking_status() {
    let fixture = Fixture::new(0);
    let permits = fixture.state.preview_slots.available_permits();
    let reservation = fixture
        .state
        .preview_slots
        .acquire_many(permits as u32)
        .await
        .unwrap();
    let (status, body) = request(
        fixture.router.clone(),
        "POST",
        "/v1/route-preview",
        Some(preview("control")),
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["error"]["code"], "preview_capacity_reached");
    assert_eq!(
        request(fixture.router, "GET", "/v1/status", None).await.0,
        StatusCode::OK
    );
    drop(reservation);
}

#[tokio::test]
async fn unavailable_chud_does_not_make_local_status_unavailable() {
    let mut fixture = Fixture::new(0);
    let upstream = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = upstream.local_addr().unwrap();
    let disconnect = tokio::spawn(async move {
        let (connection, _) = upstream.accept().await.unwrap();
        drop(connection);
    });
    let key_file = fixture.directory.path().join("chud-key");
    create_private_file(&key_file, LOCAL_TOKEN.as_bytes()).unwrap();
    fixture.configuration.chud = Some(ChudConfiguration {
        url: format!("http://{address}"),
        api_key_file: key_file,
        certificate_file: None,
    });
    let state = ApplicationState::load(&fixture.configuration).unwrap();
    let router = api::router(state);
    let (status, unavailable) = request(router.clone(), "GET", "/v1/radios", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(unavailable["error"]["code"], "chud_unavailable");
    disconnect.await.unwrap();
    let (status, body) = request(router, "GET", "/v1/status", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["chud_configured"], true);
    assert_eq!(body["network_control_enabled"], false);
}

#[test]
fn configuration_rejects_nonlocal_listeners_and_resolves_state_paths() {
    let fixture = Fixture::new(0);
    let file = fixture.directory.path().join("avian.toml");
    for address in ["0.0.0.0:4179", "[::]:4179", "192.168.1.10:4179"] {
        fs::write(&file, format!("listen = '{address}'\ntoken_file = 'token'\nmembership_file = 'membership.json'\ntrust_root_file = 'root.json'\n")).unwrap();
        assert!(Configuration::load(&file).is_err(), "{address}");
    }
    fs::write(&file, "listen = '[::1]:4179'\ntoken_file = 'token'\nmembership_file = 'membership.json'\ntrust_root_file = 'root.json'\n").unwrap();
    let loaded = Configuration::load(&file).unwrap();
    assert_eq!(loaded.token_file, fixture.configuration.token_file);
    assert_eq!(
        loaded.membership_file,
        fixture.configuration.membership_file
    );
}

#[tokio::test]
async fn membership_watcher_keeps_verified_revocations_when_an_old_file_returns() {
    let fixture = Fixture::new(1);
    let old_file = fs::read(&fixture.configuration.membership_file).unwrap();
    let root = fixture.state.membership.read().await.trust_root().clone();
    let node_id = fixture
        .state
        .membership
        .read()
        .await
        .memberships()
        .next()
        .unwrap()
        .claims
        .node_id
        .clone();
    let mut updated =
        MembershipStore::load(&fixture.configuration.membership_file, root.clone()).unwrap();
    let administrator = SigningKey::from_bytes(&[91; 32]);
    updated
        .apply_revocations(issue_revocations(&root, &administrator, 0, vec![node_id]).unwrap())
        .unwrap();
    updated
        .save(&fixture.configuration.membership_file)
        .unwrap();
    let watcher = tokio::spawn(
        fixture
            .state
            .clone()
            .watch_membership(fixture.configuration.clone()),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fixture.state.membership.read().await.revocation_revision() != 1 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    fs::write(&fixture.configuration.membership_file, old_file).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fixture.state.reload_error.read().await.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let (status, nodes) = request(fixture.router.clone(), "GET", "/v1/nodes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(nodes["revocation_revision"], 1);
    assert_eq!(nodes["items"], json!([]));
    let (_, status) = request(fixture.router, "GET", "/v1/status", None).await;
    assert!(
        status["membership_reload_error"]
            .as_str()
            .unwrap()
            .contains("rollback")
    );
    watcher.abort();
    assert!(watcher.await.unwrap_err().is_cancelled());
}
