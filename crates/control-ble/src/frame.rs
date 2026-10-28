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
//! Request/response framing over BLE chunks.
//!
//! A control message (an opaque byte string, protobuf in practice) is split
//! into frames that each fit one ATT payload:
//!
//! ```text
//! +---------+--------+-------+-------+-----------+---------...
//! | version | msg id | index | count | total len | payload chunk
//! |   u8    |   u8   |  u8   |  u8   |  u16 LE   |
//! +---------+--------+-------+-------+-----------+---------...
//! ```
//!
//! - `msg id` is chosen by the requester and echoed on every response frame;
//! - `index` runs from 0 to `count - 1`, frames arrive strictly in order;
//! - `count` and `total len` are repeated in every frame and must not change.
//!
//! The framing knows nothing about the payload and never allocates: the
//! [`Reassembler`] works in a fixed buffer, the [`Fragmenter`] borrows the
//! message.

use core::fmt;

/// Frame format version.
pub const FRAME_VERSION: u8 = 1;

/// Size of the frame header in bytes.
pub const HEADER_LEN: usize = 6;

/// Most fragments a message may have (the `count` field is a byte).
pub const MAX_FRAGMENTS: usize = 255;

/// Malformed or unacceptable frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The frame is shorter than the header or has no payload.
    Truncated,
    /// Unknown frame version.
    Version,
    /// `count` is zero or `index` is not below `count`.
    InvalidIndex,
    /// The message is larger than the receive buffer, or needs more than
    /// [`MAX_FRAGMENTS`] fragments.
    TooLarge,
    /// The fragment is not the next one: duplicate, skipped or reordered.
    OutOfOrder,
    /// `count`, `total len` or `msg id` changed in the middle of a message.
    Inconsistent,
    /// The payload bytes of the fragments do not add up to `total len`.
    LengthMismatch,
    /// The transport payload is too small to carry any data.
    PayloadTooSmall,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "truncated frame",
            Self::Version => "unsupported frame version",
            Self::InvalidIndex => "invalid fragment index",
            Self::TooLarge => "message too large",
            Self::OutOfOrder => "fragment out of order",
            Self::Inconsistent => "inconsistent fragment header",
            Self::LengthMismatch => "fragments do not match the message length",
            Self::PayloadTooSmall => "BLE payload too small for a frame",
        })
    }
}

/// A completely reassembled message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message<'a> {
    /// Message id of the requester.
    pub id: u8,
    /// Exact bytes the sender passed to the [`Fragmenter`].
    pub bytes: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Waiting for the first fragment of a message.
    Idle,
    /// Collecting fragments.
    Receiving,
    /// A frame was rejected; everything up to the next first fragment is
    /// skipped silently so one error is reported once.
    Discarding,
}

/// Reassembles frames into one message of at most `N` bytes.
///
/// One message at a time: a first fragment (`index == 0`) always starts a new
/// message and drops a partial one. After a rejected frame the remaining
/// fragments of that message are skipped.
pub struct Reassembler<const N: usize> {
    buf: [u8; N],
    state: State,
    /// Id of the frame seen last, for error replies.
    id: u8,
    /// Id of the message being collected.
    message_id: u8,
    count: u8,
    total: u16,
    next: u8,
    filled: usize,
}

impl<const N: usize> Default for Reassembler<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Reassembler<N> {
    /// Largest message this reassembler accepts, bytes.
    pub const CAPACITY: usize = N;

    /// An empty reassembler.
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            state: State::Idle,
            id: 0,
            message_id: 0,
            count: 0,
            total: 0,
            next: 0,
            filled: 0,
        }
    }

    /// Drops any partial message. Call on every disconnect.
    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.next = 0;
        self.filled = 0;
        self.count = 0;
        self.total = 0;
    }

    /// Whether a message is partially received.
    pub fn is_receiving(&self) -> bool {
        self.state == State::Receiving
    }

    /// Message id of the most recent frame, for error replies.
    pub fn last_id(&self) -> u8 {
        self.id
    }

    /// Feeds one frame. Returns the message when this frame completed it.
    ///
    /// On error the partial message is dropped and the following fragments of
    /// the same message are skipped until a new first fragment arrives.
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<Message<'_>>, FrameError> {
        match self.accept(frame) {
            Ok(true) => {
                self.state = State::Idle;
                Ok(Some(Message {
                    id: self.message_id,
                    bytes: &self.buf[..self.filled],
                }))
            }
            Ok(false) => Ok(None),
            Err(error) => {
                let silent = self.state == State::Discarding && is_continuation(frame);
                self.state = State::Discarding;
                self.next = 0;
                self.filled = 0;
                if silent { Ok(None) } else { Err(error) }
            }
        }
    }

    /// `Ok(true)` when the message is complete.
    fn accept(&mut self, frame: &[u8]) -> Result<bool, FrameError> {
        if frame.len() < HEADER_LEN {
            return Err(FrameError::Truncated);
        }
        if frame[0] != FRAME_VERSION {
            return Err(FrameError::Version);
        }
        let (id, index, count) = (frame[1], frame[2], frame[3]);
        let total = u16::from_le_bytes([frame[4], frame[5]]);
        let payload = &frame[HEADER_LEN..];
        self.id = id;

        if count == 0 || index >= count {
            return Err(FrameError::InvalidIndex);
        }
        if payload.is_empty() {
            return Err(FrameError::Truncated);
        }

        if index == 0 {
            // A new message always wins over a partial or discarded one.
            self.state = State::Receiving;
            self.message_id = id;
            self.count = count;
            self.total = total;
            self.next = 0;
            self.filled = 0;
            if usize::from(total) > N {
                return Err(FrameError::TooLarge);
            }
            if total == 0 {
                return Err(FrameError::LengthMismatch);
            }
        } else {
            if self.state != State::Receiving {
                return Err(FrameError::OutOfOrder);
            }
            if count != self.count || total != self.total || id != self.message_id {
                return Err(FrameError::Inconsistent);
            }
            if index != self.next {
                return Err(FrameError::OutOfOrder);
            }
        }

        let end = self.filled + payload.len();
        if end > usize::from(self.total) {
            return Err(FrameError::LengthMismatch);
        }
        self.buf[self.filled..end].copy_from_slice(payload);
        self.filled = end;
        self.next = index + 1;

        if self.next == self.count {
            if self.filled != usize::from(self.total) {
                return Err(FrameError::LengthMismatch);
            }
            return Ok(true);
        }
        Ok(false)
    }
}

fn is_continuation(frame: &[u8]) -> bool {
    frame.len() >= HEADER_LEN && frame[0] == FRAME_VERSION && frame[2] != 0
}

/// Splits a message into frames of at most `max_payload` bytes each
/// (header included), see [`Fragmenter::next_frame`].
pub struct Fragmenter<'a> {
    id: u8,
    message: &'a [u8],
    chunk: usize,
    count: u8,
    next: u8,
}

impl<'a> Fragmenter<'a> {
    /// Frames `message` for a link carrying `max_payload` bytes per chunk.
    pub fn new(id: u8, message: &'a [u8], max_payload: usize) -> Result<Self, FrameError> {
        let chunk = max_payload
            .checked_sub(HEADER_LEN)
            .filter(|chunk| *chunk > 0)
            .ok_or(FrameError::PayloadTooSmall)?;
        if message.is_empty() || message.len() > usize::from(u16::MAX) {
            return Err(FrameError::TooLarge);
        }
        let count = message.len().div_ceil(chunk);
        if count > MAX_FRAGMENTS {
            return Err(FrameError::TooLarge);
        }
        Ok(Self {
            id,
            message,
            chunk,
            count: count as u8,
            next: 0,
        })
    }

    /// Number of frames of the message.
    pub fn count(&self) -> usize {
        usize::from(self.count)
    }

    /// Writes the next frame into `out` and returns its length, `None` when
    /// all frames were produced. `out` must hold `max_payload` bytes.
    pub fn next_frame(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.next == self.count {
            return None;
        }
        let start = usize::from(self.next) * self.chunk;
        let end = (start + self.chunk).min(self.message.len());
        let payload = &self.message[start..end];
        let total = (self.message.len() as u16).to_le_bytes();
        out[..HEADER_LEN].copy_from_slice(&[
            FRAME_VERSION,
            self.id,
            self.next,
            self.count,
            total[0],
            total[1],
        ]);
        out[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);
        self.next += 1;
        Some(HEADER_LEN + payload.len())
    }
}
