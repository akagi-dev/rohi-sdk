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

extern crate std;

use core::cell::{Cell, RefCell};
use std::borrow::ToOwned;
use std::rc::Rc;
use std::vec::Vec;

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use rohi_service::{
    Action, ActionAccess, ActionError, ActionRegistry, ActionResult, MAX_STREAM_CHUNK, Service,
    ServiceHealth, ServiceState, Status, StatusRegistry, StatusWriter, Statuses, Value, WriteError,
};
use rohi_time::UnixTime;

use crate::storage::{LogStorage, MAX_SEGMENTS, Medium, RamMedium, RingStorage};
use crate::*;

const SECTORS: usize = 16;

#[derive(Clone)]
struct Shared(Rc<RefCell<RamMedium<SECTORS>>>, Rc<Cell<u32>>);

impl Shared {
    /// Medium operations of any kind since the start.
    fn operations(&self) -> u32 {
        self.1.get()
    }

    fn new() -> Self {
        let mut medium = RamMedium::new();
        medium.erase(0, SECTORS as u32).unwrap();
        let shared = Self(Rc::new(RefCell::new(medium)), Rc::default());
        // A formatted medium, as a device has after its first start.
        block_on(RingStorage::new(shared.clone()).recover()).unwrap();
        shared
    }
}

impl Medium for Shared {
    type Error = crate::storage::RamError;

    fn sectors(&self) -> u32 {
        SECTORS as u32
    }

    fn erase(&mut self, first: u32, count: u32) -> Result<(), Self::Error> {
        self.1.set(self.1.get() + 1);
        self.0.borrow_mut().erase(first, count)
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        self.1.set(self.1.get() + 1);
        self.0.borrow_mut().write(offset, data)
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.1.set(self.1.get() + 1);
        self.0.borrow_mut().read(offset, buf)
    }
}

struct Clock(Cell<u64>);

impl LogClock for Clock {
    fn monotonic_ms(&self) -> u64 {
        self.0.get()
    }

    fn wall_time(&self) -> Option<UnixTime> {
        None
    }
}

struct Test {
    internal: Shared,
    sd: Shared,
    sd_present: Rc<Cell<bool>>,
    opened: Rc<RefCell<Vec<LogStorageKind>>>,
    // What the platform brings from the previous run, and how often it was told
    // that the storage has it.
    boot: Rc<RefCell<Vec<LogRecord>>>,
    boot_written: Rc<Cell<u32>>,
}

impl Backends for Test {
    type Storage = RingStorage<Shared>;

    async fn open(&mut self, kind: LogStorageKind) -> Option<Self::Storage> {
        self.opened.borrow_mut().push(kind);
        match kind {
            LogStorageKind::Internal => Some(RingStorage::new(self.internal.clone())),
            LogStorageKind::Sd if self.sd_present.get() => Some(RingStorage::new(self.sd.clone())),
            LogStorageKind::Sd => None,
        }
    }

    fn boot_record(&mut self, index: usize) -> Option<LogRecord> {
        self.boot.borrow().get(index).cloned()
    }

    fn boot_records_written(&mut self) {
        self.boot_written.set(self.boot_written.get() + 1);
        self.boot.borrow_mut().clear();
    }
}

const CONFIG: LoggingConfig = LoggingConfig {
    level: LogLevel::Info,
    storage: LogStorageKind::Internal,
    compression: Compression::Lz4,
    retention_bytes: 4096 * 6,
    segment_bytes: 4096,
    flush_interval_ms: 1_000,
};

type TestLogging<'a> = Logging<NoopRawMutex, &'a Clock, Test, 64, 8>;

struct Rig {
    clock: Clock,
    internal: Shared,
    sd: Shared,
    sd_present: Rc<Cell<bool>>,
    opened: Rc<RefCell<Vec<LogStorageKind>>>,
    boot: Rc<RefCell<Vec<LogRecord>>>,
    boot_written: Rc<Cell<u32>>,
}

impl Rig {
    fn new() -> Self {
        Self {
            clock: Clock(Cell::new(10_000)),
            internal: Shared::new(),
            sd: Shared::new(),
            sd_present: Rc::new(Cell::new(false)),
            opened: Rc::default(),
            boot: Rc::default(),
            boot_written: Rc::default(),
        }
    }

    fn logging(&self, config: LoggingConfig) -> TestLogging<'_> {
        Logging::new(
            &self.clock,
            Test {
                internal: self.internal.clone(),
                sd: self.sd.clone(),
                sd_present: self.sd_present.clone(),
                opened: self.opened.clone(),
                boot: self.boot.clone(),
                boot_written: self.boot_written.clone(),
            },
            config,
        )
    }

    fn advance(&self, ms: u64) {
        self.clock.0.set(self.clock.0.get() + ms);
    }
}

fn log(logging: &TestLogging<'_>, message: &str) -> bool {
    logging
        .sink()
        .push(log::Level::Info, "test", format_args!("{message}"))
}

fn log_many(logging: &TestLogging<'_>, count: u64, first: u64) {
    for n in first..first + count {
        log(
            logging,
            &std::format!("record {n} of the stress run with some text"),
        );
    }
}

fn step(rig: &Rig, logging: &TestLogging<'_>) {
    rig.advance(1_500);
    block_on(logging.step());
}

/// Moves records through the task until the queue is empty and written.
fn settle(rig: &Rig, logging: &TestLogging<'_>) {
    for _ in 0..40 {
        step(rig, logging);
        if logging.sink().queued() == 0 {
            step(rig, logging);
            return;
        }
    }
    panic!("the queue does not drain");
}

fn args(session: u64, after: u64, limit: u64) -> [(u32, Value); 3] {
    [
        (tail_arg::SESSION, Value::U64(session)),
        (tail_arg::AFTER, Value::U64(after)),
        (tail_arg::LIMIT, Value::U64(limit)),
    ]
}

fn invoke(
    logging: &TestLogging<'_>,
    action: u32,
    args: &[(u32, Value)],
) -> Result<ActionResult, ActionError> {
    let service = logging.service();
    let registry = (&service,);
    block_on(registry.invoke(
        <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 8> as Service>::ID,
        action,
        args,
    ))
}

fn stream_of(result: Result<ActionResult, ActionError>) -> rohi_service::StreamInfo {
    match result.unwrap() {
        ActionResult::Stream(info) => info,
        other => panic!("{other:?}"),
    }
}

fn read_all(logging: &TestLogging<'_>, info: &rohi_service::StreamInfo, file: u32) -> Vec<u8> {
    let service = logging.service();
    let mut data = Vec::new();
    loop {
        let mut chunk = [0u8; MAX_STREAM_CHUNK];
        let got =
            block_on(service.read_stream(info.id, file, data.len() as u64, &mut chunk)).unwrap();
        data.extend_from_slice(&chunk[..got.len]);
        if got.eof {
            return data;
        }
    }
}

fn parse(text: &[u8]) -> Vec<LogRecord> {
    std::str::from_utf8(text)
        .unwrap()
        .lines()
        .map(|line| segment::text::parse_line(line).unwrap())
        .collect()
}

/// The archived records after `after`, up to `limit`, decoded from the segments.
fn read(logging: &TestLogging<'_>, after: u64, limit: u64) -> Vec<LogRecord> {
    logging.archive(after, limit).expect("the archive decodes")
}

fn field(logging: &TestLogging<'_>, id: u32) -> Option<Value> {
    struct Out(Vec<(u32, Value)>);
    impl StatusWriter for Out {
        fn put(&mut self, id: u32, value: Value) -> Result<(), WriteError> {
            self.0.push((id, value));
            Ok(())
        }
    }
    let mut out = Out(Vec::new());
    block_on(logging.service().status(&mut out)).unwrap();
    out.0
        .into_iter()
        .find(|(field, _)| *field == id)
        .map(|(_, value)| value)
}

fn number(logging: &TestLogging<'_>, id: u32) -> Option<u64> {
    field(logging, id).and_then(|value| value.as_u64())
}

fn issues(logging: &TestLogging<'_>) -> Vec<u32> {
    let mut out = heapless::Vec::new();
    block_on(logging.service().issues(&mut out));
    out.iter().map(|issue| issue.code).collect()
}

fn summary(logging: &TestLogging<'_>) -> (ServiceState, ServiceHealth, bool) {
    let summary = block_on(logging.service().summary());
    (summary.state, summary.health, summary.ready)
}

#[test]
fn the_service_is_a_normal_service_with_a_valid_schema() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    let service = logging.service();
    const {
        assert!(<(&LoggingService<'static, NoopRawMutex, &'static Clock, Test, 64, 8>,) as ActionRegistry>::SCHEMAS_VALID);
        assert!(<(&LoggingService<'static, NoopRawMutex, &'static Clock, Test, 64, 8>,) as ActionRegistry>::IDS_UNIQUE);
    }
    let statuses = Statuses::<NoopRawMutex, _>::new((&service,));
    assert_eq!(statuses.service_count(), 1);
    assert_eq!(
        <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 8> as Service>::NAME,
        "logging"
    );
    let names: Vec<_> = <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 8> as Action>::ACTIONS
        .iter()
        .map(|action| (action.name, action.access, action.stream))
        .collect();
    assert_eq!(
        names,
        [
            ("download", ActionAccess::Read, true),
            ("clear", ActionAccess::Mutate, false),
            ("tail", ActionAccess::Read, true)
        ]
    );
}

#[test]
fn the_service_starts_then_reports_ok_and_the_space() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    assert_eq!(
        summary(&logging),
        (ServiceState::Starting, ServiceHealth::Unknown, false)
    );
    step(&rig, &logging);
    assert_eq!(
        summary(&logging),
        (ServiceState::Running, ServiceHealth::Ok, true)
    );
    assert!(issues(&logging).is_empty());
    assert_eq!(
        number(&logging, status_field::CAPACITY_BYTES),
        Some(4096 * 6)
    );
    assert_eq!(number(&logging, status_field::USED_BYTES), Some(0));
    assert_eq!(number(&logging, status_field::FREE_BYTES), Some(4096 * 6));
    assert_eq!(field(&logging, status_field::LEVEL), Some(Value::Enum(3)));
    assert_eq!(
        field(&logging, status_field::STORAGE),
        Some(Value::text("internal").unwrap())
    );
    assert_eq!(
        field(&logging, status_field::COMPRESSION),
        Some(Value::text("lz4").unwrap())
    );
    assert_eq!(number(&logging, status_field::OLDEST_SEQUENCE), None);

    log_many(&logging, 30, 1);
    settle(&rig, &logging);
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(30));
    assert_eq!(number(&logging, status_field::SEGMENTS), Some(1));
    assert_eq!(number(&logging, status_field::USED_BYTES), Some(4096));
    assert_eq!(number(&logging, status_field::FREE_BYTES), Some(4096 * 5));
    assert_eq!(number(&logging, status_field::OLDEST_SEQUENCE), Some(1));
    assert_eq!(number(&logging, status_field::NEWEST_SEQUENCE), Some(30));
    assert_eq!(number(&logging, status_field::DROPPED_RECORDS), Some(0));
}

#[test]
fn a_record_waits_for_the_flush_interval_unless_a_read_wants_it() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    log(&logging, "first");
    // Taken into the batch, not written: the interval has not passed.
    rig.advance(10);
    block_on(logging.step());
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
    assert_eq!(logging.sink().queued(), 0);
    // A read commits what was accepted.
    let records = read(&logging, 0, 10);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].message.as_str(), "first");
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(1));
}

#[test]
fn a_tail_stream_is_retryable_by_offset_and_ends() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 10, 1);
    let info = stream_of(invoke(&logging, action::TAIL, &args(0, 0, 10)));
    let service = logging.service();
    let mut first = [0u8; 64];
    let mut again = [0u8; 64];
    let a = block_on(service.read_stream(info.id, TAIL_FILE_ID, 20, &mut first)).unwrap();
    let b = block_on(service.read_stream(info.id, TAIL_FILE_ID, 20, &mut again)).unwrap();
    assert_eq!((a.len, a.eof), (b.len, b.eof));
    assert_eq!(first[..a.len], again[..b.len]);
    let size = info.files[0].size_bytes;
    let end = block_on(service.read_stream(info.id, TAIL_FILE_ID, size, &mut first)).unwrap();
    assert_eq!((end.len, end.eof), (0, true));
    assert_eq!(
        block_on(service.read_stream(info.id, TAIL_FILE_ID, size + 1, &mut first)),
        Err(ActionError::InvalidArgument)
    );
    assert_eq!(
        block_on(service.read_stream(info.id, 9, 0, &mut first)),
        Err(ActionError::UnknownStream)
    );
    assert_eq!(
        block_on(service.read_stream(info.id + 7, TAIL_FILE_ID, 0, &mut first)),
        Err(ActionError::UnknownStream)
    );
    block_on(service.close_stream(info.id)).unwrap();
    assert_eq!(
        block_on(service.close_stream(info.id)),
        Err(ActionError::UnknownStream)
    );
}

#[test]
fn the_level_applies_at_runtime() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    assert!(
        !logging
            .sink()
            .push(log::Level::Debug, "t", format_args!("quiet"))
    );
    logging.apply(LoggingConfig {
        level: LogLevel::Debug,
        ..CONFIG
    });
    assert!(
        logging
            .sink()
            .push(log::Level::Debug, "t", format_args!("loud"))
    );
    logging.apply(LoggingConfig {
        level: LogLevel::Error,
        ..CONFIG
    });
    assert!(
        !logging
            .sink()
            .push(log::Level::Warn, "t", format_args!("quiet"))
    );
    step(&rig, &logging);
    assert_eq!(field(&logging, status_field::LEVEL), Some(Value::Enum(1)));
    // An invalid section is not applied.
    logging.apply(LoggingConfig {
        segment_bytes: 5,
        ..CONFIG
    });
    assert_eq!(field(&logging, status_field::LEVEL), Some(Value::Enum(1)));
}

#[test]
fn a_full_queue_drops_new_records_and_says_so() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    for n in 0..70 {
        log(&logging, &std::format!("burst {n}"));
    }
    assert_eq!(logging.sink().dropped(), 6);
    assert_eq!(issues(&logging), [issue::QUEUE_OVERFLOW]);
    assert_eq!(number(&logging, status_field::DROPPED_RECORDS), Some(6));
    // The queue drains into the batch and the history has the oldest 64.
    let records = read(&logging, 0, 100);
    assert_eq!(records.len(), 64);
    assert_eq!(records[0].message.as_str(), "burst 0");
    // The issue goes away when the drops are old.
    rig.advance(OVERFLOW_WINDOW_MS + 1);
    assert!(issues(&logging).is_empty());
    assert_eq!(number(&logging, status_field::DROPPED_RECORDS), Some(6));
}

#[test]
fn segments_rotate_and_retention_drops_the_oldest() {
    let rig = Rig::new();
    let logging = rig.logging(LoggingConfig {
        compression: Compression::None,
        ..CONFIG
    });
    let mut next = 1;
    for _ in 0..60 {
        log_many(&logging, 6, next);
        next += 6;
        settle(&rig, &logging);
    }
    let stats = |id| number(&logging, id).unwrap();
    assert!(stats(status_field::SEGMENTS) <= 6);
    assert!(stats(status_field::USED_BYTES) <= stats(status_field::CAPACITY_BYTES));
    assert_eq!(stats(status_field::NEWEST_SEQUENCE), 360);
    assert!(stats(status_field::OLDEST_SEQUENCE) > 1);
    let records = read(&logging, stats(status_field::OLDEST_SEQUENCE) - 1, 200);
    assert_eq!(
        records.first().unwrap().sequence,
        stats(status_field::OLDEST_SEQUENCE)
    );
    for pair in records.windows(2) {
        assert_eq!(pair[1].sequence, pair[0].sequence + 1);
    }
}

#[test]
fn the_history_and_the_sequence_survive_a_restart() {
    let rig = Rig::new();
    {
        let logging = rig.logging(CONFIG);
        log_many(&logging, 7, 1);
        settle(&rig, &logging);
        assert_eq!(number(&logging, status_field::NEWEST_SEQUENCE), Some(7));
    }
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(7));
    log_many(&logging, 3, 100);
    settle(&rig, &logging);
    let records = read(&logging, 0, 50);
    assert_eq!(records.len(), 10);
    assert_eq!(records.last().unwrap().sequence, 10);
    assert_eq!(rig.opened.borrow().len(), 2);
}

#[test]
fn download_seals_pins_and_lists_the_files() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    let mut next = 1;
    for _ in 0..30 {
        log_many(&logging, 6, next);
        next += 6;
        settle(&rig, &logging);
    }
    let info = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let names: Vec<_> = info
        .files
        .iter()
        .map(|file| file.name.as_str().to_owned())
        .collect();
    assert!(names.len() >= 2);
    for (index, name) in names.iter().enumerate() {
        assert_eq!(name, &std::format!("rohi{}.log.lz4", index + 1));
        assert_eq!(info.files[index].id, index as u32 + 1);
    }
    assert_eq!(logging.pins(), names.len());

    // Every file is a standard LZ4 frame of text lines; together they are the
    // whole history in order.
    let mut records = Vec::new();
    for file in &info.files {
        let data = read_all(&logging, &info, file.id);
        assert_eq!(data.len() as u64, file.size_bytes);
        let mut text = std::string::String::new();
        std::io::Read::read_to_string(
            &mut lz4_flex::frame::FrameDecoder::new(data.as_slice()),
            &mut text,
        )
        .unwrap();
        records.extend(parse(text.as_bytes()));
    }
    assert_eq!(records.first().unwrap().sequence, 1);
    assert_eq!(records.last().unwrap().sequence, 180);
    for pair in records.windows(2) {
        assert_eq!(pair[1].sequence, pair[0].sequence + 1);
    }

    // The snapshot is stable while records keep coming and segments rotate.
    let before = read_all(&logging, &info, 1);
    log_many(&logging, 8, 1_000);
    settle(&rig, &logging);
    assert_eq!(read_all(&logging, &info, 1), before);
    // New records are in a new segment, not in this download.
    let later = read(&logging, 180, 50);
    assert!(!later.is_empty());
    assert_eq!(later[0].sequence, 181);

    block_on(logging.service().close_stream(info.id)).unwrap();
    assert_eq!(logging.pins(), 0);
}

#[test]
fn a_download_keeps_its_segments_while_rotation_cannot_evict_them() {
    let rig = Rig::new();
    let logging = rig.logging(LoggingConfig {
        compression: Compression::None,
        ..CONFIG
    });
    let mut next = 1;
    for _ in 0..30 {
        log_many(&logging, 6, next);
        next += 6;
        settle(&rig, &logging);
    }
    let info = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let before = read_all(&logging, &info, 1);

    // Keep logging: the producer is never blocked, the oldest is never taken.
    let mut touch = [0u8; 8];
    for _ in 0..80 {
        log_many(&logging, 6, next);
        next += 6;
        step(&rig, &logging);
        // The client is still reading: the stream stays alive.
        block_on(logging.service().read_stream(info.id, 1, 0, &mut touch)).unwrap();
    }
    assert_eq!(read_all(&logging, &info, 1), before);
    assert!(
        logging.sink().dropped() > 0
            || number(&logging, status_field::DROPPED_RECORDS).unwrap() > 0,
        "records beyond the pinned budget are dropped, not blocked"
    );

    // Closing releases the pins and logging resumes.
    block_on(logging.service().close_stream(info.id)).unwrap();
    for _ in 0..10 {
        log_many(&logging, 6, next);
        next += 6;
        settle(&rig, &logging);
    }
    let newest = number(&logging, status_field::NEWEST_SEQUENCE).unwrap();
    assert!(newest > 180);
    assert_eq!(logging.sink().queued(), 0);
}

#[test]
fn an_idle_download_times_out_and_a_lost_link_releases_it() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 5, 1);
    settle(&rig, &logging);

    let info = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    assert_eq!(logging.pins(), 1);
    // Reads keep it alive.
    for _ in 0..4 {
        rig.advance(STREAM_TIMEOUT_MS / 2);
        let _ = read_all(&logging, &info, 1);
        block_on(logging.step());
        assert_eq!(logging.pins(), 1);
    }
    rig.advance(STREAM_TIMEOUT_MS + 1);
    block_on(logging.step());
    assert_eq!(logging.pins(), 0);
    let mut buf = [0u8; 8];
    assert_eq!(
        block_on(logging.service().read_stream(info.id, 1, 0, &mut buf)),
        Err(ActionError::UnknownStream)
    );

    // The link goes away.
    let info = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    assert_eq!(logging.pins(), 1);
    logging.service().link_lost();
    block_on(logging.step());
    assert_eq!(logging.pins(), 0);
    assert_eq!(
        block_on(logging.service().read_stream(info.id, 1, 0, &mut buf)),
        Err(ActionError::UnknownStream)
    );
    // And what was pinned can be evicted again: clear works.
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
}

#[test]
fn too_many_streams_are_busy_until_one_is_closed() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 5, 1);
    settle(&rig, &logging);
    let first = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let second = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    assert_eq!(
        invoke(&logging, action::DOWNLOAD, &[]).err(),
        Some(ActionError::Busy)
    );
    // Tail streams pin nothing and have a table of their own.
    assert!(invoke(&logging, action::TAIL, &args(0, 0, 5)).is_ok());
    block_on(logging.service().close_stream(second.id)).unwrap();
    assert!(invoke(&logging, action::DOWNLOAD, &[]).is_ok());
    block_on(logging.service().close_stream(first.id)).unwrap();
}

#[test]
fn clear_removes_the_history_of_the_active_backend_only() {
    let rig = Rig::new();
    rig.sd_present.set(true);
    let logging = rig.logging(CONFIG);
    log_many(&logging, 10, 1);
    settle(&rig, &logging);
    // Switch to the card and log there.
    logging.apply(LoggingConfig {
        storage: LogStorageKind::Sd,
        ..CONFIG
    });
    block_on(logging.step());
    log_many(&logging, 4, 1);
    settle(&rig, &logging);
    assert_eq!(read(&logging, 0, 50).len(), 4);

    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
    assert!(read(&logging, 0, 50).is_empty());

    // The internal history is untouched.
    logging.apply(CONFIG);
    block_on(logging.step());
    let internal = read(&logging, 0, 50);
    assert_eq!(internal.len(), 10);
    assert_eq!(internal[0].sequence, 1);

    // The sequence never goes back, also after a clear.
    logging.apply(LoggingConfig {
        storage: LogStorageKind::Sd,
        ..CONFIG
    });
    block_on(logging.step());
    log_many(&logging, 2, 1);
    settle(&rig, &logging);
    let on_sd = read(&logging, 0, 50);
    assert_eq!(on_sd.len(), 2);
    assert!(on_sd[0].sequence > 14, "{}", on_sd[0].sequence);
}

#[test]
fn switching_to_an_unavailable_backend_reports_no_data_of_the_old_one() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 10, 1);
    settle(&rig, &logging);
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(10));

    logging.apply(LoggingConfig {
        storage: LogStorageKind::Sd,
        ..CONFIG
    });
    step(&rig, &logging);
    assert_eq!(issues(&logging), [issue::STORAGE_UNAVAILABLE]);
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
    assert_eq!(number(&logging, status_field::USED_BYTES), Some(0));
    assert_eq!(number(&logging, status_field::NEWEST_SEQUENCE), None);
}

#[test]
fn an_unavailable_backend_is_reported_and_never_replaced_by_another() {
    let rig = Rig::new();
    let logging = rig.logging(LoggingConfig {
        storage: LogStorageKind::Sd,
        ..CONFIG
    });
    step(&rig, &logging);
    assert_eq!(
        summary(&logging),
        (ServiceState::Running, ServiceHealth::Error, false)
    );
    assert_eq!(issues(&logging), [issue::STORAGE_UNAVAILABLE]);
    assert_eq!(
        field(&logging, status_field::STORAGE),
        Some(Value::text("sd").unwrap())
    );
    assert_eq!(number(&logging, status_field::CAPACITY_BYTES), Some(0));

    // The application keeps logging; nothing reaches the internal flash.
    log_many(&logging, 5, 1);
    for _ in 0..5 {
        step(&rig, &logging);
    }
    assert_eq!(logging.sink().queued(), 5);
    assert_eq!(
        rig.opened
            .borrow()
            .iter()
            .filter(|kind| **kind == LogStorageKind::Internal)
            .count(),
        0
    );
    assert_eq!(
        invoke(&logging, action::CLEAR, &[]).err(),
        Some(ActionError::Unavailable)
    );
    // The RAM tail does not need the storage.
    assert!(invoke(&logging, action::TAIL, &args(0, 0, 5)).is_ok());
    assert_eq!(
        invoke(&logging, action::DOWNLOAD, &[]).err(),
        Some(ActionError::Unavailable)
    );

    // A card appears: the service opens it by itself and the waiting records
    // are written there.
    rig.sd_present.set(true);
    for _ in 0..3 {
        rig.advance(RETRY_MS_FOR_TESTS);
        block_on(logging.step());
    }
    settle(&rig, &logging);
    assert_eq!(
        summary(&logging),
        (ServiceState::Running, ServiceHealth::Ok, true)
    );
    assert!(issues(&logging).is_empty());
    let records = read(&logging, 0, 50);
    assert_eq!(records.len(), 5);
    assert_eq!(
        rig.opened
            .borrow()
            .iter()
            .filter(|kind| **kind == LogStorageKind::Internal)
            .count(),
        0
    );
}

const RETRY_MS_FOR_TESTS: u64 = 11_000;

#[test]
fn switching_the_backend_does_not_migrate_the_history() {
    let rig = Rig::new();
    rig.sd_present.set(true);
    let logging = rig.logging(CONFIG);
    log_many(&logging, 12, 1);
    settle(&rig, &logging);
    log(&logging, "last on internal");

    logging.apply(LoggingConfig {
        storage: LogStorageKind::Sd,
        ..CONFIG
    });
    block_on(logging.step());
    // The open segment was flushed and sealed before the switch.
    assert_eq!(
        field(&logging, status_field::STORAGE),
        Some(Value::text("sd").unwrap())
    );
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
    log_many(&logging, 3, 1);
    settle(&rig, &logging);
    let on_sd = read(&logging, 0, 50);
    assert_eq!(
        on_sd.iter().map(|r| r.sequence).collect::<Vec<_>>(),
        [14, 15, 16]
    );

    logging.apply(CONFIG);
    block_on(logging.step());
    let back = read(&logging, 0, 50);
    assert_eq!(back.len(), 13);
    assert_eq!(back.last().unwrap().message.as_str(), "last on internal");
}

#[test]
fn a_codec_or_size_change_applies_to_new_segments_only() {
    let rig = Rig::new();
    let logging = rig.logging(LoggingConfig {
        compression: Compression::None,
        ..CONFIG
    });
    let mut next = 1;
    for _ in 0..8 {
        log_many(&logging, 6, next);
        next += 6;
        settle(&rig, &logging);
    }
    logging.apply(CONFIG);
    for _ in 0..40 {
        log_many(&logging, 6, next);
        next += 6;
        settle(&rig, &logging);
    }
    let info = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let names: Vec<_> = info
        .files
        .iter()
        .map(|file| file.name.as_str().to_owned())
        .collect();
    assert!(
        names.iter().any(|name| name.ends_with(".log.lz4")),
        "{names:?}"
    );
    // Old plain segments, if any are left, keep their form and stay readable.
    let all = read(&logging, 0, 200);
    assert!(!all.is_empty());
    for pair in all.windows(2) {
        assert_eq!(pair[1].sequence, pair[0].sequence + 1);
    }
    for file in &info.files {
        let data = read_all(&logging, &info, file.id);
        if file.name.as_str().ends_with(".log") {
            assert!(parse(&data).len() > 1);
        }
    }
    assert_eq!(
        names.len() as u64,
        number(&logging, status_field::SEGMENTS).unwrap()
    );
    assert!(names.len() <= MAX_SEGMENTS);
}

#[test]
fn a_failing_medium_degrades_the_service_and_it_recovers() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 5, 1);
    settle(&rig, &logging);

    rig.internal.0.borrow_mut().fail_after(0);
    log_many(&logging, 5, 100);
    step(&rig, &logging);
    step(&rig, &logging);
    assert_eq!(
        summary(&logging),
        (ServiceState::Running, ServiceHealth::Degraded, false)
    );
    assert!(issues(&logging).contains(&issue::WRITE_FAILED));
    assert_eq!(number(&logging, status_field::DROPPED_RECORDS), Some(5));
    // The application is not affected: logging still returns at once.
    assert!(log(&logging, "still not blocking"));

    // The medium comes back: the service reads it again and goes on.
    rig.internal.0.borrow_mut().restore_power();
    for _ in 0..3 {
        rig.advance(11_000);
        block_on(logging.step());
    }
    settle(&rig, &logging);
    assert_eq!(
        summary(&logging),
        (ServiceState::Running, ServiceHealth::Ok, true)
    );
    assert!(issues(&logging).is_empty());
    let records = read(&logging, 0, 100);
    assert_eq!(records.first().unwrap().sequence, 1);
    assert_eq!(
        records.last().unwrap().message.as_str(),
        "still not blocking"
    );
}

#[test]
fn damaged_storage_does_not_stop_the_log_and_is_reported() {
    let rig = Rig::new();
    {
        let logging = rig.logging(CONFIG);
        let mut next = 1;
        for _ in 0..30 {
            log_many(&logging, 6, next);
            next += 6;
            settle(&rig, &logging);
        }
    }
    // Damage the start record of the oldest segment.
    let first = *rig.internal.0.borrow_mut().raw(9);
    *rig.internal.0.borrow_mut().raw(9) = first ^ 0x01;
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert_eq!(summary(&logging).0, ServiceState::Running);
    assert!(number(&logging, status_field::SEGMENTS).unwrap() > 0);
    log(&logging, "after the damage");
    settle(&rig, &logging);
    assert!(!read(&logging, 0, 5).is_empty());
}

#[test]
fn clear_mutates_and_tail_does_not() {
    let actions = <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 8> as Action>::ACTIONS;
    let access = |id| {
        actions
            .iter()
            .find(|action| action.id == id)
            .unwrap()
            .access
    };
    assert_eq!(access(action::CLEAR), ActionAccess::Mutate);
    assert_eq!(access(action::TAIL), ActionAccess::Read);
    assert_eq!(access(action::DOWNLOAD), ActionAccess::Read);
}

#[test]
fn bad_arguments_are_refused() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert_eq!(
        invoke(&logging, action::TAIL, &[(tail_arg::AFTER, Value::U64(0))]).err(),
        Some(ActionError::InvalidArgument)
    );
    assert_eq!(
        invoke(&logging, 99, &[]).err(),
        Some(ActionError::UnknownAction)
    );
}

struct Never;

impl embedded_hal_async::delay::DelayNs for Never {
    async fn delay_ns(&mut self, _: u32) {
        core::future::pending::<()>().await;
    }
}

#[test]
fn the_task_takes_records_when_they_arrive() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    let mut task = core::pin::pin!(logging.run(Never));
    // Nothing to do: it opens the backend and waits.
    assert!(embassy_futures::poll_once(task.as_mut()).is_pending());
    assert_eq!(summary(&logging).0, ServiceState::Running);

    log(&logging, "one");
    log(&logging, "two");
    assert_eq!(logging.sink().queued(), 2);
    assert!(embassy_futures::poll_once(task.as_mut()).is_pending());
    assert_eq!(logging.sink().queued(), 0);

    // A new configuration wakes it too.
    logging.apply(LoggingConfig {
        level: LogLevel::Debug,
        ..CONFIG
    });
    assert!(embassy_futures::poll_once(task.as_mut()).is_pending());
}

#[test]
fn records_that_cannot_be_stored_do_not_keep_the_task_spinning() {
    let rig = Rig::new();
    let logging = rig.logging(LoggingConfig {
        storage: LogStorageKind::Sd,
        ..CONFIG
    });
    let mut task = core::pin::pin!(logging.run(Never));
    assert!(embassy_futures::poll_once(task.as_mut()).is_pending());
    assert_eq!(issues(&logging), [issue::STORAGE_UNAVAILABLE]);

    // The queue holds records nobody can take: the task must yield, not loop.
    log_many(&logging, 5, 1);
    assert!(embassy_futures::poll_once(task.as_mut()).is_pending());
    assert_eq!(logging.sink().queued(), 5);
}

#[test]
fn clear_opens_the_backend_when_the_task_has_not_run_yet() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    assert_eq!(summary(&logging).0, ServiceState::Running);
}

#[test]
fn clear_also_drops_the_records_still_waiting_in_ram() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 5, 1);
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    assert_eq!(logging.sink().queued(), 0);
    log(&logging, "after");
    let records = read(&logging, 0, 10);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].message.as_str(), "after");
}

struct Recording(Rc<Cell<u64>>);

impl embedded_hal_async::delay::DelayNs for Recording {
    async fn delay_ns(&mut self, ns: u32) {
        self.0.set(u64::from(ns) / 1_000_000);
        core::future::pending::<()>().await;
    }
}

#[test]
fn a_pinned_full_ring_stalls_the_task_instead_of_spinning() {
    let rig = Rig::new();
    let logging = rig.logging(LoggingConfig {
        compression: Compression::None,
        ..CONFIG
    });
    let mut next = 1;
    for _ in 0..30 {
        log_many(&logging, 6, next);
        next += 6;
        settle(&rig, &logging);
    }
    let info = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    assert!(logging.pins() > 0);

    let requested = Rc::new(Cell::new(0));
    let mut task = core::pin::pin!(logging.run(Recording(requested.clone())));
    assert!(embassy_futures::poll_once(task.as_mut()).is_pending());

    // The ring is full of pinned segments: whatever the task wakes for, it
    // must not retry more often than the stall allows.
    let start = rig.clock.0.get();
    let mut wakes = 0;
    while rig.clock.0.get() - start < 10_000 {
        log_many(&logging, 6, next);
        next += 6;
        rig.advance(requested.get().max(1));
        logging.apply(CONFIG);
        assert!(embassy_futures::poll_once(task.as_mut()).is_pending());
        wakes += 1;
        assert!(wakes < 40, "the task retries without waiting");
    }

    block_on(logging.service().close_stream(info.id)).unwrap();
    assert_eq!(logging.pins(), 0);
    // Without the pins the very next pass writes again.
    logging.apply(CONFIG);
    rig.advance(requested.get().max(1));
    assert!(embassy_futures::poll_once(task.as_mut()).is_pending());
}

fn write_history(rig: &Rig, logging: &TestLogging<'_>, batches: u64) -> u64 {
    let mut next = 1;
    for _ in 0..batches {
        log_many(logging, 6, next);
        next += 6;
        settle(rig, logging);
    }
    next - 1
}

#[test]
fn a_damaged_segment_does_not_touch_the_tail() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    write_history(&rig, &logging, 60);
    assert!(read(&logging, 0, 200).len() > 20);

    // The oldest segment starts at sector 0: damage a byte of its first
    // LZ4 block (frame header 7 bytes, then the block).
    *rig.internal.0.borrow_mut().raw(16 + 20) ^= 0x10;
    assert!(logging.archive(0, 200).is_none());

    // The tail is RAM: it does not decode anything.
    let info = stream_of(invoke(&logging, action::TAIL, &args(0, 0, 8)));
    assert_eq!(parse(&read_all(&logging, &info, TAIL_FILE_ID)).len(), 8);
    assert!(issues(&logging).is_empty());
}

/// The history of the archive and its records after `after`.
fn read_in_history(logging: &TestLogging<'_>, after: u64) -> (u32, Vec<LogRecord>) {
    let records = logging.archive(after, 200).expect("the archive decodes");
    let history = number(logging, status_field::HISTORY).unwrap_or(0) as u32;
    (history, records)
}

#[test]
fn a_reboot_that_keeps_the_history_keeps_its_identity() {
    let rig = Rig::new();
    let history = {
        let logging = rig.logging(CONFIG);
        log_many(&logging, 7, 1);
        settle(&rig, &logging);
        number(&logging, status_field::HISTORY).unwrap()
    };
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert_eq!(number(&logging, status_field::HISTORY), Some(history));
    log(&logging, "after");
    settle(&rig, &logging);
    let (seen, records) = read_in_history(&logging, 7);
    assert_eq!(seen as u64, history);
    assert_eq!(records.len(), 1);
}

#[test]
fn a_cleared_history_is_not_mistaken_for_the_old_one_across_a_reboot() {
    let rig = Rig::new();
    let (old_history, cursor) = {
        let logging = rig.logging(CONFIG);
        log_many(&logging, 40, 1);
        settle(&rig, &logging);
        let (history, records) = read_in_history(&logging, 0);
        assert_eq!(
            history as u64,
            number(&logging, status_field::HISTORY).unwrap()
        );
        (history, records.last().unwrap().sequence)
    };

    // A follow client holds `cursor` in `old_history`. The history is
    // cleared, the device reboots before anything new is stored.
    {
        let logging = rig.logging(CONFIG);
        step(&rig, &logging);
        assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
        let (history, records) = read_in_history(&logging, 0);
        assert!(records.is_empty());
        assert_ne!(history, old_history);
    }
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
    assert_ne!(
        number(&logging, status_field::HISTORY),
        Some(u64::from(old_history))
    );

    // New records restart at sequence 1: the client that reconnects sees a
    // different history, whatever its cursor says.
    log_many(&logging, 5, 500);
    settle(&rig, &logging);
    let (history, records) = read_in_history(&logging, cursor);
    assert_ne!(history, old_history);
    assert!(
        records.is_empty(),
        "the old cursor would hide the new records"
    );
    let (again, records) = read_in_history(&logging, 0);
    assert_eq!(again, history);
    assert_eq!(records.len(), 5);
    assert_eq!(records[0].sequence, 1);

    // The new history keeps its identity across further reboots.
    drop(logging);
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert_eq!(
        number(&logging, status_field::HISTORY),
        Some(u64::from(history))
    );
}

#[test]
fn clearing_twice_gives_two_new_histories() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 5, 1);
    settle(&rig, &logging);
    let first = number(&logging, status_field::HISTORY).unwrap();
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    let second = number(&logging, status_field::HISTORY).unwrap();
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    let third = number(&logging, status_field::HISTORY).unwrap();
    assert!(first < second && second < third);
}

#[test]
fn a_payload_damaged_while_the_device_was_off_is_dropped_and_reported() {
    let rig = Rig::new();
    let (old_history, old_records) = {
        let logging = rig.logging(CONFIG);
        write_history(&rig, &logging, 60);
        let (history, records) = read_in_history(&logging, 0);
        (history, records.len())
    };
    // Damage a payload byte in every sealed segment of the first sector run;
    // the headers stay valid.
    for sector in 0..6u32 {
        *rig.internal.0.borrow_mut().raw(sector * 4096 + 100) ^= 0x04;
    }
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert!(issues(&logging).contains(&issue::STORAGE_CORRUPT));
    assert_eq!(summary(&logging).1, ServiceHealth::Degraded);
    log(&logging, "still logging");
    settle(&rig, &logging);
    let (history, records) = read_in_history(&logging, 0);
    assert!(records.len() < old_records);
    assert!(records.last().unwrap().message.contains("still logging"));
    // Nothing of the old history is left: it is a new one, so a client that
    // follows it starts over.
    if records.first().unwrap().sequence == 1 {
        assert_ne!(history, old_history);
    }
}

// Stream lifecycle: every way a stream ends releases its segment pins exactly
// once and leaves the other streams alone.

fn many_segments(rig: &Rig, logging: &TestLogging<'_>) -> u64 {
    let mut next = 1;
    for _ in 0..30 {
        log_many(logging, 6, next);
        next += 6;
        settle(rig, logging);
    }
    next
}

fn is_gone(logging: &TestLogging<'_>, stream: u32) -> bool {
    let mut buf = [0u8; 8];
    block_on(logging.service().read_stream(stream, 1, 0, &mut buf))
        == Err(ActionError::UnknownStream)
}

#[test]
fn clear_without_open_streams_pins_and_releases_nothing() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    assert_eq!((logging.open_streams(), logging.pinned_segments()), (0, 0));
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    assert_eq!((logging.open_streams(), logging.pinned_segments()), (0, 0));
}

#[test]
fn clear_releases_downloads_and_leaves_tail_streams_alone() {
    for downloads in [1, 2] {
        let rig = Rig::new();
        let logging = rig.logging(CONFIG);
        many_segments(&rig, &logging);

        let mut streams = Vec::new();
        for _ in 0..downloads {
            streams.push(stream_of(invoke(&logging, action::DOWNLOAD, &[])).id);
        }
        assert!(logging.pinned_segments() > 0);
        let tail = stream_of(invoke(&logging, action::TAIL, &args(0, 0, 5))).id;
        assert_eq!(logging.open_streams(), downloads + 1);

        assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
        assert_eq!((logging.open_streams(), logging.pins()), (1, 0));
        assert_eq!(logging.pinned_segments(), 0);
        assert!(streams.iter().all(|stream| is_gone(&logging, *stream)));
        assert!(!is_gone(&logging, tail));
        // The download table is free again.
        let again = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
        block_on(logging.service().close_stream(again.id)).unwrap();
    }
}

#[test]
fn two_downloads_release_their_shared_segments_independently() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);

    let first = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let second = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let (a, b) = (first.files.len(), second.files.len());
    assert_eq!(logging.pins(), a + b);
    let pinned = logging.pinned_segments();
    assert!(pinned >= a.max(b));

    // The segments both streams hold stay pinned while one of them is open.
    block_on(logging.service().close_stream(first.id)).unwrap();
    assert_eq!(logging.pins(), b);
    assert_eq!(logging.pinned_segments(), b);
    assert!(is_gone(&logging, first.id));
    let kept = read_all(&logging, &second, 1);
    assert_eq!(kept.len() as u64, second.files[0].size_bytes);

    block_on(logging.service().close_stream(second.id)).unwrap();
    assert_eq!((logging.pins(), logging.pinned_segments()), (0, 0));
}

#[test]
fn an_expired_stream_is_released_while_the_other_keeps_its_pins() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);

    let old = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    rig.advance(STREAM_TIMEOUT_MS * 6 / 10);
    let young = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    rig.advance(STREAM_TIMEOUT_MS * 5 / 10);
    block_on(logging.step());

    assert_eq!(logging.open_streams(), 1);
    assert!(is_gone(&logging, old.id));
    assert_eq!(logging.pins(), young.files.len());
    assert_eq!(logging.pinned_segments(), young.files.len());
    assert!(!read_all(&logging, &young, 1).is_empty());

    rig.advance(STREAM_TIMEOUT_MS + 1);
    block_on(logging.step());
    assert_eq!((logging.open_streams(), logging.pinned_segments()), (0, 0));
}

#[test]
fn a_lost_link_releases_every_stream_at_once() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let download = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let read = stream_of(invoke(&logging, action::TAIL, &args(0, 0, 5)));
    assert_eq!(logging.open_streams(), 2);

    logging.service().link_lost();
    block_on(logging.step());
    assert_eq!((logging.open_streams(), logging.pins()), (0, 0));
    assert_eq!(logging.pinned_segments(), 0);
    assert!(is_gone(&logging, download.id) && is_gone(&logging, read.id));
}

#[test]
fn closing_streams_in_any_order_leaves_the_other_one_intact() {
    for close_records_first in [true, false] {
        let rig = Rig::new();
        let logging = rig.logging(CONFIG);
        many_segments(&rig, &logging);
        let records = stream_of(invoke(&logging, action::TAIL, &args(0, 0, 5)));
        let files = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
        let pinned = logging.pinned_segments();
        assert_eq!(pinned, files.files.len());
        let expected = read_all(&logging, &records, 1);
        let service = logging.service();

        if close_records_first {
            block_on(service.close_stream(records.id)).unwrap();
            assert_eq!(logging.pinned_segments(), pinned);
            assert!(!read_all(&logging, &files, 1).is_empty());
            block_on(service.close_stream(files.id)).unwrap();
        } else {
            block_on(service.close_stream(files.id)).unwrap();
            assert_eq!(logging.pinned_segments(), 0);
            assert_eq!(read_all(&logging, &records, 1), expected);
            block_on(service.close_stream(records.id)).unwrap();
        }
        assert_eq!((logging.open_streams(), logging.pinned_segments()), (0, 0));
    }
}

#[test]
fn closing_a_stream_twice_is_an_error_and_releases_nothing_more() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let first = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let second = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let service = logging.service();

    block_on(service.close_stream(first.id)).unwrap();
    let held = logging.pinned_segments();
    assert_eq!(
        block_on(service.close_stream(first.id)),
        Err(ActionError::UnknownStream)
    );
    assert_eq!(logging.pinned_segments(), held);
    assert_eq!(logging.pins(), second.files.len());
}

#[test]
fn repeated_clear_and_logging_after_it_keeps_working() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    for round in 0..3 {
        many_segments(&rig, &logging);
        let download = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
        let records = stream_of(invoke(&logging, action::TAIL, &args(0, 0, 3)));
        assert!(
            invoke(&logging, action::CLEAR, &[]).is_ok(),
            "round {round}"
        );
        assert!(
            invoke(&logging, action::CLEAR, &[]).is_ok(),
            "round {round}"
        );
        assert_eq!(logging.pinned_segments(), 0);
        assert!(is_gone(&logging, download.id) && !is_gone(&logging, records.id));
        block_on(logging.service().close_stream(records.id)).unwrap();
        assert!(read_in_history(&logging, 0).1.is_empty());

        log_many(&logging, 3, 1);
        settle(&rig, &logging);
        assert_eq!(read(&logging, 0, 50).len(), 3);
    }
}

#[test]
fn a_medium_that_was_never_formatted_is_formatted_once_and_keeps_the_boot_records() {
    let rig = Rig::new();
    // What a partition holds the first time: old bytes, no format header.
    {
        let mut medium = rig.internal.clone();
        for sector in 0..SECTORS as u32 {
            medium.erase(sector, 1).unwrap();
            medium.write(sector * 4096, &[0x42; 64]).unwrap();
        }
    }
    {
        let logging = rig.logging(CONFIG);
        // Queued before the backend is open, as the boot records are.
        log_many(&logging, 5, 1);
        settle(&rig, &logging);
        assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(5));
        assert_eq!(number(&logging, status_field::HISTORY), Some(1));
    }
    let erases = rig.internal.0.borrow().erase_counts().to_vec();

    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(5));
    assert_eq!(rig.internal.0.borrow().erase_counts().to_vec(), erases);
}

fn record(ms: u64, message: &str) -> LogRecord {
    LogRecord::new(
        LogLevel::Error,
        "panic",
        format_args!("{message}"),
        ms,
        None,
    )
}

fn crashed(rig: &Rig, lines: &[&str]) {
    rig.boot
        .borrow_mut()
        .extend(lines.iter().map(|line| record(777, line)));
}

/// The newest records, whatever the size of the history.
fn tail(logging: &TestLogging<'_>) -> Vec<LogRecord> {
    let newest = number(logging, status_field::NEWEST_SEQUENCE).unwrap_or(0);
    read(logging, newest.saturating_sub(40), 100)
}

fn texts(records: &[LogRecord]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record.message.as_str())
        .collect()
}

#[test]
fn the_records_of_the_previous_run_are_written_first_and_once() {
    let rig = Rig::new();
    crashed(
        &rig,
        &["panic: boom", "at crates/hal/src/lib.rs:42:7", "crash #1"],
    );
    let logging = rig.logging(CONFIG);
    // Queued before the storage is open, as the boot records are.
    log_many(&logging, 3, 1);
    settle(&rig, &logging);

    let records = read(&logging, 0, 50);
    assert_eq!(
        texts(&records)[..3],
        ["panic: boom", "at crates/hal/src/lib.rs:42:7", "crash #1"]
    );
    assert!(texts(&records)[3].starts_with("logging:") || texts(&records)[3].contains("record"));
    // They keep what the platform stamped, and are ordered like any records.
    assert_eq!(records[0].monotonic_ms, 777);
    assert_eq!(records[0].target.as_str(), "panic");
    assert_eq!(records[0].level, LogLevel::Error);
    assert_eq!(
        records
            .iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>(),
        (1..=records.len() as u64).collect::<Vec<_>>()
    );
    assert_eq!(rig.boot_written.get(), 1);
    assert!(rig.boot.borrow().is_empty());

    // Not offered again.
    for _ in 0..3 {
        step(&rig, &logging);
    }
    assert_eq!(rig.boot_written.get(), 1);
    let again = read(&logging, 0, 200);
    assert_eq!(
        again
            .iter()
            .filter(|record| record.message.as_str() == "crash #1")
            .count(),
        1
    );
}

#[test]
fn without_records_the_platform_is_not_told_anything() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 3, 1);
    settle(&rig, &logging);
    assert_eq!(rig.boot_written.get(), 0);
    assert_eq!(read(&logging, 0, 50).first().unwrap().sequence, 1);
}

#[test]
fn the_records_of_the_previous_run_do_not_depend_on_the_level() {
    let rig = Rig::new();
    crashed(&rig, &["panic: kept"]);
    let logging = rig.logging(LoggingConfig {
        level: LogLevel::Off,
        ..CONFIG
    });
    assert!(!log(&logging, "filtered"));
    step(&rig, &logging);
    assert_eq!(texts(&read(&logging, 0, 50)), ["panic: kept"]);
    assert_eq!(rig.boot_written.get(), 1);
}

#[test]
fn the_records_of_the_previous_run_survive_a_restart() {
    let rig = Rig::new();
    crashed(&rig, &["panic: one", "crash #1"]);
    {
        let logging = rig.logging(CONFIG);
        step(&rig, &logging);
        assert_eq!(rig.boot_written.get(), 1);
    }
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert_eq!(texts(&read(&logging, 0, 50)), ["panic: one", "crash #1"]);
}

#[test]
fn nothing_is_released_while_there_is_no_storage() {
    let rig = Rig::new();
    crashed(&rig, &["panic: x"]);
    let logging = rig.logging(LoggingConfig {
        storage: LogStorageKind::Sd,
        ..CONFIG
    });
    step(&rig, &logging);
    step(&rig, &logging);
    assert_eq!(rig.boot_written.get(), 0);
    assert_eq!(rig.boot.borrow().len(), 1);

    // The card appears: the records are written then, once.
    rig.sd_present.set(true);
    for _ in 0..4 {
        rig.advance(11_000);
        block_on(logging.step());
    }
    assert_eq!(rig.boot_written.get(), 1);
    assert_eq!(texts(&read(&logging, 0, 50)), ["panic: x"]);
}

#[test]
fn a_failing_medium_keeps_the_records_for_the_next_try_and_writes_them_once() {
    let rig = Rig::new();
    crashed(&rig, &["panic: x", "crash #1"]);
    // The medium reads but cannot write: the storage opens, the write fails.
    rig.internal.0.borrow_mut().fail_after(0);
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    step(&rig, &logging);
    assert_eq!(rig.boot_written.get(), 0, "told that the storage has them");
    assert_eq!(rig.boot.borrow().len(), 2);
    assert!(issues(&logging).contains(&issue::WRITE_FAILED));

    rig.internal.0.borrow_mut().restore_power();
    for _ in 0..4 {
        rig.advance(11_000);
        block_on(logging.step());
    }
    assert_eq!(rig.boot_written.get(), 1);
    let seen = tail(&logging);
    assert_eq!(
        texts(&seen)
            .iter()
            .filter(|text| **text == "panic: x")
            .count(),
        1
    );
    assert_eq!(
        texts(&seen)
            .iter()
            .filter(|text| **text == "crash #1")
            .count(),
        1
    );
}

#[test]
fn more_records_than_one_batch_holds_are_refused_whole() {
    let rig = Rig::new();
    rig.boot.borrow_mut().extend(
        (0..1_000).map(|n| record(1, &std::format!("line {n} with padding to make it long"))),
    );
    let logging = rig.logging(CONFIG);
    log_many(&logging, 1, 1);
    settle(&rig, &logging);
    // Nothing of them is written, none is released, and the sequence is not consumed.
    assert_eq!(rig.boot_written.get(), 0);
    let records = read(&logging, 0, 50);
    assert_eq!(records.first().unwrap().sequence, 1);
    assert!(
        texts(&records)
            .iter()
            .all(|text| !text.starts_with("line "))
    );
}

#[test]
fn a_download_that_pins_the_ring_defers_the_records_without_writing_them() {
    let rig = Rig::new();
    let config = LoggingConfig {
        compression: Compression::None,
        ..CONFIG
    };
    {
        let logging = rig.logging(config);
        let mut next = 1;
        for _ in 0..30 {
            log_many(&logging, 6, next);
            next += 6;
            settle(&rig, &logging);
        }
    }
    // The next run: a full ring, a panic to report, and a download that pins the ring
    // before the task gets to write.
    crashed(&rig, &["panic: pinned"]);
    let logging = rig.logging(config);
    let info = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    assert!(logging.pins() > 0);
    for _ in 0..4 {
        rig.advance(1_500);
        block_on(logging.step());
    }
    assert_eq!(rig.boot_written.get(), 0);
    assert_eq!(rig.boot.borrow().len(), 1);
    assert!(
        texts(&tail(&logging))
            .iter()
            .all(|text| *text != "panic: pinned"),
        "written into a pinned ring"
    );

    block_on(logging.service().close_stream(info.id)).unwrap();
    for _ in 0..4 {
        rig.advance(5_000);
        block_on(logging.step());
    }
    assert_eq!(rig.boot_written.get(), 1);
    assert_eq!(
        texts(&tail(&logging))
            .iter()
            .filter(|text| **text == "panic: pinned")
            .count(),
        1
    );
}

// The RAM tail: `logging.tail`.

fn tail_args(session: u64, after: u64, limit: u64) -> [(u32, Value); 3] {
    args(session, after, limit)
}

/// What `tail` answers.
enum Tailed {
    /// Lines, with the session and the reset flag of the file name.
    Lines(u32, bool, Vec<LogRecord>),
    /// Nothing to stream: the session and the reset flag of the record.
    Empty(u64, bool),
}

fn tail_call(logging: &TestLogging<'_>, session: u64, after: u64, limit: u64) -> Tailed {
    match invoke(logging, action::TAIL, &tail_args(session, after, limit)).unwrap() {
        ActionResult::Stream(info) => {
            assert_eq!(info.files.len(), 1);
            let name = info.files[0].name.as_str();
            let (session, reset) = parse_tail_file_name(name).unwrap();
            let data = read_all(logging, &info, TAIL_FILE_ID);
            assert_eq!(data.len() as u64, info.files[0].size_bytes);
            block_on(logging.service().close_stream(info.id)).unwrap();
            Tailed::Lines(session, reset, parse(&data))
        }
        ActionResult::Records(records) => {
            assert_eq!(records.len(), 1);
            let field = |id| {
                records[0]
                    .iter()
                    .find(|(field, _)| *field == id)
                    .map(|(_, value)| value.clone())
            };
            let reset = field(tail_result::RESET) == Some(Value::Bool(true));
            let session = field(tail_result::SESSION).and_then(|value| value.as_u64());
            Tailed::Empty(session.unwrap(), reset)
        }
    }
}

fn tailed(logging: &TestLogging<'_>, session: u64, after: u64, limit: u64) -> Vec<LogRecord> {
    match tail_call(logging, session, after, limit) {
        Tailed::Lines(_, false, records) => records,
        Tailed::Empty(_, false) => Vec::new(),
        _ => panic!("unexpected reset"),
    }
}

fn sequences(records: &[LogRecord]) -> Vec<u64> {
    records.iter().map(|record| record.sequence).collect()
}

#[test]
fn the_first_tail_is_the_latest_records_in_order_with_the_session_of_the_boot() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_boot_count(7);
    log_many(&logging, 20, 1);
    let Tailed::Lines(session, reset, records) = tail_call(&logging, 0, 0, 5) else {
        panic!("lines expected");
    };
    assert_eq!((session, reset), (7, false));
    assert_eq!(sequences(&records), [16, 17, 18, 19, 20]);
    assert!(records[0].message.contains("record 16 "));
    // More than the ring holds is the ring.
    assert_eq!(
        tailed(&logging, 0, 0, 1_000).len(),
        logging.sink().tail_capacity()
    );
    assert_eq!(logging.sink().tail_session(), 7);
}

#[test]
fn an_unknown_boot_count_still_gives_a_session() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log(&logging, "one");
    assert!(matches!(
        tail_call(&logging, 0, 0, 3),
        Tailed::Lines(1, false, _)
    ));
    logging.set_boot_count(0);
    assert_eq!(logging.sink().tail_session(), 1);
}

#[test]
fn following_returns_each_record_once_and_an_empty_poll_is_a_record() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_boot_count(3);
    // Nothing yet: no stream, only the session.
    assert!(matches!(
        tail_call(&logging, 0, 0, 8),
        Tailed::Empty(3, false)
    ));
    let mut seen = 0;
    let mut all = Vec::new();
    for round in 0..6 {
        log_many(&logging, 2 + round, round * 10);
        for record in tailed(&logging, 3, seen, 8) {
            assert!(record.sequence > seen, "duplicate {}", record.sequence);
            seen = record.sequence;
            all.push(record);
        }
        assert!(matches!(
            tail_call(&logging, 3, seen, 8),
            Tailed::Empty(3, false)
        ));
    }
    assert_eq!(sequences(&all), (1..=all.len() as u64).collect::<Vec<_>>());
    assert_eq!(all.len(), (0..6).map(|r| 2 + r).sum::<u64>() as usize);
}

#[test]
fn a_page_is_bounded_by_the_limit_and_the_next_one_continues() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_boot_count(3);
    log_many(&logging, 8, 1);
    let first = tailed(&logging, 3, 1, 3);
    assert_eq!(sequences(&first), [2, 3, 4]);
    let next = tailed(&logging, 3, 4, 3);
    assert_eq!(sequences(&next), [5, 6, 7]);
}

#[test]
fn an_overwritten_cursor_skips_ahead_and_the_gap_is_visible() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_boot_count(3);
    log_many(&logging, 30, 1);
    let capacity = logging.sink().tail_capacity() as u64;
    let records = tailed(&logging, 3, 2, 100);
    assert_eq!(records.len() as u64, capacity);
    assert_eq!(records[0].sequence, 30 - capacity + 1);
    assert!(records[0].sequence > 2 + 1);
}

#[test]
fn another_boot_or_a_cursor_from_the_future_is_a_reset_with_a_fresh_snapshot() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_boot_count(5);
    log_many(&logging, 4, 1);
    let Tailed::Lines(session, reset, records) = tail_call(&logging, 4, 3, 8) else {
        panic!("lines expected");
    };
    assert_eq!((session, reset), (5, true));
    assert_eq!(sequences(&records), [1, 2, 3, 4]);
    let Tailed::Lines(_, reset, records) = tail_call(&logging, 5, 99, 2) else {
        panic!("lines expected");
    };
    assert!(reset);
    assert_eq!(sequences(&records), [3, 4]);
    let empty = rig.logging(CONFIG);
    empty.set_boot_count(6);
    assert!(matches!(tail_call(&empty, 5, 3, 8), Tailed::Empty(6, true)));
}

#[test]
fn the_tail_does_not_touch_the_storage_or_the_queue() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    log_many(&logging, 5, 1);
    let operations = rig.internal.operations();
    let queued = logging.sink().queued();
    for after in 0..6 {
        let _ = tail_call(&logging, 0, after, 8);
    }
    assert_eq!(rig.internal.operations(), operations);
    assert_eq!(logging.sink().queued(), queued);
    assert_eq!(logging.open_streams(), 0);
    // Nothing was committed: the batch is still waiting for its interval.
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
}

#[test]
fn the_tail_works_while_the_storage_is_unavailable_failing_or_pinned() {
    // No storage at all.
    let rig = Rig::new();
    let missing = LoggingConfig {
        storage: LogStorageKind::Sd,
        ..CONFIG
    };
    let logging = rig.logging(missing);
    step(&rig, &logging);
    log_many(&logging, 3, 1);
    assert_eq!(tailed(&logging, 0, 0, 8).len(), 3);

    // A medium that fails.
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    rig.internal.0.borrow_mut().fail_after(0);
    log_many(&logging, 3, 1);
    step(&rig, &logging);
    step(&rig, &logging);
    assert_eq!(tailed(&logging, 0, 0, 8).len(), 3);

    // The archive is pinned by a download.
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let download = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    log_many(&logging, 2, 1_000);
    assert!(
        tailed(&logging, 0, 0, 2)[1]
            .message
            .contains("record 1001 ")
    );
    block_on(logging.service().close_stream(download.id)).unwrap();
}

#[test]
fn a_full_persistence_queue_does_not_stop_the_tail() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    // The task never runs: the queue (64) fills and drops.
    for n in 0..100 {
        log(&logging, &std::format!("burst {n}"));
    }
    assert!(logging.sink().dropped() > 0);
    let records = tailed(&logging, 0, 0, 8);
    assert_eq!(records.len(), 8);
    assert_eq!(records.last().unwrap().message.as_str(), "burst 99");
    // The tail has its own sequence: the archive copy of a dropped record
    // does not exist, the tail copy does.
    assert_eq!(records.last().unwrap().sequence, 100);
}

#[test]
fn clear_empties_the_archive_and_not_the_tail() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_boot_count(2);
    log_many(&logging, 4, 1);
    settle(&rig, &logging);
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
    assert_eq!(sequences(&tailed(&logging, 2, 0, 8)), [1, 2, 3, 4]);
    assert_eq!(logging.sink().tail_session(), 2);
}

#[test]
fn the_tail_and_the_download_are_independent_streams() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let first = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let second = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    assert_eq!(
        invoke(&logging, action::DOWNLOAD, &[]).err(),
        Some(ActionError::Busy)
    );
    // Both downloads are open and the tail answers.
    assert_eq!(tailed(&logging, 0, 0, 4).len(), 4);
    assert_eq!(
        (logging.pins(), logging.open_streams()),
        (first.files.len() + second.files.len(), 2)
    );
}

#[test]
fn a_new_tail_stream_replaces_the_oldest_and_never_blocks() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 6, 1);
    let mut ids = Vec::new();
    for _ in 0..MAX_TAIL_STREAMS + 2 {
        ids.push(stream_of(invoke(&logging, action::TAIL, &tail_args(0, 0, 4))).id);
    }
    assert_eq!(logging.open_streams(), MAX_TAIL_STREAMS);
    let (old, young) = ids.split_at(ids.len() - MAX_TAIL_STREAMS);
    assert!(old.iter().all(|id| is_gone(&logging, *id)));
    assert!(young.iter().all(|id| !is_gone(&logging, *id)));
}

#[test]
fn a_tail_stream_whose_records_were_overwritten_is_gone() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 8, 1);
    let info = stream_of(invoke(&logging, action::TAIL, &tail_args(0, 0, 8)));
    log_many(&logging, 20, 100);
    let mut buf = [0u8; 64];
    assert_eq!(
        block_on(
            logging
                .service()
                .read_stream(info.id, TAIL_FILE_ID, 0, &mut buf)
        ),
        Err(ActionError::UnknownStream)
    );
    // The stream is released: the client asks again.
    assert_eq!(logging.open_streams(), 0);
    assert_eq!(tailed(&logging, 0, 0, 8).len(), 8);
}

#[test]
fn a_tail_stream_expires_and_is_released_with_the_link() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    log_many(&logging, 3, 1);
    let idle = stream_of(invoke(&logging, action::TAIL, &tail_args(0, 0, 3)));
    rig.advance(STREAM_TIMEOUT_MS + 1);
    block_on(logging.step());
    assert!(is_gone(&logging, idle.id));
    let linked = stream_of(invoke(&logging, action::TAIL, &tail_args(0, 0, 3)));
    logging.service().link_lost();
    block_on(logging.step());
    assert!(is_gone(&logging, linked.id));
    assert_eq!(logging.open_streams(), 0);
}

#[test]
fn a_line_longer_than_a_chunk_is_returned_whole_in_several_reads() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    let message = "\u{1}".repeat(MAX_MESSAGE_LEN);
    for _ in 0..2 {
        assert!(
            logging
                .sink()
                .push(log::Level::Error, "ctl", format_args!("{message}"))
        );
    }
    let info = stream_of(invoke(&logging, action::TAIL, &tail_args(0, 0, 2)));
    assert!(info.files[0].size_bytes > MAX_STREAM_CHUNK as u64);
    let data = read_all(&logging, &info, TAIL_FILE_ID);
    assert_eq!(data.len() as u64, info.files[0].size_bytes);
    let records = parse(&data);
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.message.as_str() == message)
    );
}

#[test]
fn tail_levels_follow_the_sink_level_and_the_tail_is_not_the_archive() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    assert!(
        !logging
            .sink()
            .push(log::Level::Debug, "x", format_args!("filtered"))
    );
    assert!(tailed(&logging, 0, 0, 8).is_empty());
    log(&logging, "kept");
    let kept = tailed(&logging, 0, 0, 8);
    assert_eq!(texts(&kept), ["kept"]);
    // The tail sequence is its own: the archive assigns another one later.
    settle(&rig, &logging);
    assert_eq!(texts(&read(&logging, 0, 10)), ["kept"]);
}

#[test]
fn the_tail_file_name_round_trips_and_fits() {
    for (session, reset) in [(1, false), (u32::MAX, true), (0, false)] {
        let name = tail_file_name(session, reset);
        assert!(name.len() <= MAX_TAIL_FILE_NAME_LEN);
        assert_eq!(parse_tail_file_name(name.as_str()), Some((session, reset)));
    }
    assert_eq!(parse_tail_file_name("records-3.log"), None);
    assert_eq!(parse_tail_file_name("tail-x.log"), None);
}

#[test]
fn the_tail_survives_a_reboot_only_as_a_new_session() {
    let rig = Rig::new();
    let session = {
        let logging = rig.logging(CONFIG);
        logging.set_boot_count(1);
        log_many(&logging, 3, 1);
        match tail_call(&logging, 0, 0, 8) {
            Tailed::Lines(session, _, _) => session,
            _ => panic!("lines expected"),
        }
    };
    let logging = rig.logging(CONFIG);
    logging.set_boot_count(2);
    // The client reconnects with the old session: it must start over.
    assert!(matches!(
        tail_call(&logging, u64::from(session), 3, 8),
        Tailed::Empty(2, true)
    ));
}
