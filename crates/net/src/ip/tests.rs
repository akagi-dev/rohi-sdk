///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025-2026 Akagi Engineering <admin@akagi.dev>
//
//  Licensed under the Apache License, Version 2.0 (the "License");
//  you may not use this file except in compliance with the License.
//  You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
//  Unless required by applicable law or agreed to in writing, software
//  distributed under the License is distributed on an "AS IS" BASIS,
//  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//  See the License for the specific language governing permissions and
//  limitations under the License.
//

use core::net::{IpAddr, Ipv4Addr, SocketAddr};
use core::pin::pin;

use embassy_futures::{block_on, poll_once};
use embedded_io_async::{Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};

use super::*;
use crate::testing::FakeIp;

const SERVER: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
const REMOTE: SocketAddr = SocketAddr::new(SERVER, 443);
const OTHER: SocketAddr = SocketAddr::new(SERVER, 80);

fn ready() -> FakeIp<2, 64> {
    let ip = FakeIp::new();
    ip.set_ready(true);
    ip.add_host("rohi.example", SERVER);
    ip
}

/// Generic over the standard traits, the way higher layers consume the handle.
async fn fetch<N: Dns<Error = NetError> + TcpConnect<Error = NetError>>(
    net: &N,
    out: &mut [u8],
) -> Result<usize, NetError> {
    let addr = net.get_host_by_name("rohi.example", AddrType::IPv4).await?;
    let mut connection = net.connect(SocketAddr::new(addr, 443)).await?;
    connection.write_all(b"ping").await?;
    connection.read(out).await
}

#[test]
fn not_ready_is_network_unavailable() {
    let ip = FakeIp::<2, 64>::new();
    ip.add_host("rohi.example", SERVER);
    let net = NetworkHandle::new(&ip);
    assert!(!net.is_ready());
    assert_eq!(
        block_on(net.resolve("rohi.example", AddrType::IPv4)),
        Err(NetError::NetworkUnavailable)
    );
    assert_eq!(
        block_on(net.connect(REMOTE)).err(),
        Some(NetError::NetworkUnavailable)
    );
    // Nothing reached the stack.
    assert_eq!(ip.dns_queries(), 0);
    assert_eq!(ip.connect_attempts(), 0);
}

#[test]
fn dns_resolves_and_fails_typed() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    assert_eq!(
        block_on(net.get_host_by_name("rohi.example", AddrType::Either)),
        Ok(SERVER)
    );
    assert_eq!(
        block_on(net.get_host_by_name("unknown.example", AddrType::IPv4)),
        Err(NetError::DnsFailed)
    );
    assert_eq!(
        block_on(net.get_host_by_name("rohi.example", AddrType::IPv6)),
        Err(NetError::DnsFailed)
    );
    assert_eq!(
        block_on(net.get_host_by_name("", AddrType::IPv4)),
        Err(NetError::DnsFailed)
    );
    let mut name = [0u8; 32];
    assert_eq!(
        block_on(net.get_host_by_address(SERVER, &mut name)),
        Err(NetError::DnsFailed)
    );
}

#[test]
fn link_lost_during_dns_is_network_unavailable() {
    let ip = ready();
    ip.hold_dns(true);
    let net = NetworkHandle::new(&ip);
    let mut query = pin!(net.resolve("rohi.example", AddrType::IPv4));
    assert!(poll_once(query.as_mut()).is_pending());
    ip.set_ready(false);
    ip.hold_dns(false);
    assert_eq!(block_on(query), Err(NetError::NetworkUnavailable));
}

#[test]
fn refused_connection_is_connect_failed() {
    let ip = ready();
    ip.refuse(REMOTE);
    let net = NetworkHandle::new(&ip);
    assert_eq!(
        block_on(net.connect(REMOTE)).err(),
        Some(NetError::ConnectFailed)
    );
    assert_eq!(ip.open_connections(), 0);
}

#[test]
fn tcp_round_trip_through_standard_traits() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    let mut out = [0u8; 16];
    let read = block_on(async {
        let mut fetch = pin!(fetch(&net, &mut out));
        assert!(poll_once(fetch.as_mut()).is_pending());
        assert_eq!(ip.peer_received(REMOTE).as_slice(), b"ping");
        ip.peer_send(REMOTE, b"pong");
        fetch.await
    });
    assert_eq!(read, Ok(4));
    assert_eq!(&out[..4], b"pong");
    assert_eq!(
        ip.open_connections(),
        0,
        "dropped connection frees its slot"
    );
}

#[test]
fn peer_close_and_reset() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    let mut buf = [0u8; 8];

    let mut connection = block_on(net.connect(REMOTE)).unwrap();
    ip.peer_send(REMOTE, b"bye");
    ip.peer_close(REMOTE);
    assert_eq!(block_on(connection.read(&mut buf)), Ok(3));
    assert_eq!(block_on(connection.read(&mut buf)), Ok(0));
    drop(connection);

    let mut connection = block_on(net.connect(REMOTE)).unwrap();
    ip.peer_reset(REMOTE);
    assert_eq!(
        block_on(connection.read(&mut buf)),
        Err(NetError::ConnectionReset)
    );
    assert_eq!(
        block_on(connection.write(b"x")),
        Err(NetError::ConnectionReset)
    );
}

#[test]
fn link_loss_resets_open_connections() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    let mut connection = block_on(net.connect(REMOTE)).unwrap();
    let mut buf = [0u8; 8];
    let mut read = pin!(connection.read(&mut buf));
    assert!(poll_once(read.as_mut()).is_pending());
    ip.set_ready(false);
    assert_eq!(block_on(read), Err(NetError::ConnectionReset));
}

#[test]
fn exhaustion_is_no_socket_available_and_slots_are_reused() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    let first = block_on(net.connect(REMOTE)).unwrap();
    let second = block_on(net.connect(OTHER)).unwrap();
    // Fails at once, it does not wait for a free slot.
    assert_eq!(
        block_on(net.connect(REMOTE)).err(),
        Some(NetError::NoSocketAvailable)
    );
    drop(first);
    let third = block_on(net.connect(REMOTE));
    assert!(third.is_ok());
    drop(second);
    drop(third);
    assert_eq!(ip.open_connections(), 0);
}

#[test]
fn exhaustion_is_reported_even_when_the_network_drops() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    let _a = block_on(net.connect(REMOTE)).unwrap();
    let _b = block_on(net.connect(OTHER)).unwrap();
    assert_eq!(
        block_on(net.connect(REMOTE)).err(),
        Some(NetError::NoSocketAvailable)
    );
}

#[test]
fn cancelled_connect_releases_its_slot() {
    let ip = ready();
    ip.hold_connects(true);
    let net = NetworkHandle::new(&ip);
    for _ in 0..3 {
        let mut connect = pin!(net.connect(REMOTE));
        assert!(poll_once(connect.as_mut()).is_pending());
        assert_eq!(ip.open_connections(), 1);
    }
    assert_eq!(ip.open_connections(), 0);
    ip.hold_connects(false);
    // Full capacity is still available.
    let a = block_on(net.connect(REMOTE));
    let b = block_on(net.connect(OTHER));
    assert!(a.is_ok() && b.is_ok());
}

#[test]
fn cancelled_read_and_write_keep_the_connection_usable() {
    let ip = FakeIp::<1, 4>::new();
    ip.set_ready(true);
    let net = NetworkHandle::new(&ip);
    let mut connection = block_on(net.connect(REMOTE)).unwrap();
    let mut buf = [0u8; 8];
    {
        let mut read = pin!(connection.read(&mut buf));
        assert!(poll_once(read.as_mut()).is_pending());
    }
    ip.peer_send(REMOTE, b"ok");
    assert_eq!(block_on(connection.read(&mut buf)), Ok(2));

    // Fill the 4-byte send buffer, then cancel a blocked write.
    assert_eq!(block_on(connection.write(b"abcdef")), Ok(4));
    {
        let mut write = pin!(connection.write(b"gh"));
        assert!(poll_once(write.as_mut()).is_pending());
    }
    assert_eq!(ip.peer_received(REMOTE).as_slice(), b"abcd");
    // A cancelled write sent nothing; the next write goes through.
    assert_eq!(block_on(connection.write(b"gh")), Ok(2));
    assert_eq!(ip.peer_received(REMOTE).as_slice(), b"gh");
}

#[test]
fn cancelled_dns_query_leaves_the_handle_usable() {
    let ip = ready();
    ip.hold_dns(true);
    let net = NetworkHandle::new(&ip);
    {
        let mut query = pin!(net.resolve("rohi.example", AddrType::IPv4));
        assert!(poll_once(query.as_mut()).is_pending());
    }
    ip.hold_dns(false);
    assert_eq!(
        block_on(net.resolve("rohi.example", AddrType::IPv4)),
        Ok(SERVER)
    );
}

#[test]
fn errors_map_to_io_error_kinds() {
    use embedded_io_async::{Error, ErrorKind};
    assert_eq!(NetError::NetworkUnavailable.kind(), ErrorKind::NotConnected);
    assert_eq!(NetError::NoSocketAvailable.kind(), ErrorKind::OutOfMemory);
    assert_eq!(NetError::ConnectionReset.kind(), ErrorKind::ConnectionReset);
    assert_eq!(NetError::Timeout.kind(), ErrorKind::TimedOut);
}

#[test]
fn udp_requires_a_ready_network() {
    let ip = FakeIp::<2, 64>::new();
    let net = NetworkHandle::new(&ip);
    assert_eq!(net.udp_socket().err(), Some(NetError::NetworkUnavailable));
    assert_eq!(ip.open_udp_sockets(), 0);
}

#[test]
fn udp_round_trip() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    let socket = net.udp_socket().unwrap();
    let remote = SocketAddr::new(SERVER, 123);
    block_on(socket.send_to(b"ping", remote)).unwrap();
    let (to, data) = ip.udp_peer_received().unwrap();
    assert_eq!((to, data.as_slice()), (remote, b"ping".as_slice()));
    assert!(ip.udp_peer_received().is_none());

    assert!(ip.udp_peer_send(remote, b"pong"));
    let mut buf = [0u8; 16];
    let (len, from) = block_on(socket.recv_from(&mut buf)).unwrap();
    assert_eq!((&buf[..len], from), (b"pong".as_slice(), remote));
}

#[test]
fn udp_slots_are_bounded_and_released() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    let first = net.udp_socket().unwrap();
    let second = net.udp_socket().unwrap();
    assert_eq!(net.udp_socket().err(), Some(NetError::NoSocketAvailable));
    drop(first);
    assert_eq!(ip.open_udp_sockets(), 1);
    let third = net.udp_socket().unwrap();
    drop((second, third));
    assert_eq!(ip.open_udp_sockets(), 0);
}

#[test]
fn udp_receive_is_cancel_safe_and_fails_when_the_link_is_lost() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    let socket = net.udp_socket().unwrap();
    let mut buf = [0u8; 16];
    {
        let mut receive = pin!(socket.recv_from(&mut buf));
        assert!(poll_once(receive.as_mut()).is_pending());
    }
    // A datagram arriving after the cancelled receive is not lost.
    assert!(ip.udp_peer_send(SocketAddr::new(SERVER, 123), b"late"));
    assert_eq!(block_on(socket.recv_from(&mut buf)).unwrap().0, 4);

    let mut receive = pin!(socket.recv_from(&mut buf));
    assert!(poll_once(receive.as_mut()).is_pending());
    ip.set_ready(false);
    assert_eq!(block_on(receive), Err(NetError::NetworkUnavailable));
    assert_eq!(
        block_on(socket.send_to(b"x", SocketAddr::new(SERVER, 123))),
        Err(NetError::NetworkUnavailable)
    );
}

#[test]
fn udp_oversized_datagrams_are_discarded() {
    let ip = ready();
    let net = NetworkHandle::new(&ip);
    let socket = net.udp_socket().unwrap();
    let remote = SocketAddr::new(SERVER, 123);
    assert!(ip.udp_peer_send(remote, &[0u8; 32]));
    assert!(ip.udp_peer_send(remote, b"ok"));
    let mut buf = [0u8; 8];
    assert_eq!(block_on(socket.recv_from(&mut buf)), Ok((2, remote)));
}
