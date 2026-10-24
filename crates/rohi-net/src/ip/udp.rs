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

use core::net::SocketAddr;

use super::NetError;

/// A bound UDP socket of an [`IpBackend`](crate::IpBackend).
///
/// The socket is bound to an ephemeral local port and owns one statically
/// allocated slot of the backend until it is dropped. Datagrams are exchanged
/// with any remote; filtering by source is up to the caller.
///
/// Methods take `&self` so the socket can be handed to protocol clients that
/// only need shared access.
///
/// Cancellation: both futures are cancel-safe. Dropping `recv_from` loses
/// nothing: a datagram that arrives is delivered to the next call. Dropping
/// `send_to` before it completes may or may not have queued the datagram;
/// UDP gives no delivery guarantee anyway.
#[allow(async_fn_in_trait)]
pub trait UdpSocket {
    /// Sends one datagram to `remote`.
    ///
    /// Fails with [`NetError::NetworkUnavailable`] while the network cannot
    /// carry traffic.
    async fn send_to(&self, data: &[u8], remote: SocketAddr) -> Result<(), NetError>;

    /// Waits for one datagram, copies it into `buf` and returns its length
    /// and source address. A datagram longer than `buf` is discarded, as if it
    /// had been lost, and the wait continues.
    ///
    /// Fails with [`NetError::NetworkUnavailable`] when the network is lost
    /// while waiting. There is no built-in timeout; callers bound the wait
    /// themselves.
    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), NetError>;
}
