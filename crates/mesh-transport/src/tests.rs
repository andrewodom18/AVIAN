use crate::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use mesh_core::{DeliveryClass, MeshPayload, MissionState, MissionStatus, NodeId};
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

fn key(byte: u8) -> String {
    STANDARD.encode([byte; 32])
}
fn config(name: &str, storage: &TempDir, secret: &str) -> MeshNodeConfig {
    MeshNodeConfig {
        name: name.to_owned(),
        formation_id: "test-formation".to_owned(),
        base64_shared_key: secret.to_owned(),
        bind_address: "127.0.0.1:0".parse().unwrap(),
        storage_path: storage.path().to_owned(),
    }
}
fn record(source: &str, sequence: u64, time: u64) -> AvianRecord {
    AvianRecord::new(
        NodeId::from(source),
        sequence,
        DeliveryClass::Mission,
        time,
        MeshPayload::Mission(MissionState {
            mission_id: Uuid::from_u128(42),
            objective: format!("version {sequence}"),
            generation: sequence,
            status: MissionStatus::Active,
        }),
    )
    .unwrap()
}
async fn wait_record(node: &MeshNode, id: &str, expected: &AvianRecord) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if node.get(expected.class, id).await.unwrap().as_ref() == Some(expected) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("records must converge");
}

#[test]
fn record_schema_and_delivery_policy_remain_compatible() {
    let mut v1 = record("a", 1, 10);
    v1.schema_version = 1;
    v1.validate().unwrap();
    assert_eq!(record("a", 1, 10).schema_version, 2);
    v1.schema_version = 3;
    assert!(v1.validate().is_err());
    let mut mismatch = record("a", 1, 10);
    mismatch.class = DeliveryClass::Telemetry;
    assert!(mismatch.validate().is_err());
    let requirements = requirements_for(DeliveryClass::Telemetry, 512);
    assert_eq!(requirements.ttl, Some(Duration::from_secs(2)));
    assert!(!requirements.reliable);
    assert!(uav_pace_config()
        .default_policy
        .unwrap()
        .ordered()
        .any(|id| id == "quic-microhard"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_node_partition_reconnect_and_fallback_converge() {
    let dirs = [
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    ];
    let a = MeshNode::start(config("a", &dirs[0], &key(1)))
        .await
        .unwrap();
    let b = MeshNode::start(config("b", &dirs[1], &key(1)))
        .await
        .unwrap();
    let c = MeshNode::start(config("c", &dirs[2], &key(1)))
        .await
        .unwrap();
    let descriptor = b.peer_descriptor().unwrap();
    let mut addresses = vec!["127.0.0.1:1".parse().unwrap()];
    addresses.extend_from_slice(descriptor.addresses());
    let fallback = PeerDescriptor::with_addresses("b", b.endpoint_id_hex(), addresses).unwrap();
    a.connect(&fallback).await.unwrap();
    b.connect(&c.peer_descriptor().unwrap()).await.unwrap();
    let original = record("a", 1, 100);
    a.put("mission", &original).await.unwrap();
    wait_record(&c, "mission", &original).await;
    assert_eq!(a.peer_count(), 1);
    assert_eq!(c.peer_count(), 1);
    assert!(a.peer_remote_address(&b.endpoint_id_hex()).is_some());
    b.disconnect(&c.peer_descriptor().unwrap()).unwrap();
    let newer_a = record("a", 2, 200);
    let newer_c = record("c", 3, 300);
    a.put("mission", &newer_a).await.unwrap();
    c.put("mission", &newer_c).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    b.connect(&c.peer_descriptor().unwrap()).await.unwrap();
    wait_record(&a, "mission", &newer_c).await;
    wait_record(&b, "mission", &newer_c).await;
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
    c.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_connections_deduplicate_and_persist_across_restart() {
    let adir = TempDir::new().unwrap();
    let bdir = TempDir::new().unwrap();
    let cfg = config("a", &adir, &key(2));
    let a = MeshNode::start(cfg.clone()).await.unwrap();
    let b = MeshNode::start(config("b", &bdir, &key(2))).await.unwrap();
    let pa = a.peer_descriptor().unwrap();
    let pb = b.peer_descriptor().unwrap();
    let _ = tokio::join!(a.connect(&pb), b.connect(&pa));
    let value = record("a", 5, 500);
    a.put("durable", &value).await.unwrap();
    wait_record(&b, "durable", &value).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(a.peer_count(), 1);
    assert_eq!(b.peer_count(), 1);
    let identity = a.endpoint_id_hex();
    assert_eq!(identity, derive_endpoint_id(&key(2), "a").unwrap());
    a.shutdown().await.unwrap();
    a.shutdown().await.unwrap();
    let restarted = MeshNode::start(cfg).await.unwrap();
    assert_eq!(restarted.endpoint_id_hex(), identity);
    assert_eq!(
        restarted
            .get(DeliveryClass::Mission, "durable")
            .await
            .unwrap(),
        Some(value)
    );
    restarted.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wrong_key_and_wrong_formation_are_rejected() {
    let dirs = [
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
        TempDir::new().unwrap(),
    ];
    let a = MeshNode::start(config("a", &dirs[0], &key(3)))
        .await
        .unwrap();
    let b = MeshNode::start(config("b", &dirs[1], &key(4)))
        .await
        .unwrap();
    let mut other = config("c", &dirs[2], &key(3));
    other.formation_id = "other".into();
    let c = MeshNode::start(other).await.unwrap();
    assert!(a.connect(&b.peer_descriptor().unwrap()).await.is_err());
    assert!(a.connect(&c.peer_descriptor().unwrap()).await.is_err());
    assert_eq!(a.peer_count(), 0);
    assert_eq!(b.peer_count(), 0);
    assert_eq!(c.peer_count(), 0);
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
    c.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_state_and_live_storage_reuse_fail_closed() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("unknown.db"), "existing state").unwrap();
    assert!(MeshNode::start(config("a", &dir, &key(5))).await.is_err());
    std::fs::remove_file(dir.path().join("unknown.db")).unwrap();
    let node = MeshNode::start(config("a", &dir, &key(5))).await.unwrap();
    assert!(MeshNode::start(config("a", &dir, &key(5))).await.is_err());
    node.shutdown().await.unwrap();
    assert!(MeshNode::start(config("other", &dir, &key(5)))
        .await
        .is_err());
}

#[test]
fn deterministic_equal_timestamp_conflicts_and_storage_bounds() {
    let a = TempDir::new().unwrap();
    let b = TempDir::new().unwrap();
    let mut sa = crate::store::Store::open(a.path(), "f", "id").unwrap();
    let mut sb = crate::store::Store::open(b.path(), "f", "id").unwrap();
    let first = crate::store::StoredRecord {
        record_id: "same".into(),
        record: record("a", 1, 100),
    };
    let second = crate::store::StoredRecord {
        record_id: "same".into(),
        record: record("b", 1, 100),
    };
    sa.merge(first.clone()).unwrap();
    sa.merge(second.clone()).unwrap();
    sb.merge(second).unwrap();
    sb.merge(first).unwrap();
    assert_eq!(
        sa.scan(DeliveryClass::Mission).unwrap(),
        sb.scan(DeliveryClass::Mission).unwrap()
    );
    assert_eq!(sa.scan(DeliveryClass::Mission).unwrap().len(), 1);
    let mut huge = record("a", 2, 200);
    if let MeshPayload::Mission(mission) = &mut huge.payload {
        mission.objective = "x".repeat(1024 * 1024);
    }
    assert!(sa
        .merge(crate::store::StoredRecord {
            record_id: "huge".into(),
            record: huge
        })
        .is_err());
}

#[test]
fn radio_attachment_mission_and_latest_telemetry_registers() {
    let assertion = mesh_core::RadioAttachmentAssertion::new(
        100,
        NodeId::from("a"),
        "drone-a",
        "00:11:22:33:44:55",
        None,
    )
    .unwrap();
    AvianRecord::new(
        NodeId::from("a"),
        1,
        DeliveryClass::Mission,
        100,
        MeshPayload::RadioAttachmentAssertion(assertion),
    )
    .unwrap();
    let dir = TempDir::new().unwrap();
    let mut store = crate::store::Store::open(dir.path(), "f", "id").unwrap();
    for sequence in [1, 3, 2] {
        let telemetry = mesh_core::Telemetry {
            source: NodeId::from("a"),
            timestamp_ms: sequence * 100,
            latitude_deg: 0.0,
            longitude_deg: 0.0,
            altitude: mesh_core::Altitude::new(10.0, 5.0, 5.0).unwrap(),
            velocity_ned_mps: [0.0; 3],
            attitude_rpy_deg: [0.0; 3],
            battery_remaining: None,
            control_link_quality: None,
            armed: false,
            landed: None,
            failsafe: false,
        };
        let value = AvianRecord::new(
            NodeId::from("a"),
            sequence,
            DeliveryClass::Telemetry,
            sequence * 100,
            MeshPayload::Telemetry(telemetry),
        )
        .unwrap();
        store
            .merge(crate::store::StoredRecord {
                record_id: "telemetry:a".into(),
                record: value,
            })
            .unwrap();
    }
    let values = store.scan(DeliveryClass::Telemetry).unwrap();
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].1.sequence, 3);
    assert!(values[0].1.is_expired_at(2300));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_joiner_catches_multiple_pages_and_an_early_record_update() {
    let adir = TempDir::new().unwrap();
    let bdir = TempDir::new().unwrap();
    let a = MeshNode::start(config("a", &adir, &key(6))).await.unwrap();
    for index in 0..130 {
        a.put(&format!("mission-{index:03}"), &record("a", 1, 100))
            .await
            .unwrap();
    }
    let assertion = mesh_core::RadioAttachmentAssertion::new(
        100,
        NodeId::from("a"),
        "drone-a",
        "00:11:22:33:44:55",
        None,
    )
    .unwrap();
    let attachment = AvianRecord::new(
        NodeId::from("a"),
        1,
        DeliveryClass::Mission,
        100,
        MeshPayload::RadioAttachmentAssertion(assertion),
    )
    .unwrap();
    a.put("attachment", &attachment).await.unwrap();
    let bcfg = config("b", &bdir, &key(6));
    let b = MeshNode::start(bcfg.clone()).await.unwrap();
    b.connect(&a.peer_descriptor().unwrap()).await.unwrap();
    let updated = record("a", 2, 200);
    // This key lies in the first inventory page; changing it during catch-up
    // must not prevent the cursor from reaching the later pages.
    a.put("mission-000", &updated).await.unwrap();
    wait_record(&b, "mission-129", &record("a", 1, 100)).await;
    wait_record(&b, "mission-000", &updated).await;
    wait_record(&b, "attachment", &attachment).await;
    assert_eq!(b.scan(DeliveryClass::Mission).await.unwrap().len(), 131);
    b.shutdown().await.unwrap();
    let restarted = MeshNode::start(bcfg).await.unwrap();
    assert_eq!(
        restarted
            .get(DeliveryClass::Mission, "attachment")
            .await
            .unwrap(),
        Some(attachment)
    );
    restarted.shutdown().await.unwrap();
    a.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changes_during_active_exchanges_do_not_wait_for_repair_interval() {
    let adir = TempDir::new().unwrap();
    let bdir = TempDir::new().unwrap();
    let a = MeshNode::start(config("fresh-a", &adir, &key(7)))
        .await
        .unwrap();
    let b = MeshNode::start(config("fresh-b", &bdir, &key(7)))
        .await
        .unwrap();
    a.connect(&b.peer_descriptor().unwrap()).await.unwrap();
    for sequence in 1..33 {
        let current = record("fresh-a", sequence, sequence * 100);
        a.put("fresh", &current).await.unwrap();
        tokio::time::timeout(
            Duration::from_millis(500),
            wait_record(&b, "fresh", &current),
        )
        .await
        .expect("a publication during synchronization must not wait two seconds");
    }
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn emergency_gets_prompt_replication_during_bulk_catchup() {
    let adir = TempDir::new().unwrap();
    let bdir = TempDir::new().unwrap();
    let a = MeshNode::start(config("priority-a", &adir, &key(8)))
        .await
        .unwrap();
    for index in 0..130 {
        let image = mesh_core::ImageManifest {
            schema_version: 2,
            image_id: Uuid::from_u128(index),
            captured_at_ms: 100,
            sensor: "camera".into(),
            media_type: "image/jpeg".into(),
            byte_count: 1024,
            sha256: "ab".repeat(32),
            imagery_ref: format!("images/{index}.jpg"),
            position: None,
            heading_deg: None,
            geotag_status: mesh_core::GeotagStatus::NotAttempted,
        };
        let bulk = AvianRecord::new(
            NodeId::from("priority-a"),
            1,
            DeliveryClass::Bulk,
            100,
            MeshPayload::ImageManifest(image),
        )
        .unwrap();
        a.put(&format!("image-{index:03}"), &bulk).await.unwrap();
    }
    let b = MeshNode::start(config("priority-b", &bdir, &key(8)))
        .await
        .unwrap();
    b.connect(&a.peer_descriptor().unwrap()).await.unwrap();
    let command = mesh_core::EmergencyCommand::issue(
        &ed25519_dalek::SigningKey::from_bytes(&[9; 32]),
        Uuid::from_u128(999),
        NodeId::from("priority-a"),
        NodeId::from("priority-b"),
        1000,
        3000,
        1,
        mesh_core::EmergencyAction::ReturnToLaunch,
    )
    .unwrap();
    let emergency = AvianRecord::new(
        NodeId::from("priority-a"),
        1,
        DeliveryClass::Emergency,
        1000,
        MeshPayload::EmergencyCommand(command),
    )
    .unwrap();
    a.put("urgent", &emergency).await.unwrap();
    tokio::time::timeout(
        Duration::from_millis(500),
        wait_record(&b, "urgent", &emergency),
    )
    .await
    .expect("bulk catch-up must not defer an emergency until the next full pass");
    tokio::time::timeout(Duration::from_secs(10), async {
        while b.scan(DeliveryClass::Bulk).await.unwrap().len() != 130 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("priority traffic must preserve bulk cursor progress");
    a.shutdown().await.unwrap();
    b.shutdown().await.unwrap();
    assert!(
        crate::store::record_key(DeliveryClass::Emergency, "z")
            < crate::store::record_key(DeliveryClass::Acknowledgement, "a")
    );
    assert!(
        crate::store::record_key(DeliveryClass::Telemetry, "z")
            < crate::store::record_key(DeliveryClass::Bulk, "a")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceled_unreachable_connect_does_not_prevent_shutdown_and_restart() {
    let dir = TempDir::new().unwrap();
    let cfg = config("cancel-test", &dir, &key(9));
    let node = MeshNode::start(cfg.clone()).await.unwrap();
    let peer = PeerDescriptor::new(
        "unreachable",
        derive_endpoint_id(&key(9), "unreachable").unwrap(),
        "127.0.0.1:1".parse().unwrap(),
    )
    .unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(25), node.connect(&peer)).await;
    tokio::time::timeout(Duration::from_secs(5), node.shutdown())
        .await
        .expect("canceled dial must not stall shutdown")
        .unwrap();
    let restarted = MeshNode::start(cfg).await.unwrap();
    restarted.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_during_authentication_closes_the_unadmitted_connection() {
    let dir = TempDir::new().unwrap();
    let node = MeshNode::start(config("auth-cancel", &dir, &key(10)))
        .await
        .unwrap();
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&[11; 32]))
        .alpns(vec![b"avian/records/1".to_vec()])
        .bind_addr("127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap())
        .unwrap()
        .bind()
        .await
        .unwrap();
    let descriptor = PeerDescriptor::with_addresses(
        "stalled-auth",
        endpoint.id().to_string(),
        endpoint.addr().ip_addrs().copied().collect(),
    )
    .unwrap();
    let dialer = node.clone();
    let task = tokio::spawn(async move { dialer.connect(&descriptor).await });
    let incoming = endpoint.accept().await.unwrap().await.unwrap();
    let (_send, mut receive) = incoming.accept_bi().await.unwrap();
    let mut header = [0; 4];
    receive.read_exact(&mut header).await.unwrap();
    // No server proof is sent. The caller cancels after its first auth frame.
    task.abort();
    let _ = task.await;
    tokio::time::timeout(Duration::from_secs(2), incoming.closed())
        .await
        .expect("canceling authentication must close QUIC");
    assert_eq!(node.peer_count(), 0);
    node.shutdown().await.unwrap();
    endpoint.close().await;
}

#[test]
fn nonfinite_local_records_never_corrupt_persistent_storage() {
    let dir = TempDir::new().unwrap();
    let mut store = crate::store::Store::open(dir.path(), "f", "id").unwrap();
    for (latitude_deg, battery_remaining) in [(f64::NAN, None), (0.0, Some(f32::NAN))] {
        let telemetry = mesh_core::Telemetry {
            source: NodeId::from("a"),
            timestamp_ms: 100,
            latitude_deg,
            longitude_deg: 0.0,
            altitude: mesh_core::Altitude::new(10.0, 5.0, 5.0).unwrap(),
            velocity_ned_mps: [0.0; 3],
            attitude_rpy_deg: [0.0; 3],
            battery_remaining,
            control_link_quality: None,
            armed: false,
            landed: None,
            failsafe: false,
        };
        let invalid = AvianRecord::new(
            NodeId::from("a"),
            1,
            DeliveryClass::Telemetry,
            100,
            MeshPayload::Telemetry(telemetry),
        )
        .unwrap();
        assert!(store
            .merge(crate::store::StoredRecord {
                record_id: "invalid".into(),
                record: invalid
            })
            .is_err());
    }
    assert!(store.scan(DeliveryClass::Telemetry).unwrap().is_empty());
    let telemetry = mesh_core::Telemetry {
        source: NodeId::from("a"),
        timestamp_ms: 100,
        latitude_deg: 51.248178375505404,
        longitude_deg: -93.31137037688033,
        altitude: mesh_core::Altitude::new(10.0, 5.0, 5.0).unwrap(),
        velocity_ned_mps: [0.0; 3],
        attitude_rpy_deg: [0.0; 3],
        battery_remaining: Some(0.9),
        control_link_quality: None,
        armed: false,
        landed: None,
        failsafe: false,
    };
    let finite = AvianRecord::new(
        NodeId::from("a"),
        1,
        DeliveryClass::Telemetry,
        100,
        MeshPayload::Telemetry(telemetry),
    )
    .unwrap();
    store
        .merge(crate::store::StoredRecord {
            record_id: "finite".into(),
            record: finite.clone(),
        })
        .unwrap();
    assert_eq!(
        store
            .get(&crate::store::record_key(
                DeliveryClass::Telemetry,
                "finite"
            ))
            .unwrap()
            .unwrap()
            .record,
        finite
    );
    drop(store);
    let reopened = crate::store::Store::open(dir.path(), "f", "id").unwrap();
    assert_eq!(
        reopened.scan(DeliveryClass::Telemetry).unwrap(),
        vec![("finite".into(), finite)]
    );
}
