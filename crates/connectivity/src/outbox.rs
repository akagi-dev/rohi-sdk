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
