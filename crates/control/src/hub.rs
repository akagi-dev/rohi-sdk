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
//! The mailbox between the transports and the one control worker.

use core::cell::{Cell, RefCell};

use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::{Mutex, MutexGuard};
use embassy_sync::signal::Signal;
use heapless::Vec;
use rohi_control_proto::limits::{MAX_REQUEST_LEN, MAX_RESPONSE_LEN};
use zeroize::Zeroize;

use crate::{
    AuthorizationPolicy, ControlAccess, ControlBackend, Controller, LinkSecurity,
    RequestAuthorization, Transport,
};

/// The most connections that are live at once, over every transport.
pub const MAX_SESSIONS: usize = 4;

// Requests in flight plus session endings that wait for the worker.
const QUEUE: usize = 8;

type Raw = CriticalSectionRawMutex;

/// The identity of a live connection, unique among the live connections of
/// every transport of the device.
///
/// Allocated by [`ControlHub::open_session`]; a transport's own counter (a
/// BLE connection epoch, a TCP accept counter) is not unique across
/// transports and never used as a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionId(u64);

impl SessionId {
    /// The value carried in [`LinkSecurity::session`].
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A request cannot be admitted now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    /// Every slot is taken, or the connection already has a request in flight,
    /// or too many connections are live.
    Busy,
    /// The session is not live (it was closed).
    Closed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Free,
    /// A frontend holds the slot and fills the request.
    Reserved,
    /// The ticket is in the mailbox.
    Queued,
    /// The worker handles the request.
    Running,
    /// The response is ready for the frontend.
    Done,
    /// The frontend is gone; the worker releases the slot.
    Abandoned,
}

#[derive(Clone, Copy)]
struct Meta {
    state: State,
    generation: u32,
    session: u64,
    request_len: usize,
    response_len: usize,
}

struct SlotData {
    request: [u8; MAX_REQUEST_LEN],
    response: [u8; MAX_RESPONSE_LEN],
}

struct Slot {
    data: Mutex<Raw, SlotData>,
    meta: BlockingMutex<Raw, RefCell<Meta>>,
    done: Signal<Raw, u32>,
}

impl Slot {
    const fn new() -> Self {
        Self {
            data: Mutex::new(SlotData {
                request: [0; MAX_REQUEST_LEN],
                response: [0; MAX_RESPONSE_LEN],
            }),
            meta: BlockingMutex::new(RefCell::new(Meta {
                state: State::Free,
                generation: 0,
                session: 0,
                request_len: 0,
                response_len: 0,
            })),
            done: Signal::new(),
        }
    }

    fn meta<R>(&self, f: impl FnOnce(&mut Meta) -> R) -> R {
        self.meta.lock(|meta| f(&mut meta.borrow_mut()))
    }
}

#[derive(Clone, Copy)]
enum Work {
    Request { slot: usize, generation: u32 },
    Close(u64),
}

#[derive(Clone, Copy)]
struct Session {
    id: u64,
    transport: Transport,
    encrypted: bool,
    authenticated: bool,
}

struct Sessions {
    // Sessions issued so far; the next identity is this plus one. It starts at
    // zero so that the whole hub is zero-initialized and lands in `.bss`.
    issued: u64,
    live: Vec<Session, MAX_SESSIONS>,
}

/// The mailbox of the control worker: `SLOTS` request/response buffers that
/// the transports fill and read, a bounded queue of tickets and the table of
/// live sessions.
///
/// ```text
/// transport frontend                          control_task
///   open_session ─► SessionId
///   request(session) ─► Reservation ─ fill ─ submit ─┐
///                                                    ├─► ticket ─► serve: Controller::handle
///   Response ◄──────────── response in the same slot ┘
///   close_session ─► ticket ─► Controller::session_closed
/// ```
///
/// Only [`serve`](Self::serve) calls the [`Controller`], so Config, Status,
/// Actions and streams of every transport run in one task, one request at a
/// time. A request and its response live in one slot for their whole life:
/// the queue carries a ticket (slot, generation), never a buffer. A slot goes
/// `free -> reserved -> queued -> running -> done -> free`; a frontend that is
/// dropped meanwhile (disconnect, timeout) abandons its ticket and the slot is
/// released by whoever may still touch it. The buffers are wiped when the
/// slot is released.
///
/// Admission is explicit: a connection has at most one request in flight and
/// there are `SLOTS` slots, so [`request`](Self::request) answers [`Busy`]
/// instead of waiting or allocating. A long action delays the requests of
/// every other connection; that is the price of the single owner and is
/// documented in the crate documentation.
pub struct ControlHub<'a, const SLOTS: usize> {
    // Empty until [`bind`](ControlHub::bind), so that a `static` hub is all
    // zeros and lands in `.bss`: a reference would put it in `.data`.
    access: BlockingMutex<Raw, Cell<Option<&'a ControlAccess>>>,
    slots: [Slot; SLOTS],
    queue: Channel<Raw, Work, QUEUE>,
    sessions: BlockingMutex<Raw, RefCell<Sessions>>,
}

impl<'a, const SLOTS: usize> ControlHub<'a, SLOTS> {
    /// An empty hub that counts no connection until it is
    /// [`bind`](Self::bind)-bound to the access state. All zeros: a `static` of
    /// it costs `.bss` only.
    pub const fn new() -> Self {
        Self {
            access: BlockingMutex::new(Cell::new(None)),
            slots: [const { Slot::new() }; SLOTS],
            queue: Channel::new(),
            sessions: BlockingMutex::new(RefCell::new(Sessions {
                issued: 0,
                live: Vec::new(),
            })),
        }
    }

    /// An empty hub whose connections are counted in `access`.
    pub fn with_access(access: &'a ControlAccess) -> Self {
        let hub = Self::new();
        hub.bind(access);
        hub
    }

    /// Reports the connections of the hub, and the errors the transports note
    /// with [`transport_error`](Self::transport_error), to `access`.
    pub fn bind(&self, access: &'a ControlAccess) {
        self.access.lock(|slot| slot.set(Some(access)));
    }

    /// The access state the hub reports to, once bound.
    pub fn access(&self) -> Option<&'a ControlAccess> {
        self.access.lock(Cell::get)
    }

    /// A transport met a framing or I/O error of the given kind (not 0).
    pub fn transport_error(&self, transport: Transport, kind: u8) {
        if let Some(access) = self.access() {
            access.transport_error(transport, kind);
        }
    }

    /// Registers a new connection of `transport` on a link without
    /// encryption.
    ///
    /// # Errors
    ///
    /// [`Busy::Busy`] when [`MAX_SESSIONS`] connections are live or the
    /// 64-bit identity space is exhausted.
    pub fn open_session(&self, transport: Transport) -> Result<SessionId, Busy> {
        let id = self.sessions.lock(|sessions| {
            let mut sessions = sessions.borrow_mut();
            if sessions.live.is_full() {
                return None;
            }
            let id = sessions.issued.checked_add(1)?;
            sessions.issued = id;
            let _ = sessions.live.push(Session {
                id,
                transport,
                encrypted: false,
                authenticated: false,
            });
            Some(id)
        });
        let id = id.ok_or(Busy::Busy)?;
        if let Some(access) = self.access() {
            access.session_opened(transport);
        }
        Ok(SessionId(id))
    }

    /// Records what the transport knows of the security of the link of
    /// `session`. It is the transport's report of its own link, copied by the
    /// worker into the [`LinkSecurity`] of every request of the session.
    pub fn set_link(&self, session: SessionId, encrypted: bool, authenticated: bool) {
        self.sessions.lock(|sessions| {
            if let Some(entry) = sessions
                .borrow_mut()
                .live
                .iter_mut()
                .find(|entry| entry.id == session.0)
            {
                entry.encrypted = encrypted;
                entry.authenticated = authenticated;
            }
        });
    }

    /// Ends a connection: its requests that did not start are not executed
    /// and the worker closes its streams and forgets its challenge, in order
    /// with the requests already queued.
    ///
    /// Waits only if the queue is full.
    pub async fn close_session(&self, session: SessionId) {
        if self.forget(session) {
            self.queue.send(Work::Close(session.0)).await;
        }
    }

    // Removes the session; `false` if it was not live.
    fn forget(&self, session: SessionId) -> bool {
        let transport = self.sessions.lock(|sessions| {
            let mut sessions = sessions.borrow_mut();
            let at = sessions
                .live
                .iter()
                .position(|entry| entry.id == session.0)?;
            Some(sessions.live.swap_remove(at).transport)
        });
        let Some(transport) = transport else {
            return false;
        };
        if let Some(access) = self.access() {
            access.session_closed(transport);
        }
        true
    }

    /// Registers a connection of `transport` that ends with its guard.
    ///
    /// # Errors
    ///
    /// As [`open_session`](Self::open_session).
    pub fn connect(&self, transport: Transport) -> Result<Connection<'_, 'a, SLOTS>, Busy> {
        Ok(Connection {
            hub: self,
            id: self.open_session(transport)?,
        })
    }

    /// Reserves a slot for one request of `session`.
    ///
    /// # Errors
    ///
    /// [`Busy::Closed`] for a session that is not live, [`Busy::Busy`] when
    /// the session already has a request in flight or no slot is free.
    pub fn request(&self, session: SessionId) -> Result<Reservation<'_, 'a, SLOTS>, Busy> {
        if !self.sessions.lock(|sessions| {
            sessions
                .borrow()
                .live
                .iter()
                .any(|entry| entry.id == session.0)
        }) {
            return Err(Busy::Closed);
        }
        if self
            .slots
            .iter()
            .any(|slot| slot.meta(|meta| meta.state != State::Free && meta.session == session.0))
        {
            return Err(Busy::Busy);
        }
        for (index, slot) in self.slots.iter().enumerate() {
            let Ok(data) = slot.data.try_lock() else {
                continue;
            };
            let generation = slot.meta(|meta| {
                if meta.state != State::Free {
                    return None;
                }
                meta.state = State::Reserved;
                meta.generation = meta.generation.wrapping_add(1);
                meta.session = session.0;
                meta.request_len = 0;
                Some(meta.generation)
            });
            let Some(generation) = generation else {
                continue;
            };
            slot.done.reset();
            return Ok(Reservation {
                hub: self,
                slot: index,
                generation,
                data: Some(data),
            });
        }
        Err(Busy::Busy)
    }

    fn link_of(&self, session: u64) -> Option<LinkSecurity> {
        self.sessions.lock(|sessions| {
            sessions
                .borrow()
                .live
                .iter()
                .find(|entry| entry.id == session)
                .map(|entry| LinkSecurity {
                    encrypted: entry.encrypted,
                    authenticated: entry.authenticated,
                    session: entry.id,
                    transport: entry.transport,
                    authorization: RequestAuthorization::Anonymous,
                })
        })
    }

    /// The control worker: serves the tickets of every transport with the one
    /// `controller`, forever. Spawn exactly one.
    pub async fn serve<B: ControlBackend, P: AuthorizationPolicy>(
        &self,
        controller: &Controller<B, P>,
    ) -> ! {
        loop {
            match self.queue.receive().await {
                Work::Close(session) => controller.session_closed(session).await,
                Work::Request { slot, generation } => self.run(controller, slot, generation).await,
            }
        }
    }

    async fn run<B: ControlBackend, P: AuthorizationPolicy>(
        &self,
        controller: &Controller<B, P>,
        index: usize,
        generation: u32,
    ) {
        let Some(slot) = self.slots.get(index) else {
            return;
        };
        let started = slot.meta(|meta| {
            if meta.generation != generation {
                return None;
            }
            match meta.state {
                State::Queued => {
                    meta.state = State::Running;
                    Some(Some((meta.session, meta.request_len)))
                }
                State::Abandoned => Some(None),
                _ => None,
            }
        });
        let Some(started) = started else {
            return;
        };
        let mut data = slot.data.lock().await;
        let Some((session, len)) = started else {
            release(slot, &mut data);
            return;
        };
        // A session that ended meanwhile gets no execution.
        let Some(link) = self.link_of(session) else {
            slot.meta(|meta| meta.state = State::Abandoned);
            release(slot, &mut data);
            return;
        };
        let SlotData { request, response } = &mut *data;
        let written = controller
            .handle(&link, &request[..len.min(MAX_REQUEST_LEN)], response)
            .await;
        request.zeroize();
        let delivered = slot.meta(|meta| {
            if meta.state == State::Abandoned {
                return false;
            }
            meta.response_len = written;
            meta.state = State::Done;
            true
        });
        if delivered {
            slot.done.signal(generation);
        } else {
            release(slot, &mut data);
        }
    }
}

// Wipes the buffers and frees the slot. The caller holds the data lock, so
// nobody else can read them.
fn release(slot: &Slot, data: &mut SlotData) {
    data.request.zeroize();
    data.response.zeroize();
    slot.meta(|meta| meta.state = State::Free);
}

/// A slot reserved for one request: fill [`buffer`](Self::buffer), then
/// [`submit`](Self::submit). Dropping it frees the slot.
pub struct Reservation<'h, 'a, const SLOTS: usize> {
    hub: &'h ControlHub<'a, SLOTS>,
    slot: usize,
    generation: u32,
    data: Option<MutexGuard<'h, Raw, SlotData>>,
}

impl<const SLOTS: usize> Reservation<'_, '_, SLOTS> {
    /// The request buffer, [`MAX_REQUEST_LEN`] bytes: the transport receives
    /// straight into it.
    pub fn buffer(&mut self) -> &mut [u8] {
        match self.data.as_mut() {
            Some(data) => &mut data.request,
            None => &mut [],
        }
    }
}

impl<const SLOTS: usize> Drop for Reservation<'_, '_, SLOTS> {
    fn drop(&mut self) {
        if let Some(mut data) = self.data.take() {
            release(&self.hub.slots[self.slot], &mut data);
        }
    }
}

impl<'h, 'a, const SLOTS: usize> Reservation<'h, 'a, SLOTS> {
    /// Hands the first `len` bytes of the buffer to the control worker and
    /// waits for its response.
    ///
    /// Cancellation: dropping the future abandons the request. One that did
    /// not start is not executed; one that is running finishes, stays
    /// committed and its response is discarded; the slot is released and
    /// wiped by the worker. The request is never repeated.
    pub async fn submit(mut self, len: usize) -> Response<'h, 'a, SLOTS> {
        let hub = self.hub;
        let slot = &hub.slots[self.slot];
        let (index, generation) = (self.slot, self.generation);
        let len = len.min(MAX_REQUEST_LEN);
        // Unlock the buffer for the worker.
        drop(self.data.take());
        slot.meta(|meta| {
            meta.request_len = len;
            meta.state = State::Queued;
        });
        let mut flight = Flight {
            hub,
            slot: index,
            generation,
            sent: false,
        };
        hub.queue
            .send(Work::Request {
                slot: index,
                generation,
            })
            .await;
        flight.sent = true;
        while slot.done.wait().await != generation {}
        let data = slot.data.lock().await;
        let len = slot.meta(|meta| meta.response_len);
        Response {
            data,
            len,
            _flight: flight,
        }
    }
}

// The ticket of a submitted request, until its response is consumed.
struct Flight<'h, 'a, const SLOTS: usize> {
    hub: &'h ControlHub<'a, SLOTS>,
    slot: usize,
    generation: u32,
    sent: bool,
}

impl<const SLOTS: usize> Drop for Flight<'_, '_, SLOTS> {
    fn drop(&mut self) {
        let slot = &self.hub.slots[self.slot];
        if !self.sent {
            // The worker never heard of the ticket.
            if let Ok(mut data) = slot.data.try_lock() {
                release(slot, &mut data);
            }
            return;
        }
        let state = slot.meta(|meta| {
            if meta.generation != self.generation {
                return meta.state;
            }
            if matches!(meta.state, State::Queued | State::Running) {
                meta.state = State::Abandoned;
            }
            meta.state
        });
        if state == State::Done
            && let Ok(mut data) = slot.data.try_lock()
        {
            release(slot, &mut data);
        }
    }
}

/// The response to a submitted request, in the slot of the request. Dropping
/// it wipes the slot and frees it.
pub struct Response<'h, 'a, const SLOTS: usize> {
    data: MutexGuard<'h, Raw, SlotData>,
    len: usize,
    _flight: Flight<'h, 'a, SLOTS>,
}

impl<const SLOTS: usize> Response<'_, '_, SLOTS> {
    /// The encoded response; empty if not even an error fit.
    pub fn bytes(&self) -> &[u8] {
        &self.data.response[..self.len.min(MAX_RESPONSE_LEN)]
    }
}

/// A live connection of a transport, see [`ControlHub::connect`].
///
/// [`close`](Self::close) ends it in order with the requests already queued.
/// Dropping it without that (a cancelled task) ends it too, but the worker is
/// told only if the queue has room.
pub struct Connection<'h, 'a, const SLOTS: usize> {
    hub: &'h ControlHub<'a, SLOTS>,
    id: SessionId,
}

impl<'h, 'a, const SLOTS: usize> Connection<'h, 'a, SLOTS> {
    /// The identity of the connection.
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// See [`ControlHub::set_link`].
    pub fn set_link(&self, encrypted: bool, authenticated: bool) {
        self.hub.set_link(self.id, encrypted, authenticated);
    }

    /// See [`ControlHub::request`].
    ///
    /// # Errors
    ///
    /// As [`ControlHub::request`].
    pub fn request(&self) -> Result<Reservation<'h, 'a, SLOTS>, Busy> {
        self.hub.request(self.id)
    }

    /// Ends the connection.
    pub async fn close(self) {
        let hub = self.hub;
        let id = self.id;
        core::mem::forget(self);
        hub.close_session(id).await;
    }
}

impl<const SLOTS: usize> Drop for Connection<'_, '_, SLOTS> {
    fn drop(&mut self) {
        if self.hub.forget(self.id) {
            let _ = self.hub.queue.try_send(Work::Close(self.id.0));
        }
    }
}

impl<const SLOTS: usize> Default for ControlHub<'_, SLOTS> {
    fn default() -> Self {
        Self::new()
    }
}
