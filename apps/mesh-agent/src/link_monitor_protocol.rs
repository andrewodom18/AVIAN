use std::path::Path;

use anyhow::Context;
use mesh_core::LinkMonitorObservation;
use serde::{Deserialize, Serialize};
use tokio::net::UnixDatagram;

pub const LINK_MONITOR_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkMonitorEnvelope {
    pub protocol_version: u16,
    pub observation: LinkMonitorObservation,
}

pub fn encode(observation: LinkMonitorObservation) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(observation.validate(), "invalid link-monitor observation");
    Ok(serde_json::to_vec(&LinkMonitorEnvelope {
        protocol_version: LINK_MONITOR_PROTOCOL_VERSION,
        observation,
    })?)
}

pub fn decode(encoded: &[u8], max_message_bytes: usize) -> anyhow::Result<LinkMonitorObservation> {
    anyhow::ensure!(
        encoded.len() <= max_message_bytes,
        "link-monitor observation exceeds {max_message_bytes} bytes"
    );
    let envelope: LinkMonitorEnvelope = serde_json::from_slice(encoded)?;
    anyhow::ensure!(
        envelope.protocol_version == LINK_MONITOR_PROTOCOL_VERSION,
        "unsupported link-monitor protocol version {}",
        envelope.protocol_version
    );
    anyhow::ensure!(
        envelope.observation.validate(),
        "invalid link-monitor observation"
    );
    Ok(envelope.observation)
}

pub fn bind(path: &Path) -> anyhow::Result<UnixDatagram> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating link socket directory {}", parent.display()))?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)
            .with_context(|| format!("removing stale link socket {}", path.display()))?,
        Ok(_) => anyhow::bail!("refusing to replace non-socket path {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("inspecting {}", path.display())),
    }
    let socket = UnixDatagram::bind(path)
        .with_context(|| format!("binding link socket {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
        .with_context(|| format!("setting link socket permissions on {}", path.display()))?;
    Ok(socket)
}

pub async fn send(path: &Path, observation: LinkMonitorObservation) -> anyhow::Result<()> {
    let encoded = encode(observation)?;
    let socket = UnixDatagram::unbound()?;
    socket
        .send_to(&encoded, path)
        .await
        .with_context(|| format!("sending link observation to {}", path.display()))?;
    Ok(())
}
