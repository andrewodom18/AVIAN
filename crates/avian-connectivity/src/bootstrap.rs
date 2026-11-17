//! Address discovery supplies hints. Only engine membership authentication admits connections.
use crate::{
    BootstrapConfiguration, ConnectivityError, DiscoveryCache, DiscoveryMessage, Endpoint,
    Metering, Underlay,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};
use url::Url;

const EVENT_BUDGET: usize = 256;
pub const DISCOVERY_PORT: u16 = 42777;
pub const DISCOVERY_GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 77, 71);

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BoundConnector {
    pub interface_name: String,
    pub source_address: IpAddr,
    pub endpoint: Endpoint,
    pub engine_url: String,
    pub restrict_best_effort: bool,
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct BootstrapDelta {
    pub added: Vec<BoundConnector>,
    pub removed: Vec<BoundConnector>,
}
#[derive(Clone, Debug, Serialize)]
pub struct BootstrapEvent {
    pub cursor: u64,
    pub added: Vec<BoundConnector>,
    pub removed: Vec<BoundConnector>,
}
#[derive(Clone, Debug, Serialize)]
pub struct BootstrapEvents {
    pub next_cursor: u64,
    pub resync_required: bool,
    pub events: Vec<BootstrapEvent>,
}

pub struct BootstrapCoordinator {
    configuration: BootstrapConfiguration,
    hints: DiscoveryCache,
    connection_budget: usize,
    desired: BTreeMap<String, BoundConnector>,
    cursor: u64,
    events: VecDeque<BootstrapEvent>,
}
impl BootstrapCoordinator {
    pub fn new(
        configuration: BootstrapConfiguration,
        network_id: String,
        local_hint_budget: usize,
        connection_budget: usize,
    ) -> Result<Self, ConnectivityError> {
        configuration.validate()?;
        if connection_budget == 0 {
            return Err(ConnectivityError::CacheFull);
        }
        Ok(Self {
            configuration,
            hints: DiscoveryCache::new(network_id, local_hint_budget)?,
            connection_budget,
            desired: BTreeMap::new(),
            cursor: 0,
            events: VecDeque::new(),
        })
    }

    pub fn accept_hint(
        &mut self,
        payload: &[u8],
        source: IpAddr,
        underlay: &Underlay,
        now_ms: u64,
    ) -> Result<(), ConnectivityError> {
        if !self.configuration.local_discovery {
            return Err(ConnectivityError::InvalidDiscovery);
        }
        self.hints.accept(payload, source, underlay, now_ms)
    }

    pub fn desired(&self) -> impl Iterator<Item = &BoundConnector> {
        self.desired.values()
    }

    pub fn events_since(&self, after_cursor: u64) -> BootstrapEvents {
        let resync_required = after_cursor > self.cursor
            || self
                .events
                .front()
                .is_some_and(|event| after_cursor.saturating_add(1) < event.cursor);
        BootstrapEvents {
            next_cursor: self.cursor,
            resync_required,
            events: if resync_required {
                Vec::new()
            } else {
                self.events
                    .iter()
                    .filter(|event| event.cursor > after_cursor)
                    .cloned()
                    .collect()
            },
        }
    }

    pub fn reconcile(
        &mut self,
        underlays: &[Underlay],
        overlay_addresses: &[IpAddr],
        now_ms: u64,
    ) -> Result<BootstrapDelta, ConnectivityError> {
        let mut names = BTreeSet::new();
        let mut address_owners = BTreeMap::<IpAddr, BTreeSet<&str>>::new();
        for underlay in underlays {
            underlay.validate()?;
            if !names.insert(underlay.interface_name.as_str()) {
                return Err(ConnectivityError::InvalidInterface);
            }
            if underlay.available {
                for address in &underlay.addresses {
                    address_owners
                        .entry(*address)
                        .or_default()
                        .insert(&underlay.interface_name);
                }
            }
        }
        let owned: BTreeSet<_> = self.configuration.owned_endpoints.iter().cloned().collect();
        let hints = if self.configuration.local_discovery {
            self.hints.endpoints(now_ms)
        } else {
            BTreeSet::new()
        };
        let candidates = owned.iter().map(|endpoint| (endpoint, true)).chain(
            hints
                .iter()
                .filter(|endpoint| !owned.contains(*endpoint))
                .map(|endpoint| (endpoint, false)),
        );
        let mut source_sets = Vec::new();
        for underlay in underlays.iter().filter(|underlay| underlay.available) {
            let mut sources = underlay.addresses.iter().copied().filter(|address|
                !address.is_loopback() && !address.is_multicast() && !address.is_unspecified()
                && !overlay_addresses.contains(address)
                && !matches!(address, IpAddr::V6(address) if address.is_unicast_link_local())
                && address_owners.get(address).is_some_and(|owners| owners.len() == 1)
            ).collect::<Vec<_>>();
            sources.sort();
            sources.dedup();
            source_sets.push((underlay, sources));
        }
        let mut next = BTreeMap::new();
        // Configured seeds have priority over unauthenticated discovery hints. A hint
        // flood must never consume their slots or shut down established owned links.
        for (endpoint, operator_owned) in candidates {
            let parsed =
                Url::parse(endpoint.as_str()).map_err(|_| ConnectivityError::InvalidEndpoint)?;
            let remote = parsed
                .host_str()
                .unwrap_or("")
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .ok();
            for (underlay, sources) in &source_sets {
                if remote.is_some_and(|remote| {
                    remote.is_loopback()
                        || overlay_addresses.contains(&remote)
                        || sources.contains(&remote)
                }) {
                    continue;
                }
                // One source per family avoids multiplying privacy-address connections.
                // DNS seeds get a family-bound connector for each available family.
                for ipv4 in [true, false] {
                    if remote.is_some_and(|remote| remote.is_ipv4() != ipv4) {
                        continue;
                    }
                    let Some(source) = sources.iter().find(|source| source.is_ipv4() == ipv4)
                    else {
                        continue;
                    };
                    if next.len() >= self.connection_budget {
                        if operator_owned {
                            return Err(ConnectivityError::CacheFull);
                        }
                        continue;
                    }
                    let mut bound = parsed.clone();
                    bound
                        .query_pairs_mut()
                        .append_pair("avian_bind", &source.to_string())
                        .append_pair("avian_interface", &underlay.interface_name);
                    let engine_url: String = bound.into();
                    let metering = self
                        .configuration
                        .metering
                        .get(&underlay.interface_name)
                        .copied()
                        .unwrap_or(underlay.metering);
                    next.insert(
                        engine_url.clone(),
                        BoundConnector {
                            interface_name: underlay.interface_name.clone(),
                            source_address: *source,
                            endpoint: endpoint.clone(),
                            engine_url,
                            restrict_best_effort: metering != Metering::Unmetered,
                        },
                    );
                }
            }
        }
        let delta = BootstrapDelta {
            added: next
                .iter()
                .filter(|(url, connector)| self.desired.get(*url) != Some(*connector))
                .map(|(_, connector)| connector.clone())
                .collect(),
            removed: self
                .desired
                .iter()
                .filter(|(url, connector)| next.get(*url) != Some(*connector))
                .map(|(_, connector)| connector.clone())
                .collect(),
        };
        if !delta.added.is_empty() || !delta.removed.is_empty() {
            self.cursor = self
                .cursor
                .checked_add(1)
                .ok_or(ConnectivityError::CacheFull)?;
            self.events.push_back(BootstrapEvent {
                cursor: self.cursor,
                added: delta.added.clone(),
                removed: delta.removed.clone(),
            });
            if self.events.len() > EVENT_BUDGET {
                self.events.pop_front();
            }
        }
        self.desired = next;
        Ok(delta)
    }
}

/// IPv4 multicast is local-link only. IPv6 local discovery is intentionally unsupported;
/// provisioned IPv6 endpoints still receive family-bound connectors from the coordinator.
pub struct DiscoverySocket {
    socket: tokio::net::UdpSocket,
    underlay: Underlay,
    source_address: Ipv4Addr,
    last_advertisement: Option<std::time::Instant>,
}
impl DiscoverySocket {
    pub fn bind(underlay: Underlay, source: Ipv4Addr) -> std::io::Result<Self> {
        underlay
            .validate()
            .map_err(|_| std::io::Error::other("invalid discovery interface"))?;
        if !underlay.available
            || source.is_loopback()
            || source.is_unspecified()
            || !underlay.addresses.contains(&IpAddr::V4(source))
        {
            return Err(std::io::Error::other(
                "discovery requires an available explicit IPv4 interface",
            ));
        }
        let socket = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )?;
        socket.set_reuse_address(true)?;
        socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)).into())?;
        socket.join_multicast_v4(&DISCOVERY_GROUP, &source)?;
        socket.set_multicast_if_v4(&source)?;
        socket.set_multicast_ttl_v4(1)?;
        socket.set_multicast_loop_v4(false)?;
        #[cfg(target_os = "linux")]
        socket.set_multicast_all_v4(false)?;
        socket.set_nonblocking(true)?;
        let socket = tokio::net::UdpSocket::from_std(socket.into())?;
        Ok(Self {
            socket,
            underlay,
            source_address: source,
            last_advertisement: None,
        })
    }
    pub fn source_address(&self) -> Ipv4Addr {
        self.source_address
    }
    pub fn underlay(&self) -> &Underlay {
        &self.underlay
    }
    pub async fn advertise(&mut self, message: &DiscoveryMessage) -> std::io::Result<bool> {
        if self
            .last_advertisement
            .is_some_and(|last| last.elapsed() < Duration::from_secs(5))
        {
            return Ok(false);
        }
        for endpoint in &message.endpoints {
            endpoint
                .validate()
                .map_err(|_| std::io::Error::other("invalid discovery endpoint"))?;
            let parsed = Url::parse(endpoint.as_str()).map_err(std::io::Error::other)?;
            if parsed
                .host_str()
                .and_then(|host| host.parse::<Ipv4Addr>().ok())
                != Some(self.source_address)
            {
                return Err(std::io::Error::other(
                    "discovery may advertise only the selected local source",
                ));
            }
        }
        let payload = serde_json::to_vec(message).map_err(std::io::Error::other)?;
        if payload.len() > 1200 || message.version != 1 || message.endpoints.len() > 8 {
            return Err(std::io::Error::other("invalid discovery advertisement"));
        }
        self.socket
            .send_to(&payload, (DISCOVERY_GROUP, DISCOVERY_PORT))
            .await?;
        self.last_advertisement = Some(std::time::Instant::now());
        Ok(true)
    }
    /// At most 32 packets are drained per poll; oversized packets are discarded, never parsed.
    pub fn poll(&self) -> std::io::Result<Vec<(Vec<u8>, IpAddr)>> {
        let mut received = Vec::new();
        for _ in 0..32 {
            let mut payload = [0u8; 1201];
            match self.socket.try_recv_from(&mut payload) {
                Ok((length, source)) if length <= 1200 => {
                    received.push((payload[..length].to_vec(), source.ip()))
                }
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        Ok(received)
    }
}
