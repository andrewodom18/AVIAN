use crate::{
    HelperError, NetworkStatus, PROTOCOL_VERSION, Request, RequestPacket, ResponsePacket, Result,
    SocketTransport, transport,
};
use rustix::net::sockopt::Timeout;
use rustix::{
    fd::OwnedFd,
    net::{self, AddressFamily, SocketAddrUnix, SocketFlags, SocketType},
};
use std::{
    net::{IpAddr, SocketAddr},
    path::Path,
    time::Duration,
};

pub struct HelperClient {
    socket_path: std::path::PathBuf,
}
impl HelperClient {
    pub fn new(socket_path: impl AsRef<Path>) -> Result<Self> {
        if !socket_path.as_ref().is_absolute() {
            return Err(HelperError::Configuration("helper socket must be absolute"));
        }
        Ok(Self {
            socket_path: socket_path.as_ref().into(),
        })
    }
    fn request(&self, request: Request) -> Result<(ResponsePacket, Option<OwnedFd>)> {
        let socket = net::socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        net::sockopt::set_socket_timeout(&socket, Timeout::Recv, Some(Duration::from_secs(3)))?;
        net::sockopt::set_socket_timeout(&socket, Timeout::Send, Some(Duration::from_secs(3)))?;
        net::connect(&socket, &SocketAddrUnix::new(&self.socket_path)?)?;
        if net::sockopt::socket_peercred(&socket)?.uid.as_raw() != 0 {
            return Err(HelperError::Denied);
        }
        let expected_descriptor = matches!(
            request,
            Request::OpenSocket { .. } | Request::OpenListener { .. }
        );
        let packet = serde_json::to_vec(&RequestPacket {
            version: PROTOCOL_VERSION,
            request,
        })?;
        transport::send(&socket, &packet, None)?;
        let (payload, mut descriptors) = transport::receive(&socket)?;
        let response: ResponsePacket = serde_json::from_slice(&payload)?;
        if response.version != PROTOCOL_VERSION || !response.allowed {
            return Err(HelperError::Denied);
        }
        if response.descriptor != expected_descriptor
            || descriptors.len() != usize::from(expected_descriptor)
        {
            return Err(HelperError::Protocol);
        }
        Ok((response, descriptors.pop()))
    }
    pub fn prepare_network(&self) -> Result<NetworkStatus> {
        self.request(Request::PrepareNetwork {})?
            .0
            .status
            .ok_or(HelperError::Protocol)
    }
    pub fn release_network(&self) -> Result<NetworkStatus> {
        self.request(Request::ReleaseNetwork {})?
            .0
            .status
            .ok_or(HelperError::Protocol)
    }
    pub fn status(&self) -> Result<NetworkStatus> {
        self.request(Request::Status {})?
            .0
            .status
            .ok_or(HelperError::Protocol)
    }
    pub fn open_socket(
        &self,
        transport: SocketTransport,
        interface_name: &str,
        source_address: IpAddr,
        destination: SocketAddr,
    ) -> Result<OwnedFd> {
        let descriptor = self
            .request(Request::OpenSocket {
                transport,
                interface_name: interface_name.into(),
                source_address,
                destination,
            })?
            .1
            .ok_or(HelperError::Protocol)?;
        let expected = match transport {
            SocketTransport::Tcp => SocketType::STREAM,
            SocketTransport::Udp => SocketType::DGRAM,
        };
        verify_descriptor(&descriptor, expected, source_address, interface_name, None)?;
        Ok(descriptor)
    }
    pub fn open_listener(
        &self,
        transport: SocketTransport,
        interface_name: &str,
        source_address: IpAddr,
    ) -> Result<OwnedFd> {
        let status = self.status()?;
        if !status.active {
            return Err(HelperError::Rejected("managed network is inactive"));
        }
        let descriptor = self
            .request(Request::OpenListener {
                transport,
                interface_name: interface_name.into(),
                source_address,
            })?
            .1
            .ok_or(HelperError::Protocol)?;
        let expected = match transport {
            SocketTransport::Tcp => SocketType::STREAM,
            SocketTransport::Udp => SocketType::DGRAM,
        };
        verify_descriptor(
            &descriptor,
            expected,
            source_address,
            interface_name,
            Some(status.listen_port),
        )?;
        if transport == SocketTransport::Tcp && !net::sockopt::socket_acceptconn(&descriptor)? {
            return Err(HelperError::Protocol);
        }
        Ok(descriptor)
    }
}
fn verify_descriptor(
    descriptor: &OwnedFd,
    expected: SocketType,
    source_address: IpAddr,
    interface_name: &str,
    port: Option<u16>,
) -> Result<()> {
    if net::sockopt::socket_type(descriptor)? != expected {
        return Err(HelperError::Protocol);
    }
    let socket = socket2::SockRef::from(descriptor);
    let local = socket
        .local_addr()?
        .as_socket()
        .ok_or(HelperError::Protocol)?;
    if local.ip() != source_address
        || local.port() < 1024
        || port.is_some_and(|port| port != local.port())
        || socket.device()?.as_deref() != Some(interface_name.as_bytes())
    {
        return Err(HelperError::Protocol);
    }
    Ok(())
}
