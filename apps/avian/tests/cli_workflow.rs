use avian::{configuration::Configuration, state::ApplicationState};
use serde_json::Value;
use std::{
    ffi::OsStr,
    fs,
    path::Path,
    process::{Command, Output},
};

fn run(arguments: &[&OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_avianctl"))
        .args(arguments)
        .output()
        .expect("run local command")
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("command returns JSON")
}

fn initialize(directory: &Path) -> Value {
    success(run(&[
        "init".as_ref(),
        "--state-dir".as_ref(),
        directory.as_os_str(),
        "--network".as_ref(),
        "field-test".as_ref(),
        "--listen".as_ref(),
        "127.0.0.1:0".as_ref(),
    ]))
}

#[tokio::test]
async fn enrollment_and_revocation_survive_independent_command_and_service_loads() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("network");
    let initialized = initialize(&directory);
    assert_eq!(initialized["network_control_enabled"], false);
    let configuration_path = directory.join("avian.toml");
    let identity_file = temporary.path().join("device.key");
    let generated = success(run(&[
        "keygen".as_ref(),
        "--output".as_ref(),
        identity_file.as_os_str(),
    ]));
    let identity = generated["identity_public_key"].as_str().unwrap();
    let transport = "12".repeat(32);
    let exported = temporary.path().join("device-membership.json");
    let enrolled = success(run(&[
        "--config".as_ref(),
        configuration_path.as_os_str(),
        "enroll".as_ref(),
        "--identity-public-key".as_ref(),
        identity.as_ref(),
        "--transport-public-key".as_ref(),
        transport.as_ref(),
        "--address".as_ref(),
        "fd42::42".as_ref(),
        "--relay".as_ref(),
        "--output".as_ref(),
        exported.as_os_str(),
    ]));
    assert_eq!(enrolled["engine_admission_applied"], false);
    let node_id = enrolled["node_id"].as_str().unwrap();
    let members = success(run(&[
        "--config".as_ref(),
        configuration_path.as_os_str(),
        "members".as_ref(),
    ]));
    assert_eq!(members.as_array().unwrap().len(), 1);
    assert_eq!(members[0]["claims"]["node_id"], node_id);
    let configuration = Configuration::load(&configuration_path).unwrap();
    let before = ApplicationState::load(&configuration).unwrap();
    assert!(
        before
            .membership
            .read()
            .await
            .authorize(node_id, &transport)
            .is_ok()
    );
    drop(before);
    let revocations = temporary.path().join("revocations.json");
    let revoked = success(run(&[
        "--config".as_ref(),
        configuration_path.as_os_str(),
        "revoke".as_ref(),
        "--node".as_ref(),
        node_id.as_ref(),
        "--output".as_ref(),
        revocations.as_os_str(),
    ]));
    assert_eq!(revoked["revocation_revision"], 1);
    assert_eq!(revoked["engine_admission_applied"], false);
    let after = ApplicationState::load(&configuration).unwrap();
    assert!(
        after
            .membership
            .read()
            .await
            .authorize(node_id, &transport)
            .is_err()
    );
    let members = success(run(&[
        "--config".as_ref(),
        configuration_path.as_os_str(),
        "members".as_ref(),
    ]));
    assert_eq!(members, serde_json::json!([]));
    let replay = run(&[
        "--config".as_ref(),
        configuration_path.as_os_str(),
        "import".as_ref(),
        "--membership".as_ref(),
        exported.as_os_str(),
    ]);
    assert!(!replay.status.success());
    assert!(String::from_utf8_lossy(&replay.stderr).contains("revoked"));
}

#[test]
fn initialization_and_key_generation_never_overwrite_existing_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("network");
    initialize(&directory);
    let administrator_before = fs::read(directory.join("administrator.key")).unwrap();
    let token_before = fs::read(directory.join("api-token")).unwrap();
    let repeated = run(&[
        "init".as_ref(),
        "--state-dir".as_ref(),
        directory.as_os_str(),
        "--network".as_ref(),
        "replacement".as_ref(),
    ]);
    assert!(!repeated.status.success());
    assert_eq!(
        fs::read(directory.join("administrator.key")).unwrap(),
        administrator_before
    );
    assert_eq!(fs::read(directory.join("api-token")).unwrap(), token_before);
    let key_path = temporary.path().join("device.key");
    success(run(&[
        "keygen".as_ref(),
        "--output".as_ref(),
        key_path.as_os_str(),
    ]));
    let before = fs::read(&key_path).unwrap();
    let repeated = run(&["keygen".as_ref(), "--output".as_ref(), key_path.as_os_str()]);
    assert!(!repeated.status.success());
    assert_eq!(fs::read(&key_path).unwrap(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o077,
            0
        );
        assert_eq!(
            fs::metadata(key_path).unwrap().permissions().mode() & 0o077,
            0
        );
    }
}

#[test]
fn public_listener_request_is_rejected_before_creating_state() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("network");
    let output = run(&[
        "init".as_ref(),
        "--state-dir".as_ref(),
        directory.as_os_str(),
        "--network".as_ref(),
        "field-test".as_ref(),
        "--listen".as_ref(),
        "0.0.0.0:4179".as_ref(),
    ]);
    assert!(!output.status.success());
    assert!(!directory.exists());
}
