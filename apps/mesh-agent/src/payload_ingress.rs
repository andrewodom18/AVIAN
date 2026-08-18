use std::path::Path;

use anyhow::Context;
use mesh_core::{Detection, ImageManifest};
use serde::{Deserialize, Serialize};
use tokio::net::UnixDatagram;

pub const PAYLOAD_PROTOCOL_VERSION: u16 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadEnvelope {
    pub protocol_version: u16,
    pub event: PayloadEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PayloadEvent {
    ImageManifest { manifest: ImageManifest },
    Detection { detection: Detection },
}

impl PayloadEvent {
    pub fn validate(&self) -> anyhow::Result<()> {
        match self {
            Self::ImageManifest { manifest } => manifest.validate()?,
            Self::Detection { detection } => detection.validate()?,
        }
        Ok(())
    }
}

pub fn decode(encoded: &[u8], max_message_bytes: usize) -> anyhow::Result<PayloadEvent> {
    anyhow::ensure!(
        encoded.len() <= max_message_bytes,
        "payload event exceeds {max_message_bytes} bytes"
    );
    let envelope: PayloadEnvelope = serde_json::from_slice(encoded)?;
    anyhow::ensure!(
        envelope.protocol_version == PAYLOAD_PROTOCOL_VERSION,
        "unsupported payload protocol version {}",
        envelope.protocol_version
    );
    envelope.event.validate()?;
    Ok(envelope.event)
}

pub fn bind(path: &Path) -> anyhow::Result<UnixDatagram> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating payload socket directory {}", parent.display()))?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => std::fs::remove_file(path)
            .with_context(|| format!("removing stale payload socket {}", path.display()))?,
        Ok(_) => anyhow::bail!("refusing to replace non-socket path {}", path.display()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspecting {}", path.display()));
        }
    }
    let socket = UnixDatagram::bind(path)
        .with_context(|| format!("binding payload socket {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
        .with_context(|| format!("setting payload socket permissions on {}", path.display()))?;
    Ok(socket)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_sender_supplied_source_identity() {
        let encoded = br#"{
            "protocol_version":1,
            "event":{
                "type":"detection",
                "source":"untrusted",
                "detection":{
                    "schema_version":2,
                    "detection_id":"00000000-0000-0000-0000-000000000000",
                    "observed_at_ms":1,
                    "image_id":null,
                    "label":"vehicle",
                    "confidence":0.5,
                    "position":null
                }
            }
        }"#;
        assert!(decode(encoded, 4096).is_err());
    }
}
