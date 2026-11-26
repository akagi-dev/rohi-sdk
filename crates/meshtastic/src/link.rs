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
//! The state a Meshtastic radio link shares with the transport and the service.
//!
//! A link backend (the UART session in [`crate::uart`], or a BLE central) owns
//! the I/O and is the **only** reader of the radio: it parses every `FromRadio` message once
//! and fans it out here. Routing results of telemetry packets go to a bounded queue the
//! transport waits on ([`LinkRadio`]); diagnostic traffic (text messages and the
//! acknowledgements of diagnostic sends) goes to a small ring the `inbox` action reads without
//! consuming. The two never compete: the queue of the transport does not depend on anybody
//! reading the inbox, and reading the inbox takes nothing from the transport.
//!
//! One `static` of [`MeshtasticLink`] per radio, built with a `const` constructor and held in
//! `.bss`. Everything is bounded: the queues drop what does not fit (a packet whose result is
//! dropped times out and is retried by the router), counters saturate at `u64::MAX`
//! (practically never; they restart with the device).

use core::cell::RefCell;
use core::task::Poll;

use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::signal::Signal;
use embassy_sync::waitqueue::MultiWakerRegistration;
use heapless::{Deque, String, Vec};

use crate::proto::encode_packet;
use crate::radio::{MeshtasticRadio, Outgoing, RadioError, RoutingResult};

/// Largest encoded `ToRadio` message the link carries.
pub const TO_RADIO_MAX: usize = 260;
/// Routing results kept for the transport: one per fragment of the largest packet.
pub const RESULTS: usize = 16;
/// Entries of the diagnostic inbox.
pub const INBOX: usize = 6;
/// Longest text of an inbox entry, in bytes.
pub const INBOX_TEXT_MAX: usize = 64;
/// Longest firmware version text kept.
pub const FIRMWARE_MAX: usize = 24;
/// Consecutive bad frames after which the stream counts as in trouble.
pub const PROTOCOL_ERROR_STREAK: u32 = 3;
/// Consecutive failed handshakes after which the radio counts as unresponsive.
pub const UNRESPONSIVE_AFTER: u32 = 3;

/// One encoded `ToRadio` message.
pub type ToRadio = Vec<u8, TO_RADIO_MAX>;

/// The most recent classified error. Bounded and stable: the numbers are part of the status schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LastError {
    /// Nothing went wrong since start.
    #[default]
    None = 0,
    /// The UART reported an error.
    UartIo = 1,
    /// The UART was closed.
    UartClosed = 2,
    /// The radio did not finish its configuration dump in time.
    HandshakeTimeout = 3,
    /// The radio restarted.
    RadioRebooted = 4,
    /// The serial stream had an invalid frame header.
    Framing = 5,
    /// A frame did not decode as `FromRadio`.
    Decode = 6,
    /// A client asked for a new session.
    Requested = 7,
    /// The baud rate could not be applied.
    Baud = 8,
}

impl LastError {
    /// The value in the status schema.
    pub const fn code(self) -> u32 {
        self as u32
    }

    /// The stable name.
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::UartIo => "uart_io",
            Self::UartClosed => "uart_closed",
            Self::HandshakeTimeout => "handshake_failed",
            Self::RadioRebooted => "radio_rebooted",
            Self::Framing => "framing",
            Self::Decode => "decode",
            Self::Requested => "requested",
            Self::Baud => "baud",
        }
    }

    /// Every variant, in schema order.
    pub const ALL: [Self; 9] = [
        Self::None,
        Self::UartIo,
        Self::UartClosed,
        Self::HandshakeTimeout,
        Self::RadioRebooted,
        Self::Framing,
        Self::Decode,
        Self::Requested,
        Self::Baud,
    ];
}

/// What an inbox entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxKind {
    /// A received text message.
    Text = 0,
    /// The routing outcome of a diagnostic send.
    Ack = 1,
}

/// One entry of the diagnostic inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxEntry {
    /// Text or acknowledgement.
    pub kind: InboxKind,
    /// The source node when known.
    pub from: Option<u32>,
    /// The packet ID; for an acknowledgement the packet it acknowledges.
    pub packet_id: u32,
    /// The text, or `ok` / `error N` for an acknowledgement.
    pub text: String<INBOX_TEXT_MAX>,
}

/// What the link knows about the radio, as plain numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkSnapshot {
    /// The handshake is complete: packets can be sent.
    pub connected: bool,
    /// The node number of the local radio, once it said.
    pub node: Option<u32>,
    /// Nodes in the radio's database other than itself.
    pub peers: u32,
    /// Telemetry packets acknowledged by a successful routing result.
    pub acked: u32,
    /// Telemetry packets that got a failed routing result.
    pub failed: u32,
    /// Sessions established since start.
    pub sessions: u32,
}

/// The full diagnostic view of the link, for the status of the service.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diagnostics {
    /// Plain numbers.
    pub link: LinkSnapshot,
    /// The firmware version of the radio, empty until known.
    pub firmware: String<FIRMWARE_MAX>,
    /// The `HardwareModel` number of the radio.
    pub hardware: Option<u32>,
    /// `ToRadio` messages written to the UART.
    pub tx_frames: u64,
    /// Valid `FromRadio` messages received.
    pub rx_frames: u64,
    /// Invalid frame headers.
    pub framing_errors: u64,
    /// Frames that did not decode.
    pub decode_errors: u64,
    /// Session attempts after the first.
    pub reconnects: u64,
    /// The most recent classified error.
    pub last_error: LastError,
    /// When the last valid message arrived (monotonic ms).
    pub last_rx_ms: Option<u64>,
    /// When the current session was established (monotonic ms).
    pub session_ms: Option<u64>,
    /// The UART could not be set up.
    pub uart_failed: bool,
    /// Handshakes that failed in a row.
    pub handshake_failures: u32,
    /// The radio sent bytes but never a valid message.
    pub not_configured: bool,
    /// Bad frames in a row.
    pub bad_streak: u32,
    /// A session is being set up (not the first attempt and not connected).
    pub recovering: bool,
}

// The number of sessions that ended, and everybody who waits to hear of the next one.
struct Epoch {
    count: u32,
    wakers: MultiWakerRegistration<4>,
}

impl Epoch {
    const fn new() -> Self {
        Self {
            count: 0,
            wakers: MultiWakerRegistration::new(),
        }
    }
}

struct Inner {
    diagnostics: Diagnostics,
    inbox: Deque<InboxEntry, INBOX>,
    // Packets sent by the diagnostics: their acknowledgements belong to the inbox.
    pending: Vec<u32, INBOX>,
    next_id: u32,
    baud: u32,
}

impl Inner {
    const fn new() -> Self {
        Self {
            diagnostics: Diagnostics {
                link: LinkSnapshot {
                    connected: false,
                    node: None,
                    peers: 0,
                    acked: 0,
                    failed: 0,
                    sessions: 0,
                },
                firmware: String::new(),
                hardware: None,
                tx_frames: 0,
                rx_frames: 0,
                framing_errors: 0,
                decode_errors: 0,
                reconnects: 0,
                last_error: LastError::None,
                last_rx_ms: None,
                session_ms: None,
                uart_failed: false,
                handshake_failures: 0,
                not_configured: false,
                bad_streak: 0,
                recovering: false,
            },
            inbox: Deque::new(),
            pending: Vec::new(),
            next_id: 1,
            baud: 115_200,
        }
    }

    fn push_inbox(&mut self, entry: InboxEntry) {
        if self.inbox.is_full() {
            self.inbox.pop_back();
        }
        // Not full any more: the entry always fits.
        let _ = self.inbox.push_front(entry);
    }
}

/// State and queues shared between a link backend, the transport and the service.
pub struct MeshtasticLink<M: RawMutex> {
    state: BlockingMutex<M, RefCell<Inner>>,
    outgoing: Channel<M, ToRadio, 1>,
    results: Channel<M, RoutingResult, RESULTS>,
    // Counts the sessions that ended; every waiter on the radio watches it, not just one.
    epoch: BlockingMutex<M, RefCell<Epoch>>,
    reconnect: Signal<M, ()>,
}

impl<M: RawMutex> Default for MeshtasticLink<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: RawMutex> MeshtasticLink<M> {
    /// A link that is not connected.
    pub const fn new() -> Self {
        Self {
            state: BlockingMutex::new(RefCell::new(Inner::new())),
            outgoing: Channel::new(),
            results: Channel::new(),
            epoch: BlockingMutex::new(RefCell::new(Epoch::new())),
            reconnect: Signal::new(),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> T {
        self.state.lock(|inner| f(&mut inner.borrow_mut()))
    }

    // ---- what the transport and the service read --------------------------------------------

    /// A copy of the plain numbers.
    pub fn snapshot(&self) -> LinkSnapshot {
        self.with(|inner| inner.diagnostics.link)
    }

    /// A copy of everything the link knows.
    pub fn diagnostics(&self) -> Diagnostics {
        self.with(|inner| inner.diagnostics.clone())
    }

    /// The entries of the inbox, newest first. A copy: reading takes nothing out.
    pub fn inbox(&self) -> Vec<InboxEntry, INBOX> {
        self.with(|inner| inner.inbox.iter().cloned().collect())
    }

    // ---- ids and baud ------------------------------------------------------------------------

    /// Starts the packet IDs at `seed`, so a restarted device does not reuse the IDs of its
    /// previous run (the firmware passes some entropy).
    pub fn seed_ids(&self, seed: u32) {
        self.with(|inner| inner.next_id = seed.max(1));
    }

    /// Reserves `count` consecutive, nonzero packet IDs: the transport and the diagnostics draw
    /// from the same counter, so no two packets of the device share an ID.
    pub fn reserve_ids(&self, count: usize) -> u32 {
        self.with(|inner| {
            if inner.next_id > u32::MAX - count as u32 {
                inner.next_id = 1;
            }
            let first = inner.next_id;
            inner.next_id += count as u32;
            first
        })
    }

    /// The configured baud rate of the UART.
    pub fn baud(&self) -> u32 {
        self.with(|inner| inner.baud)
    }

    /// Sets the baud rate; a change asks the session to start over with it.
    pub fn set_baud(&self, baud: u32) {
        let changed = self.with(|inner| core::mem::replace(&mut inner.baud, baud) != baud);
        if changed {
            self.request_reconnect();
        }
    }

    // ---- what the service asks ---------------------------------------------------------------

    /// Asks the backend to end the session and start a new one.
    pub fn request_reconnect(&self) {
        self.reconnect.signal(());
    }

    /// Forgets a request that was made before the session that is starting now.
    pub fn reconnect_pending_clear(&self) {
        self.reconnect.reset();
    }

    /// Whether a new session was requested and not yet started.
    pub fn reconnect_pending(&self) -> bool {
        self.reconnect.signaled()
    }

    /// Whether a message waits for the backend to write it.
    pub fn has_outgoing(&self) -> bool {
        !self.outgoing.is_empty()
    }

    /// Completes when a new session was requested.
    pub async fn reconnect_requested(&self) {
        self.reconnect.wait().await;
    }

    /// Hands a packet to the backend, which writes it to the radio. Fails at once while
    /// the radio is not connected; waits for room otherwise, and stops waiting when the session ends.
    ///
    /// # Errors
    ///
    /// [`RadioError`]; nothing was queued.
    pub async fn submit(&self, packet: &Outgoing<'_>) -> Result<(), RadioError> {
        let seen = self.epoch();
        if !self.snapshot().connected {
            return Err(RadioError::NotConnected);
        }
        let mut message = ToRadio::new();
        message
            .resize(TO_RADIO_MAX, 0)
            .map_err(|_| RadioError::Io)?;
        let len = encode_packet(&mut message, packet).map_err(|_| RadioError::Io)?;
        message.truncate(len);
        match select(self.outgoing.send(message), self.session_ended(seen)).await {
            Either::First(()) => Ok(()),
            Either::Second(()) => Err(RadioError::Disconnected),
        }
    }

    /// The number of sessions that ended so far; take it before checking `connected`, then
    /// wait with [`session_ended`](Self::session_ended).
    fn epoch(&self) -> u32 {
        self.epoch.lock(|epoch| epoch.borrow().count)
    }

    fn state_epoch_bump(&self) {
        self.epoch.lock(|epoch| {
            let mut epoch = epoch.borrow_mut();
            epoch.count = epoch.count.wrapping_add(1);
            epoch.wakers.wake();
        });
    }

    /// Completes when a session ended after `seen` was taken. Any number of tasks may wait.
    async fn session_ended(&self, seen: u32) {
        core::future::poll_fn(|cx| {
            self.epoch.lock(|epoch| {
                let mut epoch = epoch.borrow_mut();
                if epoch.count != seen {
                    Poll::Ready(())
                } else {
                    epoch.wakers.register(cx.waker());
                    Poll::Pending
                }
            })
        })
        .await
    }

    /// Forgets a diagnostic send that was not submitted after all.
    pub fn cancel_diagnostic_send(&self, id: u32) {
        self.with(|inner| {
            if let Some(position) = inner.pending.iter().position(|pending| *pending == id) {
                inner.pending.remove(position);
            }
        });
    }

    /// Remembers that the acknowledgement of packet `id` belongs to the diagnostics.
    pub fn note_diagnostic_send(&self, id: u32) {
        self.with(|inner| {
            if inner.pending.is_full() {
                inner.pending.remove(0);
            }
            let _ = inner.pending.push(id);
        });
    }

    /// The next message for the radio; the backend writes it to its link.
    pub async fn next_outgoing(&self) -> ToRadio {
        self.outgoing.receive().await
    }

    // ---- what the backend reports ------------------------------------------------------------

    /// The UART could not be set up (or could, after all).
    pub fn set_uart_failed(&self, failed: bool) {
        self.with(|inner| {
            inner.diagnostics.uart_failed = failed;
            if failed {
                inner.diagnostics.last_error = LastError::UartIo;
            }
        });
    }

    /// A backend starts a session: nothing of the previous one is kept.
    pub fn session_begin(&self) {
        self.with(|inner| {
            let d = &mut inner.diagnostics;
            if d.link.sessions > 0 || d.handshake_failures > 0 || d.last_error != LastError::None {
                d.reconnects = d.reconnects.saturating_add(1);
                d.recovering = true;
            }
            d.not_configured = false;
        });
        self.clear_session();
    }

    /// The radio finished its handshake: packets can be sent.
    pub fn session_up(&self, now_ms: u64) {
        self.with(|inner| {
            let d = &mut inner.diagnostics;
            d.link.connected = true;
            d.link.sessions = d.link.sessions.saturating_add(1);
            d.session_ms = Some(now_ms);
            d.handshake_failures = 0;
            d.not_configured = false;
            d.recovering = false;
            d.uart_failed = false;
        });
    }

    /// The session ended: the radio is no longer usable and what was queued for it is dropped.
    /// A failed handshake counts towards "unresponsive"; `bytes_seen` tells a silent line from
    /// one that talks something else.
    pub fn session_down(&self, error: Option<LastError>, handshake: Option<bool>) {
        self.with(|inner| {
            let d = &mut inner.diagnostics;
            if let Some(error) = error {
                d.last_error = error;
            }
            match handshake {
                Some(true) => d.not_configured = true,
                Some(false) => d.handshake_failures = d.handshake_failures.saturating_add(1),
                None => {}
            }
            // A session that ended with a reason is being recovered, also the very first one.
            if error.is_some() || d.link.sessions > 0 {
                d.recovering = true;
            }
        });
        self.clear_session();
    }

    fn clear_session(&self) {
        self.with(|inner| {
            let d = &mut inner.diagnostics;
            d.link.connected = false;
            d.link.node = None;
            d.link.peers = 0;
            d.session_ms = None;
            d.bad_streak = 0;
            // The acknowledgements of the old session never come.
            inner.pending.clear();
        });
        while self.outgoing.try_receive().is_ok() {}
        while self.results.try_receive().is_ok() {}
        self.state_epoch_bump();
    }

    /// A message of the local radio about itself.
    pub fn set_node(&self, node: u32) {
        self.with(|inner| inner.diagnostics.link.node = Some(node));
    }

    /// The firmware version and hardware model of the radio.
    pub fn set_metadata(&self, firmware: &str, hardware: Option<u32>) {
        self.with(|inner| {
            inner.diagnostics.firmware.clear();
            let end = firmware.len().min(FIRMWARE_MAX);
            // Only whole characters: a firmware version is ASCII in practice.
            let end = (0..=end)
                .rev()
                .find(|n| firmware.is_char_boundary(*n))
                .unwrap_or(0);
            let _ = inner.diagnostics.firmware.push_str(&firmware[..end]);
            inner.diagnostics.hardware = hardware;
        });
    }

    /// One more entry in the radio's node database; the radio itself is not a peer.
    pub fn add_peer(&self, node: u32) {
        self.with(|inner| {
            if inner.diagnostics.link.node != Some(node) {
                inner.diagnostics.link.peers = inner.diagnostics.link.peers.saturating_add(1);
            }
        });
    }

    /// A valid `FromRadio` message arrived.
    pub fn note_rx(&self, now_ms: u64) {
        self.with(|inner| {
            let d = &mut inner.diagnostics;
            d.rx_frames = d.rx_frames.saturating_add(1);
            d.last_rx_ms = Some(now_ms);
            d.bad_streak = 0;
        });
    }

    /// A `ToRadio` message was written to the UART.
    pub fn note_tx(&self) {
        self.with(|inner| {
            inner.diagnostics.tx_frames = inner.diagnostics.tx_frames.saturating_add(1)
        });
    }

    /// An invalid frame header.
    pub fn note_framing_error(&self) {
        self.with(|inner| {
            let d = &mut inner.diagnostics;
            d.framing_errors = d.framing_errors.saturating_add(1);
            d.bad_streak = d.bad_streak.saturating_add(1);
            d.last_error = LastError::Framing;
        });
    }

    /// A frame that did not decode.
    pub fn note_decode_error(&self) {
        self.with(|inner| {
            let d = &mut inner.diagnostics;
            d.decode_errors = d.decode_errors.saturating_add(1);
            d.bad_streak = d.bad_streak.saturating_add(1);
            d.last_error = LastError::Decode;
        });
    }

    /// A routing result. The acknowledgement of a diagnostic send goes to the inbox, every other
    /// result to the transport; neither can starve the other.
    pub fn on_routing(&self, from: u32, result: RoutingResult) {
        let diagnostic = self.with(|inner| {
            let Some(position) = inner.pending.iter().position(|id| *id == result.request_id)
            else {
                return false;
            };
            inner.pending.remove(position);
            let mut text = String::new();
            if result.error == 0 {
                let _ = text.push_str("ok");
            } else {
                let _ = core::fmt::write(&mut text, format_args!("error {}", result.error));
            }
            inner.push_inbox(InboxEntry {
                kind: InboxKind::Ack,
                from: Some(from),
                packet_id: result.request_id,
                text,
            });
            true
        });
        if diagnostic {
            return;
        }
        self.with(|inner| {
            let link = &mut inner.diagnostics.link;
            if result.error == 0 {
                link.acked = link.acked.saturating_add(1);
            } else {
                link.failed = link.failed.saturating_add(1);
            }
        });
        if self.results.try_send(result).is_err() {
            log::debug!(target: "meshtastic", "Routing result dropped, queue full");
        }
    }

    /// A text message. One that is not UTF-8 or longer than [`INBOX_TEXT_MAX`] is not stored
    /// (never cut), and nothing else is affected.
    pub fn on_text(&self, from: u32, id: u32, text: &[u8]) {
        let Ok(text) = core::str::from_utf8(text) else {
            return;
        };
        let Ok(text) = String::try_from(text) else {
            return;
        };
        self.with(|inner| {
            inner.push_inbox(InboxEntry {
                kind: InboxKind::Text,
                from: Some(from),
                packet_id: id,
                text,
            });
        });
    }
}

/// The transport's view of a link: a [`MeshtasticRadio`] over the shared queues.
pub struct LinkRadio<'a, M: RawMutex> {
    link: &'a MeshtasticLink<M>,
}

impl<'a, M: RawMutex> LinkRadio<'a, M> {
    /// The radio of `link`.
    pub const fn new(link: &'a MeshtasticLink<M>) -> Self {
        Self { link }
    }
}

impl<M: RawMutex> MeshtasticRadio for LinkRadio<'_, M> {
    fn reserve_ids(&mut self, count: usize) -> u32 {
        self.link.reserve_ids(count)
    }

    async fn send(&mut self, packet: &Outgoing<'_>) -> Result<(), RadioError> {
        self.link.submit(packet).await
    }

    async fn next_result(&mut self) -> Result<RoutingResult, RadioError> {
        let seen = self.link.epoch();
        if !self.link.snapshot().connected {
            return Err(RadioError::Disconnected);
        }
        match select(self.link.results.receive(), self.link.session_ended(seen)).await {
            Either::First(result) => Ok(result),
            Either::Second(()) => Err(RadioError::Disconnected),
        }
    }
}

#[cfg(test)]
mod tests;
