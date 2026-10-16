#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use base64::{engine::general_purpose::STANDARD, Engine};
use mesh_agent::config::Underlay;
use mesh_agent::control::request;
use mesh_agent::protocol::{ControlRequest, ControlResponse, PeerConnectionAddress};

struct AgentProcess {
    child: Child,
    errors: PathBuf,
}

impl AgentProcess {
    fn start(config: &Path) -> Self {
        let errors = config.with_extension("stderr");
        let child = Command::new(env!("CARGO_BIN_EXE_mesh-agent"))
            .arg("--config")
            .arg(config)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&errors).unwrap())
            .spawn()
            .unwrap();
        Self { child, errors }
    }

    async fn wait_ready(&mut self, socket: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(exit) = self.child.try_wait().unwrap() {
                panic!(
                    "agent exited {exit}: {}",
                    std::fs::read_to_string(&self.errors).unwrap()
                );
            }
            if socket.exists() {
                if let Ok(ControlResponse::Status { status }) = request(
                    socket,
                    65_536,
                    ControlRequest::Status {
                        require_ready: false,
                    },
                )
                .await
                {
                    return status.node.endpoint_id.unwrap();
                }
            }
            assert!(Instant::now() < deadline, "agent did not become available");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn terminate(&mut self) {
        assert!(Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status()
            .unwrap()
            .success());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(exit) = self.child.try_wait().unwrap() {
                assert!(
                    exit.success(),
                    "SIGTERM must run graceful cleanup: {exit}: {}",
                    std::fs::read_to_string(&self.errors).unwrap()
                );
                return;
            }
            assert!(
                Instant::now() < deadline,
                "agent did not stop: {}",
                std::fs::read_to_string(&self.errors).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_progresses_during_pairing_and_sigterm_releases_state_for_restart() {
    // Keep Unix socket names below the platform limit, including on macOS.
    let directory = tempfile::Builder::new()
        .prefix("avian-runtime-")
        .tempdir_in("/tmp")
        .unwrap();
    let secret = STANDARD.encode([73_u8; 32]);
    let key = directory.path().join("formation.key");
    std::fs::write(&key, &secret).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = directory.path().join("avian.toml");
    std::fs::write(
        &config,
        r#"
schema_version = 1
[node]
name = "runtime-ground"
role = "ground"
[mesh]
formation_id = "runtime-test"
formation_key_file = "formation.key"
storage = "state"
bind = "127.0.0.1:0"
[sockets]
control = "control.sock"
payload = "payload.sock"
link_observation = "links.sock"
"#,
    )
    .unwrap();
    let socket = directory.path().join("control.sock");
    let mut agent = AgentProcess::start(&config);
    let endpoint = agent.wait_ready(&socket).await;

    let peer_id = mesh_transport::derive_endpoint_id(&secret, "unreachable-aircraft").unwrap();
    let pairing_socket = socket.clone();
    let pairing = tokio::spawn(async move {
        request(
            &pairing_socket,
            65_536,
            ControlRequest::ConfigurePeer {
                formation_id: "runtime-test".into(),
                name: "unreachable-aircraft".into(),
                endpoint_id: peer_id,
                // Documentation-only address: no flight controller or radio needed.
                addresses: vec![PeerConnectionAddress {
                    underlay: Underlay::Ethernet,
                    address: "192.0.2.1:9000".parse().unwrap(),
                }],
            },
        )
        .await
    });
    // The durable file is created before the connection attempt. Waiting for
    // it proves that the worker is handling pairing, rather than only queued.
    let paired = directory.path().join("state/paired-peers.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !paired.exists() {
        assert!(
            Instant::now() < deadline,
            "pairing did not reach durable activation"
        );
        assert!(
            !pairing.is_finished(),
            "pairing should wait for its unreachable peer"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let response = tokio::time::timeout(
        Duration::from_millis(500),
        request(
            &socket,
            65_536,
            ControlRequest::Status {
                require_ready: false,
            },
        ),
    )
    .await
    .expect("status stalled behind the connection attempt")
    .unwrap();
    assert!(matches!(response, ControlResponse::Status { .. }));
    assert!(
        !pairing.is_finished(),
        "status must complete while pairing is still in flight"
    );

    // Terminate during work; the owner must finish its durable transaction and
    // release transport/storage before successful process exit.
    agent.terminate().await;
    let _ = pairing.await;
    assert!(!socket.exists(), "SIGTERM must remove owned sockets");
    let mut restarted = AgentProcess::start(&config);
    assert_eq!(restarted.wait_ready(&socket).await, endpoint);
    restarted.terminate().await;
}
