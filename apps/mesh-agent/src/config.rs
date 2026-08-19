use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context};
use clap::{Parser, ValueEnum};
use mesh_core::{FlightStack, NodeRole, DEFAULT_MAX_NEIGHBORS};
use mesh_peat::PeerDescriptor;
use serde::{Deserialize, Serialize};

pub const CONFIG_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MavlinkStack {
    #[serde(rename = "ardupilot", alias = "ardu_pilot")]
    #[value(name = "ardupilot")]
    ArduPilot,
    #[value(name = "px4")]
    Px4,
}

impl From<MavlinkStack> for FlightStack {
    fn from(value: MavlinkStack) -> Self {
        match value {
            MavlinkStack::ArduPilot => Self::ArduPilot,
            MavlinkStack::Px4 => Self::Px4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfiguredNodeRole {
    Aircraft,
    Ground,
    Observer,
}

impl From<ConfiguredNodeRole> for NodeRole {
    fn from(value: ConfiguredNodeRole) -> Self {
        match value {
            ConfiguredNodeRole::Aircraft => Self::Aircraft,
            ConfiguredNodeRole::Ground => Self::Ground,
            ConfiguredNodeRole::Observer => Self::Cloud,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandMode {
    #[default]
    Disabled,
    DryRun,
    Execute,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandEnvironment {
    #[default]
    Hardware,
    Sitl,
}

#[derive(Debug, Parser)]
#[command(
    name = "mesh-agent",
    about = "AVIAN onboard PEAT mesh service",
    version
)]
pub struct CliArgs {
    /// Strict, versioned production TOML. Relative paths resolve from this file.
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// Stable AVIAN node name used to derive the PEAT identity.
    #[arg(long)]
    pub name: Option<String>,
    /// Local IP and UDP port for the Iroh QUIC transport.
    #[arg(long)]
    pub bind: Option<SocketAddr>,
    /// Directory for persistent Automerge state.
    #[arg(long)]
    pub storage: Option<PathBuf>,
    /// PEAT formation identifier shared by authorized AVIAN nodes.
    #[arg(long)]
    pub formation_id: Option<String>,
    /// File containing the shared base64 PEAT formation secret.
    #[arg(long)]
    pub formation_key_file: Option<PathBuf>,
    /// Static peer as NAME=ENDPOINT_ID_HEX@IP:PORT[,IP:PORT...]. Repeat per peer.
    #[arg(long)]
    pub peer: Vec<PeerDescriptor>,
    /// Shared versioned aircraft membership manifest; replaces static peers.
    #[arg(long, conflicts_with = "peer")]
    pub membership_file: Option<PathBuf>,
    /// Hard limit on direct PEAT neighbors.
    #[arg(long)]
    pub max_mesh_peers: Option<usize>,
    /// Seconds between attempts to reconnect unavailable static peers.
    #[arg(long)]
    pub peer_retry_seconds: Option<u64>,
    /// MAVLink connection, such as udpin:0.0.0.0:14553.
    #[arg(long, requires = "flight_stack")]
    pub mavlink_address: Option<String>,
    /// Expected flight controller for the MAVLink heartbeat.
    #[arg(long, value_enum, requires = "mavlink_address")]
    pub flight_stack: Option<MavlinkStack>,
    /// Maximum telemetry publications per second.
    #[arg(long)]
    pub telemetry_hz: Option<f64>,
    /// Optional mission traffic policy.
    #[arg(long)]
    pub traffic_policy_file: Option<PathBuf>,
    /// Seconds before reconnecting a lost MAVLink transport.
    #[arg(long)]
    pub mavlink_retry_seconds: Option<u64>,
    /// Shared ARC runtime relay configuration.
    #[arg(long)]
    pub relay_runtime_config: Option<PathBuf>,
    /// Milliseconds between in-flight relay evaluations.
    #[arg(long)]
    pub relay_evaluation_ms: Option<u64>,
    /// Compatibility UDP listener for normalized relay observations.
    #[arg(long)]
    pub relay_observation_listen: Option<SocketAddr>,
}

#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub source_path: Option<PathBuf>,
    pub name: String,
    pub role: ConfiguredNodeRole,
    pub bind: SocketAddr,
    pub storage: PathBuf,
    pub formation_id: String,
    pub formation_key_file: PathBuf,
    pub peers: Vec<PeerDescriptor>,
    pub tagged_peers: Vec<TaggedPeer>,
    pub membership_file: Option<PathBuf>,
    pub max_mesh_peers: usize,
    pub peer_retry_seconds: u64,
    pub mavlink_address: Option<String>,
    pub flight_stack: Option<MavlinkStack>,
    pub telemetry_hz: f64,
    pub traffic_policy_file: Option<PathBuf>,
    pub mavlink_retry_seconds: u64,
    pub relay_runtime_config: Option<PathBuf>,
    pub relay_evaluation_ms: u64,
    pub relay_observation_listen: Option<SocketAddr>,
    pub sockets: SocketConfig,
    pub commands: CommandConfig,
    pub radio: RadioConfig,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedPeer {
    pub name: String,
    pub endpoint_id: String,
    pub addresses: Vec<TaggedAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedAddress {
    pub underlay: Underlay,
    pub address: SocketAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Underlay {
    Silvus,
    Satellite,
    Ethernet,
    Wifi,
    Other,
}

#[derive(Debug, Clone)]
pub struct SocketConfig {
    pub control: PathBuf,
    pub payload: PathBuf,
    pub link_observation: PathBuf,
    pub max_message_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct CommandConfig {
    pub mode: CommandMode,
    pub environment: CommandEnvironment,
    pub signing_key_file: Option<PathBuf>,
    pub issuers: Vec<IssuerConfig>,
    pub state_file: PathBuf,
    pub lifetime_ms: u64,
    pub poll_ms: u64,
    pub ack_timeout_ms: u64,
    pub retries: u8,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuerConfig {
    pub id: String,
    pub public_key_file: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct RadioConfig {
    pub enabled: bool,
    pub observation_interval_seconds: u64,
    pub probe_timeout_ms: u64,
    pub devices: Vec<RadioDeviceConfig>,
    pub probes: Vec<PeerProbeConfig>,
    pub probe_listen: Option<SocketAddr>,
    pub links: Vec<CalibratedLinkConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RadioDeviceConfig {
    pub name: String,
    pub base_url: String,
    pub credentials_file: PathBuf,
    pub local_node_id: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerProbeConfig {
    pub peer: String,
    pub underlay: Underlay,
    pub address: String,
    #[serde(default = "default_probe_packets")]
    pub packets: u16,
    #[serde(default = "default_probe_payload_bytes")]
    pub payload_bytes: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalibratedLinkConfig {
    pub first: String,
    pub second: String,
    pub underlay: Underlay,
    pub first_radio_node_id: Option<u32>,
    pub second_radio_node_id: Option<u32>,
    pub distance_m: Option<f64>,
    pub line_of_sight: Option<bool>,
    pub fresnel_clearance_ratio: Option<f32>,
    pub snr_floor_db: Option<f64>,
    pub snr_ceiling_db: Option<f64>,
    pub receiver_sensitivity_dbm: Option<f64>,
    pub energy_cost: Option<f32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    schema_version: u16,
    node: FileNode,
    peat: FilePeat,
    #[serde(default)]
    peers: Vec<FilePeer>,
    mavlink: Option<FileMavlink>,
    #[serde(default)]
    sockets: FileSockets,
    #[serde(default)]
    commands: FileCommands,
    relay: Option<FileRelay>,
    #[serde(default)]
    radio: FileRadio,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileNode {
    name: String,
    role: ConfiguredNodeRole,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePeat {
    #[serde(default = "default_bind")]
    bind: SocketAddr,
    #[serde(default = "default_storage")]
    storage: PathBuf,
    #[serde(default = "default_formation_id")]
    formation_id: String,
    formation_key_file: PathBuf,
    membership_file: Option<PathBuf>,
    #[serde(default = "default_max_mesh_peers")]
    max_mesh_peers: usize,
    #[serde(default = "default_peer_retry_seconds")]
    peer_retry_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePeer {
    name: String,
    endpoint_id: String,
    addresses: Vec<FilePeerAddress>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePeerAddress {
    underlay: Underlay,
    address: SocketAddr,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileMavlink {
    address: String,
    flight_stack: MavlinkStack,
    #[serde(default = "default_telemetry_hz")]
    telemetry_hz: f64,
    traffic_policy_file: Option<PathBuf>,
    #[serde(default = "default_mavlink_retry_seconds")]
    retry_seconds: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileSockets {
    control: PathBuf,
    payload: PathBuf,
    link_observation: PathBuf,
    max_message_bytes: usize,
}

impl Default for FileSockets {
    fn default() -> Self {
        Self {
            control: PathBuf::from("/run/avian/control.sock"),
            payload: PathBuf::from("/run/avian/payload-events.sock"),
            link_observation: PathBuf::from("/run/avian/link-observations.sock"),
            max_message_bytes: 65_536,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileCommands {
    mode: CommandMode,
    environment: CommandEnvironment,
    signing_key_file: Option<PathBuf>,
    issuers: Vec<IssuerConfig>,
    state_file: PathBuf,
    lifetime_ms: u64,
    poll_ms: u64,
    ack_timeout_ms: u64,
    retries: u8,
}

impl Default for FileCommands {
    fn default() -> Self {
        Self {
            mode: CommandMode::Disabled,
            environment: CommandEnvironment::Hardware,
            signing_key_file: None,
            issuers: Vec::new(),
            state_file: PathBuf::from("command-state.json"),
            lifetime_ms: 5_000,
            poll_ms: 250,
            ack_timeout_ms: 1_500,
            retries: 1,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRelay {
    runtime_config: Option<PathBuf>,
    #[serde(default = "default_relay_evaluation_ms")]
    evaluation_ms: u64,
    observation_udp_listen: Option<SocketAddr>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct FileRadio {
    enabled: bool,
    observation_interval_seconds: u64,
    probe_timeout_ms: u64,
    devices: Vec<RadioDeviceConfig>,
    probes: Vec<PeerProbeConfig>,
    probe_listen: Option<SocketAddr>,
    links: Vec<CalibratedLinkConfig>,
}

impl Default for FileRadio {
    fn default() -> Self {
        Self {
            enabled: false,
            observation_interval_seconds: 10,
            probe_timeout_ms: 1_000,
            devices: Vec::new(),
            probes: Vec::new(),
            probe_listen: None,
            links: Vec::new(),
        }
    }
}

impl ResolvedConfig {
    pub fn load(cli: CliArgs) -> anyhow::Result<Self> {
        let source_path = cli.config.clone();
        let file = source_path.as_deref().map(read_config).transpose()?;
        let base_path = source_path
            .as_deref()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let base = base_path.as_path();

        let file_name = file.as_ref().map(|value| value.node.name.clone());
        let name = cli
            .name
            .or(file_name)
            .context("--name or [node].name is required")?;
        let role = file
            .as_ref()
            .map_or(ConfiguredNodeRole::Aircraft, |value| value.node.role);
        let peat = file.as_ref().map(|value| &value.peat);
        let bind = cli
            .bind
            .or_else(|| peat.map(|value| value.bind))
            .unwrap_or_else(default_bind);
        let storage = resolve_path(
            base,
            cli.storage
                .or_else(|| peat.map(|value| value.storage.clone()))
                .unwrap_or_else(default_storage),
        );
        let formation_id = cli
            .formation_id
            .or_else(|| peat.map(|value| value.formation_id.clone()))
            .unwrap_or_else(default_formation_id);
        let formation_key_file = resolve_path(
            base,
            cli.formation_key_file
                .or_else(|| peat.map(|value| value.formation_key_file.clone()))
                .context("--formation-key-file or [peat].formation_key_file is required")?,
        );
        let membership_file = cli
            .membership_file
            .or_else(|| peat.and_then(|value| value.membership_file.clone()))
            .map(|path| resolve_path(base, path));
        let max_mesh_peers = cli
            .max_mesh_peers
            .or_else(|| peat.map(|value| value.max_mesh_peers))
            .unwrap_or_else(default_max_mesh_peers);
        let peer_retry_seconds = cli
            .peer_retry_seconds
            .or_else(|| peat.map(|value| value.peer_retry_seconds))
            .unwrap_or_else(default_peer_retry_seconds);

        let tagged_peers = if cli.peer.is_empty() && membership_file.is_none() {
            file.as_ref()
                .map(|value| parse_tagged_peers(&value.peers))
                .transpose()?
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let peers = if !cli.peer.is_empty() {
            cli.peer
        } else {
            tagged_peers
                .iter()
                .map(|peer| {
                    PeerDescriptor::with_addresses(
                        peer.name.clone(),
                        peer.endpoint_id.clone(),
                        peer.addresses.iter().map(|value| value.address).collect(),
                    )
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        let mavlink = file.as_ref().and_then(|value| value.mavlink.as_ref());
        let relay = file.as_ref().and_then(|value| value.relay.as_ref());
        let sockets = file
            .as_ref()
            .map_or_else(FileSockets::default, |value| FileSockets {
                control: value.sockets.control.clone(),
                payload: value.sockets.payload.clone(),
                link_observation: value.sockets.link_observation.clone(),
                max_message_bytes: value.sockets.max_message_bytes,
            });
        let commands = file
            .as_ref()
            .map_or_else(FileCommands::default, |value| FileCommands {
                mode: value.commands.mode,
                environment: value.commands.environment,
                signing_key_file: value.commands.signing_key_file.clone(),
                issuers: value.commands.issuers.clone(),
                state_file: value.commands.state_file.clone(),
                lifetime_ms: value.commands.lifetime_ms,
                poll_ms: value.commands.poll_ms,
                ack_timeout_ms: value.commands.ack_timeout_ms,
                retries: value.commands.retries,
            });
        let radio = file
            .as_ref()
            .map_or_else(FileRadio::default, |value| FileRadio {
                enabled: value.radio.enabled,
                observation_interval_seconds: value.radio.observation_interval_seconds,
                probe_timeout_ms: value.radio.probe_timeout_ms,
                devices: value.radio.devices.clone(),
                probes: value.radio.probes.clone(),
                probe_listen: value.radio.probe_listen,
                links: value.radio.links.clone(),
            });

        let resolved = Self {
            source_path,
            name,
            role,
            bind,
            storage: storage.clone(),
            formation_id,
            formation_key_file,
            peers,
            tagged_peers,
            membership_file,
            max_mesh_peers,
            peer_retry_seconds,
            mavlink_address: cli
                .mavlink_address
                .or_else(|| mavlink.map(|value| value.address.clone())),
            flight_stack: cli
                .flight_stack
                .or_else(|| mavlink.map(|value| value.flight_stack)),
            telemetry_hz: cli
                .telemetry_hz
                .or_else(|| mavlink.map(|value| value.telemetry_hz))
                .unwrap_or_else(default_telemetry_hz),
            traffic_policy_file: cli
                .traffic_policy_file
                .or_else(|| mavlink.and_then(|value| value.traffic_policy_file.clone()))
                .map(|path| resolve_path(base, path)),
            mavlink_retry_seconds: cli
                .mavlink_retry_seconds
                .or_else(|| mavlink.map(|value| value.retry_seconds))
                .unwrap_or_else(default_mavlink_retry_seconds),
            relay_runtime_config: cli
                .relay_runtime_config
                .or_else(|| relay.and_then(|value| value.runtime_config.clone()))
                .map(|path| resolve_path(base, path)),
            relay_evaluation_ms: cli
                .relay_evaluation_ms
                .or_else(|| relay.map(|value| value.evaluation_ms))
                .unwrap_or_else(default_relay_evaluation_ms),
            relay_observation_listen: cli
                .relay_observation_listen
                .or_else(|| relay.and_then(|value| value.observation_udp_listen)),
            sockets: SocketConfig {
                control: resolve_path(base, sockets.control),
                payload: resolve_path(base, sockets.payload),
                link_observation: resolve_path(base, sockets.link_observation),
                max_message_bytes: sockets.max_message_bytes,
            },
            commands: CommandConfig {
                mode: commands.mode,
                environment: commands.environment,
                signing_key_file: commands
                    .signing_key_file
                    .map(|path| resolve_path(base, path)),
                issuers: commands
                    .issuers
                    .into_iter()
                    .map(|mut issuer| {
                        issuer.public_key_file = resolve_path(base, issuer.public_key_file);
                        issuer
                    })
                    .collect(),
                state_file: resolve_path(&storage, commands.state_file),
                lifetime_ms: commands.lifetime_ms,
                poll_ms: commands.poll_ms,
                ack_timeout_ms: commands.ack_timeout_ms,
                retries: commands.retries,
            },
            radio: RadioConfig {
                enabled: radio.enabled,
                observation_interval_seconds: radio.observation_interval_seconds,
                probe_timeout_ms: radio.probe_timeout_ms,
                devices: radio
                    .devices
                    .into_iter()
                    .map(|mut device| {
                        device.credentials_file = resolve_path(base, device.credentials_file);
                        device
                    })
                    .collect(),
                probes: radio.probes,
                probe_listen: radio.probe_listen,
                links: radio.links,
            },
        };
        resolved.validate()?;
        Ok(resolved)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.name.trim().is_empty() {
            bail!("node name cannot be empty");
        }
        if self.formation_id.trim().is_empty() {
            bail!("formation ID cannot be empty");
        }
        if !self.telemetry_hz.is_finite() || !(0.1..=20.0).contains(&self.telemetry_hz) {
            bail!("telemetry_hz must be between 0.1 and 20.0");
        }
        if !(2..=DEFAULT_MAX_NEIGHBORS).contains(&self.max_mesh_peers)
            || !self.max_mesh_peers.is_multiple_of(2)
        {
            bail!("max_mesh_peers must be one of 2, 4, 6, or {DEFAULT_MAX_NEIGHBORS}");
        }
        if self.peers.len() > self.max_mesh_peers {
            bail!("configured peers exceed max_mesh_peers");
        }
        if self.relay_evaluation_ms == 0 || self.sockets.max_message_bytes == 0 {
            bail!("relay evaluation and socket message limits must be positive");
        }
        if self.mavlink_address.is_some() != self.flight_stack.is_some() {
            bail!("MAVLink address and flight stack must be configured together");
        }
        if self.commands.mode != CommandMode::Disabled
            && self.commands.signing_key_file.is_none()
            && self.commands.issuers.is_empty()
        {
            bail!("enabled commands require a signing key or at least one allowed issuer");
        }
        if self.commands.mode == CommandMode::Execute
            && self.commands.environment != CommandEnvironment::Sitl
        {
            bail!("execute command mode is permitted only when environment = \"sitl\"");
        }
        if self.commands.lifetime_ms > 5_000 {
            bail!("command lifetime cannot exceed the 5000 ms emergency record lifetime");
        }
        if self.commands.lifetime_ms == 0
            || self.commands.poll_ms == 0
            || self.commands.ack_timeout_ms == 0
        {
            bail!("command timing values must be positive");
        }
        if self.radio.enabled
            && (self.radio.observation_interval_seconds == 0 || self.radio.probe_timeout_ms == 0)
        {
            bail!("radio observation interval and probe timeout must be positive");
        }
        for probe in &self.radio.probes {
            if probe.peer.trim().is_empty()
                || !(1..=100).contains(&probe.packets)
                || !(32..=1_400).contains(&probe.payload_bytes)
            {
                bail!("radio probes require a peer, 1-100 packets, and 32-1400 byte payloads");
            }
            probe
                .address
                .parse::<SocketAddr>()
                .with_context(|| format!("invalid probe address {}", probe.address))?;
        }
        for link in &self.radio.links {
            if link.first.trim().is_empty()
                || link.second.trim().is_empty()
                || link.first == link.second
            {
                bail!("calibrated radio links require two distinct node names");
            }
        }
        Ok(())
    }
}

/// Reject secrets that are not regular files or are readable by another user.
///
/// Public verification keys are intentionally not passed through this helper.
#[cfg(unix)]
pub fn validate_private_file_permissions(path: &Path, label: &str) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path)
        .with_context(|| format!("reading permissions for {label} {}", path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "{label} {} must be a regular file",
        path.display()
    );
    anyhow::ensure!(
        metadata.permissions().mode() & 0o077 == 0,
        "{label} {} must not be accessible by group or other users",
        path.display()
    );
    Ok(())
}

#[cfg(not(unix))]
pub fn validate_private_file_permissions(_path: &Path, _label: &str) -> anyhow::Result<()> {
    Ok(())
}

fn read_config(path: &Path) -> anyhow::Result<FileConfig> {
    let encoded = std::fs::read_to_string(path)
        .with_context(|| format!("reading AVIAN configuration {}", path.display()))?;
    let config: FileConfig = toml::from_str(&encoded)
        .with_context(|| format!("decoding AVIAN configuration {}", path.display()))?;
    if config.schema_version != CONFIG_SCHEMA_VERSION {
        bail!(
            "unsupported configuration schema {}; expected {}",
            config.schema_version,
            CONFIG_SCHEMA_VERSION
        );
    }
    Ok(config)
}

fn parse_tagged_peers(peers: &[FilePeer]) -> anyhow::Result<Vec<TaggedPeer>> {
    peers
        .iter()
        .map(|peer| {
            if peer.addresses.is_empty() {
                bail!("peer {} has no addresses", peer.name);
            }
            let mut addresses = peer
                .addresses
                .iter()
                .map(|value| TaggedAddress {
                    underlay: value.underlay,
                    address: value.address,
                })
                .collect::<Vec<_>>();
            addresses.sort_by_key(|value| match value.underlay {
                Underlay::Silvus => 0,
                Underlay::Satellite => 1,
                Underlay::Ethernet => 2,
                Underlay::Wifi => 3,
                Underlay::Other => 4,
            });
            Ok(TaggedPeer {
                name: peer.name.clone(),
                endpoint_id: peer.endpoint_id.clone(),
                addresses,
            })
        })
        .collect()
}

fn resolve_path(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn default_bind() -> SocketAddr {
    SocketAddr::from_str("0.0.0.0:9000").expect("static socket address")
}

fn default_storage() -> PathBuf {
    PathBuf::from("./avian-data")
}

fn default_formation_id() -> String {
    "avian".to_owned()
}

fn default_max_mesh_peers() -> usize {
    DEFAULT_MAX_NEIGHBORS
}

fn default_peer_retry_seconds() -> u64 {
    5
}

fn default_telemetry_hz() -> f64 {
    2.0
}

fn default_mavlink_retry_seconds() -> u64 {
    2
}

fn default_relay_evaluation_ms() -> u64 {
    1_000
}

fn default_probe_packets() -> u16 {
    5
}

fn default_probe_payload_bytes() -> u16 {
    256
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_files_reject_group_read_access() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("secret.key");
        std::fs::write(&path, "secret").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(validate_private_file_permissions(&path, "test secret").is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        validate_private_file_permissions(&path, "test secret").unwrap();
    }

    fn cli(config: PathBuf) -> CliArgs {
        CliArgs::parse_from(["mesh-agent", "--config", config.to_str().unwrap()])
    }

    #[test]
    fn resolves_relative_paths_and_orders_silvus_first() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("avian.toml");
        std::fs::write(
            &path,
            r#"
schema_version = 1
[node]
name = "air-1"
role = "aircraft"
[peat]
formation_key_file = "formation.key"
storage = "state"
[[peers]]
name = "ground"
endpoint_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
[[peers.addresses]]
underlay = "satellite"
address = "10.2.0.1:9000"
[[peers.addresses]]
underlay = "silvus"
address = "10.1.0.1:9000"
"#,
        )
        .unwrap();
        let resolved = ResolvedConfig::load(cli(path)).unwrap();
        assert_eq!(resolved.storage, directory.path().join("state"));
        assert_eq!(
            resolved.formation_key_file,
            directory.path().join("formation.key")
        );
        assert_eq!(
            resolved.tagged_peers[0].addresses[0].underlay,
            Underlay::Silvus
        );
    }

    #[test]
    fn cli_peer_list_replaces_configured_peers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("avian.toml");
        std::fs::write(
            &path,
            r#"
schema_version = 1
[node]
name = "air-1"
role = "aircraft"
[peat]
formation_key_file = "formation.key"
[[peers]]
name = "configured"
endpoint_id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
addresses = [{ underlay = "silvus", address = "10.1.0.1:9000" }]
"#,
        )
        .unwrap();
        let replacement =
            "cli=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb@10.3.0.1:9000";
        let args = CliArgs::parse_from([
            "mesh-agent",
            "--config",
            path.to_str().unwrap(),
            "--peer",
            replacement,
        ]);
        let resolved = ResolvedConfig::load(args).unwrap();
        assert_eq!(resolved.peers[0].name, "cli");
        assert!(resolved.tagged_peers.is_empty());
    }

    #[test]
    fn scalar_cli_values_override_configured_values() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("avian.toml");
        std::fs::write(
            &path,
            r#"
schema_version = 1
[node]
name = "configured"
role = "aircraft"
[peat]
bind = "127.0.0.1:9000"
storage = "configured-state"
formation_id = "configured-formation"
formation_key_file = "configured.key"
peer_retry_seconds = 30
[mavlink]
address = "udpin:127.0.0.1:14550"
flight_stack = "ardupilot"
telemetry_hz = 1.0
retry_seconds = 10
"#,
        )
        .unwrap();
        let args = CliArgs::parse_from([
            "mesh-agent",
            "--config",
            path.to_str().unwrap(),
            "--name",
            "cli",
            "--bind",
            "127.0.0.1:9100",
            "--storage",
            "cli-state",
            "--formation-id",
            "cli-formation",
            "--formation-key-file",
            "cli.key",
            "--peer-retry-seconds",
            "3",
            "--mavlink-address",
            "udpin:127.0.0.1:14553",
            "--flight-stack",
            "px4",
            "--telemetry-hz",
            "4",
            "--mavlink-retry-seconds",
            "2",
        ]);
        let resolved = ResolvedConfig::load(args).unwrap();
        assert_eq!(resolved.name, "cli");
        assert_eq!(resolved.bind, "127.0.0.1:9100".parse().unwrap());
        assert_eq!(resolved.storage, directory.path().join("cli-state"));
        assert_eq!(resolved.formation_id, "cli-formation");
        assert_eq!(
            resolved.formation_key_file,
            directory.path().join("cli.key")
        );
        assert_eq!(resolved.peer_retry_seconds, 3);
        assert_eq!(
            resolved.mavlink_address.as_deref(),
            Some("udpin:127.0.0.1:14553")
        );
        assert_eq!(resolved.flight_stack, Some(MavlinkStack::Px4));
        assert_eq!(resolved.telemetry_hz, 4.0);
        assert_eq!(resolved.mavlink_retry_seconds, 2);
    }

    #[test]
    fn execute_mode_is_rejected_for_hardware() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("avian.toml");
        std::fs::write(
            &path,
            r#"
schema_version = 1
[node]
name = "air-1"
role = "aircraft"
[peat]
formation_key_file = "formation.key"
[commands]
mode = "execute"
environment = "hardware"
signing_key_file = "ground.key"
"#,
        )
        .unwrap();
        let error = ResolvedConfig::load(cli(path)).unwrap_err().to_string();
        assert!(error.contains("only when environment = \"sitl\""));
    }

    #[test]
    fn production_examples_decode() {
        for (name, contents) in [
            (
                "aircraft.toml",
                include_str!("../../../config/aircraft.toml.example"),
            ),
            (
                "ground.toml",
                include_str!("../../../config/ground.toml.example"),
            ),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(name);
            std::fs::write(&path, contents).unwrap();
            ResolvedConfig::load(cli(path)).unwrap();
        }
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("avian.toml");
        std::fs::write(
            &path,
            r#"
schema_version = 1
surprise = true
[node]
name = "air-1"
role = "aircraft"
[peat]
formation_key_file = "formation.key"
"#,
        )
        .unwrap();
        assert!(ResolvedConfig::load(cli(path)).is_err());
    }
}
