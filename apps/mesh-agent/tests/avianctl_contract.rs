#![cfg(unix)]

// These tests talk only to a temporary fake Unix socket, never an agent or radio.
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use mesh_agent::config::{CommandMode, ConfiguredNodeRole, Underlay};
use mesh_agent::protocol::{decode_request, encode_response, ControlRequest, ControlResponse};
use mesh_agent::status::AgentStatus;

fn exchange(args: &[&str], response: ControlResponse) -> (Output, ControlRequest) {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "CLI did not connect to fixture");
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("fixture accept: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        let request = decode_request(&bytes).unwrap();
        stream
            .write_all(&encode_response(response).unwrap())
            .unwrap();
        request
    });
    let output = Command::new(env!("CARGO_BIN_EXE_avianctl"))
        .arg("--socket")
        .arg(socket)
        .args(args)
        .output()
        .unwrap();
    (output, server.join().unwrap())
}

fn status(ready: bool) -> ControlResponse {
    let mut status = AgentStatus::new(
        "fixture".into(),
        ConfiguredNodeRole::Ground,
        1,
        CommandMode::DryRun,
        false,
        false,
        10_000,
    );
    status.ready = ready;
    ControlResponse::Status {
        status: Box::new(status),
    }
}

#[test]
fn status_output_and_readiness_exit_status_follow_the_response() {
    let (output, request) = exchange(&["status", "--json", "--require-ready"], status(true));
    assert!(matches!(
        request,
        ControlRequest::Status {
            require_ready: true
        }
    ));
    assert!(output.status.success());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["node"]["name"], "fixture");
    assert_eq!(body["ready"], true);
    let (output, _) = exchange(&["status", "--require-ready"], status(false));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("not ready"));
    let (output, _) = exchange(&["status"], status(false));
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("degraded"));
    let (output, _) = exchange(&["status"], status(true));
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("ready"));
}

#[test]
fn records_preserve_class_limit_and_json_shape() {
    for (name, expected) in [
        ("emergency", mesh_core::DeliveryClass::Emergency),
        ("acknowledgement", mesh_core::DeliveryClass::Acknowledgement),
        ("mission", mesh_core::DeliveryClass::Mission),
        ("telemetry", mesh_core::DeliveryClass::Telemetry),
        ("bulk", mesh_core::DeliveryClass::Bulk),
    ] {
        let (output, request) = exchange(
            &["records", "--class", name, "--limit", "7"],
            ControlResponse::Records { records: vec![] },
        );
        assert!(output.status.success());
        match request {
            ControlRequest::ListRecords { class, limit } => {
                assert_eq!(class, expected);
                assert_eq!(limit, 7);
            }
            other => panic!("unexpected request {other:?}"),
        }
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            serde_json::json!([])
        );
    }
    for limit in ["0", "501"] {
        let output = Command::new(env!("CARGO_BIN_EXE_avianctl"))
            .args(["records", "--class", "mission", "--limit", limit])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("1-500"));
    }
}

#[test]
fn connection_code_round_trips_only_public_identity_and_addresses() {
    let addresses = vec![mesh_agent::protocol::PeerConnectionAddress {
        underlay: Underlay::Ethernet,
        address: "192.0.2.4:9000".parse().unwrap(),
    }];
    let (output, request) = exchange(
        &["connection-code", "--address", "ethernet=192.0.2.4:9000"],
        ControlResponse::ConnectionInfo {
            formation_id: "test-formation".into(),
            name: "aircraft".into(),
            endpoint_id: "11".repeat(32),
            addresses: addresses.clone(),
        },
    );
    assert!(output.status.success());
    assert!(
        matches!(request, ControlRequest::ConnectionInfo { addresses: actual } if actual == addresses)
    );
    let encoded = String::from_utf8(output.stdout).unwrap();
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded.trim().strip_prefix("AVIAN1.").unwrap())
        .unwrap();
    let decoded: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded["schema_version"], 1);
    assert_eq!(decoded["formation_id"], "test-formation");
    assert_eq!(decoded["aircraft"]["name"], "aircraft");
    assert_eq!(
        decoded["aircraft"]["addresses"][0]["address"],
        "192.0.2.4:9000"
    );
    assert!(!String::from_utf8(bytes).unwrap().contains("private_key"));
}

#[test]
fn control_errors_and_unexpected_responses_fail_closed() {
    let denied = || ControlResponse::Error {
        code: "not_authorized".into(),
        detail: "fixture denial".into(),
    };
    for args in [
        vec!["status"],
        vec!["records", "--class", "mission"],
        vec!["emergency", "rtl", "--target", "fixture"],
        vec!["connection-code", "--address", "wifi=192.0.2.5:9000"],
    ] {
        let (output, _) = exchange(&args, denied());
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("not_authorized"));
    }
    let (output, _) = exchange(&["status"], ControlResponse::Records { records: vec![] });
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected response"));
    // The acknowledgement is fabricated by our socket fixture, not a flight controller.
    let (output, request) = exchange(
        &["emergency", "rtl", "--target", "fixture"],
        ControlResponse::CommandIssued {
            command_id: "fixture-command".into(),
        },
    );
    assert!(matches!(request, ControlRequest::EmergencyRtl { target } if target == "fixture"));
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        "fixture-command"
    );
}

#[test]
fn key_generation_preserves_existing_files_and_restricts_private_permissions() {
    let directory = tempfile::tempdir().unwrap();
    let private = directory.path().join("keys/private");
    let public = directory.path().join("keys/public");
    let generate = |private: &std::path::Path, public: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_avianctl"))
            .args(["keys", "generate", "--private-key"])
            .arg(private)
            .arg("--public-key")
            .arg(public)
            .output()
            .unwrap()
    };
    assert!(generate(&private, &public).status.success());
    assert_eq!(
        std::fs::metadata(&private).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let original = std::fs::read(&private).unwrap();
    assert!(!generate(&private, &public).status.success());
    assert_eq!(std::fs::read(&private).unwrap(), original);
    let rolled_back = directory.path().join("rolled-back-private");
    assert!(!generate(&rolled_back, &public).status.success());
    assert!(!rolled_back.exists());
    assert!(!generate(&public, &public).status.success());
}
