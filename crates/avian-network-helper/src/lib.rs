//! Root-configured Linux network resources. The application and packet engine stay unprivileged.
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
};
use thiserror::Error;

#[cfg(target_os = "linux")]
pub mod client;
#[cfg(all(target_os = "linux", feature = "server"))]
pub mod server;
#[cfg(target_os = "linux")]
mod transport;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAXIMUM_PACKET_BYTES: usize = 4096;

#[derive(Debug, Error)]
pub enum HelperError {
    #[error("network helper configuration rejected: {0}")]
    Configuration(&'static str),
    #[error("network helper operation rejected: {0}")]
    Rejected(&'static str),
    #[error("network helper request was denied")]
    Denied,
    #[error("network helper protocol is invalid")]
    Protocol,
    #[error("network helper is supported only on Linux")]
    Unsupported,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, HelperError>;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperConfiguration {
    pub allowed_uid: u32,
    pub runtime_directory: PathBuf,
    pub node_id: String,
    pub tun_name: String,
    pub overlay_address: Ipv4Addr,
    pub overlay_network: Ipv4Net,
    pub mtu: u16,
    pub listen_port: u16,
    pub allowed_underlay_interfaces: Vec<String>,
}
impl HelperConfiguration {
    pub fn validate(&self) -> Result<()> {
        if self.allowed_uid == 0 || self.allowed_uid > i32::MAX as u32 {
            return Err(HelperError::Configuration(
                "application UID must be unprivileged",
            ));
        }
        if self.node_id.len() != 64
            || !self
                .node_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(HelperError::Configuration("invalid node identity"));
        }
        if !valid_interface_name(&self.tun_name) || self.tun_name == "lo" {
            return Err(HelperError::Configuration("invalid owned interface name"));
        }
        let private_networks = [
            Ipv4Net::new(Ipv4Addr::new(10, 0, 0, 0), 8).unwrap(),
            Ipv4Net::new(Ipv4Addr::new(172, 16, 0, 0), 12).unwrap(),
            Ipv4Net::new(Ipv4Addr::new(192, 168, 0, 0), 16).unwrap(),
        ];
        if self.overlay_network.addr() != self.overlay_network.network()
            || !private_networks.iter().any(|network| {
                network.contains(&self.overlay_network.network())
                    && network.contains(&self.overlay_network.broadcast())
            })
            || !self.overlay_network.contains(&self.overlay_address)
            || self.overlay_address == self.overlay_network.network()
            || self.overlay_address == self.overlay_network.broadcast()
        {
            return Err(HelperError::Configuration(
                "entire overlay and host address must stay inside RFC1918",
            ));
        }
        if self.listen_port < 1024
            || !(1280..=9000).contains(&self.mtu)
            || !self.runtime_directory.is_absolute()
            || self.runtime_directory.as_os_str().len() > 70
        {
            return Err(HelperError::Configuration(
                "invalid MTU or helper directory",
            ));
        }
        let names: BTreeSet<_> = self.allowed_underlay_interfaces.iter().collect();
        if names.is_empty()
            || names.len() != self.allowed_underlay_interfaces.len()
            || names
                .iter()
                .any(|name| !valid_interface_name(name) || *name == "lo" || **name == self.tun_name)
        {
            return Err(HelperError::Configuration(
                "underlays must be explicitly approved ordinary interface names",
            ));
        }
        Ok(())
    }
    pub fn socket_path(&self) -> PathBuf {
        self.runtime_directory.join("network.sock")
    }
}

pub fn valid_interface_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 15
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SocketTransport {
    Tcp,
    Udp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    PrepareNetwork {},
    ReleaseNetwork {},
    Status {},
    OpenSocket {
        transport: SocketTransport,
        interface_name: String,
        source_address: IpAddr,
        destination: SocketAddr,
    },
    OpenListener {
        transport: SocketTransport,
        interface_name: String,
        source_address: IpAddr,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestPacket {
    pub version: u8,
    pub request: Request,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkStatus {
    pub node_id: String,
    pub active: bool,
    pub tun_name: String,
    pub overlay_address: Ipv4Addr,
    pub overlay_network: Ipv4Net,
    pub mtu: u16,
    pub listen_port: u16,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponsePacket {
    pub version: u8,
    pub allowed: bool,
    pub status: Option<NetworkStatus>,
    pub error: Option<String>,
    pub descriptor: bool,
}
#[cfg(target_os = "linux")]
impl From<rustix::io::Errno> for HelperError {
    fn from(error: rustix::io::Errno) -> Self {
        Self::Io(error.into())
    }
}
