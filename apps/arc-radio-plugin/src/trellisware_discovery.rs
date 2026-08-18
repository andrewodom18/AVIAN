use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use clap::Args;
use mesh_core::{
    reduce_radio_discoveries, stable_radio_source, RadioDiscoveryMethod, RadioDiscoveryObservation,
    RadioDiscoveryPolicy, RadioManagementAuthentication, RadioManagementEndpoint,
    RadioReachabilityStatus, RadioVendorId, RADIO_DISCOVERY_SCHEMA_VERSION,
};
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio::process::Command;

const RADIO_DISCOVERY_TOPIC: &str = "local/link/radio/discovery/v1";
const TRELLISWARE_OUIS: [&str; 2] = ["001e3f", "209b60"];

#[derive(Debug, Args)]
pub struct TrellisWareDiscoveryArgs {
    /// Known management addresses to stimulate before reading the neighbor table.
    #[arg(long = "probe-ip", default_value = "10.1.0.2")]
    probe_ips: Vec<IpAddr>,
    /// Optional ARC comms endpoint. When set, publish discoveries to the live UI.
    #[arg(long)]
    zenoh_endpoint: Option<String>,
    /// Continue inspecting the neighbor table instead of performing one pass.
    #[arg(long, default_value_t = false)]
    watch: bool,
    /// Discovery interval for --watch.
    #[arg(long, default_value_t = 5)]
    interval_seconds: u64,
    /// Optional JSON output path for the latest discovery list.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct NeighborEntry {
    #[serde(alias = "IPAddress", alias = "ip", alias = "dst")]
    ip_address: String,
    #[serde(alias = "LinkLayerAddress", alias = "mac", alias = "lladdr")]
    link_layer_address: String,
    #[serde(default, alias = "InterfaceAlias", alias = "dev")]
    interface_alias: Option<String>,
    #[serde(default, alias = "InterfaceIndex", alias = "ifindex")]
    interface_index: Option<u32>,
    #[serde(default, alias = "State", alias = "state")]
    state: Option<NeighborState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
enum NeighborState {
    Name(String),
    Names(Vec<String>),
    Code(u8),
}

pub async fn run(args: &TrellisWareDiscoveryArgs) -> anyhow::Result<()> {
    if args.interval_seconds == 0 {
        bail!("--interval-seconds must be positive");
    }
    let session = match args.zenoh_endpoint.as_deref() {
        Some(endpoint) => Some(open_zenoh(endpoint).await?),
        None => None,
    };

    loop {
        let iteration = async {
            stimulate_neighbors(&args.probe_ips).await;
            let discoveries = discover().await?;
            emit(&discoveries, args.output.as_deref())?;
            if let Some(session) = session.as_ref() {
                for discovery in &discoveries {
                    session
                        .put(RADIO_DISCOVERY_TOPIC, serde_json::to_vec(discovery)?)
                        .await
                        .map_err(|error| anyhow::anyhow!("publishing TW-950 discovery: {error}"))?;
                }
            }
            anyhow::Ok(())
        }
        .await;
        if let Err(error) = iteration {
            if !args.watch {
                return Err(error);
            }
            eprintln!("TW-950 discovery iteration failed; retrying: {error:#}");
        }
        if !args.watch {
            break;
        }
        tokio::time::sleep(Duration::from_secs(args.interval_seconds)).await;
    }
    Ok(())
}

async fn discover() -> anyhow::Result<Vec<RadioDiscoveryObservation>> {
    let observed_at_ms = now_unix_ms();
    let mut discoveries = Vec::new();
    for neighbor in system_neighbors().await? {
        if let Some(discovery) = discovery_from_neighbor(&neighbor, observed_at_ms).await {
            discoveries.push(discovery);
        }
    }
    reduce_radio_discoveries(discoveries, observed_at_ms, RadioDiscoveryPolicy::default())
        .context("reducing TW-950 discovery observations")
}

async fn discovery_from_neighbor(
    neighbor: &NeighborEntry,
    observed_at_ms: u64,
) -> Option<RadioDiscoveryObservation> {
    let mac = normalize_mac(&neighbor.link_layer_address)?;
    if !is_trellisware_mac(&mac) || neighbor_state_is_inactive(neighbor.state.as_ref()) {
        return None;
    }
    let ip: IpAddr = neighbor.ip_address.parse().ok()?;
    let observed_reachable = port_reachable(ip, neighbor.interface_index, 443).await;
    let observed_endpoint = RadioManagementEndpoint {
        address: ip.to_string(),
        port: 443,
        interface: neighbor.interface_alias.clone(),
        interface_index: neighbor.interface_index,
    };
    let mut endpoints = Vec::new();
    let mut link_local_reachable = false;
    if let Some(link_local) = eui64_link_local(&mac) {
        if ip != IpAddr::V6(link_local) {
            link_local_reachable =
                port_reachable(IpAddr::V6(link_local), neighbor.interface_index, 443).await;
            let link_local_endpoint = RadioManagementEndpoint {
                address: link_local.to_string(),
                port: 443,
                interface: neighbor.interface_alias.clone(),
                interface_index: neighbor.interface_index,
            };
            if link_local_reachable {
                endpoints.push(link_local_endpoint);
            } else {
                endpoints.push(observed_endpoint.clone());
                endpoints.push(link_local_endpoint);
            }
        }
    }
    if endpoints.is_empty() || link_local_reachable {
        endpoints.push(observed_endpoint);
    }
    let reachable = observed_reachable || link_local_reachable;
    let vendor = RadioVendorId::trellisware();
    let observation = RadioDiscoveryObservation {
        schema_version: RADIO_DISCOVERY_SCHEMA_VERSION,
        observed_at_ms,
        source: stable_radio_source(&vendor, &mac).ok()?,
        vendor,
        model_hint: "tw-950".into(),
        mac_address: mac,
        serial_number: None,
        hostname: None,
        reachability: if reachable {
            RadioReachabilityStatus::Reachable
        } else {
            RadioReachabilityStatus::Unreachable
        },
        // A TCP handshake proves only reachability. Authentication requirements
        // must come from an actual TLS/application-layer exchange.
        management_authentication: RadioManagementAuthentication::Unknown,
        management_endpoints: endpoints,
        discovery_methods: vec![
            RadioDiscoveryMethod::NeighborTable,
            RadioDiscoveryMethod::Oui,
            RadioDiscoveryMethod::TcpReachability,
        ],
        error_code: None,
    };
    observation.validate().ok()?;
    Some(observation)
}

async fn stimulate_neighbors(addresses: &[IpAddr]) {
    for address in addresses {
        let _ = port_reachable(*address, None, 443).await;
    }
}

async fn port_reachable(address: IpAddr, interface_index: Option<u32>, port: u16) -> bool {
    let address = match address {
        IpAddr::V6(address) if address.is_unicast_link_local() => {
            SocketAddr::new(IpAddr::V6(address), port).set_scope_id(interface_index.unwrap_or(0))
        }
        address => SocketAddr::new(address, port),
    };
    tokio::time::timeout(Duration::from_millis(800), TcpStream::connect(address))
        .await
        .is_ok_and(|result| result.is_ok())
}

trait ScopeId {
    fn set_scope_id(self, scope_id: u32) -> Self;
}

impl ScopeId for SocketAddr {
    fn set_scope_id(self, scope_id: u32) -> Self {
        match self {
            SocketAddr::V6(mut address) => {
                address.set_scope_id(scope_id);
                SocketAddr::V6(address)
            }
            address => address,
        }
    }
}

async fn system_neighbors() -> anyhow::Result<Vec<NeighborEntry>> {
    #[cfg(target_os = "windows")]
    {
        let output = Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "@(Get-NetNeighbor -AddressFamily IPv4,IPv6 | Select-Object IPAddress,LinkLayerAddress,InterfaceAlias,InterfaceIndex,State) | ConvertTo-Json -Compress",
            ])
            .output()
            .await
            .context("reading the Windows neighbor table")?;
        if !output.status.success() {
            bail!("Windows neighbor-table query failed");
        }
        parse_neighbor_json(&output.stdout).context("parsing the Windows neighbor table")
    }
    #[cfg(target_os = "linux")]
    {
        let output = Command::new("ip")
            .args(["-json", "neigh", "show"])
            .output()
            .await
            .context("reading the Linux neighbor table")?;
        if !output.status.success() {
            bail!("Linux neighbor-table query failed");
        }
        parse_neighbor_json(&output.stdout).context("parsing the Linux neighbor table")
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        bail!("radio neighbor discovery is not implemented on this operating system")
    }
}

fn parse_neighbor_json(encoded: &[u8]) -> serde_json::Result<Vec<NeighborEntry>> {
    let value: serde_json::Value = serde_json::from_slice(encoded)?;
    match value {
        serde_json::Value::Array(_) => serde_json::from_value(value),
        serde_json::Value::Object(_) => Ok(vec![serde_json::from_value(value)?]),
        serde_json::Value::Null => Ok(Vec::new()),
        other => serde_json::from_value(other),
    }
}

fn normalize_mac(value: &str) -> Option<String> {
    let hex = value
        .bytes()
        .filter(|byte| byte.is_ascii_hexdigit())
        .map(|byte| (byte as char).to_ascii_lowercase())
        .collect::<String>();
    if hex.len() != 12 {
        return None;
    }
    Some(
        (0..6)
            .map(|index| &hex[index * 2..index * 2 + 2])
            .collect::<Vec<_>>()
            .join(":"),
    )
}

fn is_trellisware_mac(mac: &str) -> bool {
    let compact = mac.replace(':', "");
    TRELLISWARE_OUIS.iter().any(|oui| compact.starts_with(oui))
}

fn neighbor_state_is_inactive(state: Option<&NeighborState>) -> bool {
    state.is_some_and(|state| match state {
        NeighborState::Name(state) => matches!(
            state.to_ascii_lowercase().as_str(),
            "unreachable" | "incomplete" | "failed"
        ),
        NeighborState::Names(states) => states.iter().any(|state| {
            matches!(
                state.to_ascii_lowercase().as_str(),
                "unreachable" | "incomplete" | "failed"
            )
        }),
        NeighborState::Code(state) => matches!(state, 0 | 1),
    })
}

fn eui64_link_local(mac: &str) -> Option<Ipv6Addr> {
    let bytes = mac
        .split(':')
        .map(|part| u8::from_str_radix(part, 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if bytes.len() != 6 {
        return None;
    }
    let interface = [
        bytes[0] ^ 0x02,
        bytes[1],
        bytes[2],
        0xff,
        0xfe,
        bytes[3],
        bytes[4],
        bytes[5],
    ];
    Some(Ipv6Addr::new(
        0xfe80,
        0,
        0,
        0,
        u16::from_be_bytes([interface[0], interface[1]]),
        u16::from_be_bytes([interface[2], interface[3]]),
        u16::from_be_bytes([interface[4], interface[5]]),
        u16::from_be_bytes([interface[6], interface[7]]),
    ))
}

fn emit(discoveries: &[RadioDiscoveryObservation], output: Option<&Path>) -> anyhow::Result<()> {
    let encoded = serde_json::to_string_pretty(discoveries)?;
    if let Some(path) = output {
        atomic_write(path, format!("{encoded}\n").as_bytes())?;
    } else {
        println!("{encoded}");
    }
    Ok(())
}

fn atomic_write(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("creating temporary output beside {}", path.display()))?;
    std::io::Write::write_all(&mut temporary, contents)
        .with_context(|| format!("writing temporary output for {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replacing {} atomically", path.display()))?;
    Ok(())
}

async fn open_zenoh(endpoint: &str) -> anyhow::Result<zenoh::Session> {
    let mut config = zenoh::Config::default();
    config
        .insert_json5("mode", r#""client""#)
        .map_err(|error| anyhow::anyhow!("zenoh mode: {error}"))?;
    config
        .insert_json5("connect/endpoints", &format!(r#"["{endpoint}"]"#))
        .map_err(|error| anyhow::anyhow!("zenoh endpoint: {error}"))?;
    config
        .insert_json5("scouting/multicast/enabled", "false")
        .map_err(|error| anyhow::anyhow!("zenoh multicast: {error}"))?;
    config
        .insert_json5("scouting/gossip/enabled", "false")
        .map_err(|error| anyhow::anyhow!("zenoh gossip: {error}"))?;
    zenoh::open(config)
        .await
        .map_err(|error| anyhow::anyhow!("opening Zenoh: {error}"))
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_single_neighbor_and_normalizes_mac() {
        let encoded = br#"{"IPAddress":"10.1.0.2","LinkLayerAddress":"00-1E-3F-20-9A-10","InterfaceAlias":"Ethernet 2","InterfaceIndex":6,"State":"Reachable"}"#;
        let entries = parse_neighbor_json(encoded).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            normalize_mac(&entries[0].link_layer_address).as_deref(),
            Some("00:1e:3f:20:9a:10")
        );
        assert!(is_trellisware_mac("00:1e:3f:20:9a:10"));
    }

    #[test]
    fn parses_linux_iproute2_neighbor_shape() {
        let encoded = br#"[{"dst":"10.1.0.2","dev":"eth0","lladdr":"00:1e:3f:20:9a:10","state":["REACHABLE"]}]"#;
        let entries = parse_neighbor_json(encoded).unwrap();
        assert_eq!(entries[0].ip_address, "10.1.0.2");
        assert_eq!(entries[0].link_layer_address, "00:1e:3f:20:9a:10");
        assert!(!neighbor_state_is_inactive(entries[0].state.as_ref()));
    }

    #[test]
    fn derives_the_unique_link_local_address_seen_in_the_chud_capture() {
        assert_eq!(
            eui64_link_local("00:1e:3f:20:9a:10").unwrap().to_string(),
            "fe80::21e:3fff:fe20:9a10"
        );
    }

    #[test]
    fn rejects_unrelated_and_inactive_neighbors() {
        assert!(!is_trellisware_mac("c4:7c:8d:a1:5d:23"));
        assert!(neighbor_state_is_inactive(Some(&NeighborState::Name(
            "Unreachable".into()
        ))));
        assert!(!neighbor_state_is_inactive(Some(&NeighborState::Name(
            "Stale".into()
        ))));
        assert!(neighbor_state_is_inactive(Some(&NeighborState::Code(0))));
        assert!(!neighbor_state_is_inactive(Some(&NeighborState::Code(5))));
    }
}
