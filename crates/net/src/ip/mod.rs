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

//! IP data plane: DNS, TCP and UDP over the platform IP stack.
//!
//! ```text
//! TLS / HTTP / uplink  ── embedded-nal-async (Dns, TcpConnect)
//!          │              embedded-io-async (Read, Write)
//!    NetworkHandle     ── readiness, typed NetError, UdpSocket
//!          │
//!     IpBackend        ── implemented by platform crates (rohi-net-esp)
//! ```
//!
//! The Wi-Fi control plane ([`crate::wifi`]) brings the link up; this module
//! only carries traffic once it is. Only the protocols a current SDK component
//! uses are offered: DNS, TCP and UDP over DHCPv4-configured IPv4.
//!
//! [`NetworkHandle`] is a copyable handle to the IP stack. It implements the
//! standard embedded Rust traits, so TLS, HTTP and uplink layers stay
//! independent of `embassy-net`:
//!
//! | Trait | Provided by [`NetworkHandle`] |
//! |-------|-------------------------------|
//! | [`embedded_nal_async::Dns`] | `get_host_by_name` (IPv4); reverse lookup is not supported |
//! | [`embedded_nal_async::TcpConnect`] | `connect`, giving a connection that implements [`embedded_io_async::Read`] and [`embedded_io_async::Write`] |
//!
//! UDP is offered through [`NetworkHandle::udp_socket`] and the [`UdpSocket`]
//! trait.
//!
//! # Example
//!
//! A function generic over the standard traits works with [`NetworkHandle`],
//! with any other `embedded-nal-async` stack, and is what a TLS or HTTP layer
//! would build on. The example runs against the host fake backend
//! (`rohi_net::testing::FakeIp`, feature `testing`); on the device the same
//! function runs against the firmware's handle, see
//! <https://rohi.akagi.dev/guides/use-network.html>.
//!
//! ```
//! use core::net::{IpAddr, Ipv4Addr, SocketAddr};
//!
//! use embassy_futures::block_on;
//! use embedded_io_async::{Read, Write};
//! use embedded_nal_async::{AddrType, Dns, TcpConnect};
//! use rohi_net::testing::FakeIp;
//! use rohi_net::{NetError, NetworkHandle};
//!
//! /// Sends `request` to `host:port` and reads the first chunk of the answer.
//! async fn exchange<N>(
//!     net: &N,
//!     host: &str,
//!     port: u16,
//!     request: &[u8],
//!     answer: &mut [u8],
//! ) -> Result<usize, NetError>
//! where
//!     N: Dns<Error = NetError> + TcpConnect<Error = NetError>,
//! {
//!     let address = net.get_host_by_name(host, AddrType::IPv4).await?;
//!     let mut connection = net.connect(SocketAddr::new(address, port)).await?;
//!     connection.write_all(request).await?;
//!     connection.flush().await?;
//!     connection.read(answer).await
//! }
//!
//! let server = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
//! let remote = SocketAddr::new(server, 8883);
//!
//! let ip = FakeIp::<2, 128>::new();
//! ip.add_host("uplink.example", server);
//! let net = NetworkHandle::new(&ip);
//!
//! // Wi-Fi is not connected yet: a typed error, not a DNS or connect failure.
//! let mut answer = [0u8; 16];
//! assert_eq!(
//!     block_on(exchange(&net, "uplink.example", 8883, b"hello", &mut answer)),
//!     Err(NetError::NetworkUnavailable)
//! );
//!
//! ip.set_ready(true);
//! let read = block_on(async {
//!     let mut call = core::pin::pin!(exchange(&net, "uplink.example", 8883, b"hello", &mut answer));
//!     // Poll once: the connection opens and the request is written.
//!     assert!(embassy_futures::poll_once(call.as_mut()).is_pending());
//!     assert_eq!(ip.peer_received(remote).as_slice(), b"hello");
//!     ip.peer_send(remote, b"world");
//!     call.await
//! });
//! assert_eq!(read, Ok(5));
//! assert_eq!(&answer[..5], b"world");
//! ```
//!
//! # Readiness and errors
//!
//! Every operation first checks readiness (link up **and** DHCP complete) and
//! fails with [`NetError::NetworkUnavailable`] while Wi-Fi is not configured or
//! connecting, DHCP is incomplete or the link was lost, also when readiness is
//! lost during the operation. Higher layers can therefore tell an unusable
//! network from a DNS or connect failure while the network is up.
//!
//! | [`NetError`] | Meaning |
//! |--------------|---------|
//! | `NetworkUnavailable` | Wi-Fi not configured/connecting, DHCP incomplete, link lost |
//! | `DnsFailed` | Resolution failed while the network was up |
//! | `ConnectFailed` | Connection refused or unreachable while the network was up |
//! | `NoSocketAvailable` | All statically allocated TCP connections (or UDP sockets) are in use |
//! | `ConnectionReset` | An established connection was reset or closed |
//! | `Timeout` | A timeout expired |
//! | `Backend` | Unexpected stack failure |
//!
//! [`NetError`] implements [`embedded_io_async::Error`]; raw `embassy-net`
//! errors never reach higher layers.
//!
//! # Socket resources
//!
//! TCP connections and UDP sockets are **statically bounded**; nothing
//! allocates. Opening more simultaneous connections or sockets than the
//! product configured fails **immediately** with
//! [`NetError::NoSocketAvailable`]: `connect` and
//! [`udp_socket`](NetworkHandle::udp_socket) never wait for a free slot. A slot
//! is released when the connection or socket is dropped. The capacity is a
//! product decision made in the platform backend (see [`IpUsage`] and
//! <https://rohi.akagi.dev/reference/resource-limits.html>).
//!
//! # UDP
//!
//! [`NetworkHandle::udp_socket`] opens a [`UdpSocket`] bound to an ephemeral
//! port (it fails with [`NetError::NetworkUnavailable`] while the network is
//! not ready). `send_to` and `recv_from` take `&self`; a datagram larger than
//! the receive buffer is discarded and the wait continues. There is no
//! built-in receive timeout: callers bound the wait themselves (the NTP time
//! source in [`rohi_time_ntp`](../../rohi_time_ntp/index.html) does).
//!
//! # Cancellation
//!
//! All [`NetworkHandle`] operations are cancel-safe: dropping
//! `get_host_by_name` allocates nothing; dropping a pending `connect` aborts
//! the attempt and frees its slot; dropping a UDP socket frees its slot, and a
//! datagram arriving after a cancelled `recv_from` is delivered to the next
//! call; dropping a read or write future transfers nothing that was not
//! reported.
//!
//! # Testing without hardware
//!
//! `rohi_net::testing::FakeIp` (feature `testing`) is an in-memory
//! [`IpBackend`]: scripted DNS answers, refused connections, readiness changes,
//! slot exhaustion, held operations for cancellation tests and the peer side of
//! every connection and UDP socket (`udp_peer_send`, `udp_peer_received`).
//! Host testing is described at
//! <https://rohi.akagi.dev/validation/host-testing.html>.

mod backend;
mod error;
mod handle;
mod udp;

pub use backend::{IpBackend, IpUsage};
pub use error::NetError;
pub use handle::NetworkHandle;
pub use udp::UdpSocket;

#[cfg(test)]
mod tests;
