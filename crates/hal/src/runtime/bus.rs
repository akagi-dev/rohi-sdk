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
///////////////////////////////////////////////////////////////////////////////
//! Bounded sensor event queue.
//!
//! All sensor tasks publish [`SensorEvent`]s into one [`EventBus`]; a single
//! consumer task owns the `MeasurementStore` and applies them.
//!
//! # Overflow policy
//!
//! For environmental telemetry the newest state is more useful than a backlog,
//! so when the queue is full [`EventBus::publish`] discards the **oldest**
//! queued event, enqueues the new one and counts the drop. Producers never
//! block. Because sensors publish at most a few events every several seconds,
//! [`EVENT_QUEUE_CAPACITY`] events leave ample headroom.

use core::cell::Cell;

use crate::SensorEvent;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::{Channel, TrySendError};

/// Recommended queue capacity, in events.
pub const EVENT_QUEUE_CAPACITY: usize = 16;

/// Bounded multi-producer queue of sensor events.
pub struct EventBus<M: RawMutex, const N: usize> {
    channel: Channel<M, SensorEvent, N>,
    dropped: Mutex<M, Cell<u32>>,
}

impl<M: RawMutex, const N: usize> EventBus<M, N> {
    /// Create an empty bus.
    pub const fn new() -> Self {
        Self {
            channel: Channel::new(),
            dropped: Mutex::new(Cell::new(0)),
        }
    }

    /// Publish an event without blocking, discarding the oldest queued event if
    /// the queue is full.
    pub fn publish(&self, event: SensorEvent) {
        let mut event = event;
        // Another producer may refill the slot freed below, so retry a few times.
        for _ in 0..4 {
            match self.channel.try_send(event) {
                Ok(()) => return,
                Err(TrySendError::Full(rejected)) => {
                    event = rejected;
                    if self.channel.try_receive().is_ok() {
                        self.dropped.lock(|c| c.set(c.get().saturating_add(1)));
                    }
                }
            }
        }
        self.dropped.lock(|c| c.set(c.get().saturating_add(1)));
    }

    /// Wait for the next event.
    ///
    /// Cancel-safe: an event is taken from the queue only when it is returned.
    pub async fn receive(&self) -> SensorEvent {
        self.channel.receive().await
    }

    /// Take the next event if one is queued.
    pub fn try_receive(&self) -> Option<SensorEvent> {
        self.channel.try_receive().ok()
    }

    /// Number of events discarded because the queue was full.
    pub fn dropped(&self) -> u32 {
        self.dropped.lock(|c| c.get())
    }
}

impl<M: RawMutex, const N: usize> Default for EventBus<M, N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MonotonicTimestamp, SensorErrorCode, SensorKind};
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;

    fn error_at(ms: u64) -> SensorEvent {
        SensorEvent::Error {
            sensor: SensorKind::Scd4x,
            error: SensorErrorCode::Crc,
            timestamp: MonotonicTimestamp::from_millis(ms),
        }
    }

    #[test]
    fn delivers_in_order() {
        let bus = EventBus::<NoopRawMutex, 4>::new();
        bus.publish(error_at(1));
        bus.publish(error_at(2));
        assert_eq!(bus.try_receive(), Some(error_at(1)));
        assert_eq!(bus.try_receive(), Some(error_at(2)));
        assert_eq!(bus.try_receive(), None);
        assert_eq!(bus.dropped(), 0);
    }

    #[test]
    fn full_queue_drops_oldest_and_counts() {
        let bus = EventBus::<NoopRawMutex, 2>::new();
        for ms in 1..=5 {
            bus.publish(error_at(ms));
        }
        assert_eq!(bus.dropped(), 3);
        assert_eq!(bus.try_receive(), Some(error_at(4)));
        assert_eq!(bus.try_receive(), Some(error_at(5)));
        assert_eq!(bus.try_receive(), None);
    }
}
