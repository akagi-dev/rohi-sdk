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
//! Bounded sensor event queue.
//!
//! All sensor tasks publish [`SensorEvent`]s into one [`EventBus`]; a single
//! consumer task owns the `MeasurementStore` and is the only component that
//! mutates it.
//!
//! # Events
//!
//! Sensors publish three kinds of [`SensorEvent`]:
//!
//! * `Measurements`: a successful observation;
//! * `StateChanged`: an actual lifecycle transition, published only when the
//!   state changes;
//! * `Error`: a failed operation.
//!
//! **Success is one event.** The `MeasurementStore` derives `Ready` from a
//! `Measurements` event: the new values, their timestamp and `Ready` are
//! applied in one step, also when recovering from `Degraded` or `Error`. There
//! is no separate `StateChanged(Ready)`, so a consumer can never observe
//! `Ready` together with an old value, and one successful cycle costs one
//! queue slot.
//!
//! # Overflow policy
//!
//! For environmental telemetry the newest state is more useful than a backlog,
//! so when the queue is full [`EventBus::publish`] discards the **oldest**
//! queued event, enqueues the new one and counts the drop
//! ([`EventBus::dropped`]; the firmware logs it with every snapshot).
//! Producers never block. Because sensors publish at most a few events every
//! several seconds, [`EVENT_QUEUE_CAPACITY`] (16) events leave ample headroom.
//!
//! # Cancellation
//!
//! [`EventBus::receive`] is cancel-safe: an event is taken from the queue only
//! when it is returned. [`EventBus::publish`] never waits.

use core::cell::Cell;

use crate::SensorEvent;
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::channel::{Channel, TrySendError};

/// Recommended queue capacity, in events.
pub const EVENT_QUEUE_CAPACITY: usize = 16;

/// Bounded multi-producer queue of sensor events.
///
/// Place it in a `static`; the storage is inline. `N` is the capacity in
/// events, [`EVENT_QUEUE_CAPACITY`] is the recommended value.
///
/// ```
/// use embassy_sync::blocking_mutex::raw::NoopRawMutex;
/// use rohi_hal::runtime::EventBus;
/// use rohi_hal::{MonotonicTimestamp, SensorErrorCode, SensorEvent, SensorKind};
///
/// let error_at = |ms| SensorEvent::Error {
///     sensor: SensorKind::Scd4x,
///     error: SensorErrorCode::Crc,
///     timestamp: MonotonicTimestamp::from_millis(ms),
/// };
///
/// // A full queue drops the oldest event and counts it.
/// let bus = EventBus::<NoopRawMutex, 2>::new();
/// for ms in 1..=3 {
///     bus.publish(error_at(ms));
/// }
/// assert_eq!(bus.dropped(), 1);
/// assert_eq!(bus.try_receive(), Some(error_at(2)));
/// assert_eq!(bus.try_receive(), Some(error_at(3)));
/// assert_eq!(bus.try_receive(), None);
/// ```
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
                    if let Ok(oldest) = self.channel.try_receive() {
                        let total = self.count_drop();
                        warn_dropped(&oldest, total);
                    }
                }
            }
        }
        let total = self.count_drop();
        warn_dropped(&event, total);
    }

    fn count_drop(&self) -> u32 {
        self.dropped.lock(|c| {
            let total = c.get().saturating_add(1);
            c.set(total);
            total
        })
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

#[cold]
#[inline(never)]
fn warn_dropped(event: &SensorEvent, total: u32) {
    let (kind, sensor) = match event {
        SensorEvent::Measurements(update) => ("measurement", update.sensor()),
        SensorEvent::StateChanged { sensor, .. } => ("state change", *sensor),
        SensorEvent::Error { sensor, .. } => ("error", *sensor),
    };
    log::warn!(
        target: "sensor",
        "Event queue full, {} event dropped (sensor={} total={})",
        kind,
        sensor.tag(),
        total
    );
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
