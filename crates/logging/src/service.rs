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
    MAX_PAGE_CHUNK, MAX_STREAM_CHUNK, Service, ServiceHealth, ServiceSummary, Status, StatusWriter,
    StreamChunk, StreamFile, StreamInfo, Unit, Value, ValueKind, WriteError,
};

use crate::segment::{Batch, MAX_CHUNK_LEN, file_name, scratch};
use crate::storage::{
    ChunkMeta, Limits, LogStorage, MAX_SEGMENTS, Recovery, SegmentId, StorageError, StorageStats,
};
use crate::tail::TailRequest;
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
    // `FIRST_SERVICE_ISSUE + 3` was `read_failed`, issued by the removed
    // `read` action. Ids are never reused.
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
    // Id 1 was `read`, removed with the persistent read path. Ids are never
    // reused.
    /// Exports the retained segment files, as a stream of files.
    pub const DOWNLOAD: u32 = 2;
    /// Removes the history of the active backend.
    pub const CLEAR: u32 = 3;
    /// Reads one cursor page of the RAM tail: no stream, nothing kept for the client.
    pub const TAIL: u32 = 4;
}

/// Field ids of the arguments of the `tail` action.
pub mod tail_field {
    /// The session of an earlier page, 0 to start a new one. Must fit a `u32`.
    pub const SESSION: u32 = 1;
    /// The cursor of an earlier page, 0 for the oldest line held.
    pub const AFTER: u32 = 2;
}

const TAIL_ARGS: &[FieldDescriptor] = &[
    FieldDescriptor::new(tail_field::SESSION, "session", ValueKind::U64),
    FieldDescriptor::new(tail_field::AFTER, "after", ValueKind::U64),
];

// The registry has checked that both arguments are present once and are `u64`;
// a session that does not fit `u32` is refused, never narrowed.
fn tail_request(args: &[(u32, Value)]) -> Result<TailRequest, ActionError> {
    let mut session = None;
    let mut after = None;
    for (id, value) in args {
        match (*id, value) {
            (tail_field::SESSION, Value::U64(value)) => session = Some(*value),
            (tail_field::AFTER, Value::U64(value)) => after = Some(*value),
            _ => {}
        }
    }
    let (Some(session), Some(after)) = (session, after) else {
        return Err(ActionError::InvalidArgument);
    };
    let session = u32::try_from(session).map_err(|_| {
        debug_action("Tail rejected: session out of range", "session", &session);
        ActionError::InvalidArgument
    })?;
    Ok(TailRequest { session, after })
}

/// How long an idle stream is kept before its segments are released.
pub const STREAM_TIMEOUT_MS: u64 = 60_000;
/// Records were dropped "recently" when the last drop is younger than this.
pub const OVERFLOW_WINDOW_MS: u64 = 30_000;
// Streams that pin segments of the archive: `download`.
const MAX_STREAMS: usize = 2;
const RETRY_MS: u64 = 10_000;

// The reports are kept out of line: they run once in a while, and inlined into
// the task they would only grow its frame.
#[cold]
#[inline(never)]
fn warn_storage(what: &str, error: &dyn core::fmt::Display) {
    log::warn!(target: "logging", "Cannot {what}: {error}");
}

#[cold]
#[inline(never)]
fn notice_formatted() {
    log::info!(target: "logging", "No valid format header, the region was formatted");
}

#[cold]
#[inline(never)]
fn notice_started() {
    log::info!(target: "logging", "Task started");
}

#[cold]
#[inline(never)]
fn notice_applied(config: &LoggingConfig) {
    log::info!(
        target: "logging",
        "Config applied (level={} storage={} compression={} retention_b={} segment_b={} flush_ms={})",
        config.level.name(),
        config.storage.name(),
        config.compression().name(),
        config.retention_bytes,
        config.segment_bytes,
        config.flush_interval_ms,
    );
}

#[cold]
#[inline(never)]
fn warn_rejected_config(error: &dyn core::fmt::Debug) {
    log::warn!(target: "logging", "Config rejected (err={error:?})");
}

#[cold]
#[inline(never)]
fn notice_switching(from: LogStorageKind, to: LogStorageKind) {
    log::info!(
        target: "logging",
        "Storage switching (from={} to={})",
        from.name(),
        to.name()
    );
}

// The first failure is a warning; the retries every few seconds stay out of
// the persisted levels.
#[cold]
#[inline(never)]
fn warn_unavailable(kind: LogStorageKind, first: bool) {
    if first {
        log::warn!(
            target: "logging",
            "Storage backend unavailable (kind={} retry_ms={})",
            kind.name(),
            RETRY_MS
        );
    } else {
        log::trace!(
            target: "logging",
            "Storage backend still unavailable (kind={})",
            kind.name()
        );
    }
}

#[cold]
#[inline(never)]
fn notice_opened(kind: LogStorageKind, recovery: &Recovery) {
    log::info!(
        target: "logging",
        "Storage opened (kind={} segments={} dropped={} corrupted={})",
        kind.name(),
        recovery.segments,
        recovery.dropped,
        recovery.corrupted,
    );
    if recovery.dropped > 0 {
        log::warn!(
            target: "logging",
            "Recovery dropped damaged segments (count={} corrupted={})",
            recovery.dropped,
            recovery.corrupted,
        );
    }
}

#[cold]
#[inline(never)]
fn notice_recovered(recovery: &Recovery) {
    log::info!(
        target: "logging",
        "Storage recovered after a failed write (segments={} dropped={})",
        recovery.segments,
        recovery.dropped,
    );
}

#[cold]
#[inline(never)]
fn warn_write_failed(what: &str, error: Option<&dyn core::fmt::Display>, lost: u64, retry_ms: u64) {
    match error {
        Some(error) => log::warn!(
            target: "logging",
            "Write failed at {what}: {error} (lost={lost} retry_ms={retry_ms})"
        ),
        None => log::warn!(
            target: "logging",
            "Write failed at {what} (lost={lost} retry_ms={retry_ms})"
        ),
    }
}

#[cold]
#[inline(never)]
fn trace_stalled(wait_ms: u64) {
    log::trace!(
        target: "logging",
        "Oldest segment pinned by a download, write retried (delay_ms={wait_ms})"
    );
}

#[cold]
#[inline(never)]
fn debug_released(streams: usize, pins: usize) {
    log::debug!(target: "logging", "Open streams released (streams={streams} pins={pins})");
}

#[cold]
#[inline(never)]
fn debug_action(what: &str, key: &str, detail: &dyn core::fmt::Debug) {
    log::debug!(target: "logging", "{what} ({key}={detail:?})");
}

#[cold]
#[inline(never)]
fn warn_action(what: &str, key: &str, detail: &dyn core::fmt::Debug) {
    log::warn!(target: "logging", "{what} ({key}={detail:?})");
}

#[cold]
#[inline(never)]
// Trace, not debug: a client that follows the tail polls every second, and a
// record per poll would feed the tail it reads.
fn trace_tail(session: u32, after: u64, len: usize, reset: bool, gap: u64) {
    log::trace!(
        target: "logging",
        "Tail served (session={session} after={after} size_b={len} reset={reset} gap={gap})"
    );
}

#[cold]
#[inline(never)]
fn debug_download(segments: usize, stream: u32) {
    log::debug!(
        target: "logging",
        "Segments pinned for a download (count={segments} stream={stream})"
    );
}

#[cold]
#[inline(never)]
fn notice_cleared() {
    log::info!(target: "logging", "History cleared");
}

#[cold]
#[inline(never)]
fn notice_boot_records(count: usize) {
    log::info!(target: "logging", "Records of the previous run written (count={count})");
}

#[cold]
#[inline(never)]
fn warn_boot_records_too_large() {
    log::warn!(target: "logging", "Records of the previous run too large for one batch, not written");
}
const PINNED_RETRY_MS: u64 = 1_000;
const SWEEP_MS: u64 = 10_000;
const FLUSH_BYTES: usize = 1_536;

// The shortest page the tail serves: the longest line without a message.
const MIN_TAIL_CHUNK: usize = crate::segment::text::MIN_WITHIN_LEN;
const _: () = assert!(MIN_TAIL_CHUNK < MAX_PAGE_CHUNK);

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

    /// The `index`-th record the platform brings from the previous run, from 0
    /// up to the first `None`: for example the panic that reset the device.
    ///
    /// The task asks right after the storage opens and writes the records
    /// **before** anything of this run, in order, at once, without the queue and
    /// whatever the level of the section is. They keep the stamps they carry.
    /// The default has none.
    ///
    /// It is called again, with the same answers, until
    /// [`boot_records_written`](Self::boot_records_written) says they are in
    /// the storage, so a record must stay available until then. It runs inside
    /// the logging task: keep it cheap and do not block. A platform that
    /// returns more records than one batch holds (a few lines of at most
    /// [`MAX_MESSAGE_LEN`](crate::MAX_MESSAGE_LEN) bytes) gets none written.
    fn boot_record(&mut self, _index: usize) -> Option<LogRecord> {
        None
    }

    /// Every [`boot_record`](Self::boot_record) is in the storage: the platform
    /// may release them. Not called when the write failed; the records are then
    /// offered again after the storage recovers, so a failure that came after
    /// the data was written can duplicate them.
    fn boot_records_written(&mut self) {}
}

// A download: the segments it pins, with their sizes.
struct Open {
    id: u32,
    files: Vec<(SegmentId, u32), MAX_SEGMENTS>,
    deadline_ms: u64,
}

struct State {
    config: LoggingConfig,
    started: bool,
    unavailable_since: Option<u64>,
    corrupt_since: Option<u64>,
    write_failed_since: Option<u64>,
    stats: StorageStats,
    active: Option<LogStorageKind>,
    lost: u64,
    streams: Vec<Open, MAX_STREAMS>,
    next_stream: u32,
    release_all: bool,
}

// The segments the released streams pinned: at most `MAX_SEGMENTS` for each of
// `MAX_STREAMS` streams, so it never overflows.
type Pins = Vec<SegmentId, { MAX_STREAMS * MAX_SEGMENTS }>;

impl State {
    // Removes the streams `selected` accepts, in place, and returns what they
    // pinned.
    fn release(&mut self, mut selected: impl FnMut(&Open) -> bool) -> Pins {
        let mut pins = Pins::new();
        self.streams.retain_mut(|stream| {
            if !selected(stream) {
                return true;
            }
            for (id, _) in &stream.files {
                // Cannot be full, see `Pins`.
                let _ = pins.push(*id);
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
    boot_done: bool,
}

/// The logging runtime: the queue, the storage, the status and the streams.
///
/// One `Logging` lives in a `static`. The `log` backend pushes into
/// [`sink`](Self::sink); the logging task runs [`run`](Self::run); the control
/// plane reaches it through [`service`](Self::service); the configuration owner
/// hands every committed `logging` section to [`apply`](Self::apply).
///
/// `M` is the [`RawMutex`] of the firmware, `C` the [`LogClock`], `B` the
/// [`Backends`], `N` the capacity of the record queue and `TAIL` the records of
/// the RAM tail that `tail` serves.
pub struct Logging<M: RawMutex, C, B: Backends, const N: usize, const TAIL: usize> {
    sink: LogSink<M, C, N, TAIL>,
    state: BlockingMutex<M, RefCell<State>>,
    inner: Mutex<M, Inner<B>>,
    wake: Signal<M, ()>,
}

impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const TAIL: usize>
    Logging<M, C, B, N, TAIL>
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
                boot_done: false,
            }),
            wake: Signal::new(),
        }
    }

    /// The producer side: register it as the `log` backend, or push to it.
    pub fn sink(&self) -> &LogSink<M, C, N, TAIL> {
        &self.sink
    }

    /// Identifies this boot to a client of the tail: pass a token that differs
    /// on every boot and does not depend on storage that can fail (the boot
    /// counter of the `system` service can; see
    /// [`set_tail_session`](LogSink::set_tail_session)). Call it before the
    /// first request; 0 is stored as 1.
    pub fn set_session(&self, session: u32) {
        self.sink.set_tail_session(session);
    }

    /// The service for the status and action registries.
    pub fn service(&self) -> LoggingService<'_, M, C, B, N, TAIL> {
        LoggingService(self)
    }

    /// Applies a committed `logging` section: the level takes effect for the
    /// next log call, a new backend is opened by the task, and new segments
    /// use the new codec and sizes. Synchronous and cheap.
    pub fn apply(&self, config: LoggingConfig) {
        if let Err(error) = config.validate() {
            warn_rejected_config(&error);
            return;
        }
        self.sink.set_level(config.level);
        self.state.lock(|state| state.borrow_mut().config = config);
        notice_applied(&config);
        self.wake.signal(());
    }

    // Writes the records the platform brought from the previous run, ahead of
    // the queue, so that the history follows the order of the events. Called by
    // the task right after the storage opens, with the batch empty; the
    // platform is told only when the storage has all of them, so what it keeps
    // until then survives a failed write.
    async fn write_boot_records(&self, inner: &mut Inner<B>, now: u64) {
        let ready = inner.storage.is_some()
            && !inner.faulted
            && !inner.boot_done
            && inner.batch.is_empty()
            && inner.carry.is_none()
            && now >= inner.stalled_until_ms;
        if !ready {
            return;
        }
        let first = inner.next_sequence;
        inner.batch_since_ms = now;
        let mut index = 0;
        while let Some(mut record) = inner.backends.boot_record(index) {
            record.sequence = inner.next_sequence;
            if inner.batch.push(&record).is_err() {
                // More than a batch holds: keep what the platform has, write nothing.
                inner.batch.clear();
                inner.next_sequence = first;
                inner.boot_done = true;
                warn_boot_records_too_large();
                return;
            }
            inner.next_sequence += 1;
            index += 1;
        }
        if index == 0 {
            inner.boot_done = true;
            return;
        }
        self.flush(inner, now).await;
        if inner.faulted {
            // The records are lost for this run; the platform still has them.
            inner.next_sequence = first;
            return;
        }
        if !inner.batch.is_empty() {
            // The flush waited for a download: take the records back and try again.
            inner.batch.clear();
            inner.next_sequence = first;
            return;
        }
        inner.boot_done = true;
        inner.backends.boot_records_written();
        notice_boot_records(index);
    }

    fn now(&self) -> u64 {
        self.sink.clock().monotonic_ms()
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
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
            if let Some(kind) = inner.kind {
                notice_switching(kind, wanted);
            }
            while self.drain(inner, now) {
                self.flush(inner, now).await;
            }
            self.flush(inner, now).await;
            if let Some(storage) = inner.storage.as_mut()
                && let Err(error) = storage.seal().await
            {
                warn_storage("seal before the switch", &error);
            }
            inner.storage = None;
            inner.kind = None;
            inner.boot_done = false;
            inner.open_codec = None;
            inner.faulted = false;
            self.release_streams(inner, true);
            self.with(|state| {
                state.active = None;
                state.stats = StorageStats::default();
                state.write_failed_since = None;
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
            let first = self.with(|state| {
                state.started = true;
                let first = state.unavailable_since.is_none();
                state.unavailable_since.get_or_insert(now);
                first
            });
            warn_unavailable(kind, first);
            return;
        };
        match storage.recover().await {
            Ok(recovery) => {
                if recovery.formatted {
                    notice_formatted();
                }
                notice_opened(kind, &recovery);
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
                if self.with(|state| state.unavailable_since.is_none()) {
                    warn_storage("open", &error);
                }
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
                notice_recovered(&recovery);
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
        let (pins, released) = self.with(|state| {
            let all = all || core::mem::take(&mut state.release_all);
            let before = state.streams.len();
            let pins = state.release(|stream| all || stream.deadline_ms <= now);
            (pins, before - state.streams.len())
        });
        if released > 0 {
            debug_released(released, pins.len());
        }
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
        let mut scratch = scratch();
        for attempt in 0..2 {
            let codec = inner.open_codec.unwrap_or(config.compression());
            let Some(chunk) = inner.batch.encode(codec, &mut scratch).ok() else {
                break;
            };
            if inner.open_codec.is_none() || storage.open_capacity() < chunk.len() {
                if attempt == 1 && inner.open_codec.is_some() {
                    break;
                }
                let begun = match storage.seal().await {
                    Ok(()) => storage.begin_segment(config.compression(), limits).await,
                    Err(error) => Err(error),
                };
                inner.open_codec = None;
                match begun {
                    Ok(_) => inner.open_codec = Some(config.compression()),
                    Err(StorageError::Pinned) => {
                        // A download holds the oldest segment: wait, the batch stays.
                        inner.stalled_until_ms = now + PINNED_RETRY_MS;
                        trace_stalled(PINNED_RETRY_MS);
                        return;
                    }
                    Err(error) => {
                        return self.write_failed(inner, now, "begin segment", Some(&error));
                    }
                }
                continue;
            }
            let meta = ChunkMeta {
                records: inner.batch.records(),
                first_sequence: inner.batch.first_sequence(),
                last_sequence: inner.batch.last_sequence(),
                raw_len: inner.batch.len() as u32,
            };
            match storage.append(chunk, meta).await {
                Ok(()) => {
                    inner.batch.clear();
                    if storage.open_capacity() < MAX_CHUNK_LEN {
                        // Sealed as soon as it is full, so it can be downloaded.
                        if let Err(error) = storage.seal().await {
                            return self.write_failed(
                                inner,
                                now,
                                "seal full segment",
                                Some(&error),
                            );
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
                Err(error) => return self.write_failed(inner, now, "append", Some(&error)),
            }
        }
        self.write_failed(inner, now, "encode or rotate", None);
    }

    fn write_failed(
        &self,
        inner: &mut Inner<B>,
        now: u64,
        what: &str,
        error: Option<&dyn core::fmt::Display>,
    ) {
        let lost = u64::from(inner.batch.records());
        inner.batch.clear();
        inner.faulted = true;
        inner.retry_at_ms = now + RETRY_MS;
        warn_write_failed(what, error, lost, RETRY_MS);
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
        self.write_boot_records(inner, now).await;
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
        notice_started();
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
pub struct LoggingService<'a, M: RawMutex, C, B: Backends, const N: usize, const TAIL: usize>(
    &'a Logging<M, C, B, N, TAIL>,
);

impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const TAIL: usize> Status
    for LoggingService<'_, M, C, B, N, TAIL>
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
                state.corrupt_since.is_some(),
            )
        });
        if !started {
            ServiceSummary::new(ServiceHealth::Unknown)
        } else if unavailable {
            ServiceSummary::new(ServiceHealth::Error)
        } else if write_failed || degraded {
            ServiceSummary::new(ServiceHealth::Degraded)
        } else {
            ServiceSummary::new(ServiceHealth::Ok)
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
        let (unavailable, write, corrupt) = logging.with(|state| {
            (
                state.unavailable_since,
                state.write_failed_since,
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
        out.text(status_field::COMPRESSION, config.compression().name())?;
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

impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const TAIL: usize> Action
    for LoggingService<'_, M, C, B, N, TAIL>
{
    const ACTIONS: &'static [ActionDescriptor] = &[
        ActionDescriptor::new(action::DOWNLOAD, "download", ActionAccess::Read).stream(),
        ActionDescriptor::new(action::CLEAR, "clear", ActionAccess::Mutate),
        ActionDescriptor::new(action::TAIL, "tail", ActionAccess::Read)
            .args(TAIL_ARGS)
            .page(),
    ];

    async fn invoke(
        &self,
        action: u32,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        let logging = self.0;
        match action {
            action::TAIL => logging.serve_tail(args, out),
            action::DOWNLOAD => {
                let info = logging.download().await?;
                out.stream(info).map_err(|_| ActionError::Internal)
            }
            action::CLEAR => logging.clear().await,
            _ => {
                warn_action("Unknown action", "id", &action);
                Err(ActionError::UnknownAction)
            }
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
        log::debug!(target: "logging", "Link lost, open streams are released");
        self.0.with(|state| state.release_all = true);
        self.0.wake.signal(());
    }
}

impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const TAIL: usize>
    Logging<M, C, B, N, TAIL>
{
    // A download holds at most `MAX_STREAMS` streams and is refused with `Busy`
    // beyond that.
    fn register_download(
        &self,
        files: Vec<(SegmentId, u32), MAX_SEGMENTS>,
    ) -> Result<u32, ActionError> {
        let deadline_ms = self.now() + STREAM_TIMEOUT_MS;
        self.with(|state| {
            let id = state.next_stream;
            state
                .streams
                .push(Open {
                    id,
                    files,
                    deadline_ms,
                })
                .map_err(|_| {
                    debug_action("Stream table is full", "max", &MAX_STREAMS);
                    ActionError::Busy
                })?;
            state.next_stream = state.next_stream.checked_add(1).unwrap_or(1);
            Ok(id)
        })
    }

    // The tail reads the RAM ring and nothing else: no storage, no commit, no
    // lock shared with the logging task, so it answers while the archive is
    // busy, faulted or pinned by a download, and a clear of the archive does
    // not touch it. The page buffer lives in this frame only, apart from the
    // future of `invoke`, which awaits the archive.
    #[inline(never)]
    fn serve_tail(
        &self,
        args: &[(u32, Value)],
        out: &mut impl ActionOutput,
    ) -> Result<(), ActionError> {
        let request = tail_request(args)?;
        let mut buf = [0u8; MAX_PAGE_CHUNK];
        let page = self.sink.tail_page(request, &mut buf);
        trace_tail(
            request.session,
            request.after,
            page.len,
            page.reset,
            page.gap,
        );
        let data = buf.get(..page.len).ok_or(ActionError::Internal)?;
        out.page(page.info(), data)
            .map_err(|_| ActionError::Internal)
    }

    async fn download(&self) -> Result<StreamInfo, ActionError> {
        let mut inner = self.inner.lock().await;
        self.commit(&mut inner).await;
        inner.open_codec = None;
        let Some(storage) = inner.storage.as_mut() else {
            debug_action(
                "Download rejected: no storage",
                "err",
                &ActionError::Unavailable,
            );
            return Err(ActionError::Unavailable);
        };
        if self.with(|state| state.streams.is_full()) {
            debug_action(
                "Download rejected: stream table is full",
                "max",
                &MAX_STREAMS,
            );
            return Err(ActionError::Busy);
        }
        // From now on new records go to a new segment; this download holds the
        // ones that exist.
        storage.seal().await.map_err(|error| {
            warn_storage("seal for download", &error);
            ActionError::Internal
        })?;
        let segments = storage.segments().await.map_err(|error| {
            warn_storage("list segments for download", &error);
            ActionError::Internal
        })?;
        let mut pinned = Vec::<(SegmentId, u32), MAX_SEGMENTS>::new();
        for info in &segments {
            if let Err(error) = storage.pin(info.id).await {
                for (id, _) in &pinned {
                    storage.unpin(*id);
                }
                warn_storage("pin segment for download", &error);
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
        match self.register_download(pinned) {
            Ok(id) => {
                debug_download(files.len(), id);
                Ok(StreamInfo { id, files })
            }
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
            debug_action(
                "Clear rejected: no storage",
                "err",
                &ActionError::Unavailable,
            );
            return Err(ActionError::Unavailable);
        }
        // A download holds the segments it was given, so it is not cut short: a
        // stream that expired, or whose link was lost, is released first, and one
        // that is still open makes the clear wait (`Busy`) before anything is
        // changed. A download registers its stream under the same lock, so one
        // cannot start between this check and the erase. The RAM tail is not part
        // of the archive and is not touched.
        self.release_streams(&mut inner, false);
        if self.with(|state| !state.streams.is_empty()) {
            debug_action("Clear rejected: a download is open", "max", &MAX_STREAMS);
            return Err(ActionError::Busy);
        }
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
        });
        notice_cleared();
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
        let source = self.with(|state| {
            if let Some(entry) = state
                .streams
                .iter_mut()
                .find(|entry| entry.id == stream && entry.deadline_ms > now)
            {
                entry.deadline_ms = now + STREAM_TIMEOUT_MS;
                let (segment, size) = *entry
                    .files
                    .get((file as usize).wrapping_sub(1))
                    .ok_or(ActionError::UnknownStream)?;
                if offset > u64::from(size) {
                    return Err(ActionError::InvalidArgument);
                }
                return Ok((segment, size));
            }
            Err(ActionError::UnknownStream)
        });
        let (segment, size) = match source {
            Ok(source) => source,
            Err(error) => {
                debug_action("Stream read rejected", "err", &error);
                return Err(error);
            }
        };
        let mut inner = self.inner.lock().await;
        let Some(storage) = inner.storage.as_mut() else {
            debug_action("Stream read rejected: no storage", "stream", &stream);
            return Err(ActionError::UnknownStream);
        };
        let wanted = buf.len().min((u64::from(size) - offset) as usize);
        let len = storage
            .read(segment, offset as u32, &mut buf[..wanted])
            .await
            .map_err(|error| {
                warn_storage("read stream segment", &error);
                ActionError::Internal
            })?;
        Ok(StreamChunk {
            len,
            eof: offset + len as u64 >= u64::from(size),
        })
    }

    async fn close_stream(&self, stream: u32) -> Result<(), ActionError> {
        if !self.with(|state| state.streams.iter().any(|entry| entry.id == stream)) {
            debug_action("Stream close rejected: unknown stream", "stream", &stream);
            return Err(ActionError::UnknownStream);
        }
        // The storage first, then the stream is removed and its segments
        // are unpinned without waiting in between: a caller that gives up
        // while waiting leaves the stream to expire, with its pins.
        let mut inner = self.inner.lock().await;
        let pins = self.with(|state| state.release(|entry| entry.id == stream));
        Self::unpin(&mut inner, &pins);
        debug_action("Stream closed", "stream", &stream);
        self.wake.signal(());
        Ok(())
    }
}

#[cfg(test)]
impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const TAIL: usize>
    Logging<M, C, B, N, TAIL>
{
    /// One pass of the task, for tests.
    pub(crate) async fn step(&self) {
        let mut inner = self.inner.lock().await;
        self.maintain(&mut inner).await;
    }

    pub(crate) fn pins(&self) -> usize {
        self.with(|state| state.streams.iter().map(|stream| stream.files.len()).sum())
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

#[cfg(test)]
impl<M: RawMutex, C: LogClock, B: Backends, const N: usize, const TAIL: usize>
    Logging<M, C, B, N, TAIL>
{
    /// The archived records above `after`, decoded from the segments, at most
    /// `limit` of them; `None` when a segment does not decode. What the tests
    /// compare the archive with, now that no action reads it.
    pub(crate) fn archive(&self, after: u64, limit: u64) -> Option<std::vec::Vec<LogRecord>> {
        use crate::segment::SegmentDecoder;
        embassy_futures::block_on(async {
            let mut inner = self.inner.lock().await;
            self.commit(&mut inner).await;
            self.refresh(&mut inner).await;
            let storage = inner.storage.as_mut()?;
            let segments = storage.segments().await.ok()?;
            let mut records = std::vec::Vec::new();
            for info in segments.iter().filter(|info| info.last_sequence > after) {
                let mut decoder = SegmentDecoder::new(info.codec);
                let mut offset = 0u32;
                let mut piece = [0u8; 256];
                loop {
                    let count = storage.read(info.id, offset, &mut piece).await.ok()?;
                    if count == 0 {
                        break;
                    }
                    offset += count as u32;
                    decoder
                        .push(&piece[..count], &mut |record: LogRecord| {
                            if record.sequence > after && (records.len() as u64) < limit {
                                records.push(record);
                            }
                        })
                        .ok()?;
                }
                if !decoder.is_complete() {
                    return None;
                }
            }
            Some(records)
        })
    }
}
