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

//! The IP data plane over the sockets of the host.
//!
//! [`NativeIp`] implements the portable [`IpBackend`] contract of `rohi-net`:
//! name resolution through the system resolver and real TCP and UDP sockets.
//! Higher-level code (NTP time, an uplink, a client of a local service) uses it
//! through [`NetworkHandle`](rohi_net::NetworkHandle) exactly as it uses the
//! ESP stack.

use core::net::{IpAddr, SocketAddr};
use std::io::ErrorKind;
use std::sync::atomic::{AtomicUsize, Ordering};

use embedded_nal_async::AddrType;
use rohi_net::{IpBackend, IpUsage, NetError, UdpSocket};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpStream, UdpSocket as TokioUdp};

use crate::runtime::within;

/// TCP connections at the same time, the default of [`NativeIp`] (as on the ESP
/// firmware).
pub const DEFAULT_TCP_CONNECTIONS: usize = 2;
/// UDP sockets at the same time, the default of [`NativeIp`] (as on the ESP
/// firmware).
pub const DEFAULT_UDP_SOCKETS: usize = 1;

/// The IP stack of the host, with at most `TCP` connections and `UDP` sockets
/// open at the same time.
///
/// The limits are those of the portable contract: a request beyond them fails
/// at once with [`NetError::NoSocketAvailable`] and nothing waits for a free
/// slot. The defaults are the ones of the ESP firmware, so code that fits a
/// device behaves the same on the host.
///
/// The IP plane is always ready: the host has a network stack of its own and
/// the data plane reports [`NetError::NetworkUnavailable`] never. Failures of
/// the host surface as the error of the operation that hit them.
///
/// ```
/// use embassy_futures::block_on;
/// use rohi_net::{IpBackend, NetworkHandle};
/// use rohi_net_std::NativeIp;
///
/// let ip = NativeIp::new();
/// assert!(ip.is_ready());
/// let net = NetworkHandle::new(&ip);
/// let usage = net.usage();
/// assert_eq!((usage.tcp_in_use, usage.tcp_capacity), (0, 2));
/// let address = block_on(net.resolve("127.0.0.1", embedded_nal_async::AddrType::IPv4)).unwrap();
/// assert!(address.is_loopback());
/// ```
pub struct NativeIp<
    const TCP: usize = DEFAULT_TCP_CONNECTIONS,
    const UDP: usize = DEFAULT_UDP_SOCKETS,
> {
    tcp: AtomicUsize,
    udp: AtomicUsize,
}

impl NativeIp {
    /// A stack without open sockets and the limits of the ESP firmware.
    pub const fn new() -> Self {
        Self::with_limits()
    }
}

impl<const TCP: usize, const UDP: usize> NativeIp<TCP, UDP> {
    /// A stack without open sockets that allows `TCP` connections and `UDP`
    /// sockets.
    pub const fn with_limits() -> Self {
        Self {
            tcp: AtomicUsize::new(0),
            udp: AtomicUsize::new(0),
        }
    }

    /// Takes a slot of `counter` or fails at once.
    fn reserve(counter: &AtomicUsize, capacity: usize) -> Result<Slot<'_>, NetError> {
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
///
/// Dropping it closes the socket and releases its slot.
pub struct NativeConnection<'a> {
    stream: TcpStream,
    _slot: Slot<'a>,
}

/// Maps an error of the host into the portable categories.
pub fn net_error(error: &std::io::Error) -> NetError {
    match error.kind() {
        ErrorKind::TimedOut => NetError::Timeout,
        ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted | ErrorKind::BrokenPipe => {
            NetError::ConnectionReset
        }
        ErrorKind::ConnectionRefused => NetError::ConnectFailed,
        _ => NetError::Backend,
    }
}

impl embedded_io_async::ErrorType for NativeConnection<'_> {
    type Error = NetError;
}

impl embedded_io_async::Read for NativeConnection<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        within(self.stream.read(buf))
            .await
            .map_err(|e| net_error(&e))
    }
}

impl embedded_io_async::Write for NativeConnection<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, NetError> {
        within(self.stream.write(buf))
            .await
            .map_err(|e| net_error(&e))
    }

    async fn flush(&mut self) -> Result<(), NetError> {
        Ok(())
    }
}

/// A UDP socket of the host, bound to an ephemeral port.
///
/// Dropping it closes the socket and releases its slot.
pub struct NativeUdp<'a> {
    socket: TokioUdp,
    _slot: Slot<'a>,
}

impl UdpSocket for NativeUdp<'_> {
    async fn send_to(&self, data: &[u8], remote: SocketAddr) -> Result<(), NetError> {
        within(self.socket.send_to(data, remote))
            .await
            .map(|_| ())
            .map_err(|e| net_error(&e))
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), NetError> {
        within(self.socket.recv_from(buf))
            .await
            .map_err(|e| net_error(&e))
    }
}

impl<const TCP: usize, const UDP: usize> IpBackend for NativeIp<TCP, UDP> {
    type Connection<'a>
        = NativeConnection<'a>
    where
        Self: 'a;
    type Udp<'a>
        = NativeUdp<'a>
    where
        Self: 'a;

    fn is_ready(&self) -> bool {
        true
    }

    fn usage(&self) -> IpUsage {
        IpUsage {
            tcp_in_use: self.tcp.load(Ordering::Acquire),
            tcp_capacity: TCP,
            udp_in_use: self.udp.load(Ordering::Acquire),
            udp_capacity: UDP,
        }
    }

    async fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, NetError> {
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
        let resolved = within(tokio::net::lookup_host((host, 0)))
            .await
            .map_err(|_| NetError::DnsFailed)?;
        resolved
            .map(|address| address.ip())
            .find(wanted)
            .ok_or(NetError::DnsFailed)
    }

    async fn connect(&self, remote: SocketAddr) -> Result<Self::Connection<'_>, NetError> {
        let slot = Self::reserve(&self.tcp, TCP)?;
        let stream = within(TcpStream::connect(remote))
            .await
            .map_err(|e| match net_error(&e) {
                NetError::Backend => NetError::ConnectFailed,
                other => other,
            })?;
        let _ = stream.set_nodelay(true);
        Ok(NativeConnection {
            stream,
            _slot: slot,
        })
    }

    fn udp_socket(&self) -> Result<Self::Udp<'_>, NetError> {
        let slot = Self::reserve(&self.udp, UDP)?;
        let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
            .map_err(|_| NetError::Backend)?;
        socket
            .set_nonblocking(true)
            .map_err(|_| NetError::Backend)?;
        let _entered = crate::runtime::handle().enter();
        let socket = TokioUdp::from_std(socket).map_err(|_| NetError::Backend)?;
        Ok(NativeUdp {
            socket,
            _slot: slot,
        })
    }
}
