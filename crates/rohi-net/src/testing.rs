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

//! Host test support: an in-memory [`IpBackend`].
//!
//! [`FakeIp`] stands in for the platform IP stack so that code built on
//! [`NetworkHandle`](crate::NetworkHandle) (or on the `embedded-nal-async`
//! traits) can be tested without hardware: readiness, DNS answers, refused
//! connections, slot exhaustion, held operations (for cancellation tests) and
//! the peer side of every connection.
//!
//! Enabled with the `testing` feature; it is `no_std` and heapless like the
//! rest of the crate, but not meant for firmware.

use core::cell::RefCell;
use core::future::poll_fn;
use core::net::{IpAddr, SocketAddr};
use core::task::{Poll, Waker};

use embedded_io_async::{ErrorType, Read, Write};
use embedded_nal_async::AddrType;
use heapless::{Deque, String, Vec};

use crate::{IpBackend, NetError};

const MAX_HOSTS: usize = 8;
const MAX_HOST_LEN: usize = 64;
const MAX_REFUSED: usize = 8;
const MAX_WAITERS: usize = 8;

struct Slot<const BUF: usize> {
    remote: SocketAddr,
    /// Peer to device.
    rx: Deque<u8, BUF>,
    /// Device to peer.
    tx: Vec<u8, BUF>,
    closed: bool,
    reset: bool,
    waker: Option<Waker>,
}

impl<const BUF: usize> Slot<BUF> {
    fn wake(&mut self) {
        if let Some(waker) = self.waker.take() {
            waker.wake();
        }
    }
}

struct State<const SLOTS: usize, const BUF: usize> {
    ready: bool,
    hosts: Vec<(String<MAX_HOST_LEN>, IpAddr), MAX_HOSTS>,
    refused: Vec<SocketAddr, MAX_REFUSED>,
    hold_connect: bool,
    hold_dns: bool,
    waiters: Vec<Waker, MAX_WAITERS>,
    slots: [Option<Slot<BUF>>; SLOTS],
    connects: usize,
    dns_queries: usize,
}

impl<const SLOTS: usize, const BUF: usize> State<SLOTS, BUF> {
    fn slot_of(&mut self, remote: SocketAddr) -> Option<&mut Slot<BUF>> {
        self.slots
            .iter_mut()
            .flatten()
            .find(|slot| slot.remote == remote)
    }

    fn wake_waiters(&mut self) {
        for waker in self.waiters.drain(..) {
            waker.wake();
        }
    }
}

/// In-memory IP stack with `SLOTS` TCP connections of `BUF` bytes per
/// direction.
///
/// It starts *not ready*. Making it not ready resets every open connection,
/// like a lost link does on a real stack.
pub struct FakeIp<const SLOTS: usize = 2, const BUF: usize = 512> {
    state: RefCell<State<SLOTS, BUF>>,
}

impl<const SLOTS: usize, const BUF: usize> Default for FakeIp<SLOTS, BUF> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SLOTS: usize, const BUF: usize> FakeIp<SLOTS, BUF> {
    pub const fn new() -> Self {
        Self {
            state: RefCell::new(State {
                ready: false,
                hosts: Vec::new(),
                refused: Vec::new(),
                hold_connect: false,
                hold_dns: false,
                waiters: Vec::new(),
                slots: [const { None }; SLOTS],
                connects: 0,
                dns_queries: 0,
            }),
        }
    }

    /// Sets readiness; `false` resets every open connection.
    pub fn set_ready(&self, ready: bool) {
        let mut state = self.state.borrow_mut();
        state.ready = ready;
        if !ready {
            for slot in state.slots.iter_mut().flatten() {
                slot.reset = true;
                slot.wake();
            }
        }
        state.wake_waiters();
    }

    /// Answers DNS queries for `host` with `addr`.
    ///
    /// # Panics
    ///
    /// If more than 8 hosts are added or `host` exceeds 64 bytes.
    pub fn add_host(&self, host: &str, addr: IpAddr) {
        let name = String::try_from(host).expect("host name too long");
        self.state
            .borrow_mut()
            .hosts
            .push((name, addr))
            .expect("too many hosts");
    }

    /// Connections to `remote` are refused.
    ///
    /// # Panics
    ///
    /// If more than 8 addresses are refused.
    pub fn refuse(&self, remote: SocketAddr) {
        self.state
            .borrow_mut()
            .refused
            .push(remote)
            .expect("too many refused addresses");
    }

    /// While set, `connect` pends after taking its slot.
    pub fn hold_connects(&self, hold: bool) {
        let mut state = self.state.borrow_mut();
        state.hold_connect = hold;
        state.wake_waiters();
    }

    /// While set, DNS queries pend.
    pub fn hold_dns(&self, hold: bool) {
        let mut state = self.state.borrow_mut();
        state.hold_dns = hold;
        state.wake_waiters();
    }

    /// Number of open (or opening) connections.
    pub fn open_connections(&self) -> usize {
        self.state.borrow().slots.iter().flatten().count()
    }

    /// Connection attempts that reached the backend.
    pub fn connect_attempts(&self) -> usize {
        self.state.borrow().connects
    }

    /// DNS queries that reached the backend.
    pub fn dns_queries(&self) -> usize {
        self.state.borrow().dns_queries
    }

    /// The peer `remote` sends `data` to the device. Returns the number of
    /// bytes buffered, `0` if no connection to `remote` is open.
    pub fn peer_send(&self, remote: SocketAddr, data: &[u8]) -> usize {
        let mut state = self.state.borrow_mut();
        let Some(slot) = state.slot_of(remote) else {
            return 0;
        };
        let mut sent = 0;
        for byte in data {
            if slot.rx.push_back(*byte).is_err() {
                break;
            }
            sent += 1;
        }
        slot.wake();
        sent
    }

    /// Takes everything the device wrote to `remote`.
    pub fn peer_received(&self, remote: SocketAddr) -> Vec<u8, BUF> {
        let mut state = self.state.borrow_mut();
        match state.slot_of(remote) {
            Some(slot) => {
                let data = core::mem::take(&mut slot.tx);
                slot.wake();
                data
            }
            None => Vec::new(),
        }
    }

    /// The peer closes its side: reads return `Ok(0)` once drained.
    pub fn peer_close(&self, remote: SocketAddr) {
        if let Some(slot) = self.state.borrow_mut().slot_of(remote) {
            slot.closed = true;
            slot.wake();
        }
    }

    /// The peer resets the connection.
    pub fn peer_reset(&self, remote: SocketAddr) {
        if let Some(slot) = self.state.borrow_mut().slot_of(remote) {
            slot.reset = true;
            slot.wake();
        }
    }

    /// Pends while `held` is true for the current state.
    async fn wait_released(&self, held: impl Fn(&State<SLOTS, BUF>) -> bool) {
        poll_fn(|cx| {
            let mut state = self.state.borrow_mut();
            if held(&state) {
                if !state.waiters.iter().any(|w| w.will_wake(cx.waker())) {
                    let _ = state.waiters.push(cx.waker().clone());
                }
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        })
        .await
    }
}

impl<const SLOTS: usize, const BUF: usize> IpBackend for FakeIp<SLOTS, BUF> {
    type Connection<'a>
        = FakeConnection<'a, SLOTS, BUF>
    where
        Self: 'a;

    fn is_ready(&self) -> bool {
        self.state.borrow().ready
    }

    async fn resolve(&self, host: &str, addr_type: AddrType) -> Result<IpAddr, NetError> {
        self.state.borrow_mut().dns_queries += 1;
        self.wait_released(|state| state.hold_dns).await;
        let state = self.state.borrow();
        if !state.ready {
            return Err(NetError::DnsFailed);
        }
        state
            .hosts
            .iter()
            .filter(|(name, _)| name.as_str() == host)
            .map(|(_, addr)| *addr)
            .find(|addr| match addr_type {
                AddrType::IPv4 => addr.is_ipv4(),
                AddrType::IPv6 => addr.is_ipv6(),
                AddrType::Either => true,
            })
            .ok_or(NetError::DnsFailed)
    }

    async fn connect(
        &self,
        remote: SocketAddr,
    ) -> Result<FakeConnection<'_, SLOTS, BUF>, NetError> {
        let index = {
            let mut state = self.state.borrow_mut();
            state.connects += 1;
            let index = state
                .slots
                .iter()
                .position(Option::is_none)
                .ok_or(NetError::NoSocketAvailable)?;
            state.slots[index] = Some(Slot {
                remote,
                rx: Deque::new(),
                tx: Vec::new(),
                closed: false,
                reset: false,
                waker: None,
            });
            index
        };
        // The connection owns the slot from here on: dropping this future
        // drops it and frees the slot.
        let connection = FakeConnection {
            ip: self,
            slot: index,
        };
        self.wait_released(|state| state.hold_connect).await;
        let state = self.state.borrow();
        if !state.ready {
            return Err(NetError::ConnectFailed);
        }
        if state.refused.contains(&remote) {
            return Err(NetError::ConnectFailed);
        }
        drop(state);
        Ok(connection)
    }
}

/// A connection of [`FakeIp`]; dropping it frees its slot.
pub struct FakeConnection<'a, const SLOTS: usize, const BUF: usize> {
    ip: &'a FakeIp<SLOTS, BUF>,
    slot: usize,
}

impl<const SLOTS: usize, const BUF: usize> Drop for FakeConnection<'_, SLOTS, BUF> {
    fn drop(&mut self) {
        self.ip.state.borrow_mut().slots[self.slot] = None;
    }
}

impl<const SLOTS: usize, const BUF: usize> ErrorType for FakeConnection<'_, SLOTS, BUF> {
    type Error = NetError;
}

impl<const SLOTS: usize, const BUF: usize> Read for FakeConnection<'_, SLOTS, BUF> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, NetError> {
        if buf.is_empty() {
            return Ok(0);
        }
        poll_fn(|cx| {
            let mut state = self.ip.state.borrow_mut();
            let Some(slot) = state.slots[self.slot].as_mut() else {
                return Poll::Ready(Err(NetError::ConnectionReset));
            };
            if slot.reset {
                return Poll::Ready(Err(NetError::ConnectionReset));
            }
            if !slot.rx.is_empty() {
                let mut read = 0;
                while read < buf.len() {
                    match slot.rx.pop_front() {
                        Some(byte) => {
                            buf[read] = byte;
                            read += 1;
                        }
                        None => break,
                    }
                }
                return Poll::Ready(Ok(read));
            }
            if slot.closed {
                return Poll::Ready(Ok(0));
            }
            slot.waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

impl<const SLOTS: usize, const BUF: usize> Write for FakeConnection<'_, SLOTS, BUF> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, NetError> {
        if buf.is_empty() {
            return Ok(0);
        }
        poll_fn(|cx| {
            let mut state = self.ip.state.borrow_mut();
            let Some(slot) = state.slots[self.slot].as_mut() else {
                return Poll::Ready(Err(NetError::ConnectionReset));
            };
            if slot.reset {
                return Poll::Ready(Err(NetError::ConnectionReset));
            }
            let room = BUF - slot.tx.len();
            if room == 0 {
                slot.waker = Some(cx.waker().clone());
                return Poll::Pending;
            }
            let written = room.min(buf.len());
            let _ = slot.tx.extend_from_slice(&buf[..written]);
            Poll::Ready(Ok(written))
        })
        .await
    }

    async fn flush(&mut self) -> Result<(), NetError> {
        Ok(())
    }
}
