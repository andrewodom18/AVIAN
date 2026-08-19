use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio::time::timeout;

use crate::protocol::{decode_request, encode_response, ControlRequest, ControlResponse};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONTROL_CONNECTIONS: usize = 32;

pub struct ControlEnvelope {
    pub request: ControlRequest,
    pub respond_to: oneshot::Sender<ControlResponse>,
}

pub async fn spawn_control_server(
    path: PathBuf,
    max_message_bytes: usize,
    sender: mpsc::Sender<ControlEnvelope>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    prepare_socket_path(&path)?;
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("binding control socket {}", path.display()))?;
    set_socket_permissions(&path)?;
    let permits = Arc::new(Semaphore::new(MAX_CONTROL_CONNECTIONS));
    Ok(tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("Control socket accept failed: {error}");
                    continue;
                }
            };
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                drop(stream);
                continue;
            };
            let request_sender = sender.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(error) =
                    handle_connection(stream, max_message_bytes, request_sender).await
                {
                    eprintln!("Control request failed: {error}");
                }
            });
        }
    }))
}

async fn handle_connection(
    mut stream: UnixStream,
    max_message_bytes: usize,
    sender: mpsc::Sender<ControlEnvelope>,
) -> anyhow::Result<()> {
    let mut encoded = Vec::new();
    timeout(
        CONTROL_TIMEOUT,
        (&mut stream)
            .take(max_message_bytes.saturating_add(1) as u64)
            .read_to_end(&mut encoded),
    )
    .await
    .context("control request read timed out")?
    .context("reading control request")?;
    let response = if encoded.len() > max_message_bytes {
        ControlResponse::Error {
            code: "message_too_large".into(),
            detail: format!("control message exceeds {max_message_bytes} bytes"),
        }
    } else {
        match decode_request(&encoded) {
            Ok(request) => {
                let (respond_to, response) = oneshot::channel();
                sender
                    .send(ControlEnvelope {
                        request,
                        respond_to,
                    })
                    .await
                    .context("dispatching control request")?;
                timeout(CONTROL_TIMEOUT, response)
                    .await
                    .context("control response timed out")?
                    .context("waiting for control response")?
            }
            Err(error) => ControlResponse::Error {
                code: "invalid_request".into(),
                detail: error.to_string(),
            },
        }
    };
    let mut encoded_response = encode_response(response)?;
    if encoded_response.len() > max_message_bytes {
        encoded_response = encode_response(ControlResponse::Error {
            code: "response_too_large".into(),
            detail: format!("control response exceeds {max_message_bytes} bytes"),
        })?;
    }
    anyhow::ensure!(
        encoded_response.len() <= max_message_bytes,
        "configured control message limit is too small for an error response"
    );
    timeout(CONTROL_TIMEOUT, stream.write_all(&encoded_response))
        .await
        .context("control response write timed out")?
        .context("writing control response")?;
    stream
        .shutdown()
        .await
        .context("closing control response")?;
    Ok(())
}

pub async fn request(
    path: &Path,
    max_message_bytes: usize,
    request: ControlRequest,
) -> anyhow::Result<ControlResponse> {
    let mut stream = timeout(CONTROL_TIMEOUT, UnixStream::connect(path))
        .await
        .context("control connection timed out")?
        .with_context(|| format!("connecting to {}", path.display()))?;
    let encoded = crate::protocol::encode_request(request)?;
    anyhow::ensure!(
        encoded.len() <= max_message_bytes,
        "request exceeds local message limit"
    );
    timeout(CONTROL_TIMEOUT, stream.write_all(&encoded))
        .await
        .context("control request write timed out")??;
    stream.shutdown().await?;
    let mut response = Vec::new();
    timeout(
        CONTROL_TIMEOUT,
        (&mut stream)
            .take(max_message_bytes.saturating_add(1) as u64)
            .read_to_end(&mut response),
    )
    .await
    .context("control response read timed out")??;
    anyhow::ensure!(
        response.len() <= max_message_bytes,
        "response exceeds local message limit"
    );
    crate::protocol::decode_response(&response)
}

fn prepare_socket_path(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating socket directory {}", parent.display()))?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)
            .with_context(|| format!("removing stale socket {}", path.display())),
        Ok(_) => anyhow::bail!("refusing to replace non-socket path {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspecting {}", path.display())),
    }
}

#[cfg(unix)]
fn set_socket_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("setting socket permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_socket_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ControlResponse;

    #[tokio::test]
    async fn round_trips_a_bounded_request() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.sock");
        let (sender, mut receiver) = mpsc::channel(1);
        let task = spawn_control_server(path.clone(), 4096, sender)
            .await
            .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let responder = tokio::spawn(async move {
            let envelope = receiver.recv().await.unwrap();
            envelope
                .respond_to
                .send(ControlResponse::Records {
                    records: Vec::new(),
                })
                .unwrap();
        });
        let response = request(
            &path,
            4096,
            ControlRequest::ListRecords {
                class: mesh_core::DeliveryClass::Mission,
                limit: 10,
            },
        )
        .await
        .unwrap();
        assert!(matches!(response, ControlResponse::Records { .. }));
        responder.await.unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn oversized_response_is_replaced_with_bounded_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.sock");
        let (sender, mut receiver) = mpsc::channel(1);
        let task = spawn_control_server(path.clone(), 256, sender)
            .await
            .unwrap();
        let responder = tokio::spawn(async move {
            let envelope = receiver.recv().await.unwrap();
            envelope
                .respond_to
                .send(ControlResponse::Error {
                    code: "large".into(),
                    detail: "x".repeat(1_024),
                })
                .unwrap();
        });
        let response = request(
            &path,
            256,
            ControlRequest::Status {
                require_ready: false,
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            response,
            ControlResponse::Error { code, .. } if code == "response_too_large"
        ));
        responder.await.unwrap();
        task.abort();
    }
}
