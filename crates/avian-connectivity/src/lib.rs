//! Underlay observations and untrusted discovery hints. Membership remains the trust authority.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};
use thiserror::Error;
use url::Url;

pub mod bootstrap;
pub mod inventory;

#[derive(Debug, Error)]
pub enum ConnectivityError {
    #[error("invalid connection endpoint")]
    InvalidEndpoint,
    #[error("invalid interface observation")]
    InvalidInterface,
    #[error("discovery message rejected")]
    InvalidDiscovery,
    #[error("discovery cache resource budget exhausted")]
    CacheFull,
    #[error("interface inventory is unavailable")]
    InventoryUnavailable,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metering {
    Unmetered,
    Metered,
    #[default]
    Unknown,
}

impl Metering {
    /// An unknown billing policy must not silently authorize paid video or bulk traffic.
    pub fn restrict_best_effort(self) -> bool {
        self != Self::Unmetered
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Underlay {
    pub interface_name: String,
    pub addresses: Vec<IpAddr>,
    pub available: bool,
    pub metering: Metering,
}

impl Underlay {
    pub fn validate(&self) -> Result<(), ConnectivityError> {
        if self.interface_name.is_empty()
            || self.interface_name.len() > 255
            || self.interface_name.chars().any(char::is_control)
            || self.addresses.len() > 64
            || self
                .addresses
                .iter()
                .any(|address| address.is_unspecified() || address.is_multicast())
        {
            return Err(ConnectivityError::InvalidInterface);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Endpoint(String);
impl<'de> Deserialize<'de> for Endpoint {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let encoded = String::deserialize(deserializer)?;
        Self::parse(&encoded).map_err(serde::de::Error::custom)
    }
}

impl Endpoint {
    pub fn parse(value: &str) -> Result<Self, ConnectivityError> {
        if value.len() > 1024 || value.chars().any(char::is_control) {
            return Err(ConnectivityError::InvalidEndpoint);
        }
        let mut parsed = Url::parse(value).map_err(|_| ConnectivityError::InvalidEndpoint)?;
        if !matches!(parsed.scheme(), "tcp" | "udp")
            || parsed.port().is_none_or(|port| port == 0)
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || !matches!(parsed.path(), "" | "/")
        {
            return Err(ConnectivityError::InvalidEndpoint);
        }
        let host = parsed.host_str().unwrap_or("").trim_matches(['[', ']']);
        if let Ok(address) = host.parse::<IpAddr>() {
            if address.is_unspecified() || address.is_multicast() {
                return Err(ConnectivityError::InvalidEndpoint);
            }
        } else if host
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        {
            return Err(ConnectivityError::InvalidEndpoint);
        }
        parsed.set_path("");
        Ok(Self(parsed.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn validate(&self) -> Result<(), ConnectivityError> {
        Self::parse(&self.0).map(|_| ())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapConfiguration {
    #[serde(default)]
    pub owned_endpoints: Vec<Endpoint>,
    #[serde(default = "default_discovery")]
    pub local_discovery: bool,
    #[serde(default)]
    pub metering: BTreeMap<String, Metering>,
}
fn default_discovery() -> bool {
    true
}
impl Default for BootstrapConfiguration {
    fn default() -> Self {
        Self {
            owned_endpoints: Vec::new(),
            local_discovery: true,
            metering: BTreeMap::new(),
        }
    }
}
impl BootstrapConfiguration {
    pub fn validate(&self) -> Result<(), ConnectivityError> {
        for endpoint in &self.owned_endpoints {
            endpoint.validate()?;
        }
        for name in self.metering.keys() {
            if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
                return Err(ConnectivityError::InvalidInterface);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryMessage {
    pub version: u8,
    pub network_id: String,
    pub endpoints: Vec<Endpoint>,
}

struct Hint {
    endpoint: Endpoint,
    expires_at_ms: u64,
}

/// Discovery supplies address hints only. It cannot enroll a peer or grant relay permission.
pub struct DiscoveryCache {
    network_id: String,
    maximum_hints: usize,
    hints: BTreeMap<(IpAddr, Endpoint), Hint>,
}
impl DiscoveryCache {
    pub fn new(network_id: String, maximum_hints: usize) -> Result<Self, ConnectivityError> {
        if network_id.is_empty() || network_id.len() > 128 || maximum_hints == 0 {
            return Err(ConnectivityError::InvalidDiscovery);
        }
        Ok(Self {
            network_id,
            maximum_hints,
            hints: BTreeMap::new(),
        })
    }
    pub fn accept(
        &mut self,
        payload: &[u8],
        source: IpAddr,
        underlay: &Underlay,
        now_ms: u64,
    ) -> Result<(), ConnectivityError> {
        underlay.validate()?;
        if !underlay.available
            || source.is_unspecified()
            || source.is_multicast()
            || payload.len() > 1200
        {
            return Err(ConnectivityError::InvalidDiscovery);
        }
        let message: DiscoveryMessage =
            serde_json::from_slice(payload).map_err(|_| ConnectivityError::InvalidDiscovery)?;
        if message.version != 1
            || message.network_id != self.network_id
            || message.endpoints.is_empty()
            || message.endpoints.len() > 8
        {
            return Err(ConnectivityError::InvalidDiscovery);
        }
        let mut accepted = Vec::new();
        for endpoint in message.endpoints {
            endpoint.validate()?;
            let parsed =
                Url::parse(endpoint.as_str()).map_err(|_| ConnectivityError::InvalidDiscovery)?;
            // A local advertisement may identify only its source. It cannot inject public
            // relay addresses, hostnames, loopback targets, or arbitrary third-party endpoints.
            let advertised: IpAddr = parsed
                .host_str()
                .unwrap_or("")
                .trim_matches(['[', ']'])
                .parse()
                .map_err(|_| ConnectivityError::InvalidDiscovery)?;
            if advertised != source || advertised.is_loopback() {
                return Err(ConnectivityError::InvalidDiscovery);
            }
            accepted.push(endpoint);
        }
        self.hints.retain(|_, hint| hint.expires_at_ms > now_ms);
        let additional = accepted
            .iter()
            .map(|endpoint| (source, endpoint.clone()))
            .collect::<BTreeSet<_>>()
            .iter()
            .filter(|key| !self.hints.contains_key(key))
            .count();
        if self.hints.len().saturating_add(additional) > self.maximum_hints {
            return Err(ConnectivityError::CacheFull);
        }
        let expires_at_ms = now_ms
            .checked_add(30_000)
            .ok_or(ConnectivityError::InvalidDiscovery)?;
        for endpoint in accepted {
            self.hints.insert(
                (source, endpoint.clone()),
                Hint {
                    endpoint,
                    expires_at_ms,
                },
            );
        }
        Ok(())
    }
    pub fn endpoints(&mut self, now_ms: u64) -> BTreeSet<Endpoint> {
        self.hints.retain(|_, hint| hint.expires_at_ms > now_ms);
        self.hints
            .values()
            .map(|hint| hint.endpoint.clone())
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ConnectionDelta {
    pub added: Vec<Underlay>,
    pub removed: Vec<String>,
    pub changed: Vec<Underlay>,
}

#[derive(Default)]
pub struct ConnectionInventory {
    interfaces: BTreeMap<String, Underlay>,
}
impl ConnectionInventory {
    pub fn update(
        &mut self,
        observations: Vec<Underlay>,
        owned_interface: &str,
    ) -> Result<ConnectionDelta, ConnectivityError> {
        let mut next = BTreeMap::new();
        for mut observation in observations {
            observation.validate()?;
            if observation.interface_name == owned_interface {
                continue;
            }
            observation
                .addresses
                .retain(|address| !address.is_loopback());
            observation.addresses.sort();
            observation.addresses.dedup();
            if next
                .insert(observation.interface_name.clone(), observation)
                .is_some()
            {
                return Err(ConnectivityError::InvalidInterface);
            }
        }
        let added = next
            .iter()
            .filter(|(name, _)| !self.interfaces.contains_key(*name))
            .map(|(_, value)| value.clone())
            .collect();
        let changed = next
            .iter()
            .filter(|(name, value)| {
                self.interfaces
                    .get(*name)
                    .is_some_and(|previous| previous != *value)
            })
            .map(|(_, value)| value.clone())
            .collect();
        let removed = self
            .interfaces
            .keys()
            .filter(|name| !next.contains_key(*name))
            .cloned()
            .collect();
        self.interfaces = next;
        Ok(ConnectionDelta {
            added,
            removed,
            changed,
        })
    }
    pub fn interfaces(&self) -> impl Iterator<Item = &Underlay> {
        self.interfaces.values()
    }
}
