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

use crate::storage::{MAX_SEGMENTS, Medium, RamMedium, RingStorage};
use crate::*;

const SECTORS: usize = 16;

#[derive(Clone)]
struct Shared(Rc<RefCell<RamMedium<SECTORS>>>);

impl Shared {
    fn new() -> Self {
        let mut medium = RamMedium::new();
        medium.erase(0, SECTORS as u32).unwrap();
        Self(Rc::new(RefCell::new(medium)))
    }
}

impl Medium for Shared {
    type Error = crate::storage::RamError;

    fn sectors(&self) -> u32 {
        SECTORS as u32
    }

    fn erase(&mut self, first: u32, count: u32) -> Result<(), Self::Error> {
        self.0.borrow_mut().erase(first, count)
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        self.0.borrow_mut().write(offset, data)
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
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
}

const CONFIG: LoggingConfig = LoggingConfig {
    level: LogLevel::Info,
    storage: LogStorageKind::Internal,
    compression: Compression::Lz4,
    retention_bytes: 4096 * 6,
    segment_bytes: 4096,
    flush_interval_ms: 1_000,
};

type TestLogging<'a> = Logging<NoopRawMutex, &'a Clock, Test, 64, 4096>;

struct Rig {
    clock: Clock,
    internal: Shared,
    sd: Shared,
    sd_present: Rc<Cell<bool>>,
    opened: Rc<RefCell<Vec<LogStorageKind>>>,
}

impl Rig {
    fn new() -> Self {
        Self {
            clock: Clock(Cell::new(10_000)),
            internal: Shared::new(),
            sd: Shared::new(),
            sd_present: Rc::new(Cell::new(false)),
            opened: Rc::default(),
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

fn args(after: u64, limit: u64) -> [(u32, Value); 2] {
    [
        (read_arg::AFTER, Value::U64(after)),
        (read_arg::LIMIT, Value::U64(limit)),
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
        <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 4096> as Service>::ID,
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

/// The records `read` returns after `after`, up to `limit`.
fn read(logging: &TestLogging<'_>, after: u64, limit: u64) -> Vec<LogRecord> {
    let info = stream_of(invoke(logging, action::READ, &args(after, limit)));
    assert_eq!(info.files.len(), 1);
    assert!(history_of_read_file(info.files[0].name.as_str()).is_some());
    let data = read_all(logging, &info, READ_FILE_ID);
    assert_eq!(data.len() as u64, info.files[0].size_bytes);
    block_on(logging.service().close_stream(info.id)).unwrap();
    parse(&data)
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
        assert!(<(&LoggingService<'static, NoopRawMutex, &'static Clock, Test, 64, 4096>,) as ActionRegistry>::SCHEMAS_VALID);
        assert!(<(&LoggingService<'static, NoopRawMutex, &'static Clock, Test, 64, 4096>,) as ActionRegistry>::IDS_UNIQUE);
    }
    let statuses = Statuses::<NoopRawMutex, _>::new((&service,));
    assert_eq!(statuses.service_count(), 1);
    assert_eq!(
        <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 4096> as Service>::NAME,
        "logging"
    );
    let names: Vec<_> =
        <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 4096> as Action>::ACTIONS
            .iter()
            .map(|action| (action.name, action.access, action.stream))
            .collect();
    assert_eq!(
        names,
        [
            ("read", ActionAccess::Read, true),
            ("download", ActionAccess::Read, true),
            ("clear", ActionAccess::Mutate, false)
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
fn the_latest_n_records_are_read_with_bounded_calls() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 6, 1);
    settle(&rig, &logging);
    for _ in 0..30 {
        log_many(&logging, 6, 1);
        settle(&rig, &logging);
    }
    let newest = number(&logging, status_field::NEWEST_SEQUENCE).unwrap();
    assert_eq!(newest, 31 * 6);

    // `rohi logs -n 20`
    let last = read(&logging, newest - 20, 20);
    assert_eq!(last.len(), 20);
    assert_eq!(last.first().unwrap().sequence, newest - 19);
    assert_eq!(last.last().unwrap().sequence, newest);

    // `-n 100`: pages of at most 30 records until there are 100.
    let mut got: Vec<LogRecord> = Vec::new();
    let mut after = newest - 100;
    while got.len() < 100 {
        let page = read(&logging, after, 30.min(100 - got.len() as u64));
        assert!(!page.is_empty());
        after = page.last().unwrap().sequence;
        got.extend(page);
    }
    assert_eq!(got.len(), 100);
    for pair in got.windows(2) {
        assert_eq!(pair[1].sequence, pair[0].sequence + 1);
    }
    assert_eq!(got.last().unwrap().sequence, newest);

    // One answer is bounded by its buffer: the oldest records, and the caller
    // asks again for the rest.
    let all = read(&logging, 0, 200);
    assert!((30..=200).contains(&all.len()), "{}", all.len());
    assert_eq!(all[0].sequence, 1);
    let rest = read(&logging, all.last().unwrap().sequence, 200);
    assert_eq!(rest[0].sequence, all.len() as u64 + 1);

    // The limit and the size of the answer are bounded.
    assert_eq!(read(&logging, 0, 1).len(), 1);
    let capped = read(&logging, 0, u64::MAX);
    assert!(capped.len() <= MAX_READ_RECORDS as usize);
}

#[test]
fn following_returns_each_record_once() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    let mut seen = 0;
    let mut all = Vec::new();
    for round in 0..8 {
        log_many(&logging, 3 + round, round * 10);
        for record in read(&logging, seen, 50) {
            assert!(record.sequence > seen, "duplicate {}", record.sequence);
            seen = record.sequence;
            all.push(record);
        }
        // Nothing new: nothing returned.
        assert!(read(&logging, seen, 50).is_empty());
    }
    assert_eq!(all.len(), (0..8).map(|r| 3 + r).sum::<u64>() as usize);
}

#[test]
fn a_read_stream_is_retryable_by_offset_and_ends() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log_many(&logging, 10, 1);
    let info = stream_of(invoke(&logging, action::READ, &args(0, 10)));
    let service = logging.service();
    let mut first = [0u8; 64];
    let mut again = [0u8; 64];
    let a = block_on(service.read_stream(info.id, READ_FILE_ID, 20, &mut first)).unwrap();
    let b = block_on(service.read_stream(info.id, READ_FILE_ID, 20, &mut again)).unwrap();
    assert_eq!((a.len, a.eof), (b.len, b.eof));
    assert_eq!(first[..a.len], again[..b.len]);
    let size = info.files[0].size_bytes;
    let end = block_on(service.read_stream(info.id, READ_FILE_ID, size, &mut first)).unwrap();
    assert_eq!((end.len, end.eof), (0, true));
    assert_eq!(
        block_on(service.read_stream(info.id, READ_FILE_ID, size + 1, &mut first)),
        Err(ActionError::InvalidArgument)
    );
    assert_eq!(
        block_on(service.read_stream(info.id, 9, 0, &mut first)),
        Err(ActionError::UnknownStream)
    );
    assert_eq!(
        block_on(service.read_stream(info.id + 7, READ_FILE_ID, 0, &mut first)),
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
    let second = stream_of(invoke(&logging, action::READ, &args(0, 5)));
    assert_eq!(
        invoke(&logging, action::DOWNLOAD, &[]).err(),
        Some(ActionError::Busy)
    );
    assert_eq!(
        invoke(&logging, action::READ, &args(0, 5)).err(),
        Some(ActionError::Busy)
    );
    block_on(logging.service().close_stream(second.id)).unwrap();
    assert!(invoke(&logging, action::READ, &args(0, 5)).is_ok());
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
    assert_eq!(
        invoke(&logging, action::READ, &args(0, 5)).err(),
        Some(ActionError::Unavailable)
    );
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
fn clear_mutates_and_read_does_not() {
    let actions = <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 4096> as Action>::ACTIONS;
    let access = |id| {
        actions
            .iter()
            .find(|action| action.id == id)
            .unwrap()
            .access
    };
    assert_eq!(access(action::CLEAR), ActionAccess::Mutate);
    assert_eq!(access(action::READ), ActionAccess::Read);
    assert_eq!(access(action::DOWNLOAD), ActionAccess::Read);
}

#[test]
fn bad_arguments_are_refused() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    assert_eq!(
        invoke(&logging, action::READ, &[(read_arg::AFTER, Value::U64(0))]).err(),
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
fn a_damaged_segment_fails_the_read_that_needs_it_and_says_so() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    let newest = write_history(&rig, &logging, 60);
    assert!(read(&logging, 0, 200).len() > 20);

    // The oldest segment starts at sector 0: damage a byte of its first
    // LZ4 block (frame header 7 bytes, then the block).
    *rig.internal.0.borrow_mut().raw(16 + 20) ^= 0x10;

    let failed = invoke(&logging, action::READ, &args(0, 200));
    assert_eq!(failed.err(), Some(ActionError::Rejected(READ_CORRUPT)));
    assert_eq!(issues(&logging), [issue::READ_FAILED]);
    assert_eq!(summary(&logging).1, ServiceHealth::Degraded);

    // Pagination is not an error: a small limit stops cleanly after the
    // records it has.
    let info = stream_of(invoke(&logging, action::READ, &args(newest - 3, 2)));
    block_on(logging.service().close_stream(info.id)).unwrap();
    assert!(issues(&logging).is_empty());
}

/// What a following client sees: the history the records come from and the
/// records after its cursor.
fn read_in_history(logging: &TestLogging<'_>, after: u64) -> (u32, Vec<LogRecord>) {
    let info = stream_of(invoke(logging, action::READ, &args(after, 200)));
    let history = history_of_read_file(info.files[0].name.as_str()).unwrap();
    let data = read_all(logging, &info, READ_FILE_ID);
    block_on(logging.service().close_stream(info.id)).unwrap();
    (history, parse(&data))
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
fn clear_releases_one_and_two_open_streams() {
    for downloads in [false, true] {
        let rig = Rig::new();
        let logging = rig.logging(CONFIG);
        many_segments(&rig, &logging);

        let mut streams = Vec::new();
        if downloads {
            streams.push(stream_of(invoke(&logging, action::DOWNLOAD, &[])).id);
            assert!(logging.pinned_segments() > 0);
        }
        streams.push(stream_of(invoke(&logging, action::READ, &args(0, 5))).id);
        if !downloads {
            streams.push(stream_of(invoke(&logging, action::READ, &args(0, 9))).id);
        }
        assert_eq!(logging.open_streams(), 2);

        assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
        assert_eq!((logging.open_streams(), logging.pins()), (0, 0));
        assert_eq!(logging.pinned_segments(), 0);
        assert!(streams.iter().all(|stream| is_gone(&logging, *stream)));
        // The table is free again.
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
    let read = stream_of(invoke(&logging, action::READ, &args(0, 5)));
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
        let records = stream_of(invoke(&logging, action::READ, &args(0, 5)));
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
        let records = stream_of(invoke(&logging, action::READ, &args(0, 3)));
        assert!(
            invoke(&logging, action::CLEAR, &[]).is_ok(),
            "round {round}"
        );
        assert!(
            invoke(&logging, action::CLEAR, &[]).is_ok(),
            "round {round}"
        );
        assert_eq!((logging.open_streams(), logging.pinned_segments()), (0, 0));
        assert!(is_gone(&logging, download.id) && is_gone(&logging, records.id));
        assert!(read_in_history(&logging, 0).1.is_empty());

        log_many(&logging, 3, 1);
        settle(&rig, &logging);
        assert_eq!(read(&logging, 0, 50).len(), 3);
    }
}
