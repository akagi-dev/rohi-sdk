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
//! Bounded in-memory outbox of signed frames.
//!
//! The queue is statically bounded and allocation free
//! ([`DEFAULT_OUTBOX_DEPTH`](crate::service::DEFAULT_OUTBOX_DEPTH) frames by
//! default, 4 in the Altruist Urban firmware). When it is full **no new frame
//! is created** (nothing is evicted) and `skipped_outbox_full` is
//! incremented. The queue is in memory: frames are lost on reboot or when the
//! service is replaced (a flash spool is out of scope).
//!
//! ```
//! use rohi_connectivity::{EnvelopeBuilder, EnvelopeFrame, NONCE_LEN, NonceError, NonceSource, Outbox};
//! use rohi_identity::Identity;
//!
//! struct Counter(u8);
//! impl NonceSource for Counter {
//!     fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError> {
//!         self.0 += 1;
//!         dst.fill(self.0);
//!         Ok(())
//!     }
//! }
//!
//! let signer = Identity::from_secret(&[1; 32]).unwrap().signer();
//! let mut nonce = Counter(0);
//! let mut frame = || -> EnvelopeFrame {
//!     EnvelopeBuilder::new(&signer, &mut nonce).sign(&[0x0a, 0x00]).unwrap()
//! };
//!
//! let mut outbox = Outbox::<2>::new();
//! let (first, second) = (frame(), frame());
//! outbox.push(first.clone()).unwrap();
//! outbox.push(second).unwrap();
//! // Full: the third frame is refused, nothing is evicted.
//! assert!(outbox.is_full());
//! assert!(outbox.push(frame()).is_err());
//! assert_eq!(outbox.front(), Some(&first));
//! assert_eq!(outbox.pop(), Some(first));
//! assert_eq!(outbox.len(), 1);
//! ```

use heapless::Deque;

use crate::envelope::EnvelopeFrame;

/// The outbox has no room for another frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboxFull;

/// FIFO queue of frames waiting for delivery, statically bounded by `DEPTH`.
///
/// Frames leave the outbox only when delivered or permanently rejected; a
/// failed attempt leaves the very same frame at the front. When the outbox is
/// full nothing is evicted and nothing is allocated: the producer skips the
/// frame instead (see
/// [`Diagnostics::skipped_outbox_full`](crate::Diagnostics::skipped_outbox_full)).
pub struct Outbox<const DEPTH: usize> {
    frames: Deque<EnvelopeFrame, DEPTH>,
}

impl<const DEPTH: usize> Outbox<DEPTH> {
    /// An empty outbox.
    pub const fn new() -> Self {
        Self {
            frames: Deque::new(),
        }
    }

    /// Queues `frame` behind the others.
    ///
    /// # Errors
    ///
    /// [`OutboxFull`] when `DEPTH` frames are already queued; the queue is
    /// left unchanged.
    pub fn push(&mut self, frame: EnvelopeFrame) -> Result<(), OutboxFull> {
        self.frames.push_back(frame).map_err(|_| OutboxFull)
    }

    /// The oldest frame, the next to be delivered.
    pub fn front(&self) -> Option<&EnvelopeFrame> {
        self.frames.front()
    }

    /// Removes the oldest frame.
    pub fn pop(&mut self) -> Option<EnvelopeFrame> {
        self.frames.pop_front()
    }

    /// Number of queued frames.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// `true` if no frame is queued.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// `true` if another frame does not fit.
    pub fn is_full(&self) -> bool {
        self.frames.is_full()
    }
}

impl<const DEPTH: usize> Default for Outbox<DEPTH> {
    fn default() -> Self {
        Self::new()
    }
}
