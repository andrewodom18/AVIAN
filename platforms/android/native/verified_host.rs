#![forbid(unsafe_code)]

use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime},
};

use anyhow::{Context, Result, bail};
use avian_engine::TrustFileExport;
use avian_membership::{MembershipStore, TrustRoot};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use easytier::common::config::{ConfigFileControl, TomlConfigLoader};
use serde::Deserialize;
use zeroize::Zeroizing;

mod overlay;
mod probes;
mod underlay;

fn default_automatic_probes() -> bool {
    true
}

fn default_exchange_port() -> u16 {
    4192
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Configuration {
    version: u32,
    #[serde(default = "default_automatic_probes")]
    automatic_capacity_probes: bool,
    node_id: String,
    overlay_address: Ipv4Addr,
    overlay_network: String,
    trust_root_file: PathBuf,
    membership_file: PathBuf,
    credential_file: PathBuf,
    policy_file: PathBuf,
    working_directory: PathBuf,
    peers: Vec<String>,
    #[serde(default = "default_exchange_port")]
    exchange_port: u16,
    #[serde(default)]
    exchange_peers: Vec<overlay::OverlayPeer>,
    #[serde(default)]
    service_limits: avian_services::Limits,
}

struct RunningState {
    overlay: Arc<overlay::OverlayState>,
    background: Option<tokio::runtime::Runtime>,
    transport_public_key: String,
    last_publication: Instant,
    underlays: underlay::Underlays,
    policy_files: Vec<PathBuf>,
}

static VERIFIED_STATE: Mutex<Option<RunningState>> = Mutex::new(None);
static PENDING_POLICY: Mutex<Option<Vec<u8>>> = Mutex::new(None);

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct MeteredPolicy {
    metered_addresses: std::collections::BTreeSet<IpAddr>,
}

pub fn set_policy(document: &str) -> Result<()> {
    if document.len() > 65536 {
        bail!("Android policy exceeds budget");
    }
    let policy: MeteredPolicy = serde_json::from_str(document)?;
    *PENDING_POLICY
        .lock()
        .map_err(|_| anyhow::anyhow!("Android policy lock failed"))? =
        Some(serde_json::to_vec(&policy)?);
    Ok(())
}

impl Configuration {
    fn trust_file(&self) -> PathBuf {
        self.working_directory.join("engine-trust.json")
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1 || !self.overlay_address.is_private() || self.exchange_port == 0 {
            bail!("invalid Android engine configuration");
        }
        let network: ipnet::Ipv4Net = self.overlay_network.parse()?;
        if !network.network().is_private()
            || !network.contains(&self.overlay_address)
            || network.prefix_len() < 8
        {
            bail!("overlay route must stay within its private address range");
        }
        for path in [
            &self.trust_root_file,
            &self.membership_file,
            &self.credential_file,
            &self.working_directory,
        ] {
            if !path.is_absolute() {
                bail!("Android runtime paths must be absolute");
            }
        }
        avian_storage::create_private_directory(&self.working_directory)?;
        if self.policy_file.parent() != Some(self.working_directory.as_path()) {
            bail!("Android policy file must stay in the runtime directory");
        }
        if std::env::var_os("AVIAN_TRAFFIC_POLICY_FILE").map(PathBuf::from)
            != Some(self.policy_file.clone())
        {
            bail!("Android policy path does not match native startup");
        }
        for peer in &self.peers {
            let url: url::Url = peer.parse()?;
            if !matches!(url.scheme(), "tcp" | "udp")
                || url.host_str().is_none()
                || url.port().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                bail!("unsupported Android underlay peer");
            }
            // DNS discovery is owned by AVIAN; native startup accepts explicit peer addresses only.
            let address: IpAddr = url.host_str().unwrap().trim_matches(['[', ']']).parse()?;
            if address.is_unspecified()
                || address.is_loopback()
                || address.is_multicast()
                || url.port() == Some(0)
            {
                bail!("invalid Android underlay peer");
            }
        }
        Ok(())
    }
}

pub fn start(configuration_json: &str, credential_bytes: &[u8]) -> Result<()> {
    let mut running = VERIFIED_STATE
        .lock()
        .map_err(|_| anyhow::anyhow!("Android runtime lock failed"))?;
    if running.is_some() {
        bail!("Android engine already exists");
    }
    if configuration_json.len() > 65536 || credential_bytes.len() > 65 {
        bail!("Android startup exceeds budget");
    }
    let configuration: Configuration = serde_json::from_str(configuration_json)?;
    configuration.validate()?;
    let pending_policy = PENDING_POLICY
        .lock()
        .map_err(|_| anyhow::anyhow!("Android policy lock failed"))?;
    let policy = pending_policy
        .as_ref()
        .context("Android underlay metering has not been supplied")?;
    let mut owned_policy: serde_json::Value = serde_json::from_slice(policy)?;
    let mut control_ports = std::collections::BTreeSet::from([configuration.exchange_port]);
    for peer in &configuration.exchange_peers {
        control_ports.insert(peer.service_endpoint.port());
        control_ports.insert(peer.revocation_endpoint.port());
    }
    owned_policy["control_tcp_ports"] = serde_json::to_value(control_ports)?;
    avian_storage::atomic_write_private_file(
        &configuration.policy_file,
        &serde_json::to_vec(&owned_policy)?,
    )?;
    drop(pending_policy);
    let credential = Zeroizing::new(std::str::from_utf8(credential_bytes)?.trim().to_owned());
    let decoded = Zeroizing::new(STANDARD.decode(credential.as_bytes())?);
    let private: Zeroizing<[u8; 32]> = Zeroizing::new(
        decoded
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid transport credential"))?,
    );
    let public = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*private));
    let transport_public_key = hex::encode(public.as_bytes());
    let root: TrustRoot = serde_json::from_slice(&avian_storage::read_private_file(
        &configuration.trust_root_file,
        16384,
    )?)?;
    let membership = MembershipStore::load(&configuration.membership_file, root)?;
    let enrolled = membership.authorize(&configuration.node_id, &transport_public_key)?;
    if !enrolled
        .overlay_addresses
        .contains(&IpAddr::V4(configuration.overlay_address))
    {
        bail!("overlay ownership mismatch");
    }
    TrustFileExport::from_membership(&membership, SystemTime::now())?
        .write_startup_file(configuration.trust_file())?;
    let document = serde_json::json!({
        "instance_name": "avian-android", "hostname": configuration.node_id,
        "ipv4": format!("{}/{}", configuration.overlay_address, configuration.overlay_network.parse::<ipnet::Ipv4Net>()?.prefix_len()),
        "listeners": [], "credential_file": configuration.trust_file().to_string_lossy(),
        "stun_servers": [], "stun_servers_v6": [],
        "network_identity": {"network_name": membership.trust_root().network_id},
        "secure_mode": {"enabled": true,"local_private_key": credential.as_str(),"local_public_key": STANDARD.encode(public.as_bytes())},
        "peer": [],
        "flags": {"disable_p2p":true,"disable_tcp_hole_punching":true,"disable_udp_hole_punching":true,"disable_upnp":true,"accept_dns":false,"enable_exit_node":false}
    });
    let rendered = Zeroizing::new(toml::to_string(&document)?);
    let loader = TomlConfigLoader::new_from_str(&rendered)
        .context("Android engine configuration was rejected")?;
    let overlay_state = overlay::OverlayState::new(configuration.clone(), membership)?;
    let instance = super::INSTANCE_MANAGER.run_network_instance(
        loader,
        false,
        ConfigFileControl::STATIC_CONFIG,
    )?;
    super::INSTANCE_NAME_ID_MAP.insert("avian-android".into(), instance);
    let engine_events = super::INSTANCE_MANAGER.iter().find_map(|entry| {
        (*entry.key() == instance)
            .then(|| entry.value().subscribe_event())
            .flatten()
    });
    let background = match overlay::start(overlay_state.clone(), engine_events) {
        Ok(background) => background,
        Err(error) => {
            let _ = super::INSTANCE_MANAGER.retain_network_instance(Vec::new());
            super::INSTANCE_NAME_ID_MAP.clear();
            return Err(error);
        }
    };
    let revision = overlay_state.snapshot()?.revocation_revision();
    *running = Some(RunningState {
        overlay: overlay_state,
        background: Some(background),
        transport_public_key,
        last_publication: Instant::now(),
        underlays: underlay::Underlays::default(),
        policy_files: vec![configuration.policy_file.clone()],
    });
    overlay::event("runtime_started", revision);
    Ok(())
}

pub fn refresh() -> Result<()> {
    let mut running = VERIFIED_STATE
        .lock()
        .map_err(|_| anyhow::anyhow!("Android runtime lock failed"))?;
    let state = running.as_mut().context("Android engine is not running")?;
    let _operation = state
        .overlay
        .operations
        .lock()
        .map_err(|_| anyhow::anyhow!("Android operation lock failed"))?;
    let previous = state.overlay.snapshot()?;
    let configuration = &state.overlay.configuration;
    let current = MembershipStore::load(
        &configuration.membership_file,
        previous.trust_root().clone(),
    )?;
    current.validate_successor(&previous)?;
    let enrolled = current.authorize(&configuration.node_id, &state.transport_public_key)?;
    if !enrolled
        .overlay_addresses
        .contains(&IpAddr::V4(configuration.overlay_address))
    {
        bail!("overlay ownership changed");
    }
    let changed = current.revocation_revision() != previous.revocation_revision()
        || current.memberships().ne(previous.memberships());
    if changed || state.last_publication.elapsed().as_secs() >= 30 {
        TrustFileExport::from_membership(&current, SystemTime::now())?
            .write_startup_file(configuration.trust_file())?;
        state.last_publication = Instant::now();
    }
    if changed {
        state.overlay.replace_membership(current)?;
    }
    Ok(())
}

fn overlay_state() -> Result<Arc<overlay::OverlayState>> {
    Ok(VERIFIED_STATE
        .lock()
        .map_err(|_| anyhow::anyhow!("Android runtime lock failed"))?
        .as_ref()
        .context("Android runtime is not running")?
        .overlay
        .clone())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalEndpoint {
    kind: avian_services::ServiceKind,
    protocol: avian_services::TransportProtocol,
    port: u16,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    ttl_seconds: i64,
    endpoints: Vec<LocalEndpoint>,
}

pub fn publish_services(document: &str, identity_bytes: &[u8]) -> Result<String> {
    let state = overlay_state()?;
    if document.len()
        > state
            .configuration
            .service_limits
            .maximum_announcement_bytes
        || identity_bytes.len() > 65
    {
        bail!("service publication exceeds budget");
    }
    let registration: Registration = serde_json::from_str(document)?;
    let encoded = std::str::from_utf8(identity_bytes)?.trim();
    let decoded = Zeroizing::new(hex::decode(encoded)?);
    let seed: Zeroizing<[u8; 32]> = Zeroizing::new(
        decoded
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid owner identity"))?,
    );
    let identity = ed25519_dalek::SigningKey::from_bytes(&seed);
    let publication = avian::services::LocalPublication {
        ttl_seconds: registration.ttl_seconds,
        endpoints: registration
            .endpoints
            .into_iter()
            .map(|endpoint| avian_services::ServiceEndpoint {
                kind: endpoint.kind,
                address: state.configuration.overlay_address.into(),
                protocol: endpoint.protocol,
                port: endpoint.port,
            })
            .collect(),
    };
    Ok(serde_json::to_string(
        &state.publish(&identity, publication)?,
    )?)
}

pub fn service_page(after: Option<&str>, limit: usize) -> Result<String> {
    Ok(serde_json::to_string(
        &overlay_state()?.page(after, limit)?,
    )?)
}

pub fn event_page(after: u64, limit: usize) -> Result<String> {
    overlay::events(after, limit)
}

pub fn stop() {
    let previous = VERIFIED_STATE
        .lock()
        .ok()
        .and_then(|mut running| running.take());
    if let Some(mut previous) = previous {
        if let Some(background) = previous.background.take() {
            background.shutdown_timeout(std::time::Duration::from_secs(1));
        }
        let _ = super::INSTANCE_MANAGER.retain_network_instance(Vec::new());
        super::INSTANCE_NAME_ID_MAP.clear();
        for path in previous.policy_files {
            let _ = std::fs::remove_file(path);
        }
        overlay::event(
            "runtime_stopped",
            previous
                .overlay
                .snapshot()
                .map_or(0, |membership| membership.revocation_revision()),
        );
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PhysicalUpdate {
    assignments: Vec<underlay::Assignment>,
    metered_addresses: std::collections::BTreeSet<IpAddr>,
}

/// Persist metering before the JNI boundary activates the returned policy path.
pub fn update_underlays(document: &str) -> Result<String> {
    if document.len() > 65536 {
        bail!("underlay update exceeds budget");
    }
    let update: PhysicalUpdate = serde_json::from_str(document)?;
    let mut running = VERIFIED_STATE
        .lock()
        .map_err(|_| anyhow::anyhow!("Android runtime lock failed"))?;
    let state = running.as_mut().context("Android engine is not running")?;
    let configuration = &state.overlay.configuration;
    let mut ports = std::collections::BTreeSet::from([configuration.exchange_port]);
    for peer in &configuration.exchange_peers {
        ports.insert(peer.service_endpoint.port());
        ports.insert(peer.revocation_endpoint.port());
    }
    let path = configuration.working_directory.join(format!(
        "policy-live-{}.json",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos()
    ));
    avian_storage::atomic_write_private_file(
        &path,
        &serde_json::to_vec(&serde_json::json!({
            "metered_addresses": update.metered_addresses, "control_tcp_ports": ports
        }))?,
    )?;
    state.policy_files.push(path.clone());
    while state.policy_files.len() > 3 {
        let obsolete = state.policy_files.remove(0);
        let _ = std::fs::remove_file(obsolete);
    }
    // Socket callbacks are re-enabled by Java only after this call returns. The
    // native boundary activates the file before reconciling physical sockets.
    Ok(path.to_string_lossy().into_owned())
}

pub fn reconcile_underlays(document: &str) -> Result<bool> {
    let update: PhysicalUpdate = serde_json::from_str(document)?;
    let mut running = VERIFIED_STATE
        .lock()
        .map_err(|_| anyhow::anyhow!("Android runtime lock failed"))?;
    let state = running.as_mut().context("Android engine is not running")?;
    let complete = state
        .background
        .as_ref()
        .context("control runtime unavailable")?
        .block_on(
            state
                .underlays
                .reconcile(&state.overlay.configuration.peers, &update.assignments),
        )?;
    overlay::event(
        if complete {
            "underlays_reconciled"
        } else {
            "underlay_update_rejected"
        },
        state.overlay.snapshot()?.revocation_revision(),
    );
    Ok(complete)
}

/// Expose operational state only; the engine's serialized configuration is never returned.
pub fn status() -> Result<String> {
    use easytier::proto::{
        api::instance::{
            ListConnectorRequest, ListPeerRequest, ListRouteRequest, ShowNodeInfoRequest,
        },
        rpc_types::controller::BaseController,
    };
    let running = VERIFIED_STATE
        .lock()
        .map_err(|_| anyhow::anyhow!("Android runtime lock failed"))?;
    let state = running.as_ref().context("Android engine is not running")?;
    state.background.as_ref().context("control runtime unavailable")?.block_on(async {
        let identifier = *super::INSTANCE_NAME_ID_MAP.get("avian-android").context("mobile instance missing")?;
        let service = super::INSTANCE_MANAGER.get_instance_service(&identifier).context("mobile instance unavailable")?;
        let controller = BaseController::default();
        let node = service.get_peer_manage_service().show_node_info(controller.clone(), ShowNodeInfoRequest::default()).await?.node_info;
        let peers = service.get_peer_manage_service().list_peer(controller.clone(), ListPeerRequest::default()).await?.peer_infos;
        let routes = service.get_peer_manage_service().list_route(controller.clone(), ListRouteRequest::default()).await?.routes;
        let connectors = service.get_connector_manage_service().list_connector(controller, ListConnectorRequest::default()).await?.connectors;
        let peer_count = peers.len();
        let route_count = routes.len();
        let connector_count = connectors.len();
        let peers: Vec<_> = peers.into_iter().map(|peer| serde_json::json!({
            "peer_id":peer.peer_id,
            "connections":peer.conns.into_iter().map(|connection|serde_json::json!({
                "closed":connection.is_closed,"auth_level":connection.secure_auth_level,
                "rx_bytes":connection.stats.as_ref().map_or(0,|stats|stats.rx_bytes),
                "tx_bytes":connection.stats.as_ref().map_or(0,|stats|stats.tx_bytes),
            })).collect::<Vec<_>>()
        })).collect();
        Ok(serde_json::to_string(&serde_json::json!({
            "peer_id":node.as_ref().map(|node|node.peer_id),
            "listeners":node.map(|node|node.listeners),
            "peer_count":peer_count,"route_count":route_count,"connector_count":connector_count,
            "capacity":probes::status(),
            "tun_state":overlay::tun_state(),
            "peers":peers,
            "routes":routes.into_iter().take(100).map(|route|serde_json::json!({
                "peer_id":route.peer_id,"address":route.ipv4_addr,"next_hop":route.next_hop_peer_id
            })).collect::<Vec<_>>(),
            "connectors":connectors.into_iter().take(100).collect::<Vec<_>>()
        }))?)
    })
}

async fn instance_service() -> Result<Arc<dyn easytier::rpc_service::InstanceRpcService>> {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Some(identifier) = super::INSTANCE_NAME_ID_MAP
                .get("avian-android")
                .map(|entry| *entry)
                && let Some(service) = super::INSTANCE_MANAGER.get_instance_service(&identifier)
            {
                break service;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .context("mobile instance did not finish startup")
}

/// Instance::run clears its initial NIC before publishing this API. Attaching
/// earlier can race that clear and replace the real mobile NIC with a mock sink.
pub fn wait_instance_ready() -> Result<()> {
    let running = VERIFIED_STATE
        .lock()
        .map_err(|_| anyhow::anyhow!("Android runtime lock failed"))?;
    let state = running.as_ref().context("Android engine is not running")?;
    state
        .background
        .as_ref()
        .context("control runtime unavailable")?
        .block_on(instance_service())?;
    Ok(())
}
