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

use core::net::{IpAddr, SocketAddr};

use embedded_io_async::{Read, Write};
use embedded_nal_async::AddrType;

use super::{NetError, UdpSocket};

/// Platform IP stack seen by a [`NetworkHandle`](crate::NetworkHandle).
///
/// Implemented by platform crates (for example `rohi-net-esp` on top of
/// `embassy-net`). Only ROHI and standard `core` / `embedded-io-async` types
/// appear here; stack and vendor errors are mapped into [`NetError`].
///
/// Methods take `&self`: the backend is shared by every copy of the handle and
/// keeps its bounded connection pool behind interior mutability.
///
/// Cancellation: every future must be cancel-safe. Dropping `resolve` or
/// `connect` leaves no resource allocated; dropping a connection or a UDP
/// socket releases its slot.
#[allow(async_fn_in_trait)]
pub trait IpBackend {
    /// An established TCP connection. Dropping it closes the connection
    /// (abort if needed) and releases its statically allocated slot.
    type Connection<'a>: Read<Error = NetError> + Write<Error = NetError>
    where
        Self: 'a;

    /// A UDP socket bound to an ephemeral local port. Dropping it closes the
    /// socket and releases its statically allocated slot.
    type Udp<'a>: UdpSocket
    where
        Self: 'a;

    /// `true` while the stack can carry traffic: link up and IPv4
    /// configuration (DHCP) complete.
    fn is_ready(&self) -> bool;

    /// Resolves `host` into one address of the requested family.
    async fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, NetError>;

    /// Opens a TCP connection to `remote`.
    ///
    /// Fails with [`NetError::NoSocketAvailable`] immediately when all
    /// connection slots are in use.
    async fn connect(&self, remote: SocketAddr) -> Result<Self::Connection<'_>, NetError>;

    /// Opens a UDP socket bound to an ephemeral local port.
    ///
    /// Fails with [`NetError::NoSocketAvailable`] immediately when all UDP
    /// slots are in use.
    fn udp_socket(&self) -> Result<Self::Udp<'_>, NetError>;
}
