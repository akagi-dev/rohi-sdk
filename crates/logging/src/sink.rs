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

use core::cell::{Cell, RefCell};
use core::fmt;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, RawMutex};
use embassy_sync::channel::Channel;
use log::{Log, Metadata, Record};

use crate::tail::{DEFAULT_TAIL_RECORDS, Overwritten, Page, Rendered, Tail, render};
use crate::{LogClock, LogLevel, LogRecord};

/// Records whose target starts with this are never persisted, so the logging
/// task can report its own failures without feeding them back into itself.
pub const INTERNAL_TARGET: &str = "rohi_logging";

/// The producer side of persistent logging.
///
/// A `LogSink` is a bounded queue of `N` [`LogRecord`]s with a runtime level,
/// and a RAM tail of the last `T` records for `logging.tail`.
/// A log call filters by level first, then formats into an owned record,
/// stamps it from the [`LogClock`] and `try_send`s it. It never waits: when
/// the queue is full the new record is dropped and counted. The logging task
/// is the only consumer, see [`recv`](Self::recv).
///
/// A record that passes the filters is also copied into the tail, a ring of
/// `T` records that overwrites the oldest and is independent of the queue: it
/// keeps recent records while the storage is busy, faulted or the queue is full.
///
/// `M` is the [`RawMutex`] guarding the queue and the counters; a sink used as
/// a `log` backend needs [`CriticalSectionRawMutex`].
pub struct LogSink<M: RawMutex, C, const N: usize, const T: usize = DEFAULT_TAIL_RECORDS> {
    clock: C,
    level: Mutex<M, Cell<LogLevel>>,
    dropped: Mutex<M, Cell<u64>>,
    last_drop_ms: Mutex<M, Cell<Option<u64>>>,
    queue: Channel<M, LogRecord, N>,
    tail: Mutex<M, RefCell<Tail<T>>>,
}

impl<M: RawMutex, C: LogClock, const N: usize, const T: usize> LogSink<M, C, N, T> {
    /// An empty sink keeping records up to `level`.
    pub const fn new(clock: C, level: LogLevel) -> Self {
        Self {
            clock,
            level: Mutex::new(Cell::new(level)),
            dropped: Mutex::new(Cell::new(0)),
            last_drop_ms: Mutex::new(Cell::new(None)),
            queue: Channel::new(),
            tail: Mutex::new(RefCell::new(Tail::new())),
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

    /// When a record was last dropped because the queue was full, in
    /// milliseconds since boot.
    pub fn last_drop_ms(&self) -> Option<u64> {
        self.last_drop_ms.lock(Cell::get)
    }

    /// Completes when a record is waiting, without taking it.
    pub async fn ready(&self) {
        self.queue.ready_to_receive().await;
    }

    /// The clock records are stamped with.
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Records waiting for the logging task.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// The token that identifies this boot to a client of `logging.tail`.
    /// Never 0. The default is 1, until the firmware calls
    /// [`set_tail_session`](Self::set_tail_session).
    pub fn tail_session(&self) -> u32 {
        self.tail.lock(|tail| tail.borrow().session())
    }

    /// Sets the token that identifies this boot to a client of `logging.tail`:
    /// a number that differs on every boot, for example the boot counter of the
    /// `system` service. 0 is stored as 1, because 0 means "no session" in a
    /// request.
    pub fn set_tail_session(&self, session: u32) {
        self.tail
            .lock(|tail| tail.borrow_mut().set_session(session));
    }

    /// Records the RAM tail holds at most.
    pub const fn tail_capacity(&self) -> usize {
        T
    }

    pub(crate) fn tail_page(&self, session: u64, after: u64, limit: u64) -> Page {
        self.tail
            .lock(|tail| tail.borrow().page(session, after, limit))
    }

    pub(crate) fn tail_render(
        &self,
        first: u64,
        count: u64,
        skip: u64,
        out: &mut [u8],
    ) -> Result<Rendered, Overwritten> {
        render(
            |sequence| self.tail.lock(|tail| tail.borrow().get(sequence)),
            first,
            count,
            skip,
            out,
        )
    }

    /// Converts one event into a record, copies it into the RAM tail and queues it
    /// for the archive. Returns whether it was queued: `false` when the level or
    /// target filters it out or the queue is full (then it is counted in
    /// [`dropped`](Self::dropped); the tail keeps the record all the same).
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
        self.tail.lock(|tail| tail.borrow_mut().publish(&record));
        if self.queue.try_send(record).is_ok() {
            true
        } else {
            self.dropped
                .lock(|cell| cell.set(cell.get().saturating_add(1)));
            let now = self.clock.monotonic_ms();
            self.last_drop_ms.lock(|cell| cell.set(Some(now)));
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

impl<C: LogClock + Send + Sync, const N: usize, const T: usize> Log
    for LogSink<CriticalSectionRawMutex, C, N, T>
{
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.level().keeps(metadata.level()) && !metadata.target().starts_with(INTERNAL_TARGET)
    }

    fn log(&self, record: &Record<'_>) {
        self.push(record.level(), record.target(), *record.args());
    }

    fn flush(&self) {}
}
