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
//! The non-blocking producer side: the `log` frontend and its queue.

use core::cell::Cell;
use core::fmt;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex};
use embassy_sync::channel::Channel;
use log::{Log, Metadata, Record};

use crate::{LogClock, LogLevel, LogRecord};

/// Records whose target starts with this are never persisted, so the logging
/// task can report its own failures without feeding them back into itself.
pub const INTERNAL_TARGET: &str = "rohi_logging";

/// The producer side of persistent logging.
///
/// A `LogSink` is a bounded queue of `N` [`LogRecord`]s with a runtime level.
/// A log call filters by level first, then formats into an owned record,
/// stamps it from the [`LogClock`] and `try_send`s it. It never waits: when
/// the queue is full the new record is dropped and counted. The logging task
/// is the only consumer, see [`recv`](Self::recv).
///
/// `M` is the [`RawMutex`] guarding the queue and the counters; a sink used as
/// a `log` backend needs [`CriticalSectionRawMutex`].
pub struct LogSink<M: RawMutex, C, const N: usize> {
    clock: C,
    level: Mutex<M, Cell<LogLevel>>,
    dropped: Mutex<M, Cell<u64>>,
    queue: Channel<M, LogRecord, N>,
}

impl<M: RawMutex, C: LogClock, const N: usize> LogSink<M, C, N> {
    /// An empty sink keeping records up to `level`.
    pub const fn new(clock: C, level: LogLevel) -> Self {
        Self {
            clock,
            level: Mutex::new(Cell::new(level)),
            dropped: Mutex::new(Cell::new(0)),
            queue: Channel::new(),
        }
    }

    /// The level records are kept at.
    pub fn level(&self) -> LogLevel {
        self.level.lock(Cell::get)
    }

    /// Changes the level; applies to the next log call.
    pub fn set_level(&self, level: LogLevel) {
        self.level.lock(|cell| cell.set(level));
    }

    /// Records dropped because the queue was full, since boot.
    pub fn dropped(&self) -> u64 {
        self.dropped.lock(Cell::get)
    }

    /// Records waiting for the logging task.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Converts one event into a record and queues it. Returns whether it was
    /// queued: `false` when the level or target filters it out or the queue is
    /// full (then it is counted in [`dropped`](Self::dropped)).
    ///
    /// Never blocks, allocates or panics. A filtered event costs one level
    /// comparison; nothing is formatted.
    pub fn push(&self, level: log::Level, target: &str, args: fmt::Arguments<'_>) -> bool {
        if !self.level().keeps(level) || target.starts_with(INTERNAL_TARGET) {
            return false;
        }
        let Some(level) = LogLevel::from_log(level) else {
            return false;
        };
        let record = LogRecord::new(
            level,
            target,
            args,
            self.clock.monotonic_ms(),
            self.clock.wall_time(),
        );
        if self.queue.try_send(record).is_ok() {
            true
        } else {
            self.dropped
                .lock(|cell| cell.set(cell.get().saturating_add(1)));
            false
        }
    }

    /// Waits for the next record; the logging task's input.
    ///
    /// Cancel safe: a record is removed from the queue only when this
    /// completes.
    pub async fn recv(&self) -> LogRecord {
        self.queue.receive().await
    }
    /// The next record if one is waiting.
    pub fn try_recv(&self) -> Option<LogRecord> {
        self.queue.try_receive().ok()
    }
}

impl<C: LogClock + Send + Sync, const N: usize> Log for LogSink<CriticalSectionRawMutex, C, N> {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.level().keeps(metadata.level()) && !metadata.target().starts_with(INTERNAL_TARGET)
    }

    fn log(&self, record: &Record<'_>) {
        self.push(record.level(), record.target(), *record.args());
    }

    fn flush(&self) {}
}
