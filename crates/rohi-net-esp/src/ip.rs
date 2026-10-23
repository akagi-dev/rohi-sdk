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

//! ESP32 IP data plane: DNS and TCP on the `embassy-net` stack.
//!
//! Implements the portable [`IpBackend`]. The stack and its bounded socket
//! storage are owned by [`NetResources`], which the firmware places in a
//! `static` and sizes itself; see [`EspWireless::wifi`](crate::EspWireless::wifi).
//!
//! TCP connections use a fixed pool of buffers inside [`NetResources`]. A
//! connection holds one pool entry from `connect` until it is dropped; when
//! all entries are in use `connect` fails at once with
//! [`NetError::NoSocketAvailable`]. No `embassy-net` type is public.

use core::cell::{Cell, UnsafeCell};
use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::{ConnectError, TcpSocket};
use embassy_net::{IpAddress, IpEndpoint, Stack, StackResources};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_nal_async::AddrType;
use rohi_net::{IpBackend, NetError};

/// Sockets the stack itself needs: DHCPv4 and DNS.
pub const STACK_INTERNAL_SOCKETS: usize = 2;

struct TcpBuffers<const RX: usize, const TX: usize> {
    rx: [u8; RX],
    tx: [u8; TX],
}

/// Fixed pool of TCP buffers; entry `i` is in use while bit `i` is set.
pub(crate) struct TcpPool<const N: usize, const RX: usize, const TX: usize> {
    used: Mutex<CriticalSectionRawMutex, Cell<u32>>,
    buffers: [UnsafeCell<TcpBuffers<RX, TX>>; N],
}

impl<const N: usize, const RX: usize, const TX: usize> TcpPool<N, RX, TX> {
    const CAPACITY_OK: () = assert!(N <= 32, "at most 32 TCP connections are supported");

    const fn new() -> Self {
        let () = Self::CAPACITY_OK;
        Self {
            used: Mutex::new(Cell::new(0)),
            buffers: [const {
                UnsafeCell::new(TcpBuffers {
                    rx: [0; RX],
                    tx: [0; TX],
                })
            }; N],
        }
    }

    fn alloc(&self) -> Option<PoolEntry<'_, N, RX, TX>> {
        let index = self.used.lock(|used| {
            let mask = used.get();
            let index = (0..N).find(|i| mask & (1 << i) == 0)?;
            used.set(mask | (1 << index));
            Some(index)
        })?;
        Some(PoolEntry { pool: self, index })
    }
}

/// An allocated pool entry; released on drop.
struct PoolEntry<'a, const N: usize, const RX: usize, const TX: usize> {
    pool: &'a TcpPool<N, RX, TX>,
    index: usize,
}

impl<'a, const N: usize, const RX: usize, const TX: usize> PoolEntry<'a, N, RX, TX> {
    /// The receive and transmit buffer of this entry.
    ///
    /// # Safety
    ///
    /// Call once per entry: the entry's bit in the pool mask is set for as
    /// long as `self` lives, so no other entry can alias the buffers, and
    /// the caller must not create a second pair of references from `self`.
    unsafe fn buffers(&self) -> (&'a mut [u8], &'a mut [u8]) {
        let buffers = unsafe { &mut *self.pool.buffers[self.index].get() };
        (&mut buffers.rx, &mut buffers.tx)
    }
}

impl<const N: usize, const RX: usize, const TX: usize> Drop for PoolEntry<'_, N, RX, TX> {
    fn drop(&mut self) {
        self.pool
            .used
            .lock(|used| used.set(used.get() & !(1 << self.index)));
    }
}

/// Statically allocated storage of the IP stack. Place it in a `static`
/// (for example with `static_cell::StaticCell`) and pass it to
/// [`EspWireless::wifi`](crate::EspWireless::wifi).
///
/// - `SOCKETS`: socket slots of the stack, shared by everything that opens a
///   socket: DHCPv4 and DNS ([`STACK_INTERNAL_SOCKETS`]) plus `TCP`.
/// - `TCP`: simultaneous TCP connections.
/// - `RX` / `TX`: receive and transmit buffer of each connection, in bytes.
///
/// RAM use is about `TCP * (RX + TX)` plus the stack's socket table.
pub struct NetResources<
    const SOCKETS: usize,
    const TCP: usize,
    const RX: usize = 1024,
    const TX: usize = 1024,
> {
    stack: StackResources<SOCKETS>,
    pool: TcpPool<TCP, RX, TX>,
}

impl<const SOCKETS: usize, const TCP: usize, const RX: usize, const TX: usize>
    NetResources<SOCKETS, TCP, RX, TX>
{
    /// The stack aborts when it runs out of socket slots, so the budget is
    /// checked when the value is built.
    const BUDGET_OK: () = assert!(
        SOCKETS >= TCP + STACK_INTERNAL_SOCKETS,
        "SOCKETS must cover DHCP and DNS plus every TCP connection"
    );

    pub const fn new() -> Self {
        let () = Self::BUDGET_OK;
        Self {
            stack: StackResources::new(),
            pool: TcpPool::new(),
        }
    }

    pub(crate) fn split(
        &'static mut self,
    ) -> (
        &'static mut StackResources<SOCKETS>,
        &'static TcpPool<TCP, RX, TX>,
    ) {
        (&mut self.stack, &self.pool)
    }
}

impl<const SOCKETS: usize, const TCP: usize, const RX: usize, const TX: usize> Default
    for NetResources<SOCKETS, TCP, RX, TX>
{
    fn default() -> Self {
        Self::new()
    }
}

/// ESP32 implementation of [`IpBackend`], created together with the Wi-Fi
/// driver by [`EspWireless::wifi`](crate::EspWireless::wifi).
///
/// Place it in a `static` and hand out
/// [`NetworkHandle`](rohi_net::NetworkHandle)s to it.
pub struct EspIp<const TCP: usize, const RX: usize = 1024, const TX: usize = 1024> {
    stack: Stack<'static>,
    pool: &'static TcpPool<TCP, RX, TX>,
}

impl<const TCP: usize, const RX: usize, const TX: usize> EspIp<TCP, RX, TX> {
    pub(crate) fn new(stack: Stack<'static>, pool: &'static TcpPool<TCP, RX, TX>) -> Self {
        Self { stack, pool }
    }
}

impl<const TCP: usize, const RX: usize, const TX: usize> IpBackend for EspIp<TCP, RX, TX> {
    type Connection<'a>
        = EspTcpConnection<'a, TCP, RX, TX>
    where
        Self: 'a;

    fn is_ready(&self) -> bool {
        self.stack.is_link_up() && self.stack.is_config_up()
    }

    async fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, NetError> {
        // Only IPv4 is configured (DHCPv4).
        if matches!(addr_type, AddrType::IPv6) {
            return Err(NetError::DnsFailed);
        }
        let addresses = self
            .stack
            .dns_query(host, DnsQueryType::A)
            .await
            .map_err(|_| NetError::DnsFailed)?;
        let IpAddress::Ipv4(address) = addresses.first().copied().ok_or(NetError::DnsFailed)?;
        Ok(IpAddr::V4(Ipv4Addr::from(address.octets())))
    }

    async fn connect(
        &self,
        remote: SocketAddr,
    ) -> Result<EspTcpConnection<'_, TCP, RX, TX>, NetError> {
        let IpAddr::V4(address) = remote.ip() else {
            return Err(NetError::ConnectFailed);
        };
        let entry = self.pool.alloc().ok_or(NetError::NoSocketAvailable)?;
        // SAFETY: first and only use of this entry, see `PoolEntry::buffers`.
        let (rx, tx) = unsafe { entry.buffers() };
        let socket = TcpSocket::new(self.stack, rx, tx);
        // From here on the connection owns the entry: cancelling this future
        // drops it, which aborts the socket and frees the entry.
        let mut connection = EspTcpConnection {
            socket,
            _entry: entry,
        };
        let endpoint = IpEndpoint::new(IpAddress::Ipv4(address), remote.port());
        connection
            .socket
            .connect(endpoint)
            .await
            .map_err(map_connect_error)?;
        Ok(connection)
    }
}

fn map_connect_error(error: ConnectError) -> NetError {
    match error {
        ConnectError::TimedOut => NetError::Timeout,
        ConnectError::InvalidState => NetError::Backend,
        _ => NetError::ConnectFailed,
    }
}

/// An established TCP connection of [`EspIp`]. Dropping it aborts the
/// connection and releases its pool entry.
pub struct EspTcpConnection<'a, const TCP: usize, const RX: usize, const TX: usize> {
    // Field order matters: the socket must be gone before its buffers are
    // handed back to the pool.
    socket: TcpSocket<'a>,
    _entry: PoolEntry<'a, TCP, RX, TX>,
}

impl<const TCP: usize, const RX: usize, const TX: usize> Drop
    for EspTcpConnection<'_, TCP, RX, TX>
{
    fn drop(&mut self) {
        self.socket.abort();
    }
}

impl<const TCP: usize, const RX: usize, const TX: usize> ErrorType
    for EspTcpConnection<'_, TCP, RX, TX>
{
    type Error = NetError;
}

impl<const TCP: usize, const RX: usize, const TX: usize> Read
    for EspTcpConnection<'_, TCP, RX, TX>
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        self.socket
            .read(buf)
            .await
            .map_err(|_| NetError::ConnectionReset)
    }
}

impl<const TCP: usize, const RX: usize, const TX: usize> Write
    for EspTcpConnection<'_, TCP, RX, TX>
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, NetError> {
        self.socket
            .write(buf)
            .await
            .map_err(|_| NetError::ConnectionReset)
    }

    async fn flush(&mut self) -> Result<(), NetError> {
        self.socket
            .flush()
            .await
            .map_err(|_| NetError::ConnectionReset)
    }
}
