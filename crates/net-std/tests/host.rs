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
///////////////////////////////////////////////////////////////////////////////

//! `NativeIp` and `TcpAcceptor` against host sockets, polled by a plain
//! executor that is not the reactor.

use core::net::{IpAddr, SocketAddr};
use std::io::{Read as _, Write as _};
use std::time::Duration;

use embassy_futures::block_on;
use embassy_futures::select::{Either, select};
use embedded_io_async::{Read as _, Write as _};
use embedded_nal_async::AddrType;
use rohi_control_tcp::Acceptor as _;
use rohi_net::{NetError, NetworkHandle, UdpSocket};
use rohi_net_std::{NativeIp, TcpAcceptor, runtime};

#[test]
fn host_sockets_behind_the_portable_network_handle() {
    let ip = NativeIp::new();
    let net = NetworkHandle::new(&ip);
    assert!(net.is_ready());

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0u8; 5];
            if stream.read_exact(&mut buffer).is_ok() {
                stream.write_all(&buffer).unwrap();
            }
        }
    });

    block_on(async {
        // Localhost resolves through the system resolver, literals directly.
        let host = net.resolve("localhost", AddrType::Either).await.unwrap();
        assert!(host.is_loopback());
        let literal = net.resolve("127.0.0.1", AddrType::IPv4).await.unwrap();
        assert_eq!(literal, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(
            net.resolve("127.0.0.1", AddrType::IPv6).await,
            Err(NetError::DnsFailed)
        );
        assert_eq!(
            net.resolve("no-such-host.invalid", AddrType::Either).await,
            Err(NetError::DnsFailed)
        );

        // TCP echo.
        let remote = SocketAddr::new(literal, port);
        let mut connection = net.connect(remote).await.unwrap();
        connection.write_all(b"hello").await.unwrap();
        let mut echoed = [0u8; 5];
        connection.read_exact(&mut echoed).await.unwrap();
        assert_eq!(&echoed, b"hello");
        assert_eq!(net.usage().tcp_in_use, 1);

        // The bounded pool is the one of the device.
        let second = net.connect(remote).await.unwrap();
        assert_eq!(
            net.connect(remote).await.err(),
            Some(NetError::NoSocketAvailable)
        );
        drop(second);
        drop(connection);
        assert_eq!(net.usage().tcp_in_use, 0);
    });
    server.join().unwrap();
}

#[test]
fn a_refused_connection_is_a_connect_failure_and_frees_its_slot() {
    let ip = NativeIp::new();
    let net = NetworkHandle::new(&ip);
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let remote = free.local_addr().unwrap();
    drop(free);
    block_on(async {
        assert_eq!(
            net.connect(remote).await.err(),
            Some(NetError::ConnectFailed)
        );
    });
    assert_eq!(net.usage().tcp_in_use, 0);
}

#[test]
fn a_connection_closed_by_the_peer_reads_zero() {
    let ip = NativeIp::new();
    let net = NetworkHandle::new(&ip);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let remote = listener.local_addr().unwrap();
    let peer = std::thread::spawn(move || drop(listener.accept().unwrap()));
    block_on(async {
        let mut connection = net.connect(remote).await.unwrap();
        let mut buffer = [0u8; 4];
        assert_eq!(connection.read(&mut buffer).await, Ok(0));
    });
    peer.join().unwrap();
}

#[test]
fn a_dropped_connect_releases_its_slot() {
    let ip = NativeIp::<1, 1>::with_limits();
    let net = NetworkHandle::new(&ip);
    // A non-routable address keeps the connect pending while the timer wins.
    let pending = SocketAddr::from(([10, 255, 255, 1], 9));
    block_on(async {
        let outcome = select(
            net.connect(pending),
            runtime_timer(Duration::from_millis(50)),
        )
        .await;
        if let Either::First(result) = outcome {
            // The environment answered at once (no route); nothing is pending.
            assert!(result.is_err());
        }
    });
    assert_eq!(net.usage().tcp_in_use, 0);
}

/// A timer of the reactor, awaited by the test executor.
async fn runtime_timer(wait: Duration) {
    runtime::within(async { tokio::time::sleep(wait).await }).await;
}

#[test]
fn udp_exchanges_datagrams_and_is_bounded() {
    let ip = NativeIp::new();
    let net = NetworkHandle::new(&ip);
    let peer = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let peer_address = peer.local_addr().unwrap();

    let socket = net.udp_socket().unwrap();
    assert!(matches!(net.udp_socket(), Err(NetError::NoSocketAvailable)));
    block_on(async {
        socket.send_to(b"ping", peer_address).await.unwrap();
    });
    let mut buffer = [0u8; 16];
    let (len, from) = peer.recv_from(&mut buffer).unwrap();
    assert_eq!(&buffer[..len], b"ping");

    peer.send_to(b"pong", from).unwrap();
    block_on(async {
        let (len, source) = socket.recv_from(&mut buffer).await.unwrap();
        assert_eq!(&buffer[..len], b"pong");
        assert_eq!(source.port(), peer_address.port());
    });
    assert_eq!(net.usage().udp_in_use, 1);
    drop(socket);
    assert_eq!(net.usage().udp_in_use, 0);
}

#[test]
fn the_acceptor_yields_clients_one_after_another() {
    let mut acceptor = TcpAcceptor::bind(([127, 0, 0, 1], 0).into()).unwrap();
    let address = acceptor.local_addr().unwrap();
    let clients = std::thread::spawn(move || {
        for message in [b"one", b"two"] {
            let mut stream = std::net::TcpStream::connect(address).unwrap();
            stream.write_all(message).unwrap();
        }
    });
    block_on(async {
        for expected in [b"one", b"two"] {
            let mut stream = acceptor.accept().await.unwrap();
            let mut buffer = [0u8; 3];
            stream.read_exact(&mut buffer).await.unwrap();
            assert_eq!(&buffer, expected);
        }
    });
    clients.join().unwrap();
}

#[test]
fn the_reactor_is_one_thread_that_is_started_once() {
    let first: *const _ = runtime::handle();
    let second: *const _ = runtime::handle();
    assert_eq!(first, second);
}

#[test]
fn the_control_transport_serves_a_client_over_a_host_socket() {
    use rohi_control::{Controller, Device, DeviceInfo, OpenPolicy};
    use rohi_control_proto::control::{
        self as pb, Request_::Request as Call, Response_::Response as Reply,
    };
    use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};
    use rohi_control_tcp::ControlTcp;
    use rohi_control_tcp::frame::{HEADER_LEN, decode_header, encode_header};

    let acceptor = TcpAcceptor::bind(([127, 0, 0, 1], 0).into()).unwrap();
    let address = acceptor.local_addr().unwrap();
    let client = std::thread::spawn(move || {
        let request = pb::Request {
            protocol_version: PROTOCOL_VERSION,
            request_id: 3,
            request: Some(Call::GetInfo(pb::GetInfoRequest {})),
        };
        let mut bytes = [0u8; 16];
        let len = encode(&request, &mut bytes).unwrap();
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.write_all(&encode_header(len as u32)).unwrap();
        stream.write_all(&bytes[..len]).unwrap();
        let mut header = [0u8; HEADER_LEN];
        stream.read_exact(&mut header).unwrap();
        let mut payload = vec![0u8; decode_header(header, 2048).unwrap()];
        stream.read_exact(&mut payload).unwrap();
        decode::<pb::Response>(&payload).unwrap()
    });

    let info = DeviceInfo {
        firmware: "demo-0.1.0",
        board: "demo-board",
    };
    let controller = Controller::new(Device::new(info, || 0, (), (), ()), OpenPolicy);
    let mut control = ControlTcp::new(acceptor, &controller);
    // The client closes its end after the response, which ends the connection.
    block_on(control.serve_next()).unwrap();
    let response = client.join().unwrap();
    assert_eq!(response.request_id, 3);
    assert!(matches!(response.response, Some(Reply::GetInfo(_))));
}
