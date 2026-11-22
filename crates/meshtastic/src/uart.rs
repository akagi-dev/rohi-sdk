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
//! The serial (UART) link to a local Meshtastic radio.
//!
//! The radio speaks the Meshtastic *stream API* on its serial port: every
//! protobuf message is preceded by `0x94 0xC3` and a 16-bit big-endian length
//! (at most [`MAX_FRAME`] bytes). A session starts with `ToRadio.want_config_id`,
//! the radio answers with its configuration dump (`MyNodeInfo`, one `NodeInfo`
//! per known node, ...) and ends it with `FromRadio.config_complete_id` carrying the same
//! number; only then are packets accepted. After that the session keeps the serial
//! mode alive with a `heartbeat`, reports routing results and watches for a
//! restart of the radio.
//!
//! [`UartSession::run`] is the task body. It owns the two halves of the UART, which
//! the **firmware** chose and configured (port, pins, baud rate), and writes
//! everything it learns to a [`MeshtasticLink`]. Any failure (I/O error, closed
//! line, handshake timeout, radio restart) ends the session: the link is marked
//! down, what was queued is dropped and a new session starts after a
//! growing pause, so the rest of the device never waits for the radio.
//!
//! `Io::read` must be cancel-safe (bytes that were not returned stay in the
//! UART), which holds for the interrupt/DMA-driven UARTs of the ESP HALs; the
//! hardware FIFO must also hold what the radio sends while a frame of the
//! device is being written.

use core::future::pending;

use embassy_futures::select::{Either4, select, select4};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use embedded_io_async::{Read, Write};
use rohi_time::{Backoff, Clock};

use crate::link::{LastError, MeshtasticLink};
use crate::proto::{FromRadio, decode_from_radio, encode_heartbeat, encode_want_config};

/// The two bytes that start a serial frame.
pub const MAGIC: [u8; 2] = [0x94, 0xc3];
/// Largest message in either direction, as the radio defines it.
pub const MAX_FRAME: usize = 512;
/// How long the radio gets to finish its configuration dump, in milliseconds.
pub const HANDSHAKE_TIMEOUT_MS: u64 = 30_000;
/// How often an idle session talks to the radio so it keeps the serial mode, in milliseconds.
pub const HEARTBEAT_MS: u64 = 60_000;

// `want_config_id` values the radio reserves for partial dumps.
const RESERVED_NONCES: [u32; 2] = [69_420, 69_421];

/// What a byte did to the parser.
#[derive(Debug, PartialEq, Eq)]
pub enum Push<'a> {
    /// Nothing complete yet.
    Pending,
    /// A frame is complete: its payload.
    Frame(&'a [u8]),
    /// A frame header with an impossible length: a framing error.
    BadHeader,
}

enum State {
    Magic0,
    Magic1,
    LengthHigh,
    LengthLow(u8),
    Body,
}

/// Finds frames in the byte stream of the radio.
///
/// Bytes outside a frame (boot logs, noise) are skipped; a bad length restarts the
/// search at the next byte, so a truncated or corrupted frame costs only itself.
pub struct FrameParser {
    state: State,
    buffer: [u8; MAX_FRAME],
    len: usize,
    expected: usize,
    skipped: u32,
}

impl Default for FrameParser {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameParser {
    /// A parser at the start of the stream.
    pub const fn new() -> Self {
        Self {
            state: State::Magic0,
            buffer: [0; MAX_FRAME],
            len: 0,
            expected: 0,
            skipped: 0,
        }
    }

    /// Forgets a partial frame and the count of skipped bytes.
    pub fn reset(&mut self) {
        self.state = State::Magic0;
        self.skipped = 0;
    }

    /// Bytes seen outside any frame since the last [`reset`](Self::reset): a radio that is not in
    /// the serial PhoneAPI mode prints text there.
    pub const fn skipped(&self) -> u32 {
        self.skipped
    }

    /// Feeds one byte.
    pub fn push(&mut self, byte: u8) -> Push<'_> {
        match self.state {
            State::Magic0 => {
                if byte == MAGIC[0] {
                    self.state = State::Magic1;
                } else {
                    self.skipped = self.skipped.saturating_add(1);
                }
            }
            State::Magic1 => {
                self.state = match byte {
                    b if b == MAGIC[1] => State::LengthHigh,
                    b if b == MAGIC[0] => State::Magic1,
                    _ => State::Magic0,
                };
            }
            State::LengthHigh => self.state = State::LengthLow(byte),
            State::LengthLow(high) => {
                let len = usize::from(u16::from_be_bytes([high, byte]));
                if len == 0 || len > MAX_FRAME {
                    // Not a frame header after all: look again from the next byte.
                    self.state = if byte == MAGIC[0] {
                        State::Magic1
                    } else {
                        State::Magic0
                    };
                    return Push::BadHeader;
                } else {
                    self.len = 0;
                    self.expected = len;
                    self.state = State::Body;
                }
            }
            State::Body => {
                self.buffer[self.len] = byte;
                self.len += 1;
                if self.len == self.expected {
                    self.state = State::Magic0;
                    return Push::Frame(&self.buffer[..self.len]);
                }
            }
        }
        Push::Pending
    }
}

/// The 4-byte header of a frame of `len` payload bytes.
pub const fn header(len: usize) -> [u8; 4] {
    let len = (len as u16).to_be_bytes();
    [MAGIC[0], MAGIC[1], len[0], len[1]]
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// The UART reported an error.
    Io,
    /// The UART was closed.
    Closed,
    /// The radio did not finish its configuration dump in time.
    HandshakeTimeout,
    /// The radio restarted.
    Rebooted,
    /// A client asked for a new session, or the baud rate changed.
    Requested,
    /// The UART could not be set to the configured baud rate.
    Baud,
}

impl SessionError {
    const fn classified(self) -> LastError {
        match self {
            Self::Io => LastError::UartIo,
            Self::Closed => LastError::UartClosed,
            Self::HandshakeTimeout => LastError::HandshakeTimeout,
            Self::Rebooted => LastError::RadioRebooted,
            Self::Requested => LastError::Requested,
            Self::Baud => LastError::Baud,
        }
    }
}

/// The UART cannot run at the requested baud rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaudError;

/// Applies the baud rate to the UART the session reads from. The firmware implements it over
/// its HAL (`apply_config` of the UART); a platform that cannot change the rate on a live UART
/// fails here and the service reports it.
pub trait SetBaudRate {
    /// Sets the baud rate; 8N1 and no flow control stay as the board integration made them.
    ///
    /// # Errors
    ///
    /// [`BaudError`] when the rate cannot be applied.
    fn set_baud_rate(&mut self, baud: u32) -> Result<(), BaudError>;
}

/// One serial link to a radio: the session loop over the halves of a UART.
pub struct UartSession<'a, M: RawMutex, Io, D, C> {
    link: &'a MeshtasticLink<M>,
    io: Io,
    delay: D,
    clock: C,
    parser: FrameParser,
    nonce: u32,
}

impl<'a, M, Io, D, C> UartSession<'a, M, Io, D, C>
where
    M: RawMutex,
    Io: Read + Write + SetBaudRate,
    D: DelayNs,
    C: Clock,
{
    /// A session over the UART `io` reporting to `link`. `nonce_seed` starts the
    /// handshake numbers (any value; the firmware may pass entropy).
    pub const fn new(
        link: &'a MeshtasticLink<M>,
        io: Io,
        delay: D,
        clock: C,
        nonce_seed: u32,
    ) -> Self {
        Self {
            link,
            io,
            delay,
            clock,
            parser: FrameParser::new(),
            nonce: nonce_seed,
        }
    }

    fn fresh_nonce(&mut self) -> u32 {
        loop {
            self.nonce = self.nonce.wrapping_add(1);
            if self.nonce != 0 && !RESERVED_NONCES.contains(&self.nonce) {
                return self.nonce;
            }
        }
    }

    async fn write(
        link: &MeshtasticLink<M>,
        io: &mut Io,
        message: &[u8],
    ) -> Result<(), SessionError> {
        io.write_all(&header(message.len()))
            .await
            .map_err(|_| SessionError::Io)?;
        io.write_all(message).await.map_err(|_| SessionError::Io)?;
        io.flush().await.map_err(|_| SessionError::Io)?;
        link.note_tx();
        Ok(())
    }

    /// Runs sessions forever: connect, serve, and after a failure pause with a growing delay.
    pub async fn run(&mut self) -> ! {
        let mut backoff = Backoff::new(2_000, 30_000);
        loop {
            let up_before = self.link.snapshot().sessions;
            let (error, talked) = self.session().await;
            if self.link.snapshot().sessions > up_before {
                // It worked for a while: the next failure starts the pauses over.
                backoff.reset();
            }
            let handshake = (error == SessionError::HandshakeTimeout).then_some(talked);
            self.link.session_down(Some(error.classified()), handshake);
            log::warn!(target: "meshtastic.uart", "Session ended (err={:?})", error);
            // A requested restart does not wait.
            if error != SessionError::Requested {
                let pause = self.delay.delay_ms(backoff.next_delay_ms());
                select(pause, self.link.reconnect_requested()).await;
            }
        }
    }

    /// One session: handshake, then serving until something fails. Returns why it ended and
    /// whether the radio sent anything at all (a radio that sends bytes but never a frame is
    /// not in the serial PhoneAPI mode).
    pub async fn session(&mut self) -> (SessionError, bool) {
        let id = self.fresh_nonce();
        let Self {
            link,
            io,
            delay,
            clock,
            parser,
            ..
        } = self;
        link.session_begin();
        parser.reset();
        let mut seen_bytes = false;
        let result = Self::serve(link, io, delay, clock, parser, id, &mut seen_bytes).await;
        // `serve` only ever ends with an error.
        let error = result.err().unwrap_or(SessionError::Io);
        (error, seen_bytes && parser.skipped() > 0)
    }

    #[allow(clippy::too_many_arguments)]
    async fn serve(
        link: &MeshtasticLink<M>,
        io: &mut Io,
        delay: &mut D,
        clock: &C,
        parser: &mut FrameParser,
        id: u32,
        seen_bytes: &mut bool,
    ) -> Result<(), SessionError> {
        io.set_baud_rate(link.baud())
            .map_err(|_| SessionError::Baud)?;
        // A request made before this session started is answered by starting it.
        link.reconnect_pending_clear();
        let mut message = [0u8; 16];
        let len = encode_want_config(&mut message, id).map_err(|_| SessionError::Io)?;
        Self::write(link, io, &message[..len]).await?;

        let started = clock.now().millis;
        let mut last_tx = started;
        let mut connected = false;
        let mut chunk = [0u8; 64];
        loop {
            let now = clock.now().millis;
            let wait_ms = if connected {
                HEARTBEAT_MS.saturating_sub(now.saturating_sub(last_tx))
            } else {
                HANDSHAKE_TIMEOUT_MS.saturating_sub(now.saturating_sub(started))
            };
            let outgoing = async {
                if connected {
                    link.next_outgoing().await
                } else {
                    pending().await
                }
            };
            let timer = delay.delay_ms(u32::try_from(wait_ms).unwrap_or(u32::MAX));
            let event = select4(
                io.read(&mut chunk),
                outgoing,
                timer,
                link.reconnect_requested(),
            )
            .await;
            match event {
                Either4::First(Ok(0)) => return Err(SessionError::Closed),
                Either4::First(Err(_)) => return Err(SessionError::Io),
                Either4::First(Ok(read)) => {
                    *seen_bytes = true;
                    for byte in &chunk[..read] {
                        match parser.push(*byte) {
                            Push::Pending => {}
                            Push::BadHeader => link.note_framing_error(),
                            Push::Frame(frame) => {
                                let now = clock.now().millis;
                                match decode_from_radio(frame) {
                                    Ok(message) => {
                                        link.note_rx(now);
                                        match message {
                                            FromRadio::MyInfo(node) => link.set_node(node),
                                            FromRadio::NodeInfo(node) => link.add_peer(node),
                                            FromRadio::Metadata { firmware, hardware } => {
                                                link.set_metadata(
                                                    core::str::from_utf8(firmware).unwrap_or(""),
                                                    hardware,
                                                );
                                            }
                                            FromRadio::ConfigComplete(done)
                                                if done == id && !connected =>
                                            {
                                                connected = true;
                                                last_tx = now;
                                                link.session_up(now);
                                            }
                                            FromRadio::Routing { from, result } if connected => {
                                                link.on_routing(from, result);
                                            }
                                            FromRadio::Text { from, id, text } if connected => {
                                                link.on_text(from, id, text);
                                            }
                                            FromRadio::Rebooted => {
                                                return Err(SessionError::Rebooted);
                                            }
                                            _ => {}
                                        }
                                    }
                                    Err(_) => link.note_decode_error(),
                                }
                            }
                        }
                    }
                }
                Either4::Second(message) => {
                    Self::write(link, io, &message).await?;
                    last_tx = clock.now().millis;
                }
                Either4::Third(()) if connected => {
                    let len = encode_heartbeat(&mut message).map_err(|_| SessionError::Io)?;
                    Self::write(link, io, &message[..len]).await?;
                    last_tx = clock.now().millis;
                }
                Either4::Third(()) => return Err(SessionError::HandshakeTimeout),
                Either4::Fourth(()) => return Err(SessionError::Requested),
            }
        }
    }
}

#[cfg(test)]
mod tests;
