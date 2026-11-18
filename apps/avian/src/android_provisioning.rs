//! Android enrollment accepts decrypted credentials only within the local native call.
use crate::provisioning::{DeviceBundle, DeviceRequest, lock_device};
use anyhow::{Context, Result, bail, ensure};
use avian_membership::{MembershipStore, TrustRoot, node_id_for_key};
use base64::{Engine, engine::general_purpose::STANDARD};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
};
use zeroize::Zeroizing;

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ExchangePeer {
    node_id: String,
    service_endpoint: SocketAddr,
    revocation_endpoint: SocketAddr,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeProfile {
    overlay_address: Ipv4Addr,
    overlay_network: String,
    peers: Vec<String>,
    #[serde(default)]
    control_tcp_ports: Vec<u16>,
    #[serde(default = "exchange_port")]
    exchange_port: u16,
    #[serde(default)]
    exchange_peers: Vec<ExchangePeer>,
    #[serde(default = "automatic_probes")]
    automatic_capacity_probes: bool,
}
fn exchange_port() -> u16 {
    4192
}
fn automatic_probes() -> bool {
    true
}

fn credentials(identity_bytes: &[u8], transport_bytes: &[u8]) -> Result<(SigningKey, [u8; 32])> {
    ensure!(
        identity_bytes.len() <= 65 && transport_bytes.len() <= 65,
        "invalid device credential size"
    );
    let identity = Zeroizing::new(hex::decode(std::str::from_utf8(identity_bytes)?.trim())?);
    let identity_seed =
        Zeroizing::new(<[u8; 32]>::try_from(identity.as_slice()).context("invalid identity seed")?);
    let transport = Zeroizing::new(STANDARD.decode(std::str::from_utf8(transport_bytes)?.trim())?);
    let transport_seed = Zeroizing::new(
        <[u8; 32]>::try_from(transport.as_slice()).context("invalid transport seed")?,
    );
    let public = *x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*transport_seed))
        .as_bytes();
    Ok((SigningKey::from_bytes(&identity_seed), public))
}

/// Request, import, and configure never open an overlay or an administration listener.
/// The host serializes this call with engine lifecycle operations and wipes JNI input buffers.
pub fn provision(
    operation: u8,
    directory: &Path,
    document: &str,
    administrator_fingerprint: &str,
    identity_bytes: &[u8],
    transport_bytes: &[u8],
) -> Result<String> {
    ensure!(
        document.len() <= 16 * 1024 * 1024,
        "provisioning document exceeds local resource budget"
    );
    avian_storage::validate_private_directory(directory)?;
    let _lock = lock_device(directory)?;
    let directory = std::fs::canonicalize(directory)?;
    let (identity, transport) = credentials(identity_bytes, transport_bytes)?;
    match operation {
        0 => Ok(serde_json::to_string_pretty(&DeviceRequest::new(
            &identity, transport,
        )?)?),
        1 => {
            let bundle: DeviceBundle = serde_json::from_str(document)?;
            let incoming = bundle.verify_for_device(
                administrator_fingerprint,
                &identity.verifying_key(),
                transport,
            )?;
            let root = &bundle.claims.trust_root;
            let root_path = directory.join("trust-root.json");
            if root_path.exists() {
                let previous: TrustRoot =
                    serde_json::from_slice(&avian_storage::read_private_file(&root_path, 16384)?)?;
                ensure!(&previous == root, "device already trusts another network");
            }
            let membership_path = directory.join("membership.json");
            if membership_path.exists() {
                incoming
                    .validate_successor(&MembershipStore::load(&membership_path, root.clone())?)?;
            }
            if !root_path.exists() {
                avian_storage::create_private_file(root_path, &serde_json::to_vec_pretty(root)?)?;
            }
            incoming.save(membership_path)?;
            Ok(serde_json::json!({"node_id":bundle.claims.node_id,"network_id":root.network_id,"network_started":false}).to_string())
        }
        2 => {
            ensure!(
                document.len() <= 65536,
                "runtime profile exceeds resource budget"
            );
            let profile: RuntimeProfile = serde_json::from_str(document)?;
            let root: TrustRoot = serde_json::from_slice(&avian_storage::read_private_file(
                directory.join("trust-root.json"),
                16384,
            )?)?;
            let membership = MembershipStore::load(directory.join("membership.json"), root)?;
            let node_id = node_id_for_key(&identity.verifying_key());
            let owner = membership.authorize(&node_id, &hex::encode(transport))?;
            ensure!(
                owner
                    .overlay_addresses
                    .contains(&IpAddr::V4(profile.overlay_address)),
                "runtime address is not assigned to this device"
            );
            let network: ipnet::Ipv4Net = profile.overlay_network.parse()?;
            ensure!(
                profile.exchange_port > 0
                    && (8..=30).contains(&network.prefix_len())
                    && network.network().is_private()
                    && network.broadcast().is_private()
                    && profile.overlay_address.is_private()
                    && network.contains(&profile.overlay_address)
                    && profile.overlay_address != network.network()
                    && profile.overlay_address != network.broadcast(),
                "invalid private overlay network"
            );
            ensure!(
                !profile.control_tcp_ports.contains(&0),
                "invalid control TCP port"
            );
            for peer in &profile.peers {
                let url = reqwest::Url::parse(peer)?;
                let address: IpAddr = url
                    .host_str()
                    .context("peer address missing")?
                    .trim_matches(['[', ']'])
                    .parse()?;
                ensure!(
                    matches!(url.scheme(), "tcp" | "udp")
                        && url.port().is_some_and(|port| port > 0)
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.fragment().is_none()
                        && !address.is_unspecified()
                        && !address.is_multicast()
                        && !address.is_loopback(),
                    "invalid underlay peer"
                );
            }
            for peer in &profile.exchange_peers {
                let member = membership
                    .memberships()
                    .find(|member| member.claims.node_id == peer.node_id)
                    .context("exchange peer is not enrolled")?;
                ensure!(
                    peer.service_endpoint.port() > 0
                        && peer.revocation_endpoint.port() > 0
                        && member
                            .claims
                            .overlay_addresses
                            .contains(&peer.service_endpoint.ip())
                        && member
                            .claims
                            .overlay_addresses
                            .contains(&peer.revocation_endpoint.ip()),
                    "exchange endpoints do not belong to their enrolled owner"
                );
            }
            let working = directory.join("runtime");
            if !working.exists() {
                avian_storage::create_private_directory(&working)?;
            }
            avian_storage::validate_private_directory(&working)?;
            let mut runtime = serde_json::to_value(&profile)?;
            let fields = runtime
                .as_object_mut()
                .context("runtime profile must be an object")?;
            fields.insert("version".into(), 1.into());
            fields.insert("node_id".into(), node_id.clone().into());
            for (field, path) in [
                ("trust_root_file", directory.join("trust-root.json")),
                ("membership_file", directory.join("membership.json")),
                ("credential_file", directory.join("transport.key")),
                ("working_directory", working),
            ] {
                fields.insert(field.into(), path.to_string_lossy().as_ref().into());
            }
            avian_storage::atomic_write_private_file(
                directory.join("runtime.json"),
                &serde_json::to_vec_pretty(&runtime)?,
            )?;
            Ok(serde_json::json!({"node_id":node_id,"runtime_configured":true,"network_started":false}).to_string())
        }
        _ => bail!("unknown provisioning operation"),
    }
}

/// Shared by initial and refreshed Android policies so physical link updates
/// retain application control traffic and authenticated metadata priority.
pub fn control_ports(
    configured: &[u16],
    exchange_port: u16,
    peers: impl Iterator<Item = (u16, u16)>,
) -> std::collections::BTreeSet<u16> {
    let mut ports = std::collections::BTreeSet::from([exchange_port]);
    ports.extend(configured.iter().copied());
    for (service_port, revocation_port) in peers {
        ports.insert(service_port);
        ports.insert(revocation_port);
    }
    ports
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provisioning::issue_bundle;
    use avian_membership::{Permissions, fingerprint, generate_signing_key};

    struct Device {
        directory: tempfile::TempDir,
        identity: Vec<u8>,
        transport: Vec<u8>,
        bundle: DeviceBundle,
        fingerprint: String,
    }
    impl Device {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            avian_storage::create_private_directory(directory.path().join("device")).unwrap();
            let identity = generate_signing_key().unwrap();
            let transport_seed = [19u8; 32];
            let transport_public =
                *x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(transport_seed))
                    .as_bytes();
            let issuer = generate_signing_key().unwrap();
            let root = TrustRoot::new("isolated-android", &issuer.verifying_key()).unwrap();
            let request = DeviceRequest::new(&identity, transport_public).unwrap();
            let mut membership = MembershipStore::new(root).unwrap();
            let bundle = issue_bundle(
                &request,
                &mut membership,
                &issuer,
                vec!["10.77.0.2".parse().unwrap()],
                Permissions {
                    relay: true,
                    administer: false,
                },
                1,
            )
            .unwrap();
            Self {
                directory,
                identity: hex::encode(identity.to_bytes()).into_bytes(),
                transport: STANDARD.encode(transport_seed).into_bytes(),
                bundle,
                fingerprint: fingerprint(&issuer.verifying_key()),
            }
        }
        fn invoke(&self, operation: u8, document: &str) -> Result<String> {
            provision(
                operation,
                &self.directory.path().join("device"),
                document,
                &self.fingerprint,
                &self.identity,
                &self.transport,
            )
        }
        fn import(&self) {
            self.invoke(1, &serde_json::to_string(&self.bundle).unwrap())
                .unwrap();
        }
    }

    #[test]
    fn public_request_import_and_profile_preserve_android_secret_boundary() {
        let device = Device::new();
        let request: DeviceRequest = serde_json::from_str(&device.invoke(0, "").unwrap()).unwrap();
        request.verify().unwrap();
        assert_eq!(
            device.bundle.claims.node_id,
            node_id_for_key(&request.verify().unwrap())
        );
        device.import();
        device.import();
        device.invoke(2,r#"{"overlay_address":"10.77.0.2","overlay_network":"10.77.0.0/24","peers":["tcp://192.168.10.2:11010"],"control_tcp_ports":[7447]}"#).unwrap();
        let runtime: serde_json::Value = serde_json::from_slice(
            &std::fs::read(device.directory.path().join("device").join("runtime.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(runtime["node_id"], device.bundle.claims.node_id);
        assert_eq!(runtime["exchange_port"], 4192);
        assert_eq!(runtime["control_tcp_ports"], serde_json::json!([7447]));
        assert!(
            !device
                .directory
                .path()
                .join("device")
                .join("identity.key")
                .exists()
        );
        assert!(
            !device
                .directory
                .path()
                .join("device")
                .join("transport.key")
                .exists()
        );
        assert!(
            !device
                .directory
                .path()
                .join("device")
                .join("administrator.key")
                .exists()
        );
        assert!(
            !device
                .directory
                .path()
                .join("runtime/engine-trust.json")
                .exists()
        );
    }

    #[test]
    fn wrong_pin_has_no_trust_side_effect() {
        let device = Device::new();
        let bundle = serde_json::to_string(&device.bundle).unwrap();
        assert!(
            provision(
                1,
                &device.directory.path().join("device"),
                &bundle,
                "wrong",
                &device.identity,
                &device.transport
            )
            .is_err()
        );
        assert!(
            !device
                .directory
                .path()
                .join("device")
                .join("trust-root.json")
                .exists()
        );
        assert!(
            !device
                .directory
                .path()
                .join("device")
                .join("membership.json")
                .exists()
        );
    }

    #[test]
    fn runtime_profile_rejects_unassigned_address_public_route_and_unknown_exchange_owner() {
        let device = Device::new();
        device.import();
        for profile in [
            r#"{"overlay_address":"10.77.0.3","overlay_network":"10.77.0.0/24","peers":[]}"#,
            r#"{"overlay_address":"10.77.0.2","overlay_network":"10.0.0.0/7","peers":[]}"#,
            r#"{"overlay_address":"10.77.0.2","overlay_network":"10.77.0.0/24","peers":["tcp://localhost:11010"]}"#,
            r#"{"overlay_address":"10.77.0.2","overlay_network":"10.77.0.0/24","peers":[],"exchange_peers":[{"node_id":"unknown","service_endpoint":"10.77.0.3:4191","revocation_endpoint":"10.77.0.3:9180"}]}"#,
            r#"{"overlay_address":"10.77.0.2","overlay_network":"10.77.0.0/24","peers":[],"credential_file":"/tmp/other"}"#,
            r#"{"overlay_address":"10.77.0.2","overlay_network":"10.77.0.0/24","peers":[],"control_tcp_ports":[0]}"#,
        ] {
            assert!(device.invoke(2, profile).is_err());
            assert!(
                !device
                    .directory
                    .path()
                    .join("device")
                    .join("runtime.json")
                    .exists()
            );
        }
    }
    #[test]
    fn control_ports_include_application_and_metadata_without_duplicates() {
        let configured = [7447, 4192, 7447];
        let initial = control_ports(&configured, 4192, [(4191, 9180)].into_iter());
        assert_eq!(
            initial,
            std::collections::BTreeSet::from([4191, 4192, 7447, 9180])
        );
    }
}
