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

//! ESP32 IP data plane: DNS, TCP and UDP on the `embassy-net` stack.
//!
//! Implements the portable [`IpBackend`]. The stack and its bounded socket
//! storage are owned by [`NetResources`], which the firmware places in a
//! `static` and sizes itself; see [`EspWireless::wifi`](crate::EspWireless::wifi).
//!
//! TCP connections use a fixed pool of buffers inside [`NetResources`]. A
//! connection holds one pool entry from `connect` until it is dropped; when
//! all entries are in use `connect` fails at once with
//! [`NetError::NoSocketAvailable`]. UDP sockets use a second fixed pool the
//! same way, see [`IpBackend::udp_socket`]. No `embassy-net` type is public.

use core::cell::{Cell, UnsafeCell};
use core::net::{IpAddr, Ipv4Addr, SocketAddr};

use embassy_futures::select::{Either, select};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::{ConnectError, TcpSocket};
use embassy_net::udp::{PacketMetadata, RecvError, SendError, UdpSocket as EmbassyUdpSocket};
use embassy_net::{IpAddress, IpEndpoint, Stack, StackResources};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_nal_async::AddrType;
use rohi_net::{IpBackend, IpUsage, NetError, UdpSocket};

/// Sockets the stack itself needs: DHCPv4 and DNS.
pub const STACK_INTERNAL_SOCKETS: usize = 2;

struct TcpBuffers<const RX: usize, const TX: usize> {
    rx: [u8; RX],
    tx: [u8; TX],
}

/// Occupancy of a fixed pool: slot `i` is in use while bit `i` is set.
struct SlotMask<const N: usize> {
    used: Mutex<CriticalSectionRawMutex, Cell<u32>>,
}

impl<const N: usize> SlotMask<N> {
    const CAPACITY_OK: () = assert!(N <= 32, "at most 32 sockets per pool are supported");

    const fn new() -> Self {
        let () = Self::CAPACITY_OK;
        Self {
            used: Mutex::new(Cell::new(0)),
        }
    }

    fn acquire(&self) -> Option<usize> {
        self.used.lock(|used| {
            let mask = used.get();
            let index = (0..N).find(|i| mask & (1 << i) == 0)?;
            used.set(mask | (1 << index));
            Some(index)
        })
    }

    fn release(&self, index: usize) {
        self.used.lock(|used| used.set(used.get() & !(1 << index)));
    }

    fn in_use(&self) -> usize {
        self.used.lock(|used| used.get().count_ones() as usize)
    }
}

/// Fixed pool of TCP buffers; entry `i` is in use while bit `i` is set.
pub(crate) struct TcpPool<const N: usize, const RX: usize, const TX: usize> {
    slots: SlotMask<N>,
    buffers: [UnsafeCell<TcpBuffers<RX, TX>>; N],
}

impl<const N: usize, const RX: usize, const TX: usize> TcpPool<N, RX, TX> {
    const fn new() -> Self {
        Self {
            slots: SlotMask::new(),
            buffers: [const {
                UnsafeCell::new(TcpBuffers {
                    rx: [0; RX],
                    tx: [0; TX],
                })
            }; N],
        }
    }

    fn alloc(&self) -> Option<PoolEntry<'_, N, RX, TX>> {
        let index = self.slots.acquire()?;
        Some(PoolEntry { pool: self, index })
    }

    fn in_use(&self) -> usize {
        self.slots.in_use()
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
        self.pool.slots.release(self.index);
    }
}

/// Datagrams each UDP socket can queue per direction.
pub const UDP_PACKETS: usize = 2;

struct UdpBuffers<const BUF: usize> {
    rx_meta: [PacketMetadata; UDP_PACKETS],
    tx_meta: [PacketMetadata; UDP_PACKETS],
    rx: [u8; BUF],
    tx: [u8; BUF],
}

/// Fixed pool of UDP socket buffers, allocated like [`TcpPool`].
pub(crate) struct UdpPool<const N: usize, const BUF: usize> {
    slots: SlotMask<N>,
    buffers: [UnsafeCell<UdpBuffers<BUF>>; N],
}

impl<const N: usize, const BUF: usize> UdpPool<N, BUF> {
    const fn new() -> Self {
        Self {
            slots: SlotMask::new(),
            buffers: [const {
                UnsafeCell::new(UdpBuffers {
                    rx_meta: [PacketMetadata::EMPTY; UDP_PACKETS],
                    tx_meta: [PacketMetadata::EMPTY; UDP_PACKETS],
                    rx: [0; BUF],
                    tx: [0; BUF],
                })
            }; N],
        }
    }

    fn alloc(&self) -> Option<UdpEntry<'_, N, BUF>> {
        let index = self.slots.acquire()?;
        Some(UdpEntry { pool: self, index })
    }

    fn in_use(&self) -> usize {
        self.slots.in_use()
    }
}

/// An allocated UDP pool entry; released on drop.
struct UdpEntry<'a, const N: usize, const BUF: usize> {
    pool: &'a UdpPool<N, BUF>,
    index: usize,
}

/// The four buffers of one UDP socket.
type UdpParts<'a> = (
    &'a mut [PacketMetadata],
    &'a mut [u8],
    &'a mut [PacketMetadata],
    &'a mut [u8],
);

impl<'a, const N: usize, const BUF: usize> UdpEntry<'a, N, BUF> {
    /// Receive and transmit metadata and buffers of this entry.
    ///
    /// # Safety
    ///
    /// As [`PoolEntry::buffers`]: call once per entry.
    unsafe fn buffers(&self) -> UdpParts<'a> {
        let buffers = unsafe { &mut *self.pool.buffers[self.index].get() };
        (
            &mut buffers.rx_meta,
            &mut buffers.rx,
            &mut buffers.tx_meta,
            &mut buffers.tx,
        )
    }
}

impl<const N: usize, const BUF: usize> Drop for UdpEntry<'_, N, BUF> {
    fn drop(&mut self) {
        self.pool.slots.release(self.index);
    }
}

/// Statically allocated storage of the IP stack. Place it in a `static`
/// (for example with `static_cell::StaticCell`) and pass it to
/// [`EspWireless::wifi`](crate::EspWireless::wifi).
///
/// - `SOCKETS`: socket slots of the stack, shared by everything that opens a
///   socket: DHCPv4 and DNS ([`STACK_INTERNAL_SOCKETS`]) plus `TCP` and
///   `UDP`.
/// - `TCP`: simultaneous TCP connections.
/// - `RX` / `TX`: receive and transmit buffer of each connection, in bytes.
/// - `UDP`: simultaneous UDP sockets, none by default.
/// - `UDP_BUF`: receive and transmit buffer of each UDP socket, in bytes;
///   each direction queues at most [`UDP_PACKETS`] datagrams.
///
/// RAM use is about `TCP * (RX + TX) + UDP * 2 * UDP_BUF` plus the stack's
/// socket table.
pub struct NetResources<
    const SOCKETS: usize,
    const TCP: usize,
    const RX: usize = 1024,
    const TX: usize = 1024,
    const UDP: usize = 0,
    const UDP_BUF: usize = 512,
> {
    stack: StackResources<SOCKETS>,
    tcp: TcpPool<TCP, RX, TX>,
    udp: UdpPool<UDP, UDP_BUF>,
}

impl<
    const SOCKETS: usize,
    const TCP: usize,
    const RX: usize,
    const TX: usize,
    const UDP: usize,
    const UDP_BUF: usize,
> NetResources<SOCKETS, TCP, RX, TX, UDP, UDP_BUF>
{
    /// The stack aborts when it runs out of socket slots, so the budget is
    /// checked when the value is built.
    const BUDGET_OK: () = assert!(
        SOCKETS >= TCP + UDP + STACK_INTERNAL_SOCKETS,
        "SOCKETS must cover DHCP and DNS plus every TCP connection and UDP socket"
    );

    pub const fn new() -> Self {
        let () = Self::BUDGET_OK;
        Self {
            stack: StackResources::new(),
            tcp: TcpPool::new(),
            udp: UdpPool::new(),
        }
    }

    #[allow(clippy::type_complexity)]
    pub(crate) fn split(
        &'static mut self,
    ) -> (
        &'static mut StackResources<SOCKETS>,
        &'static TcpPool<TCP, RX, TX>,
        &'static UdpPool<UDP, UDP_BUF>,
    ) {
        (&mut self.stack, &self.tcp, &self.udp)
    }
}

impl<
    const SOCKETS: usize,
    const TCP: usize,
    const RX: usize,
    const TX: usize,
    const UDP: usize,
    const UDP_BUF: usize,
> Default for NetResources<SOCKETS, TCP, RX, TX, UDP, UDP_BUF>
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
pub struct EspIp<
    const TCP: usize,
    const RX: usize = 1024,
    const TX: usize = 1024,
    const UDP: usize = 0,
    const UDP_BUF: usize = 512,
> {
    stack: Stack<'static>,
    tcp: &'static TcpPool<TCP, RX, TX>,
    udp: &'static UdpPool<UDP, UDP_BUF>,
}

impl<const TCP: usize, const RX: usize, const TX: usize, const UDP: usize, const UDP_BUF: usize>
    EspIp<TCP, RX, TX, UDP, UDP_BUF>
{
    pub(crate) fn new(
        stack: Stack<'static>,
        tcp: &'static TcpPool<TCP, RX, TX>,
        udp: &'static UdpPool<UDP, UDP_BUF>,
    ) -> Self {
        Self { stack, tcp, udp }
    }
}

impl<const TCP: usize, const RX: usize, const TX: usize, const UDP: usize, const UDP_BUF: usize>
    IpBackend for EspIp<TCP, RX, TX, UDP, UDP_BUF>
{
    type Connection<'a>
        = EspTcpConnection<'a, TCP, RX, TX>
    where
        Self: 'a;

    type Udp<'a>
        = EspUdpSocket<'a, UDP, UDP_BUF>
    where
        Self: 'a;

    fn is_ready(&self) -> bool {
        self.stack.is_link_up() && self.stack.is_config_up()
    }

    fn usage(&self) -> IpUsage {
        IpUsage {
            tcp_in_use: self.tcp.in_use(),
            tcp_capacity: TCP,
            udp_in_use: self.udp.in_use(),
            udp_capacity: UDP,
        }
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
        let entry = self.tcp.alloc().ok_or(NetError::NoSocketAvailable)?;
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

    fn udp_socket(&self) -> Result<EspUdpSocket<'_, UDP, UDP_BUF>, NetError> {
        let entry = self.udp.alloc().ok_or(NetError::NoSocketAvailable)?;
        // SAFETY: first and only use of this entry, see `UdpEntry::buffers`.
        let (rx_meta, rx, tx_meta, tx) = unsafe { entry.buffers() };
        let mut socket = EmbassyUdpSocket::new(self.stack, rx_meta, rx, tx_meta, tx);
        // Port 0: the stack picks an ephemeral port.
        socket.bind(0).map_err(|_| NetError::Backend)?;
        Ok(EspUdpSocket {
            socket,
            stack: self.stack,
            _entry: entry,
        })
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

/// A UDP socket of [`EspIp`]. Dropping it closes the socket and releases its
/// pool entry.
pub struct EspUdpSocket<'a, const UDP: usize, const UDP_BUF: usize> {
    // Field order matters: the socket must be gone before its buffers are
    // handed back to the pool.
    socket: EmbassyUdpSocket<'a>,
    stack: Stack<'static>,
    _entry: UdpEntry<'a, UDP, UDP_BUF>,
}

async fn wait_for_network_down(stack: Stack<'static>) {
    let _ = select(stack.wait_link_down(), stack.wait_config_down()).await;
}

impl<const UDP: usize, const UDP_BUF: usize> UdpSocket for EspUdpSocket<'_, UDP, UDP_BUF> {
    async fn send_to(&self, data: &[u8], remote: SocketAddr) -> Result<(), NetError> {
        let IpAddr::V4(address) = remote.ip() else {
            return Err(NetError::Backend);
        };
        let endpoint = IpEndpoint::new(IpAddress::Ipv4(address), remote.port());
        self.socket
            .send_to(data, endpoint)
            .await
            .map_err(|error| match error {
                SendError::NoRoute => NetError::NetworkUnavailable,
                _ => NetError::Backend,
            })
    }

    async fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SocketAddr), NetError> {
        loop {
            let result = match select(
                self.socket.recv_from(buf),
                wait_for_network_down(self.stack),
            )
            .await
            {
                Either::First(result) => result,
                Either::Second(()) => return Err(NetError::NetworkUnavailable),
            };
            if !self.stack.is_link_up() || !self.stack.is_config_up() {
                return Err(NetError::NetworkUnavailable);
            }
            match result {
                Ok((len, meta)) => {
                    let IpAddress::Ipv4(address) = meta.endpoint.addr;
                    let from = SocketAddr::new(
                        IpAddr::V4(Ipv4Addr::from(address.octets())),
                        meta.endpoint.port,
                    );
                    return Ok((len, from));
                }
                // The stack already dropped the oversized datagram.
                Err(RecvError::Truncated) => {}
            }
        }
    }
}
