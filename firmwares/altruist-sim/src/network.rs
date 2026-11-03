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

//! The native IP data plane.
//!
//! [`NativeIp`] implements the portable [`IpBackend`] contract on the sockets
//! of the host: name resolution through the system resolver and real TCP/UDP,
//! so the device can talk to `127.0.0.1`, containers and local services.
//! Higher-level crates (NTP time, the telemetry uplink) use it through
//! [`NetworkHandle`](rohi_sdk::net::NetworkHandle) exactly as they use the ESP
//! stack; nothing above this module knows it is a host socket.
//!
//! ```text
//! NetworkHandle -> NativeIp -> async-io sockets / system resolver
//! ```
//!
//! The Embassy executor stays the only runtime of the device: sockets are
//! registered with the reactor thread of `async-io`, which only wakes the
//! waiting Embassy tasks, and name resolution (a blocking system call) runs on
//! the `blocking` thread pool.
//!
//! The limits of the ESP resource budget apply: two TCP connections and one
//! UDP socket, and a request beyond them fails at once with
//! `NoSocketAvailable`.
//!
//! The IP plane is ready while the simulated Wi-Fi link is up. Faults above
//! the socket layer (offline, DNS failure, connect failure, reset, latency,
//! dropped connections) come from the world and never from the device logic.

use core::net::{IpAddr, SocketAddr};
use std::io::{Read as _, Write as _};
use std::net::{TcpStream, ToSocketAddrs, UdpSocket as StdUdpSocket};
use std::sync::atomic::{AtomicUsize, Ordering};

use async_io::Async;
use embassy_time::Timer;
use embedded_nal_async::AddrType;
use rohi_sdk::net::{IpBackend, IpUsage, NetError, UdpSocket};

use crate::command::IpFault;
use crate::world::{World, world};

/// TCP connections at the same time, as on the ESP.
pub const TCP_CONNECTIONS: usize = 2;
/// UDP sockets at the same time, as on the ESP.
pub const UDP_SOCKETS: usize = 1;

/// The host IP stack.
pub struct NativeIp {
    world: &'static World,
    tcp: AtomicUsize,
    udp: AtomicUsize,
}

impl NativeIp {
    pub fn new() -> Self {
        Self {
            world: world(),
            tcp: AtomicUsize::new(0),
            udp: AtomicUsize::new(0),
        }
    }

    fn fault(&self) -> Option<IpFault> {
        self.world.lock().ip.fault
    }

    async fn latency(&self) {
        let ms = self.world.lock().ip.latency_ms;
        if ms > 0 {
            Timer::after_millis(u64::from(ms)).await;
        }
    }

    /// Takes a slot of `counter` or fails at once.
    fn reserve<'a>(counter: &'a AtomicUsize, capacity: usize) -> Result<Slot<'a>, NetError> {
        counter
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                (used < capacity).then_some(used + 1)
            })
            .map(|_| Slot(counter))
            .map_err(|_| NetError::NoSocketAvailable)
    }
}

impl Default for NativeIp {
    fn default() -> Self {
        Self::new()
    }
}

/// A used slot, released when dropped.
struct Slot<'a>(&'a AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// An established TCP connection of the host.
pub struct NativeConnection<'a> {
    stream: Async<TcpStream>,
    world: &'static World,
    _slot: Slot<'a>,
}

impl NativeConnection<'_> {
    fn check(&self) -> Result<(), NetError> {
        let state = self.world.lock();
        if !state.wifi.link_up {
            return Err(NetError::ConnectionReset);
        }
        match state.ip.fault {
            Some(IpFault::Offline) => Err(NetError::NetworkUnavailable),
            Some(IpFault::Reset) => Err(NetError::ConnectionReset),
            _ => Ok(()),
        }
    }
}

fn io_error(error: &std::io::Error) -> NetError {
    match error.kind() {
        std::io::ErrorKind::TimedOut => NetError::Timeout,
        std::io::ErrorKind::ConnectionReset
        | std::io::ErrorKind::ConnectionAborted
        | std::io::ErrorKind::BrokenPipe => NetError::ConnectionReset,
        std::io::ErrorKind::ConnectionRefused => NetError::ConnectFailed,
        _ => NetError::Backend,
    }
}

impl embedded_io_async::ErrorType for NativeConnection<'_> {
    type Error = NetError;
}

impl embedded_io_async::Read for NativeConnection<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        self.check()?;
        self.stream
            .read_with(|stream| {
                let mut stream = stream;
                stream.read(buf)
            })
            .await
            .map_err(|e| io_error(&e))
    }
}

impl embedded_io_async::Write for NativeConnection<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, NetError> {
        self.check()?;
        self.stream
            .write_with(|stream| {
                let mut stream = stream;
                stream.write(buf)
            })
            .await
            .map_err(|e| io_error(&e))
    }

    async fn flush(&mut self) -> Result<(), NetError> {
        Ok(())
    }
}

/// A UDP socket of the host.
pub struct NativeUdp<'a> {
    socket: Async<StdUdpSocket>,
    world: &'static World,
    _slot: Slot<'a>,
}

impl NativeUdp<'_> {
    fn check(&self) -> Result<(), NetError> {
        let state = self.world.lock();
        if !state.wifi.link_up || state.ip.fault == Some(IpFault::Offline) {
            return Err(NetError::NetworkUnavailable);
        }
        Ok(())
    }
}

impl UdpSocket for NativeUdp<'_> {
    async fn send_to(&self, data: &[u8], remote: SocketAddr) -> Result<(), NetError> {
        self.check()?;
        self.socket
            .send_to(data, remote)
            .await
            .map(|_| ())
            .map_err(|e| io_error(&e))
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), NetError> {
        self.check()?;
        self.socket.recv_from(buf).await.map_err(|e| io_error(&e))
    }
}

impl IpBackend for NativeIp {
    type Connection<'a>
        = NativeConnection<'a>
    where
        Self: 'a;
    type Udp<'a>
        = NativeUdp<'a>
    where
        Self: 'a;

    fn is_ready(&self) -> bool {
        let state = self.world.lock();
        state.wifi.link_up && state.ip.fault != Some(IpFault::Offline)
    }

    fn usage(&self) -> IpUsage {
        IpUsage {
            tcp_in_use: self.tcp.load(Ordering::Acquire),
            tcp_capacity: TCP_CONNECTIONS,
            udp_in_use: self.udp.load(Ordering::Acquire),
            udp_capacity: UDP_SOCKETS,
        }
    }

    async fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, NetError> {
        self.latency().await;
        if matches!(self.fault(), Some(IpFault::Dns | IpFault::Offline)) {
            return Err(NetError::DnsFailed);
        }
        let wanted = |address: &IpAddr| match addr_type {
            AddrType::IPv4 => address.is_ipv4(),
            AddrType::IPv6 => address.is_ipv6(),
            AddrType::Either => true,
        };
        if let Ok(address) = host.parse::<IpAddr>() {
            return if wanted(&address) {
                Ok(address)
            } else {
                Err(NetError::DnsFailed)
            };
        }
        let host = host.to_owned();
        let resolved = blocking::unblock(move || {
            (host.as_str(), 0)
                .to_socket_addrs()
                .map(|addresses| addresses.map(|a| a.ip()).collect::<Vec<_>>())
        })
        .await
        .map_err(|_| NetError::DnsFailed)?;
        resolved.into_iter().find(wanted).ok_or(NetError::DnsFailed)
    }

    async fn connect(&self, remote: SocketAddr) -> Result<Self::Connection<'_>, NetError> {
        let slot = Self::reserve(&self.tcp, TCP_CONNECTIONS)?;
        self.latency().await;
        {
            let mut state = self.world.lock();
            if matches!(state.ip.fault, Some(IpFault::Connect | IpFault::Offline)) {
                return Err(NetError::ConnectFailed);
            }
            if state.ip.drop_next > 0 {
                state.ip.drop_next -= 1;
                return Err(NetError::ConnectFailed);
            }
        }
        let stream = Async::<TcpStream>::connect(remote)
            .await
            .map_err(|e| match io_error(&e) {
                NetError::Backend => NetError::ConnectFailed,
                other => other,
            })?;
        let _ = stream.get_ref().set_nodelay(true);
        Ok(NativeConnection {
            stream,
            world: self.world,
            _slot: slot,
        })
    }

    fn udp_socket(&self) -> Result<Self::Udp<'_>, NetError> {
        let slot = Self::reserve(&self.udp, UDP_SOCKETS)?;
        let socket =
            Async::<StdUdpSocket>::bind(([0, 0, 0, 0], 0)).map_err(|_| NetError::Backend)?;
        Ok(NativeUdp {
            socket,
            world: self.world,
            _slot: slot,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use embedded_io_async::{Read as _, Write as _};
    use rohi_sdk::net::NetworkHandle;

    use super::*;
    use crate::scenario::Scenario;

    /// One test: the world is a process-wide singleton.
    #[test]
    fn host_sockets_behind_the_portable_network_handle() {
        crate::world::init_for_tests(&Scenario::default());
        let ip = NativeIp::new();
        let net = NetworkHandle::new(&ip);

        // Not ready until the Wi-Fi link is up.
        assert!(!net.is_ready());
        world().lock().wifi.link_up = true;
        assert!(net.is_ready());

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
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

        async_io::block_on(async {
            // Localhost resolves through the system resolver, literals directly.
            let host = net.resolve("localhost", AddrType::Either).await.unwrap();
            assert!(host.is_loopback());
            let literal = net.resolve("127.0.0.1", AddrType::IPv4).await.unwrap();
            assert_eq!(literal, IpAddr::from([127, 0, 0, 1]));
            assert_eq!(
                net.resolve("127.0.0.1", AddrType::IPv6).await,
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

            // Faults above the socket layer.
            world().apply(&crate::command::Command::IpFail(IpFault::Dns));
            assert_eq!(
                net.resolve("localhost", AddrType::Either).await,
                Err(NetError::DnsFailed)
            );
            world().apply(&crate::command::Command::IpFail(IpFault::Connect));
            assert_eq!(
                net.connect(remote).await.err(),
                Some(NetError::ConnectFailed)
            );
            world().apply(&crate::command::Command::IpRecover);

            // The link going down resets what was open and stops new traffic.
            world().lock().wifi.link_up = false;
            assert_eq!(
                net.connect(remote).await.err(),
                Some(NetError::NetworkUnavailable)
            );
        });
        server.join().unwrap();
    }
}
