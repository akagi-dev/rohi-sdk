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

//! The `logging` service: the runtime that turns the queue of records into
//! segments, and what it exposes to the control plane.

use core::cell::RefCell;

use embassy_futures::select::{Either3, select3};
use embassy_sync::blocking_mutex::Mutex as BlockingMutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embedded_hal_async::delay::DelayNs;
use heapless::Vec;
use rohi_service::{
    Action, ActionAccess, ActionDescriptor, ActionError, ActionOutput, EnumDescriptor,
    FIRST_SERVICE_ISSUE, FieldDescriptor, Issue, IssueDescriptor, IssueSeverity, MAX_ISSUES,
    MAX_STREAM_CHUNK, Service, ServiceHealth, ServiceState, ServiceSummary, Status, StatusWriter,
    StreamChunk, StreamFile, StreamInfo, Unit, Value, ValueKind, WriteError,
};

use crate::segment::text::write_line;
use crate::segment::{Batch, MAX_CHUNK_LEN, SegmentDecoder, file_name};
use crate::storage::{
    ChunkMeta, Limits, LogStorage, MAX_SEGMENTS, SegmentId, StorageError, StorageStats,
};
use crate::{
    Compression, LEVEL_ENUM, LEVELS, LogClock, LogRecord, LogSink, LogStorageKind, LoggingConfig,
};

/// Issue codes of the `logging` service.
pub mod issue {
    use super::FIRST_SERVICE_ISSUE;

    /// The selected backend cannot be opened (no partition, no card). The
    /// service stays on it; records wait in the queue and are dropped when it
    /// is full.
    pub const STORAGE_UNAVAILABLE: u32 = FIRST_SERVICE_ISSUE;
    /// Recovery found damaged data and dropped it.
    pub const STORAGE_CORRUPT: u32 = FIRST_SERVICE_ISSUE + 1;
    /// Writing a segment failed; the records of that batch are lost.
    pub const WRITE_FAILED: u32 = FIRST_SERVICE_ISSUE + 2;
    /// Reading the history failed.
    pub const READ_FAILED: u32 = FIRST_SERVICE_ISSUE + 3;
    /// Records were dropped because the queue was full in the last
    /// [`OVERFLOW_WINDOW_MS`](super::OVERFLOW_WINDOW_MS).
    pub const QUEUE_OVERFLOW: u32 = FIRST_SERVICE_ISSUE + 4;
}

/// Field ids of the `logging` service status.
pub mod status_field {
    /// Persistent level, an enum.
    pub const LEVEL: u32 = 1;
    /// Active backend: `internal` or `sd`.
    pub const STORAGE: u32 = 2;
    /// Codec of new segments: `none` or `lz4`.
    pub const COMPRESSION: u32 = 3;
    /// Records retained.
    pub const STORED_RECORDS: u32 = 4;
    /// Segments retained.
    pub const SEGMENTS: u32 = 5;
    /// Budget of the history, in bytes.
    pub const CAPACITY_BYTES: u32 = 6;
    /// Storage allocated to the history, in bytes.
    pub const USED_BYTES: u32 = 7;
    /// Storage that can be used before the oldest segment is dropped.
    pub const FREE_BYTES: u32 = 8;
    /// Records lost since boot: queue overflow and failed writes.
    pub const DROPPED_RECORDS: u32 = 9;
    /// Sequence of the oldest retained record; absent when there is none.
    pub const OLDEST_SEQUENCE: u32 = 10;
    /// Sequence of the newest retained record; absent when there is none.
    pub const NEWEST_SEQUENCE: u32 = 11;
    /// Identifies the retained history; changes when it is cleared or lost.
    pub const HISTORY: u32 = 12;
}

/// Action ids of the `logging` service.
pub mod action {
    /// Reads records after a sequence, as a stream of text lines.
    pub const READ: u32 = 1;
    /// Exports the retained segment files, as a stream of files.
    pub const DOWNLOAD: u32 = 2;
    /// Removes the history of the active backend.
    pub const CLEAR: u32 = 3;
}

/// Field ids of the arguments of `read`.
pub mod read_arg {
    /// Records with a sequence above this are returned; 0 starts at the
    /// oldest record.
    pub const AFTER: u32 = 1;
    /// The most records returned, 1 to [`MAX_READ_RECORDS`](super::MAX_READ_RECORDS).
    pub const LIMIT: u32 = 2;
}

/// The most records one `read` returns.
pub const MAX_READ_RECORDS: u64 = 200;
/// How long an idle stream is kept before its segments are released.
pub const STREAM_TIMEOUT_MS: u64 = 60_000;
/// Records were dropped "recently" when the last drop is younger than this.
pub const OVERFLOW_WINDOW_MS: u64 = 30_000;
/// The longest name of the file `read` returns.
pub const MAX_READ_FILE_NAME_LEN: usize = 22;
/// The reason of the `rejected` error of a `read` that met a damaged segment.
pub const READ_CORRUPT: &str = "segment_corrupt";
/// The id of the one file `read` returns.
pub const READ_FILE_ID: u32 = 1;

/// The name of the file `read` returns for the history `history`:
/// `records-<history>.log`.
///
/// A cursor (the sequence of a record) belongs to one history, the
/// `history` of the status. The name tells which history the records come
/// from, so a client that follows the log sees that it was cleared or lost
/// and starts over instead of waiting for sequences it already passed.
pub fn read_file_name(history: u32) -> heapless::String<MAX_READ_FILE_NAME_LEN> {
    use core::fmt::Write;
    let mut name = heapless::String::new();
    let _ = write!(name, "records-{history}.log");
    name
}

/// The history a file name of `read` belongs to, see [`read_file_name`].
pub fn history_of_read_file(name: &str) -> Option<u32> {
    name.strip_prefix("records-")?
        .strip_suffix(".log")?
        .parse()
        .ok()
}

const MAX_STREAMS: usize = 2;
const RETRY_MS: u64 = 10_000;

// The reports are kept out of line: they run once in a while, and inlined into
// the task they would only grow its frame.
#[cold]
#[inline(never)]
fn warn_storage(what: &str, error: &dyn core::fmt::Display) {
    log::warn!("Log storage: cannot {what}: {error}");
}

#[cold]
#[inline(never)]
fn notice_formatted() {
    log::info!("Log storage: no valid format header, the region was formatted");
}
const PINNED_RETRY_MS: u64 = 1_000;
const SWEEP_MS: u64 = 10_000;
const FLUSH_BYTES: usize = 1_536;

/// Opens the backend a `storage` setting names.
///
/// The service owns what it returns until the setting changes or the backend
/// fails, then drops it. Opening may fail for good (no such backend on this
/// device) or for now (no card); the service retries every few seconds.
#[allow(async_fn_in_trait)]
pub trait Backends {
    /// The storage type of every backend, an enum when they differ.
    type Storage: LogStorage;

    /// The storage of `kind`, `None` when it is not available.
    async fn open(&mut self, kind: LogStorageKind) -> Option<Self::Storage>;
}

enum Content<const READ: usize> {
    Records(Vec<u8, READ>),
    Files(Vec<(SegmentId, u32), MAX_SEGMENTS>),
}

struct Open<const READ: usize> {
    id: u32,
    content: Content<READ>,
    deadline_ms: u64,
}

struct State<const READ: usize> {
    config: LoggingConfig,
    started: bool,
    unavailable_since: Option<u64>,
    corrupt_since: Option<u64>,
    write_failed_since: Option<u64>,
    read_failed_since: Option<u64>,
    stats: StorageStats,
    active: Option<LogStorageKind>,
    lost: u64,
    streams: Vec<Open<READ>, MAX_STREAMS>,
    next_stream: u32,
    release_all: bool,
}

// The segments the released streams pinned: at most `MAX_SEGMENTS` for each of
// `MAX_STREAMS` streams, so it never overflows.
type Pins = Vec<SegmentId, { MAX_STREAMS * MAX_SEGMENTS }>;

impl<const READ: usize> State<READ> {
    // Removes the streams `selected` accepts, in place, and returns what they
    // pinned. Nothing else of a stream is moved or copied: a stream holds up to
    // `READ` bytes of records.
    fn release(&mut self, mut selected: impl FnMut(&Open<READ>) -> bool) -> Pins {
        let mut pins = Pins::new();
        self.streams.retain_mut(|stream| {
            if !selected(stream) {
                return true;
            }
            if let Content::Files(files) = &stream.content {
                for (id, _) in files {
                    // Cannot be full, see `Pins`.
                    let _ = pins.push(*id);
                }
            }
            false
        });
        pins
    }
}

pub(crate) struct Inner<B: Backends> {
    backends: B,
    storage: Option<B::Storage>,
    kind: Option<LogStorageKind>,
    open_codec: Option<Compression>,
    batch: Batch,
    batch_since_ms: u64,
    next_sequence: u64,
    retry_at_ms: u64,
    faulted: bool,
    stalled_until_ms: u64,
    carry: Option<LogRecord>,
}

/// The logging runtime: the queue, the storage, the status and the streams.
///
/// One `Logging` lives in a `static`. The `log` backend pushes into
/// [`sink`](Self::sink); the logging task runs [`run`](Self::run); the control
/// plane reaches it through [`service`](Self::service); the configuration owner
/// hands every committed `logging` section to [`apply`](Self::apply).
///
/// `M` is the [`RawMutex`] of the firmware, `C` the [`LogClock`], `B` the
/// [`Backends`], `N` the capacity of the record queue and `READ` the bytes one
/// `read` can return.
pub struct Logging<M: RawMutex, C, B: Backends, const N: usize, const READ: usize> {
    sink: LogSink<M, C, N>,
    state: BlockingMutex<M, RefCell<State<READ>>>,
    inner: Mutex<M, Inner<B>>,
    wake: Signal<M, ()>,
}

impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const READ: usize>
    Logging<M, C, B, N, READ>
{
    /// A runtime that persists as `config` says until it is
    /// [applied](Self::apply) another one.
    pub const fn new(clock: C, backends: B, config: LoggingConfig) -> Self {
        Self {
            sink: LogSink::new(clock, config.level),
            state: BlockingMutex::new(RefCell::new(State {
                config,
                started: false,
                unavailable_since: None,
                corrupt_since: None,
                write_failed_since: None,
                read_failed_since: None,
                stats: StorageStats {
                    capacity_bytes: 0,
                    used_bytes: 0,
                    free_bytes: 0,
                    segments: 0,
                    records: 0,
                    oldest_sequence: None,
                    newest_sequence: None,
                    generation: 0,
                },
                active: None,
                lost: 0,
                streams: Vec::new(),
                next_stream: 1,
                release_all: false,
            })),
            inner: Mutex::new(Inner {
                backends,
                storage: None,
                kind: None,
                open_codec: None,
                batch: Batch::new(),
                batch_since_ms: 0,
                next_sequence: 1,
                retry_at_ms: 0,
                faulted: false,
                stalled_until_ms: 0,
                carry: None,
            }),
            wake: Signal::new(),
        }
    }

    /// The producer side: register it as the `log` backend, or push to it.
    pub fn sink(&self) -> &LogSink<M, C, N> {
        &self.sink
    }

    /// The service for the status and action registries.
    pub fn service(&self) -> LoggingService<'_, M, C, B, N, READ> {
        LoggingService(self)
    }

    /// Applies a committed `logging` section: the level takes effect for the
    /// next log call, a new backend is opened by the task, and new segments
    /// use the new codec and sizes. Synchronous and cheap.
    pub fn apply(&self, config: LoggingConfig) {
        if config.validate().is_err() {
            return;
        }
        self.sink.set_level(config.level);
        self.state.lock(|state| state.borrow_mut().config = config);
        self.wake.signal(());
    }

    fn now(&self) -> u64 {
        self.sink.clock().monotonic_ms()
    }

    fn with<R>(&self, f: impl FnOnce(&mut State<READ>) -> R) -> R {
        self.state.lock(|state| f(&mut state.borrow_mut()))
    }

    fn config(&self) -> LoggingConfig {
        self.with(|state| state.config)
    }

    async fn refresh(&self, inner: &mut Inner<B>) {
        let stats = match inner.storage.as_mut() {
            Some(storage) => storage.stats().await.ok(),
            None => Some(StorageStats::default()),
        };
        if let Some(stats) = stats {
            self.with(|state| state.stats = stats);
        }
    }

    // Opens or switches the backend to what the configuration selects.
    async fn reconcile(&self, inner: &mut Inner<B>, now: u64) {
        let wanted = self.config().storage;
        let switching = inner.kind.is_some_and(|kind| kind != wanted);
        if switching {
            while self.drain(inner, now) {
                self.flush(inner, now).await;
            }
            self.flush(inner, now).await;
            if let Some(storage) = inner.storage.as_mut() {
                let _ = storage.seal().await;
            }
            inner.storage = None;
            inner.kind = None;
            inner.open_codec = None;
            inner.faulted = false;
            self.release_streams(inner, true);
            self.with(|state| {
                state.active = None;
                state.stats = StorageStats::default();
                state.write_failed_since = None;
                state.read_failed_since = None;
                state.corrupt_since = None;
            });
        }
        if inner.storage.is_none() && now >= inner.retry_at_ms {
            self.open(inner, wanted, now).await;
        } else if inner.faulted && now >= inner.retry_at_ms {
            self.recover(inner, now).await;
        }
    }

    async fn open(&self, inner: &mut Inner<B>, kind: LogStorageKind, now: u64) {
        let Some(mut storage) = inner.backends.open(kind).await else {
            inner.retry_at_ms = now + RETRY_MS;
            self.with(|state| {
                state.started = true;
                state.unavailable_since.get_or_insert(now);
            });
            return;
        };
        match storage.recover().await {
            Ok(recovery) => {
                if recovery.formatted {
                    notice_formatted();
                }
                if let Some(newest) = recovery.newest_sequence {
                    inner.next_sequence = inner.next_sequence.max(newest + 1);
                }
                inner.storage = Some(storage);
                inner.kind = Some(kind);
                inner.open_codec = None;
                self.with(|state| {
                    state.started = true;
                    state.active = Some(kind);
                    state.unavailable_since = None;
                    state.corrupt_since = (recovery.dropped > 0).then_some(now);
                });
            }
            Err(error) => {
                warn_storage("open", &error);
                inner.retry_at_ms = now + RETRY_MS;
                self.with(|state| {
                    state.started = true;
                    state.unavailable_since.get_or_insert(now);
                });
            }
        }
        self.refresh(inner).await;
    }

    // After a failed write: read the medium again, so the storage knows what
    // is really there.
    async fn recover(&self, inner: &mut Inner<B>, now: u64) {
        let Some(storage) = inner.storage.as_mut() else {
            return;
        };
        match storage.recover().await {
            Ok(recovery) => {
                if let Some(newest) = recovery.newest_sequence {
                    inner.next_sequence = inner.next_sequence.max(newest + 1);
                }
                inner.faulted = false;
                inner.open_codec = None;
                self.with(|state| {
                    state.write_failed_since = None;
                    if recovery.dropped > 0 {
                        state.corrupt_since.get_or_insert(now);
                    }
                });
            }
            Err(error) => {
                warn_storage("recover", &error);
                inner.retry_at_ms = now + RETRY_MS;
            }
        }
        self.refresh(inner).await;
    }

    fn release_streams(&self, inner: &mut Inner<B>, all: bool) {
        let now = self.now();
        let pins = self.with(|state| {
            let all = all || core::mem::take(&mut state.release_all);
            state.release(|stream| all || stream.deadline_ms <= now)
        });
        Self::unpin(inner, &pins);
    }

    // Gives the storage back the segments of released streams.
    fn unpin(inner: &mut Inner<B>, pins: &[SegmentId]) {
        let Some(storage) = inner.storage.as_mut() else {
            return;
        };
        if pins.is_empty() {
            return;
        }
        for id in pins {
            storage.unpin(*id);
        }
        // The segments the stall waited for may be free now.
        inner.stalled_until_ms = 0;
    }

    // Takes the queued records into the batch, in order, while the storage can
    // take them.
    fn drain(&self, inner: &mut Inner<B>, now: u64) -> bool {
        let mut full = false;
        while inner.storage.is_some() && !inner.faulted && now >= inner.stalled_until_ms && !full {
            let Some(mut record) = inner.carry.take().or_else(|| self.sink.try_recv()) else {
                break;
            };
            record.sequence = inner.next_sequence;
            if inner.batch.is_empty() {
                inner.batch_since_ms = now;
            }
            if inner.batch.push(&record).is_err() {
                // The batch is full: this record opens the next one.
                inner.carry = Some(record);
                return true;
            }
            inner.next_sequence += 1;
            full = inner.batch.len() >= FLUSH_BYTES;
        }
        full
    }

    async fn flush(&self, inner: &mut Inner<B>, now: u64) {
        if inner.batch.is_empty() || now < inner.stalled_until_ms {
            return;
        }
        let Some(storage) = inner.storage.as_mut() else {
            return;
        };
        let config = self.config();
        let limits = Limits {
            segment_bytes: config.segment_bytes,
            retention_bytes: config.retention_bytes,
        };
        let mut chunk = [0u8; MAX_CHUNK_LEN];
        for attempt in 0..2 {
            let codec = inner.open_codec.unwrap_or(config.compression);
            let Ok(len) = inner.batch.encode(codec, &mut chunk) else {
                break;
            };
            if inner.open_codec.is_none() || storage.open_capacity() < len {
                if attempt == 1 && inner.open_codec.is_some() {
                    break;
                }
                let begun = match storage.seal().await {
                    Ok(()) => storage.begin_segment(config.compression, limits).await,
                    Err(error) => Err(error),
                };
                inner.open_codec = None;
                match begun {
                    Ok(_) => inner.open_codec = Some(config.compression),
                    Err(StorageError::Pinned) => {
                        // A download holds the oldest segment: wait, the batch stays.
                        inner.stalled_until_ms = now + PINNED_RETRY_MS;
                        return;
                    }
                    Err(_) => return self.write_failed(inner, now),
                }
                continue;
            }
            let meta = ChunkMeta {
                records: inner.batch.records(),
                first_sequence: inner.batch.first_sequence(),
                last_sequence: inner.batch.last_sequence(),
                raw_len: inner.batch.len() as u32,
            };
            match storage.append(&chunk[..len], meta).await {
                Ok(()) => {
                    inner.batch.clear();
                    if storage.open_capacity() < MAX_CHUNK_LEN {
                        // Sealed as soon as it is full, so it can be downloaded.
                        if storage.seal().await.is_err() {
                            return self.write_failed(inner, now);
                        }
                        inner.open_codec = None;
                    }
                    self.with(|state| state.write_failed_since = None);
                    self.refresh(inner).await;
                    return;
                }
                Err(StorageError::SegmentFull) => {
                    let _ = storage.seal().await;
                    inner.open_codec = None;
                }
                Err(_) => return self.write_failed(inner, now),
            }
        }
        self.write_failed(inner, now);
    }

    fn write_failed(&self, inner: &mut Inner<B>, now: u64) {
        let lost = u64::from(inner.batch.records());
        inner.batch.clear();
        inner.faulted = true;
        inner.retry_at_ms = now + RETRY_MS;
        self.with(|state| {
            state.lost += lost;
            state.write_failed_since.get_or_insert(now);
        });
    }

    // Everything that has to happen whenever the task wakes or an action
    // wants current data.
    pub(crate) async fn maintain(&self, inner: &mut Inner<B>) {
        let now = self.now();
        self.release_streams(inner, false);
        self.reconcile(inner, now).await;
        loop {
            let full = self.drain(inner, now);
            let interval = u64::from(self.config().flush_interval_ms);
            let due = !inner.batch.is_empty()
                && now >= inner.stalled_until_ms
                && (full || now.saturating_sub(inner.batch_since_ms) >= interval);
            if !due {
                break;
            }
            self.flush(inner, now).await;
            if inner.faulted || !full || !inner.batch.is_empty() {
                break;
            }
        }
    }

    // Makes everything accepted so far readable.
    async fn commit(&self, inner: &mut Inner<B>) {
        let now = self.now();
        self.release_streams(inner, false);
        self.reconcile(inner, now).await;
        while self.drain(inner, now) {
            self.flush(inner, now).await;
            if inner.faulted {
                return;
            }
        }
        self.flush(inner, now).await;
    }

    fn next_wait(&self, inner: &Inner<B>) -> Option<u64> {
        let now = self.now();
        let mut wait: Option<u64> = None;
        let mut at = |deadline: u64| {
            let ms = deadline.saturating_sub(now).max(1);
            wait = Some(wait.map_or(ms, |current| current.min(ms)));
        };
        if !inner.batch.is_empty() {
            // A stalled batch is not retried before the stall ends.
            at(
                (inner.batch_since_ms + u64::from(self.config().flush_interval_ms))
                    .max(inner.stalled_until_ms),
            );
        }
        if inner.stalled_until_ms > now {
            at(inner.stalled_until_ms);
        }
        if inner.storage.is_none() || inner.faulted {
            at(inner.retry_at_ms);
        }
        if self.with(|state| !state.streams.is_empty()) {
            at(now + SWEEP_MS);
        }
        wait
    }

    /// The logging task: takes records from the queue, writes them in batches,
    /// rotates segments and keeps the status current. Never returns.
    ///
    /// Every await point is a place a power loss or a cancel may stop; the
    /// task is meant to run for the life of the device.
    pub async fn run<D: DelayNs>(&self, mut delay: D) -> ! {
        loop {
            let (wait, accepting) = {
                let mut inner = self.inner.lock().await;
                self.maintain(&mut inner).await;
                let accepting = inner.storage.is_some()
                    && !inner.faulted
                    && self.now() >= inner.stalled_until_ms;
                (self.next_wait(&inner), accepting)
            };
            // Records that cannot be taken (no storage, a failed write, a pinned
            // segment) stay queued and must not wake the task again and again:
            // it waits for the retry deadline of `next_wait` instead.
            let ready = async {
                if accepting {
                    self.sink.ready().await;
                } else {
                    core::future::pending::<()>().await;
                }
            };
            match wait {
                Some(ms) => {
                    let ms = u32::try_from(ms).unwrap_or(u32::MAX);
                    match select3(ready, self.wake.wait(), delay.delay_ms(ms)).await {
                        Either3::First(()) | Either3::Second(()) | Either3::Third(()) => {}
                    }
                }
                None => {
                    embassy_futures::select::select(ready, self.wake.wait()).await;
                }
            }
        }
    }
}

/// The `logging` service of the control plane. Create it with
/// [`Logging::service`].
#[derive(Service)]
pub struct LoggingService<'a, M: RawMutex, C, B: Backends, const N: usize, const READ: usize>(
    &'a Logging<M, C, B, N, READ>,
);

impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const READ: usize> Status
    for LoggingService<'_, M, C, B, N, READ>
{
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::LEVEL, "level", ValueKind::Enum).with_enum(LEVEL_ENUM),
        FieldDescriptor::new(status_field::STORAGE, "storage", ValueKind::String),
        FieldDescriptor::new(status_field::COMPRESSION, "compression", ValueKind::String),
        FieldDescriptor::new(
            status_field::STORED_RECORDS,
            "stored_records",
            ValueKind::U64,
        ),
        FieldDescriptor::new(status_field::SEGMENTS, "segments", ValueKind::U64),
        FieldDescriptor::new(
            status_field::CAPACITY_BYTES,
            "capacity_bytes",
            ValueKind::U64,
        )
        .unit(Unit::Bytes),
        FieldDescriptor::new(status_field::USED_BYTES, "used_bytes", ValueKind::U64)
            .unit(Unit::Bytes),
        FieldDescriptor::new(status_field::FREE_BYTES, "free_bytes", ValueKind::U64)
            .unit(Unit::Bytes),
        FieldDescriptor::new(
            status_field::DROPPED_RECORDS,
            "dropped_records",
            ValueKind::U64,
        ),
        FieldDescriptor::new(
            status_field::OLDEST_SEQUENCE,
            "oldest_sequence",
            ValueKind::U64,
        ),
        FieldDescriptor::new(
            status_field::NEWEST_SEQUENCE,
            "newest_sequence",
            ValueKind::U64,
        ),
        FieldDescriptor::new(status_field::HISTORY, "history", ValueKind::U64),
    ];
    const ENUMS: &'static [EnumDescriptor] = LEVELS;
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::STORAGE_UNAVAILABLE, "storage_unavailable"),
        IssueDescriptor::new(issue::STORAGE_CORRUPT, "storage_corrupt"),
        IssueDescriptor::new(issue::WRITE_FAILED, "write_failed"),
        IssueDescriptor::new(issue::READ_FAILED, "read_failed"),
        IssueDescriptor::new(issue::QUEUE_OVERFLOW, "queue_overflow"),
    ];
    // Diagnostics: the device works without them.
    const ESSENTIAL: bool = false;

    async fn summary(&self) -> ServiceSummary {
        let logging = self.0;
        let (started, unavailable, write_failed, degraded) = logging.with(|state| {
            (
                state.started,
                state.unavailable_since.is_some(),
                state.write_failed_since.is_some(),
                state.corrupt_since.is_some() || state.read_failed_since.is_some(),
            )
        });
        if !started {
            ServiceSummary::new(ServiceState::Starting, ServiceHealth::Unknown, false)
        } else if unavailable {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Error, false)
        } else if write_failed {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Degraded, false)
        } else if degraded {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Degraded, true)
        } else {
            ServiceSummary::new(ServiceState::Running, ServiceHealth::Ok, true)
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let logging = self.0;
        let now = logging.now();
        let age = |since: u64| Some(now.saturating_sub(since));
        let overflow = logging
            .sink
            .last_drop_ms()
            .filter(|at| now.saturating_sub(*at) <= OVERFLOW_WINDOW_MS);
        let (unavailable, write, read, corrupt) = logging.with(|state| {
            (
                state.unavailable_since,
                state.write_failed_since,
                state.read_failed_since,
                state.corrupt_since,
            )
        });
        for (since, code, severity) in [
            (
                unavailable,
                issue::STORAGE_UNAVAILABLE,
                IssueSeverity::Error,
            ),
            (write, issue::WRITE_FAILED, IssueSeverity::Error),
            (read, issue::READ_FAILED, IssueSeverity::Warning),
            (corrupt, issue::STORAGE_CORRUPT, IssueSeverity::Warning),
            (overflow, issue::QUEUE_OVERFLOW, IssueSeverity::Warning),
        ] {
            if let Some(since) = since {
                let _ = out.push(Issue {
                    severity,
                    code,
                    age_ms: age(since),
                });
            }
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let logging = self.0;
        let (config, stats, active, lost) =
            logging.with(|state| (state.config, state.stats, state.active, state.lost));
        out.variant(status_field::LEVEL, config.level as u32)?;
        out.text(
            status_field::STORAGE,
            active.unwrap_or(config.storage).name(),
        )?;
        out.text(status_field::COMPRESSION, config.compression.name())?;
        out.u64(status_field::STORED_RECORDS, stats.records)?;
        out.u64(status_field::SEGMENTS, u64::from(stats.segments))?;
        // The budget is what the configuration allows, within what the backend has.
        let capacity = stats.capacity_bytes.min(config.retention_bytes);
        let free = stats
            .free_bytes
            .min(capacity.saturating_sub(stats.used_bytes));
        out.u64(status_field::CAPACITY_BYTES, u64::from(capacity))?;
        out.u64(status_field::USED_BYTES, u64::from(stats.used_bytes))?;
        out.u64(status_field::FREE_BYTES, u64::from(free))?;
        out.u64(status_field::DROPPED_RECORDS, logging.sink.dropped() + lost)?;
        if let Some(oldest) = stats.oldest_sequence {
            out.u64(status_field::OLDEST_SEQUENCE, oldest)?;
        }
        if let Some(newest) = stats.newest_sequence {
            out.u64(status_field::NEWEST_SEQUENCE, newest)?;
        }
        if stats.generation != 0 {
            out.u64(status_field::HISTORY, u64::from(stats.generation))?;
        }
        Ok(())
    }
}

/// A [`Bounded`](crate) text buffer for the records of a `read`.
struct ReadBuffer<'a, const READ: usize>(&'a mut Vec<u8, READ>);

impl<const READ: usize> core::fmt::Write for ReadBuffer<'_, READ> {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        self.0
            .extend_from_slice(text.as_bytes())
            .map_err(|_| core::fmt::Error)
    }
}

impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const READ: usize> Action
    for LoggingService<'_, M, C, B, N, READ>
{
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::READ, "read", ActionAccess::Read)
            .args(&[
                FieldDescriptor::new(read_arg::AFTER, "after", ValueKind::U64),
                FieldDescriptor::new(read_arg::LIMIT, "limit", ValueKind::U64),
            ])
            .stream(),
        ActionDescriptor::new(action::DOWNLOAD, "download", ActionAccess::Read).stream(),
        ActionDescriptor::new(action::CLEAR, "clear", ActionAccess::Mutate),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        let logging = self.0;
        match action {
            action::READ => {
                let number = |id| {
                    args.iter()
                        .find(|(field, _)| *field == id)
                        .and_then(|(_, value)| value.as_u64())
                };
                let after = number(read_arg::AFTER).ok_or(ActionError::InvalidArgument)?;
                let limit = number(read_arg::LIMIT).ok_or(ActionError::InvalidArgument)?;
                let info = logging
                    .read(after, limit.clamp(1, MAX_READ_RECORDS))
                    .await?;
                out.stream(info).map_err(|_| ActionError::Internal)
            }
            action::DOWNLOAD => {
                let info = logging.download().await?;
                out.stream(info).map_err(|_| ActionError::Internal)
            }
            action::CLEAR => logging.clear().await,
            _ => Err(ActionError::UnknownAction),
        }
    }

    async fn read_stream(
        &self,
        stream: u32,
        file: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<StreamChunk, ActionError> {
        self.0.read_stream(stream, file, offset, buf).await
    }

    async fn close_stream(&self, stream: u32) -> Result<(), ActionError> {
        self.0.close_stream(stream).await
    }

    fn link_lost(&self) {
        self.0.with(|state| state.release_all = true);
        self.0.wake.signal(());
    }
}

impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const READ: usize>
    Logging<M, C, B, N, READ>
{
    fn register(&self, content: Content<READ>) -> Result<u32, ActionError> {
        let deadline_ms = self.now() + STREAM_TIMEOUT_MS;
        self.with(|state| {
            let id = state.next_stream;
            state
                .streams
                .push(Open {
                    id,
                    content,
                    deadline_ms,
                })
                .map_err(|_| ActionError::Busy)?;
            state.next_stream = state.next_stream.checked_add(1).unwrap_or(1);
            Ok(id)
        })
    }

    async fn read(&self, after: u64, limit: u64) -> Result<StreamInfo, ActionError> {
        let mut inner = self.inner.lock().await;
        self.commit(&mut inner).await;
        let mut records = Vec::<u8, READ>::new();
        let Some(storage) = inner.storage.as_mut() else {
            return Err(ActionError::Unavailable);
        };
        let result = Self::collect(storage, after, limit, &mut records).await;
        match result {
            Ok(()) => self.with(|state| state.read_failed_since = None),
            Err(error) => {
                let now = self.now();
                self.with(|state| {
                    state.read_failed_since.get_or_insert(now);
                });
                return Err(error);
            }
        }
        let size = records.len() as u64;
        let history = storage
            .stats()
            .await
            .map_err(|_| ActionError::Internal)?
            .generation;
        let id = self.register(Content::Records(records))?;
        let name = read_file_name(history);
        Self::stream_info(id, &[(READ_FILE_ID, name.as_str(), size)])
    }

    // Decodes the segments that can hold records above `after`. Stopping at
    // the limit or at the end of the buffer is the pagination of a read; a
    // segment that cannot be read to its end is an error.
    async fn collect(
        storage: &mut B::Storage,
        after: u64,
        limit: u64,
        records: &mut Vec<u8, READ>,
    ) -> Result<(), ActionError> {
        let segments = storage
            .segments()
            .await
            .map_err(|_| ActionError::Internal)?;
        let mut taken = 0u64;
        let mut finished = false;
        for info in segments.iter().filter(|info| info.last_sequence > after) {
            let mut decoder = SegmentDecoder::new(info.codec);
            let mut offset = 0u32;
            let mut piece = [0u8; 256];
            while !finished {
                let count = storage
                    .read(info.id, offset, &mut piece)
                    .await
                    .map_err(|_| ActionError::Internal)?;
                if count == 0 {
                    break;
                }
                offset += count as u32;
                let pushed = decoder.push(&piece[..count], &mut |record: LogRecord| {
                    if finished || record.sequence <= after {
                        return;
                    }
                    let before = records.len();
                    if taken >= limit
                        || write_line(&mut ReadBuffer(&mut *records), &record).is_err()
                    {
                        records.truncate(before);
                        finished = true;
                        return;
                    }
                    taken += 1;
                });
                if pushed.is_err() {
                    return Err(ActionError::Rejected(READ_CORRUPT));
                }
            }
            if finished {
                break;
            }
            if !decoder.is_complete() {
                return Err(ActionError::Rejected(READ_CORRUPT));
            }
        }
        Ok(())
    }

    fn stream_info(id: u32, files: &[(u32, &str, u64)]) -> Result<StreamInfo, ActionError> {
        let mut list = Vec::new();
        for (file, name, size) in files {
            list.push(StreamFile {
                id: *file,
                name: heapless::String::try_from(*name).map_err(|_| ActionError::Internal)?,
                size_bytes: *size,
            })
            .map_err(|_| ActionError::Internal)?;
        }
        Ok(StreamInfo { id, files: list })
    }

    async fn download(&self) -> Result<StreamInfo, ActionError> {
        let mut inner = self.inner.lock().await;
        self.commit(&mut inner).await;
        inner.open_codec = None;
        let Some(storage) = inner.storage.as_mut() else {
            return Err(ActionError::Unavailable);
        };
        if self.with(|state| state.streams.is_full()) {
            return Err(ActionError::Busy);
        }
        // From now on new records go to a new segment; this download holds the
        // ones that exist.
        storage.seal().await.map_err(|_| ActionError::Internal)?;
        let segments = storage
            .segments()
            .await
            .map_err(|_| ActionError::Internal)?;
        let mut pinned = Vec::<(SegmentId, u32), MAX_SEGMENTS>::new();
        for info in &segments {
            if storage.pin(info.id).await.is_err() {
                for (id, _) in &pinned {
                    storage.unpin(*id);
                }
                return Err(ActionError::Internal);
            }
            let _ = pinned.push((info.id, info.file_len));
        }
        let mut files = Vec::new();
        for (index, info) in segments.iter().enumerate() {
            let _ = files.push(StreamFile {
                id: index as u32 + 1,
                name: heapless::String::try_from(file_name(index as u32 + 1, info.codec).as_str())
                    .unwrap_or_default(),
                size_bytes: u64::from(info.file_len),
            });
        }
        match self.register(Content::Files(pinned)) {
            Ok(id) => Ok(StreamInfo { id, files }),
            Err(error) => {
                for info in &segments {
                    storage.unpin(info.id);
                }
                Err(error)
            }
        }
    }

    async fn clear(&self) -> Result<(), ActionError> {
        let mut inner = self.inner.lock().await;
        // The backend may not be open yet (the task has not run): open it, so a
        // clear right after boot works.
        let now = self.now();
        self.reconcile(&mut inner, now).await;
        if inner.storage.is_none() {
            return Err(ActionError::Unavailable);
        }
        self.release_streams(&mut inner, true);
        inner.batch.clear();
        // Records still waiting in RAM belong to the history being removed.
        inner.carry = None;
        while self.sink.try_recv().is_some() {}
        inner.open_codec = None;
        let Some(storage) = inner.storage.as_mut() else {
            return Err(ActionError::Unavailable);
        };
        if let Err(error) = storage.clear().await {
            warn_storage("clear", &error);
            if error != StorageError::Busy {
                // What the erase left is finished by a recovery.
                inner.faulted = true;
                inner.retry_at_ms = now + RETRY_MS;
            }
            // The history may be gone already, the status must say what is left.
            self.refresh(&mut inner).await;
            return Err(match error {
                StorageError::Busy => ActionError::Busy,
                _ => ActionError::Internal,
            });
        }
        self.with(|state| {
            state.corrupt_since = None;
            state.read_failed_since = None;
        });
        self.refresh(&mut inner).await;
        Ok(())
    }

    async fn read_stream(
        &self,
        stream: u32,
        file: u32,
        offset: u64,
        buf: &mut [u8],
    ) -> Result<StreamChunk, ActionError> {
        let now = self.now();
        let limit = buf.len().min(MAX_STREAM_CHUNK);
        let buf = &mut buf[..limit];
        enum Source {
            Done(StreamChunk),
            Segment(SegmentId, u32),
        }
        let source = self.with(|state| {
            let entry = state
                .streams
                .iter_mut()
                .find(|entry| entry.id == stream && entry.deadline_ms > now)
                .ok_or(ActionError::UnknownStream)?;
            entry.deadline_ms = now + STREAM_TIMEOUT_MS;
            match &entry.content {
                Content::Records(data) => {
                    if file != READ_FILE_ID {
                        return Err(ActionError::UnknownStream);
                    }
                    let offset =
                        usize::try_from(offset).map_err(|_| ActionError::InvalidArgument)?;
                    let rest = data.get(offset..).ok_or(ActionError::InvalidArgument)?;
                    let len = rest.len().min(buf.len());
                    buf[..len].copy_from_slice(&rest[..len]);
                    Ok(Source::Done(StreamChunk {
                        len,
                        eof: len == rest.len(),
                    }))
                }
                Content::Files(files) => {
                    let (segment, size) = *files
                        .get((file as usize).wrapping_sub(1))
                        .ok_or(ActionError::UnknownStream)?;
                    if offset > u64::from(size) {
                        return Err(ActionError::InvalidArgument);
                    }
                    Ok(Source::Segment(segment, size))
                }
            }
        })?;
        match source {
            Source::Done(chunk) => Ok(chunk),
            Source::Segment(segment, size) => {
                let mut inner = self.inner.lock().await;
                let storage = inner.storage.as_mut().ok_or(ActionError::UnknownStream)?;
                let wanted = buf.len().min((u64::from(size) - offset) as usize);
                let len = storage
                    .read(segment, offset as u32, &mut buf[..wanted])
                    .await
                    .map_err(|_| ActionError::Internal)?;
                Ok(StreamChunk {
                    len,
                    eof: offset + len as u64 >= u64::from(size),
                })
            }
        }
    }

    async fn close_stream(&self, stream: u32) -> Result<(), ActionError> {
        let pinning = self.with(|state| {
            state
                .streams
                .iter()
                .find(|entry| entry.id == stream)
                .map(|entry| matches!(entry.content, Content::Files(_)))
        });
        match pinning {
            None => return Err(ActionError::UnknownStream),
            Some(false) => {
                self.with(|state| state.release(|entry| entry.id == stream));
            }
            Some(true) => {
                // The storage first, then the stream is removed and its segments
                // are unpinned without waiting in between: a caller that gives up
                // while waiting leaves the stream to expire, with its pins.
                let mut inner = self.inner.lock().await;
                let pins = self.with(|state| state.release(|entry| entry.id == stream));
                Self::unpin(&mut inner, &pins);
            }
        }
        self.wake.signal(());
        Ok(())
    }
}

#[cfg(test)]
impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const READ: usize>
    Logging<M, C, B, N, READ>
{
    /// One pass of the task, for tests.
    pub(crate) async fn step(&self) {
        let mut inner = self.inner.lock().await;
        self.maintain(&mut inner).await;
    }

    pub(crate) fn pins(&self) -> usize {
        self.with(|state| {
            state
                .streams
                .iter()
                .map(|stream| match &stream.content {
                    Content::Files(files) => files.len(),
                    Content::Records(_) => 0,
                })
                .sum()
        })
    }

    pub(crate) fn open_streams(&self) -> usize {
        self.with(|state| state.streams.len())
    }

    /// How many segments the storage keeps pinned.
    pub(crate) fn pinned_segments(&self) -> usize {
        embassy_futures::block_on(async {
            let mut inner = self.inner.lock().await;
            match inner.storage.as_mut() {
                Some(storage) => storage
                    .segments()
                    .await
                    .map(|segments| segments.iter().filter(|segment| segment.pinned).count())
                    .unwrap_or(0),
                None => 0,
            }
        })
    }
}
