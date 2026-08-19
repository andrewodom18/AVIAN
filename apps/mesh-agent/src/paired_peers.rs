use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::config::TaggedPeer;

const PAIRED_PEER_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairedPeerState {
    schema_version: u16,
    peers: Vec<TaggedPeer>,
}

pub fn load(path: &Path) -> anyhow::Result<Vec<TaggedPeer>> {
    match std::fs::read(path) {
        Ok(encoded) => {
            let state: PairedPeerState = serde_json::from_slice(&encoded)
                .with_context(|| format!("decoding paired peers {}", path.display()))?;
            anyhow::ensure!(
                state.schema_version == PAIRED_PEER_SCHEMA_VERSION,
                "unsupported paired-peer schema {}",
                state.schema_version
            );
            Ok(state.peers)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => {
            Err(error).with_context(|| format!("reading paired peers {}", path.display()))
        }
    }
}

pub fn persist(path: &Path, peers: &[TaggedPeer]) -> anyhow::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("creating paired-peer directory {}", parent.display()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating paired-peer state in {}", parent.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let state = PairedPeerState {
        schema_version: PAIRED_PEER_SCHEMA_VERSION,
        peers: peers.to_vec(),
    };
    temporary.write_all(&serde_json::to_vec(&state)?)?;
    temporary.as_file_mut().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("atomically replacing paired peers {}", path.display()))?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;
    use crate::config::{TaggedAddress, Underlay};

    fn peer() -> TaggedPeer {
        TaggedPeer {
            name: "aircraft-001".into(),
            endpoint_id: "a".repeat(64),
            addresses: vec![TaggedAddress {
                underlay: Underlay::Ethernet,
                address: "192.0.2.4:9000".parse::<SocketAddr>().unwrap(),
            }],
        }
    }

    #[test]
    fn paired_peers_round_trip_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paired-peers.json");
        persist(&path, &[peer()]).unwrap();
        assert_eq!(load(&path).unwrap(), vec![peer()]);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn unknown_or_future_state_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("paired-peers.json");
        std::fs::write(&path, br#"{"schema_version":2,"peers":[]}"#).unwrap();
        assert!(load(&path).is_err());
        std::fs::write(
            &path,
            br#"{"schema_version":1,"peers":[],"unexpected":true}"#,
        )
        .unwrap();
        assert!(load(&path).is_err());
    }
}
