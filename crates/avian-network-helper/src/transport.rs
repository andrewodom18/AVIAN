use crate::{HelperError, MAXIMUM_PACKET_BYTES, Result};
use rustix::{
    fd::{AsFd, OwnedFd},
    net::{
        self, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
        SendAncillaryBuffer, SendAncillaryMessage, SendFlags,
    },
};
use std::{
    io::{IoSlice, IoSliceMut},
    mem::MaybeUninit,
};

pub(crate) fn send(socket: impl AsFd, payload: &[u8], descriptor: Option<&OwnedFd>) -> Result<()> {
    if payload.len() > MAXIMUM_PACKET_BYTES {
        return Err(HelperError::Protocol);
    }
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    let descriptors = descriptor.map(|descriptor| [descriptor.as_fd()]);
    if let Some(descriptors) = &descriptors
        && !ancillary.push(SendAncillaryMessage::ScmRights(descriptors))
    {
        return Err(HelperError::Protocol);
    }
    let sent = net::sendmsg(
        socket,
        &[IoSlice::new(payload)],
        &mut ancillary,
        SendFlags::NOSIGNAL,
    )?;
    if sent != payload.len() {
        return Err(HelperError::Protocol);
    }
    Ok(())
}

pub(crate) fn receive(socket: impl AsFd) -> Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut bytes = [0u8; MAXIMUM_PACKET_BYTES + 1];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let received = net::recvmsg(
        socket,
        &mut [IoSliceMut::new(&mut bytes)],
        &mut ancillary,
        RecvFlags::CMSG_CLOEXEC,
    )?;
    let mut descriptors = Vec::new();
    for message in ancillary.drain() {
        match message {
            RecvAncillaryMessage::ScmRights(rights) => descriptors.extend(rights),
            _ => return Err(HelperError::Protocol),
        }
    }
    if descriptors.len() > 2
        || received.bytes == 0
        || received.bytes > MAXIMUM_PACKET_BYTES
        || received
            .flags
            .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
    {
        return Err(HelperError::Protocol);
    }
    Ok((bytes[..received.bytes].to_vec(), descriptors))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::net::{AddressFamily, SocketFlags, SocketType};
    #[test]
    fn descriptor_transfer_preserves_owned_socket_and_close_on_exec() {
        let (sender, receiver) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        let original = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let expected = original.local_addr().unwrap();
        let descriptor: OwnedFd = original.into();
        send(&sender, b"approved socket", Some(&descriptor)).unwrap();
        let (payload, mut descriptors) = receive(&receiver).unwrap();
        assert_eq!(payload, b"approved socket");
        assert_eq!(descriptors.len(), 1);
        let transferred = descriptors.pop().unwrap();
        assert!(
            rustix::io::fcntl_getfd(&transferred)
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        drop(descriptor);
        let socket = std::net::UdpSocket::from(transferred);
        assert_eq!(socket.local_addr().unwrap(), expected);
        let receiving = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        receiving
            .set_read_timeout(Some(std::time::Duration::from_secs(1)))
            .unwrap();
        socket
            .send_to(b"owned descriptor", receiving.local_addr().unwrap())
            .unwrap();
        let mut bytes = [0u8; 32];
        let (length, source) = receiving.recv_from(&mut bytes).unwrap();
        assert_eq!(&bytes[..length], b"owned descriptor");
        assert_eq!(source, expected);
    }
    #[test]
    fn truncated_descriptor_messages_close_every_received_descriptor() {
        let (sender, receiver) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        let sockets = (0..4)
            .map(|_| std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
            .collect::<Vec<_>>();
        let addresses = sockets
            .iter()
            .map(|socket| socket.local_addr().unwrap())
            .collect::<Vec<_>>();
        let descriptors = sockets.iter().map(AsFd::as_fd).collect::<Vec<_>>();
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4))];
        let mut ancillary = SendAncillaryBuffer::new(&mut space);
        assert!(ancillary.push(SendAncillaryMessage::ScmRights(&descriptors)));
        net::sendmsg(
            &sender,
            &[IoSlice::new(b"too many sockets")],
            &mut ancillary,
            SendFlags::NOSIGNAL,
        )
        .unwrap();
        assert!(matches!(receive(&receiver), Err(HelperError::Protocol)));
        drop(sockets);
        // Any leaked duplicate would keep its UDP port reserved.
        for address in addresses {
            std::net::UdpSocket::bind(address).unwrap();
        }
    }
    #[test]
    fn oversized_seqpacket_messages_are_rejected_without_partial_parsing() {
        let (sender, receiver) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        rustix::net::send(
            &sender,
            &[0u8; MAXIMUM_PACKET_BYTES + 100],
            SendFlags::NOSIGNAL,
        )
        .unwrap();
        assert!(matches!(receive(&receiver), Err(HelperError::Protocol)));
    }
}
