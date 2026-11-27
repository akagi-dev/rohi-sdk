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
//! Meshtastic Transport v1 framing of an opaque payload.
//!
//! The sealed `SignedEnvelope` bytes are never interpreted: they are sliced
//! into transport frames of at most [`MTU`] bytes and reproduced byte for byte
//! by the receiver (`crates/telemetry-robonomics/proto/transport/meshtastic/v1.md`).
//!
//! | Frame | Layout | Body |
//! |-------|--------|------|
//! | `SINGLE` | `0x01`, body | 1..=218 bytes |
//! | `FRAGMENT` | `0x41`, 6-byte message ID, descriptor, body | 1..=211 bytes, exactly 211 unless final |
//!
//! The message ID is `SHA256(payload)[0..6]` and the descriptor packs
//! `(fragment_count - 1) << 4 | fragment_index`. At most [`MAX_FRAGMENTS`]
//! fragments, [`MAX_PAYLOAD`] bytes in total, are allowed; anything larger, or
//! empty, is refused **before** any frame is built, so a partial sequence is
//! never sent.
//!
//! ```
//! use rohi_connectivity_meshtastic::frame::{Framer, MTU};
//!
//! let payload = [7u8; 300];
//! let framer = Framer::new(&payload).unwrap();
//! assert_eq!(framer.count(), 2);
//! let mut frame = [0u8; MTU];
//! let len = framer.frame(0, &mut frame);
//! assert_eq!((frame[0], len), (0x41, 8 + 211));
//! assert_eq!(frame[7], 0x10); // two fragments, index 0
//! ```

use sha2::{Digest, Sha256};

/// Most bytes of one Meshtastic packet payload this transport uses.
pub const MTU: usize = 219;
/// `SINGLE` control byte: version 1, type `00`.
pub const CONTROL_SINGLE: u8 = 0x01;
/// `FRAGMENT` control byte: version 1, type `01`.
pub const CONTROL_FRAGMENT: u8 = 0x41;
/// Largest payload of a `SINGLE` frame.
pub const SINGLE_MAX: usize = MTU - 1;
/// Bytes before the body of a `FRAGMENT`: control, message ID, descriptor.
pub const FRAGMENT_HEADER: usize = 8;
/// Largest body of a `FRAGMENT`.
pub const FRAGMENT_BODY_MAX: usize = MTU - FRAGMENT_HEADER;
/// Most fragments of one payload.
pub const MAX_FRAGMENTS: usize = 16;
/// Largest payload this framing can carry.
pub const MAX_PAYLOAD: usize = MAX_FRAGMENTS * FRAGMENT_BODY_MAX;

/// Why a payload cannot be framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// A zero-length payload has no frame.
    Empty,
    /// More than [`MAX_PAYLOAD`] bytes.
    TooLarge,
}

/// The frames of one payload, built on demand into a caller's buffer.
#[derive(Debug, Clone, Copy)]
pub struct Framer<'a> {
    payload: &'a [u8],
    message_id: [u8; 6],
    count: usize,
}

impl<'a> Framer<'a> {
    /// Checks the size of `payload` and, when it needs fragments, computes its
    /// message ID.
    ///
    /// # Errors
    ///
    /// [`FrameError`] for an empty or an oversized payload; nothing is built.
    pub fn new(payload: &'a [u8]) -> Result<Self, FrameError> {
        if payload.is_empty() {
            return Err(FrameError::Empty);
        }
        if payload.len() > MAX_PAYLOAD {
            return Err(FrameError::TooLarge);
        }
        let mut message_id = [0u8; 6];
        let count = if payload.len() <= SINGLE_MAX {
            1
        } else {
            message_id.copy_from_slice(&Sha256::digest(payload)[..6]);
            payload.len().div_ceil(FRAGMENT_BODY_MAX)
        };
        Ok(Self {
            payload,
            message_id,
            count,
        })
    }

    /// The number of frames: 1 for a `SINGLE`, 2 to 16 fragments otherwise.
    pub const fn count(&self) -> usize {
        self.count
    }

    /// The message ID shared by the fragments; zero for a `SINGLE`.
    pub const fn message_id(&self) -> [u8; 6] {
        self.message_id
    }

    /// Writes frame `index` (`0..count()`) into `out` and returns its length.
    ///
    /// # Panics
    ///
    /// Never: an index outside the frames yields an empty frame (length 0).
    pub fn frame(&self, index: usize, out: &mut [u8; MTU]) -> usize {
        if index >= self.count {
            return 0;
        }
        if self.count == 1 {
            out[0] = CONTROL_SINGLE;
            out[1..=self.payload.len()].copy_from_slice(self.payload);
            return 1 + self.payload.len();
        }
        let start = index * FRAGMENT_BODY_MAX;
        let end = (start + FRAGMENT_BODY_MAX).min(self.payload.len());
        let body = &self.payload[start..end];
        out[0] = CONTROL_FRAGMENT;
        out[1..7].copy_from_slice(&self.message_id);
        // `count` is at most 16 and `index` below it: both fit a nibble.
        out[7] = (((self.count - 1) as u8) << 4) | index as u8;
        out[FRAGMENT_HEADER..FRAGMENT_HEADER + body.len()].copy_from_slice(body);
        FRAGMENT_HEADER + body.len()
    }
}

#[cfg(test)]
mod tests;
