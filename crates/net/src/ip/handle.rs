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

use core::net::{IpAddr, SocketAddr};

use embedded_nal_async::{AddrType, Dns, TcpConnect};

use super::{IpBackend, IpUsage, NetError};

/// Cheap, copyable handle to the IP data plane: DNS, TCP and UDP.
///
/// Implements the standard [`embedded_nal_async::Dns`] and
/// [`embedded_nal_async::TcpConnect`] traits; connections implement
/// [`embedded_io_async::Read`] and [`embedded_io_async::Write`]. Higher layers
/// (TLS, HTTP, uplinks) should be generic over those traits rather than over
/// this type, and never use the platform IP stack directly.
///
/// The handle is independent of the Wi-Fi control plane
/// ([`WifiHandle`](crate::WifiHandle)): Wi-Fi being connected is a
/// prerequisite, not part of this API. Every operation fails with
/// [`NetError::NetworkUnavailable`] while the stack is not ready (Wi-Fi not
/// configured or connecting, DHCP incomplete, link lost), also when readiness
/// is lost during the operation, so callers can tell an unusable network from
/// a DNS or connect failure while the network is up.
///
/// TCP connections are statically bounded by the backend. When all are in
/// use, [`connect`](Self::connect) fails immediately with
/// [`NetError::NoSocketAvailable`]; it never waits for a free slot.
/// UDP sockets ([`udp_socket`](Self::udp_socket)) are bounded the same way.
pub struct NetworkHandle<'a, B> {
    backend: &'a B,
}

impl<B> Clone for NetworkHandle<'_, B> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B> Copy for NetworkHandle<'_, B> {}

impl<'a, B: IpBackend> NetworkHandle<'a, B> {
    /// Handle to `backend`; usually created once by the firmware from a
    /// `static` backend.
    pub const fn new(backend: &'a B) -> Self {
        Self { backend }
    }

    /// `true` while the network can carry traffic.
    pub fn is_ready(&self) -> bool {
        self.backend.is_ready()
    }

    /// Socket slots in use and available.
    pub fn usage(&self) -> IpUsage {
        self.backend.usage()
    }

    fn ensure_ready(&self) -> Result<(), NetError> {
        if self.is_ready() {
            Ok(())
        } else {
            Err(NetError::NetworkUnavailable)
        }
    }

    /// A failure while the network went down is reported as
    /// [`NetError::NetworkUnavailable`], not as a DNS or connect error.
    fn classify(&self, error: NetError) -> NetError {
        match error {
            NetError::NoSocketAvailable => error,
            _ if !self.is_ready() => NetError::NetworkUnavailable,
            _ => error,
        }
    }

    /// Resolves `host` into an address of the requested family.
    ///
    /// Cancellation: cancel-safe, nothing is allocated across the await.
    pub async fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, NetError> {
        self.ensure_ready()?;
        if host.is_empty() {
            return Err(NetError::DnsFailed);
        }
        self.backend
            .resolve(host, addr_type)
            .await
            .map_err(|error| self.classify(error))
    }

    /// Opens a TCP connection to `remote`.
    ///
    /// Cancellation: cancel-safe. Dropping the future before it completes
    /// aborts the attempt and releases the connection slot.
    pub async fn connect(&self, remote: SocketAddr) -> Result<B::Connection<'a>, NetError> {
        self.ensure_ready()?;
        self.backend
            .connect(remote)
            .await
            .map_err(|error| self.classify(error))
    }

    /// Opens a UDP socket bound to an ephemeral local port.
    ///
    /// Fails with [`NetError::NetworkUnavailable`] while the network cannot
    /// carry traffic and with [`NetError::NoSocketAvailable`] when every UDP
    /// slot is in use. Dropping the socket releases its slot.
    pub fn udp_socket(&self) -> Result<B::Udp<'a>, NetError> {
        self.ensure_ready()?;
        self.backend
            .udp_socket()
            .map_err(|error| self.classify(error))
    }
}

impl<B: IpBackend> Dns for NetworkHandle<'_, B> {
    type Error = NetError;

    async fn get_host_by_name(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, NetError> {
        self.resolve(host, addr_type).await
    }

    /// Reverse lookups are not supported: fails with
    /// [`NetError::NetworkUnavailable`] or [`NetError::DnsFailed`].
    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, NetError> {
        self.ensure_ready()?;
        Err(NetError::DnsFailed)
    }
}

impl<'a, B: IpBackend> TcpConnect for NetworkHandle<'a, B> {
    type Error = NetError;
    type Connection<'c>
        = B::Connection<'a>
    where
        Self: 'c;

    async fn connect<'c>(&'c self, remote: SocketAddr) -> Result<B::Connection<'a>, NetError> {
        NetworkHandle::connect(self, remote).await
    }
}
