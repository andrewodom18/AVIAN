use std::path::{Path, PathBuf};

use anyhow::Context;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{decode_request, encode_response, ControlRequest, ControlResponse};

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
    Ok(tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("Control socket accept failed: {error}");
                    continue;
                }
            };
            let request_sender = sender.clone();
            tokio::spawn(async move {
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
    (&mut stream)
        .take(max_message_bytes.saturating_add(1) as u64)
        .read_to_end(&mut encoded)
        .await
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
                response.await.context("waiting for control response")?
            }
            Err(error) => ControlResponse::Error {
                code: "invalid_request".into(),
                detail: error.to_string(),
            },
        }
    };
    stream
        .write_all(&encode_response(response)?)
        .await
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
    let mut stream = UnixStream::connect(path)
        .await
        .with_context(|| format!("connecting to {}", path.display()))?;
    let encoded = crate::protocol::encode_request(request)?;
    anyhow::ensure!(
        encoded.len() <= max_message_bytes,
        "request exceeds local message limit"
    );
    stream.write_all(&encoded).await?;
    stream.shutdown().await?;
    let mut response = Vec::new();
    (&mut stream)
        .take(max_message_bytes.saturating_add(1) as u64)
        .read_to_end(&mut response)
        .await?;
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
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
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
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.sock");
        let (sender, mut receiver) = mpsc::channel(1);
        let task = spawn_control_server(path.clone(), 4096, sender)
            .await
            .unwrap();
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
}
