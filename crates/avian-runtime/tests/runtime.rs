use avian_runtime::{
    RuntimeConfiguration, RuntimePhase, RuntimeSupervisor, verify_engine_directory,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use tempfile::TempDir;

fn configuration(root: &Path) -> RuntimeConfiguration {
    RuntimeConfiguration {
        execution: Default::default(),
        network_helper_socket: None,
        interface_name: "avian0".into(),
        overlay_prefix_length: 24,
        bootstrap: Default::default(),
        measured_routing: false,
        private_rpc: false,
        underlay_binding: false,
        connection_budget: 1024,
        discovery_hint_budget: 4096,
        automatic_capacity_probes: true,
        probe_payload_budget_bytes: 512 * 1024,
        probe_window_ms: 30_000,
        isolation_required: true,
        namespace_name: "avian-missing-test-namespace".into(),
        engine_directory: root.join("engine"),
        runtime_directory: root.join("runtime"),
        membership_file: root.join("membership.json"),
        trust_root_file: root.join("root.json"),
        credential_file: root.join("credential"),
        node_id: "a".repeat(64),
        overlay_address: "10.201.0.2".parse().unwrap(),
        listen: "172.30.0.2:11010".parse().unwrap(),
        rpc_listen: "127.0.0.1:15888".parse().unwrap(),
        peers: vec!["172.30.0.3:11010".parse().unwrap()],
        metered_addresses: vec![],
        control_tcp_ports: vec![],
        allow_metered_video: false,
        allow_metered_bulk: false,
        fast_failover: false,
        priority_forwarding: false,
    }
}

#[test]
fn unsafe_configuration_is_rejected_before_any_process_can_start() {
    let directory = TempDir::new().unwrap();
    let valid = configuration(directory.path());
    valid.validate().unwrap();
    let mut bad = valid.clone();
    bad.isolation_required = false;
    assert!(bad.validate().is_err());
    bad = valid.clone();
    bad.namespace_name = "../host".into();
    assert!(bad.validate().is_err());
    bad = valid.clone();
    bad.rpc_listen = "0.0.0.0:15888".parse().unwrap();
    assert!(bad.validate().is_err());
    bad = valid.clone();
    bad.listen = "0.0.0.0:11010".parse().unwrap();
    assert!(bad.validate().is_err());
    bad = valid.clone();
    bad.peers.push("8.8.8.8:11010".parse().unwrap());
    assert!(bad.validate().is_err());
    bad = valid.clone();
    bad.control_tcp_ports.push(0);
    assert!(bad.validate().is_err());
    bad = valid;
    bad.overlay_address = "192.0.2.2".parse().unwrap();
    assert!(bad.validate().is_err());
}

#[tokio::test]
async fn missing_namespace_or_unsupported_platform_cannot_start_a_runtime() {
    let directory = TempDir::new().unwrap();
    let supervisor = RuntimeSupervisor::new(configuration(directory.path()));
    assert!(supervisor.start().await.is_err());
    let status = supervisor.status().await;
    assert_eq!(status.state, RuntimePhase::Failed);
    assert!(status.process_id.is_none());
    assert!(!status.isolated);
    assert!(!status.engine_ready);
    assert!(supervisor.inspect_routes().await.is_err());
    assert_eq!(
        supervisor.stop().await.unwrap().state,
        RuntimePhase::Stopped
    );
    assert_eq!(
        supervisor.stop().await.unwrap().state,
        RuntimePhase::Stopped
    );
    assert!(!directory.path().join("runtime").exists());
}

fn write_artifacts(directory: &Path) -> serde_json::Value {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/build.json")).unwrap();
    let mut manifest = reference;
    let mut hashes = serde_json::Map::new();
    for name in ["easytier-core", "easytier-cli"] {
        let content = format!("test artifact for {name}");
        fs::write(directory.join(name), content.as_bytes()).unwrap();
        hashes.insert(
            name.into(),
            json!(hex::encode(Sha256::digest(content.as_bytes()))),
        );
    }
    manifest["binary_sha256"] = hashes.into();
    fs::write(
        directory.join("build.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    manifest
}

#[test]
fn binary_and_patch_tampering_cannot_pass_provenance_checks() {
    let directory = TempDir::new().unwrap();
    let mut manifest = write_artifacts(directory.path());
    verify_engine_directory(directory.path()).unwrap();
    fs::write(directory.path().join("easytier-core"), b"altered binary").unwrap();
    assert!(verify_engine_directory(directory.path()).is_err());
    write_artifacts(directory.path());
    manifest["patches"][0]["sha256"] = json!("0".repeat(64));
    fs::write(
        directory.path().join("build.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert!(verify_engine_directory(directory.path()).is_err());
    manifest = write_artifacts(directory.path());
    manifest["source_commit"] = json!("0".repeat(40));
    fs::write(
        directory.path().join("build.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    assert!(verify_engine_directory(directory.path()).is_err());
}

#[cfg(unix)]
#[test]
fn writable_or_symbolically_linked_artifacts_are_rejected() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = TempDir::new().unwrap();
    write_artifacts(directory.path());
    fs::set_permissions(
        directory.path().join("easytier-cli"),
        fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    assert!(verify_engine_directory(directory.path()).is_err());
    fs::remove_file(directory.path().join("easytier-cli")).unwrap();
    symlink(
        directory.path().join("easytier-core"),
        directory.path().join("easytier-cli"),
    )
    .unwrap();
    assert!(verify_engine_directory(directory.path()).is_err());
}

#[test]
fn optional_priority_build_requires_the_exact_reviewed_patch_and_feature() {
    let directory = TempDir::new().unwrap();
    let mut manifest = write_artifacts(directory.path());
    manifest["patches"].as_array_mut().unwrap().push(json!({
        "name": "congestion.patch",
        "sha256": hex::encode(Sha256::digest(include_bytes!("../../../qualification/patches/congestion.patch")))
    }));
    let save = |manifest: &serde_json::Value| {
        fs::write(
            directory.path().join("build.json"),
            serde_json::to_vec(manifest).unwrap(),
        )
        .unwrap();
    };
    save(&manifest);
    assert!(verify_engine_directory(directory.path()).is_err());
    manifest["priority_forwarding"] = json!(true);
    save(&manifest);
    verify_engine_directory(directory.path()).unwrap();
    manifest["patches"][6]["sha256"] = json!("0".repeat(64));
    save(&manifest);
    assert!(verify_engine_directory(directory.path()).is_err());
    manifest["patches"].as_array_mut().unwrap().pop();
    save(&manifest);
    assert!(verify_engine_directory(directory.path()).is_err());
}

#[test]
fn native_scope_requires_helper_and_an_entirely_private_network() {
    let directory = TempDir::new().unwrap();
    let mut native = configuration(directory.path());
    native.execution = avian_runtime::ExecutionMode::Native;
    native.isolation_required = false;
    native.namespace_name.clear();
    native.measured_routing = true;
    native.underlay_binding = true;
    native.priority_forwarding = true;
    native.private_rpc = true;
    assert!(native.validate().is_err());
    native.network_helper_socket = Some(directory.path().join("network.sock"));
    if cfg!(target_os = "linux") {
        native.validate().unwrap();
    }
    native.overlay_address = "192.168.1.2".parse().unwrap();
    native.overlay_prefix_length = 8;
    assert!(native.validate().is_err());
    native.overlay_prefix_length = 24;
    native.overlay_address = "192.168.1.0".parse().unwrap();
    assert!(native.validate().is_err());
    native.overlay_address = "192.168.1.255".parse().unwrap();
    assert!(native.validate().is_err());
    native.overlay_address = "192.168.1.2".parse().unwrap();
    native.network_helper_socket = Some("relative.sock".into());
    assert!(native.validate().is_err());
}
