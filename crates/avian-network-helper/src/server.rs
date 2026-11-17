use crate::{
    HelperConfiguration, HelperError, NetworkStatus, PROTOCOL_VERSION, Request, RequestPacket,
    ResponsePacket, Result, SocketTransport, transport,
};
use rustix::net::sockopt::Timeout;
use rustix::{
    fd::{AsFd, OwnedFd},
    net::{self, AddressFamily, SocketAddrUnix, SocketFlags, SocketType},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::Read,
    net::{IpAddr, SocketAddr},
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt},
    path::Path,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const IP_TOOL: &str = "/usr/sbin/ip";
const JOURNAL_VERSION: u8 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespaceIdentity {
    device: u64,
    inode: u64,
}
impl NamespaceIdentity {
    fn read(path: &str) -> Result<Self> {
        let metadata = fs::metadata(path)?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LeaseOwner {
    pid: i32,
    uid: u32,
    boot_id: String,
    started_at_ticks: u64,
    helper_pid_namespace: NamespaceIdentity,
    helper_network_namespace: NamespaceIdentity,
}
impl LeaseOwner {
    fn capture(pid: rustix::process::Pid, uid: u32) -> Result<Self> {
        let pid = pid.as_raw_pid();
        let status = read_process_file(&format!("/proc/{pid}/status"))?;
        let identities = status
            .lines()
            .find_map(|line| line.strip_prefix("Uid:"))
            .ok_or(HelperError::Protocol)?;
        let identities = identities
            .split_whitespace()
            .map(str::parse::<u32>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| HelperError::Protocol)?;
        if identities.len() != 4 || identities.iter().any(|identity| *identity != uid) {
            return Err(HelperError::Denied);
        }
        let stat = read_process_file(&format!("/proc/{pid}/stat"))?;
        let started_at_ticks = process_start_ticks(&stat, pid)?;
        let boot_id = read_process_file("/proc/sys/kernel/random/boot_id")?
            .trim()
            .to_owned();
        if boot_id.len() != 36
            || !boot_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(HelperError::Protocol);
        }
        Ok(Self {
            pid,
            uid,
            boot_id,
            started_at_ticks,
            helper_pid_namespace: NamespaceIdentity::read("/proc/self/ns/pid")?,
            helper_network_namespace: NamespaceIdentity::read("/proc/self/ns/net")?,
        })
    }
    fn reopen(&self, allowed_uid: u32) -> Option<(rustix::process::Pid, OwnedFd)> {
        if self.uid != allowed_uid {
            return None;
        }
        let pid = rustix::process::Pid::from_raw(self.pid)?;
        if Self::capture(pid, allowed_uid).ok().as_ref() != Some(self) {
            return None;
        }
        let descriptor =
            rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::NONBLOCK).ok()?;
        // The second capture and live pidfd close the read/open PID-reuse race.
        if Self::capture(pid, allowed_uid).ok().as_ref() != Some(self)
            || process_exited(&descriptor).ok()?
        {
            return None;
        }
        Some((pid, descriptor))
    }
}

fn read_process_file(path: &str) -> Result<String> {
    let mut bytes = String::new();
    File::open(path)?
        .take(64 * 1024 + 1)
        .read_to_string(&mut bytes)?;
    if bytes.len() > 64 * 1024 {
        return Err(HelperError::Protocol);
    }
    Ok(bytes)
}
fn process_start_ticks(stat: &str, expected_pid: i32) -> Result<u64> {
    let opening = stat.find('(').ok_or(HelperError::Protocol)?;
    let closing = stat.rfind(')').ok_or(HelperError::Protocol)?;
    if closing <= opening || stat[..opening].trim().parse::<i32>().ok() != Some(expected_pid) {
        return Err(HelperError::Protocol);
    }
    // Field 22 follows state (field 3), so its offset after the final comm parenthesis is 19.
    stat[closing + 1..]
        .split_whitespace()
        .nth(19)
        .ok_or(HelperError::Protocol)?
        .parse()
        .map_err(|_| HelperError::Protocol)
}
fn process_exited(process: &OwnedFd) -> Result<bool> {
    let mut pending = [rustix::event::PollFd::new(
        process,
        rustix::event::PollFlags::IN,
    )];
    Ok(rustix::event::poll(
        &mut pending,
        Some(&rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        }),
    )? > 0)
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    version: u8,
    node_id: String,
    tun_name: String,
    alias: String,
    interface_index: Option<u32>,
    overlay_address: std::net::Ipv4Addr,
    overlay_network: ipnet::Ipv4Net,
    mtu: u16,
    #[serde(default)]
    listen_port: u16,
    #[serde(default)]
    allowed_underlay_interfaces: Vec<String>,
    #[serde(default)]
    lease_owner: Option<LeaseOwner>,
}

pub struct NetworkHelper {
    configuration: HelperConfiguration,
    ownership: Option<Ownership>,
    lease: Option<(rustix::process::Pid, OwnedFd)>,
    _lock: File,
}
impl NetworkHelper {
    pub fn load(configuration_path: &Path) -> Result<Self> {
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(HelperError::Denied);
        }
        let bytes =
            avian_storage::read_private_file(configuration_path, 64 * 1024).map_err(|_| {
                HelperError::Configuration("configuration must be a root-owned private file")
            })?;
        let configuration: HelperConfiguration = serde_json::from_slice(&bytes)?;
        configuration.validate()?;
        let directory = &configuration.runtime_directory;
        if !directory.exists() {
            avian_storage::create_private_directory(directory).map_err(|_| {
                HelperError::Configuration("cannot create root-owned helper directory")
            })?;
        }
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.permissions().mode() & 0o022 != 0 {
            return Err(HelperError::Configuration(
                "helper directory must be root-owned and not writable by others",
            ));
        }
        let marker = directory.join(".avian-network-helper");
        let expected_marker = format!("AVIAN network helper v1 {}\n", configuration.node_id);
        if marker.exists() {
            let existing = avian_storage::read_private_file(&marker, 256)
                .map_err(|_| HelperError::Configuration("invalid helper directory marker"))?;
            if existing.as_slice() != expected_marker.as_bytes() {
                return Err(HelperError::Configuration(
                    "helper directory belongs to another scope",
                ));
            }
        } else {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let metadata = fs::symlink_metadata(entry.path())?;
                if entry.file_name() != "network.sock"
                    || !metadata.file_type().is_socket()
                    || metadata.uid() != configuration.allowed_uid
                {
                    return Err(HelperError::Configuration(
                        "helper directory must be empty or socket-activated",
                    ));
                }
            }
            avian_storage::create_private_file(&marker, expected_marker.as_bytes())
                .map_err(|_| HelperError::Configuration("could not mark helper directory"))?;
        }
        // The approved UID can traverse to the protected socket, but cannot list or modify this directory.
        fs::set_permissions(directory, fs::Permissions::from_mode(0o711))?;
        let lock = avian_storage::open_private_lock(directory.join("helper.lock"))
            .map_err(|_| HelperError::Configuration("invalid helper lock"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
            .map_err(|_| HelperError::Rejected("another helper owns this configuration"))?;
        let mut helper = Self {
            configuration,
            ownership: None,
            lease: None,
            _lock: lock,
        };
        helper.recover()?;
        Ok(helper)
    }
    pub fn configuration(&self) -> &HelperConfiguration {
        &self.configuration
    }
    fn journal_path(&self) -> std::path::PathBuf {
        self.configuration.runtime_directory.join("network.json")
    }
    fn save_ownership(&self, ownership: &Ownership) -> Result<()> {
        avian_storage::atomic_write_private_file(
            self.journal_path(),
            &serde_json::to_vec(ownership)?,
        )
        .map_err(|_| HelperError::Rejected("ownership journal could not be committed"))
    }
    fn clear_journal(&mut self) -> Result<()> {
        match fs::remove_file(self.journal_path()) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        File::open(&self.configuration.runtime_directory)?.sync_all()?;
        self.ownership = None;
        self.lease = None;
        Ok(())
    }
    fn recover(&mut self) -> Result<()> {
        if self.journal_path().exists() {
            let bytes = avian_storage::read_private_file(self.journal_path(), 16 * 1024)
                .map_err(|_| HelperError::Rejected("invalid recovery journal"))?;
            let ownership: Ownership = serde_json::from_slice(&bytes)?;
            if !matches!(ownership.version, 1 | JOURNAL_VERSION)
                || ownership.node_id != self.configuration.node_id
                || ownership.tun_name != self.configuration.tun_name
                || !ownership
                    .alias
                    .starts_with(&format!("avian:{}:", self.configuration.node_id))
                || ownership.alias.len() != 103
            {
                return Err(HelperError::Rejected(
                    "ownership journal belongs to another scope",
                ));
            }
            let scope_matches = ownership.version == JOURNAL_VERSION
                && ownership.overlay_address == self.configuration.overlay_address
                && ownership.overlay_network == self.configuration.overlay_network
                && ownership.mtu == self.configuration.mtu
                && ownership.listen_port == self.configuration.listen_port
                && ownership.allowed_underlay_interfaces
                    == self.configuration.allowed_underlay_interfaces;
            let owner = ownership
                .lease_owner
                .as_ref()
                .and_then(|owner| owner.reopen(self.configuration.allowed_uid));
            self.ownership = Some(ownership);
            if scope_matches && owner.is_some() && self.status()?.active {
                self.validate_owned_network(self.ownership.as_ref().ok_or(HelperError::Protocol)?)?;
                self.lease = owner;
            } else {
                self.release()?;
            }
        }
        Ok(())
    }
    pub fn status(&self) -> Result<NetworkStatus> {
        let active = if let Some(ownership) = &self.ownership {
            if let Some(link) = self.validate_owned_link(ownership)? {
                let ready = link["mtu"].as_u64() == Some(u64::from(self.configuration.mtu))
                    && link["flags"]
                        .as_array()
                        .is_some_and(|flags| flags.iter().any(|flag| flag.as_str() == Some("UP")));
                let addressed = self
                    .addresses()?
                    .iter()
                    .filter(|interface| interface["ifname"].as_str() == Some(&ownership.tun_name))
                    .any(|interface| {
                        interface["addr_info"].as_array().is_some_and(|addresses| {
                            addresses.iter().any(|address| {
                                address["local"].as_str()
                                    == Some(&self.configuration.overlay_address.to_string())
                                    && address["prefixlen"].as_u64()
                                        == Some(u64::from(
                                            self.configuration.overlay_network.prefix_len(),
                                        ))
                            })
                        })
                    });
                let routed = self.routes()?.iter().any(|route| {
                    route["dev"].as_str() == Some(&ownership.tun_name)
                        && route["dst"].as_str()
                            == Some(&self.configuration.overlay_network.to_string())
                        && route["protocol"].as_str() == Some("static")
                });
                ready && addressed && routed
            } else {
                false
            }
        } else {
            false
        };
        Ok(NetworkStatus {
            node_id: self.configuration.node_id.clone(),
            active,
            tun_name: self.configuration.tun_name.clone(),
            overlay_address: self.configuration.overlay_address,
            overlay_network: self.configuration.overlay_network,
            mtu: self.configuration.mtu,
            listen_port: self.configuration.listen_port,
        })
    }
    fn links(&self) -> Result<Vec<serde_json::Value>> {
        serde_json::from_slice(&ip(&["-details", "-json", "link", "show"])?).map_err(Into::into)
    }
    fn addresses(&self) -> Result<Vec<serde_json::Value>> {
        serde_json::from_slice(&ip(&["-json", "address", "show"])?).map_err(Into::into)
    }
    fn routes(&self) -> Result<Vec<serde_json::Value>> {
        serde_json::from_slice(&ip(&["-json", "-4", "route", "show", "table", "all"])?)
            .map_err(Into::into)
    }
    fn validate_owned_link(&self, ownership: &Ownership) -> Result<Option<serde_json::Value>> {
        let link = self
            .links()?
            .into_iter()
            .find(|link| link["ifname"].as_str() == Some(&ownership.tun_name));
        if let Some(link) = &link
            && (link["ifalias"].as_str() != Some(&ownership.alias)
                || link["ifindex"].as_u64() != ownership.interface_index.map(u64::from)
                || link["linkinfo"]["info_kind"].as_str() != Some("tun"))
        {
            return Err(HelperError::Rejected(
                "interface identity differs from the owned journal",
            ));
        }
        Ok(link)
    }
    fn reject_conflicting_routes(&self) -> Result<()> {
        for route in self.routes()? {
            let Some(destination) = route["dst"].as_str() else {
                continue;
            };
            if destination == "default" {
                continue;
            }
            let network = parse_route(destination)?;
            if network.prefix_len() != 0
                && networks_overlap(network, self.configuration.overlay_network)
            {
                return Err(HelperError::Rejected("overlay overlaps an existing route"));
            }
        }
        for interface in self.addresses()? {
            if let Some(addresses) = interface["addr_info"].as_array() {
                for address in addresses {
                    if let Some(local) = address["local"]
                        .as_str()
                        .and_then(|local| local.parse::<std::net::Ipv4Addr>().ok())
                        && self.configuration.overlay_network.contains(&local)
                    {
                        return Err(HelperError::Rejected(
                            "overlay overlaps an assigned address",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    fn prepare(&mut self, owner: LeaseOwner) -> Result<NetworkStatus> {
        if self.ownership.is_some() && self.status()?.active {
            return self.status();
        }
        if self
            .links()?
            .iter()
            .any(|link| link["ifname"].as_str() == Some(&self.configuration.tun_name))
        {
            return Err(HelperError::Rejected(
                "interface name already exists without owned identity",
            ));
        }
        self.reject_conflicting_routes()?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| HelperError::Rejected("randomness unavailable"))?;
        let suffix = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut ownership = Ownership {
            version: JOURNAL_VERSION,
            node_id: self.configuration.node_id.clone(),
            tun_name: self.configuration.tun_name.clone(),
            alias: format!("avian:{}:{suffix}", self.configuration.node_id),
            interface_index: None,
            overlay_address: self.configuration.overlay_address,
            overlay_network: self.configuration.overlay_network,
            mtu: self.configuration.mtu,
            listen_port: self.configuration.listen_port,
            allowed_underlay_interfaces: self.configuration.allowed_underlay_interfaces.clone(),
            lease_owner: Some(owner),
        };
        self.save_ownership(&ownership)?;
        self.ownership = Some(ownership.clone());
        let outcome = (|| -> Result<()> {
            // The TUN remains nonpersistent until its kernel alias and ifindex are
            // journaled. Process death before that point closes and removes it.
            let mut settings = tun::Configuration::default();
            settings.tun_name(&ownership.tun_name).layer(tun::Layer::L3);
            let mut device = tun::create(&settings)
                .map_err(|_| HelperError::Rejected("could not create scoped TUN"))?;
            ip(&[
                "link",
                "set",
                "dev",
                &ownership.tun_name,
                "alias",
                &ownership.alias,
            ])?;
            let link = self
                .links()?
                .into_iter()
                .find(|link| link["ifname"].as_str() == Some(&ownership.tun_name))
                .ok_or(HelperError::Rejected("created interface disappeared"))?;
            ownership.interface_index = Some(
                u32::try_from(link["ifindex"].as_u64().ok_or(HelperError::Protocol)?)
                    .map_err(|_| HelperError::Protocol)?,
            );
            self.save_ownership(&ownership)?;
            self.ownership = Some(ownership.clone());
            device
                .user(self.configuration.allowed_uid as i32)
                .map_err(|_| HelperError::Rejected("could not assign TUN owner"))?;
            device
                .persist()
                .map_err(|_| HelperError::Rejected("could not persist owned TUN"))?;
            drop(device);
            let ipv6_control = Path::new("/proc/sys/net/ipv6/conf")
                .join(&ownership.tun_name)
                .join("disable_ipv6");
            if ipv6_control.exists() {
                fs::write(ipv6_control, b"1\n")?;
            }
            ip(&[
                "link",
                "set",
                "dev",
                &ownership.tun_name,
                "mtu",
                &ownership.mtu.to_string(),
                "up",
            ])?;
            ip(&[
                "address",
                "add",
                &format!(
                    "{}/{}",
                    ownership.overlay_address,
                    ownership.overlay_network.prefix_len()
                ),
                "dev",
                &ownership.tun_name,
                "noprefixroute",
            ])?;
            ip(&[
                "route",
                "add",
                &ownership.overlay_network.to_string(),
                "dev",
                &ownership.tun_name,
                "proto",
                "static",
                "scope",
                "link",
            ])?;
            Ok(())
        })();
        if let Err(error) = outcome {
            let _ = self.release();
            return Err(error);
        }
        self.status()
    }
    fn validate_owned_network(&self, ownership: &Ownership) -> Result<()> {
        for interface in self
            .addresses()?
            .iter()
            .filter(|interface| interface["ifname"].as_str() == Some(&ownership.tun_name))
        {
            for address in interface["addr_info"].as_array().into_iter().flatten() {
                if address["family"].as_str() != Some("inet")
                    || address["local"].as_str() != Some(&ownership.overlay_address.to_string())
                    || address["prefixlen"].as_u64()
                        != Some(u64::from(ownership.overlay_network.prefix_len()))
                {
                    return Err(HelperError::Rejected(
                        "owned interface now carries an unapproved address",
                    ));
                }
            }
        }
        for route in self
            .routes()?
            .iter()
            .filter(|route| route["dev"].as_str() == Some(&ownership.tun_name))
        {
            let destination = route["dst"].as_str().ok_or(HelperError::Protocol)?;
            let network = parse_route(destination)?;
            let kernel_address_route = route["table"].as_str() == Some("local")
                && route["protocol"].as_str() == Some("kernel")
                && network.prefix_len() == 32
                && ((route["type"].as_str() == Some("local")
                    && network.network() == ownership.overlay_address)
                    || (route["type"].as_str() == Some("broadcast")
                        && network.network() == ownership.overlay_network.broadcast()));
            let overlay_route = network == ownership.overlay_network
                && route["protocol"].as_str() == Some("static")
                && route["scope"].as_str() == Some("link");
            if !overlay_route && !kernel_address_route {
                return Err(HelperError::Rejected(
                    "owned interface now carries an unapproved route",
                ));
            }
        }
        Ok(())
    }
    pub fn release(&mut self) -> Result<NetworkStatus> {
        if let Some(ownership) = &self.ownership
            && self.validate_owned_link(ownership)?.is_some()
        {
            self.validate_owned_network(ownership)?;
            ip(&["link", "delete", "dev", &ownership.tun_name])?;
        }
        self.clear_journal()?;
        self.status()
    }
    fn open_socket(
        &self,
        protocol: SocketTransport,
        interface_name: &str,
        source_address: IpAddr,
        destination: Option<SocketAddr>,
    ) -> Result<OwnedFd> {
        if self.lease.is_none()
            || !self.status()?.active
            || !self
                .configuration
                .allowed_underlay_interfaces
                .iter()
                .any(|name| name == interface_name)
            || interface_name == self.configuration.tun_name
            || source_address.is_loopback()
            || source_address.is_unspecified()
            || source_address.is_multicast()
            || matches!(source_address, IpAddr::V4(address) if self.configuration.overlay_network.contains(&address))
        {
            return Err(HelperError::Rejected(
                "socket lies outside the approved underlay scope",
            ));
        }
        if destination.is_some_and(|destination| source_address.is_ipv4() != destination.is_ipv4()
            || destination.ip().is_loopback() || destination.ip().is_unspecified() || destination.ip().is_multicast()
            || destination.port() < 1024 || matches!(destination.ip(), IpAddr::V4(address) if self.configuration.overlay_network.contains(&address))) {
            return Err(HelperError::Rejected("destination lies outside the permitted transport scope"));
        }
        let addresses = self.addresses()?;
        let owners = addresses
            .iter()
            .filter(|interface| {
                interface["addr_info"].as_array().is_some_and(|addresses| {
                    addresses.iter().any(|address| {
                        address["local"]
                            .as_str()
                            .and_then(|value| value.parse::<IpAddr>().ok())
                            == Some(source_address)
                    })
                })
            })
            .collect::<Vec<_>>();
        if owners.len() != 1
            || owners[0]["ifname"].as_str() != Some(interface_name)
            || !owners[0]["flags"]
                .as_array()
                .is_some_and(|flags| flags.iter().any(|flag| flag.as_str() == Some("UP")))
        {
            return Err(HelperError::Rejected(
                "source is not uniquely assigned to an available approved interface",
            ));
        }
        let socket = socket2::Socket::new(
            if source_address.is_ipv4() {
                socket2::Domain::IPV4
            } else {
                socket2::Domain::IPV6
            },
            match protocol {
                SocketTransport::Tcp => socket2::Type::STREAM,
                SocketTransport::Udp => socket2::Type::DGRAM,
            },
            Some(match protocol {
                SocketTransport::Tcp => socket2::Protocol::TCP,
                SocketTransport::Udp => socket2::Protocol::UDP,
            }),
        )?;
        if source_address.is_ipv6() {
            socket.set_only_v6(true)?;
        }
        if destination.is_none() && protocol == SocketTransport::Tcp {
            // Permit rebinding the fixed listener after accepted streams enter TIME_WAIT.
            // SO_REUSEPORT stays disabled, so simultaneous listeners cannot share it.
            socket.set_reuse_address(true)?;
        }
        socket.bind_device(Some(interface_name.as_bytes()))?;
        socket.bind(
            &SocketAddr::new(
                source_address,
                if destination.is_none() {
                    self.configuration.listen_port
                } else {
                    0
                },
            )
            .into(),
        )?;
        if destination.is_none() && protocol == SocketTransport::Tcp {
            socket.listen(128)?;
        }
        socket.set_nonblocking(true)?;
        if socket
            .local_addr()?
            .as_socket()
            .is_none_or(|address| address.port() < 1024)
        {
            return Err(HelperError::Rejected(
                "kernel selected a privileged source port",
            ));
        }
        Ok(socket.into())
    }
    pub fn serve(&mut self, stop: &AtomicBool, activated_stdin: bool) -> Result<()> {
        let listener = if activated_stdin {
            self.activated_listener()?
        } else {
            self.bind_listener()?
        };
        net::sockopt::set_socket_timeout(
            &listener,
            Timeout::Recv,
            Some(Duration::from_millis(250)),
        )?;
        while !stop.load(Ordering::SeqCst) {
            self.reap_dead_owner()?;
            let client = match net::accept_with(&listener, SocketFlags::CLOEXEC) {
                Ok(client) => client,
                Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            net::sockopt::set_socket_timeout(
                &client,
                Timeout::Recv,
                Some(Duration::from_millis(250)),
            )?;
            net::sockopt::set_socket_timeout(
                &client,
                Timeout::Send,
                Some(Duration::from_millis(250)),
            )?;
            let response = self.handle_client(&client);
            if let Err(error) = response {
                let packet = ResponsePacket {
                    version: PROTOCOL_VERSION,
                    allowed: false,
                    status: None,
                    error: Some(error.to_string()),
                    descriptor: false,
                };
                let _ = transport::send(&client, &serde_json::to_vec(&packet)?, None);
            }
        }
        self.release()?;
        if !activated_stdin {
            fs::remove_file(self.configuration.socket_path())?;
        }
        Ok(())
    }
    fn reap_dead_owner(&mut self) -> Result<()> {
        let expired = self
            .lease
            .as_ref()
            .map(|(_, process)| process_exited(process))
            .transpose()?
            .unwrap_or(false);
        if expired {
            self.release()?;
        }
        Ok(())
    }
    fn handle_client(&mut self, client: &OwnedFd) -> Result<()> {
        let credentials = net::sockopt::socket_peercred(client)?;
        if credentials.uid.as_raw() != self.configuration.allowed_uid {
            return Err(HelperError::Denied);
        }
        let (payload, descriptors) = transport::receive(client)?;
        if !descriptors.is_empty() {
            return Err(HelperError::Protocol);
        }
        let packet: RequestPacket = serde_json::from_slice(&payload)?;
        if packet.version != PROTOCOL_VERSION {
            return Err(HelperError::Protocol);
        }
        let descriptor = match packet.request {
            Request::PrepareNetwork {} => {
                if self
                    .lease
                    .as_ref()
                    .is_some_and(|(owner, _)| *owner != credentials.pid)
                {
                    return Err(HelperError::Denied);
                }
                let process = rustix::process::pidfd_open(
                    credentials.pid,
                    rustix::process::PidfdFlags::NONBLOCK,
                )?;
                let owner = LeaseOwner::capture(credentials.pid, self.configuration.allowed_uid)?;
                if process_exited(&process)? {
                    return Err(HelperError::Denied);
                }
                self.prepare(owner)?;
                self.lease = Some((credentials.pid, process));
                None
            }
            Request::ReleaseNetwork {} => {
                if self
                    .lease
                    .as_ref()
                    .is_some_and(|(owner, _)| *owner != credentials.pid)
                {
                    return Err(HelperError::Denied);
                }
                self.release()?;
                None
            }
            Request::Status {} => None,
            Request::OpenSocket {
                transport,
                interface_name,
                source_address,
                destination,
            } => Some(self.open_socket(
                transport,
                &interface_name,
                source_address,
                Some(destination),
            )?),
            Request::OpenListener {
                transport,
                interface_name,
                source_address,
            } => Some(self.open_socket(transport, &interface_name, source_address, None)?),
        };
        let packet = ResponsePacket {
            version: PROTOCOL_VERSION,
            allowed: true,
            status: Some(self.status()?),
            error: None,
            descriptor: descriptor.is_some(),
        };
        transport::send(client, &serde_json::to_vec(&packet)?, descriptor.as_ref())
    }
    fn activated_listener(&self) -> Result<OwnedFd> {
        let descriptor = rustix::io::dup(std::io::stdin().as_fd())?;
        if net::sockopt::socket_type(&descriptor)? != SocketType::SEQPACKET
            || net::sockopt::socket_domain(&descriptor)? != AddressFamily::UNIX
            || !net::sockopt::socket_acceptconn(&descriptor)?
        {
            return Err(HelperError::Rejected(
                "activation must supply the configured listening socket",
            ));
        }
        let address = net::getsockname(&descriptor)?;
        let expected = SocketAddrUnix::new(self.configuration.socket_path())?;
        if address != expected.into() {
            return Err(HelperError::Rejected(
                "activation socket path differs from configuration",
            ));
        }
        Ok(descriptor)
    }
    fn bind_listener(&self) -> Result<OwnedFd> {
        let path = self.configuration.socket_path();
        if let Ok(metadata) = fs::symlink_metadata(&path) {
            if !metadata.file_type().is_socket() || metadata.uid() != self.configuration.allowed_uid
            {
                return Err(HelperError::Rejected(
                    "helper socket path is not an owned socket",
                ));
            }
            let probe = net::socket_with(
                AddressFamily::UNIX,
                SocketType::SEQPACKET,
                SocketFlags::CLOEXEC,
                None,
            )?;
            net::sockopt::set_socket_timeout(
                &probe,
                Timeout::Send,
                Some(Duration::from_millis(250)),
            )?;
            match net::connect(&probe, &SocketAddrUnix::new(&path)?) {
                Err(rustix::io::Errno::CONNREFUSED | rustix::io::Errno::NOENT) => {
                    fs::remove_file(&path)?
                }
                _ => {
                    return Err(HelperError::Rejected(
                        "an active process still owns the helper socket",
                    ));
                }
            }
        }
        let socket = net::socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        net::bind(&socket, &SocketAddrUnix::new(&path)?)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        // Manual-root mode only. Production socket activation assigns ownership in systemd.
        rustix::fs::chown(
            &path,
            Some(rustix::process::Uid::from_raw(
                self.configuration.allowed_uid,
            )),
            None,
        )?;
        net::listen(&socket, 32)?;
        Ok(socket)
    }
}
fn parse_route(value: &str) -> Result<ipnet::Ipv4Net> {
    value
        .parse()
        .or_else(|_| {
            value
                .parse::<std::net::Ipv4Addr>()
                .map(|address| ipnet::Ipv4Net::new(address, 32).unwrap())
        })
        .map_err(|_| HelperError::Rejected("cannot interpret an existing route"))
}
fn networks_overlap(first: ipnet::Ipv4Net, second: ipnet::Ipv4Net) -> bool {
    first.contains(&second.network()) || second.contains(&first.network())
}
fn ip(arguments: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new(IP_TOOL)
        .args(arguments)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let output = child.stdout.take().ok_or(HelperError::Protocol)?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        output.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        std::io::Result::Ok(bytes)
    });
    let started = Instant::now();
    let success = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.success(),
            Ok(None) if started.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let output = reader
        .join()
        .map_err(|_| HelperError::Rejected("network query failed"))??;
    if !success || output.len() > 8 * 1024 * 1024 {
        return Err(HelperError::Rejected("bounded network operation failed"));
    }
    Ok(output)
}

#[cfg(test)]
mod recovery_identity_tests {
    use super::*;

    #[test]
    fn process_stat_parser_handles_parentheses_spaces_and_newlines_in_names() {
        let mut fields = vec!["S".to_owned(); 19];
        fields.push("123456".into());
        let stat = format!("42 (odd ) (name\nwith spaces) {}", fields.join(" "));
        assert_eq!(process_start_ticks(&stat, 42).unwrap(), 123456);
        assert!(process_start_ticks(&stat, 41).is_err());
        assert!(process_start_ticks("42 (truncated) S 1", 42).is_err());
    }

    #[test]
    fn reopening_requires_the_exact_boot_start_uid_and_helper_namespaces() {
        let uid = rustix::process::getuid().as_raw();
        let owner = LeaseOwner::capture(rustix::process::getpid(), uid).unwrap();
        assert!(owner.reopen(uid).is_some());
        let mut changed = owner.clone();
        changed.started_at_ticks += 1;
        assert!(changed.reopen(uid).is_none());
        changed = owner.clone();
        changed.boot_id = "00000000-0000-0000-0000-000000000000".into();
        assert!(changed.reopen(uid).is_none());
        changed = owner.clone();
        changed.helper_pid_namespace.inode += 1;
        assert!(changed.reopen(uid).is_none());
        changed = owner.clone();
        changed.helper_network_namespace.inode += 1;
        assert!(changed.reopen(uid).is_none());
        assert!(owner.reopen(uid + 1).is_none());
    }

    #[test]
    fn legacy_journal_has_no_identity_that_could_adopt_a_live_network() {
        let journal = serde_json::json!({"version":1,"node_id":"a".repeat(64),
            "tun_name":"avian0","alias":"legacy-owned-alias","interface_index":7,
            "overlay_address":"10.88.0.1","overlay_network":"10.88.0.0/24","mtu":1320});
        let ownership: Ownership = serde_json::from_value(journal).unwrap();
        assert!(ownership.lease_owner.is_none());
        assert_ne!(ownership.version, JOURNAL_VERSION);
    }
}
