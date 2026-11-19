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
    Action, ActionAccess, ActionError, ActionRegistry, ActionResult, MAX_STREAM_CHUNK,
    MAX_TAIL_CHUNK, Service, ServiceHealth, ServiceState, Status, StatusRegistry, StatusWriter,
    Statuses, TailPage, TailRequest, Value, WriteError,
};
use rohi_time::UnixTime;

#[cfg(feature = "lz4")]
use crate::storage::MAX_SEGMENTS;
use crate::storage::{LogStorage, Medium, RamMedium, RingStorage};
use crate::*;

const SECTORS: usize = 32;

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

/// The name suffix of a downloaded file of the codec of the build.
#[cfg(feature = "lz4")]
const FILE_EXTENSION: &str = ".log.lz4";
#[cfg(not(feature = "lz4"))]
const FILE_EXTENSION: &str = ".log";

/// The text of a downloaded file: LZ4 frames are decoded with the reference implementation.
fn file_text(data: &[u8]) -> std::string::String {
    #[cfg(feature = "lz4")]
    {
        let mut text = std::string::String::new();
        std::io::Read::read_to_string(&mut lz4_flex::frame::FrameDecoder::new(data), &mut text)
            .unwrap();
        text
    }
    #[cfg(not(feature = "lz4"))]
    {
        std::string::String::from_utf8(data.to_vec()).unwrap()
    }
}

const CONFIG: LoggingConfig = LoggingConfig {
    level: LogLevel::Info,
    storage: LogStorageKind::Internal,
    compression: crate::DEFAULT_COMPRESSION,
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
        Some(Value::text(crate::DEFAULT_COMPRESSION.name()).unwrap())
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
    // Room for the history of the download and for new records next to it, which plain
    // text (a bigger history than a compressed one) needs more of.
    let logging = rig.logging(LoggingConfig {
        retention_bytes: 4096 * 12,
        ..CONFIG
    });
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
        assert_eq!(name, &std::format!("rohi{}{FILE_EXTENSION}", index + 1));
        assert_eq!(info.files[index].id, index as u32 + 1);
    }
    assert_eq!(logging.pins(), names.len());

    // Every file is a standard LZ4 frame of text lines; together they are the
    // whole history in order.
    let mut records = Vec::new();
    for file in &info.files {
        let data = read_all(&logging, &info, file.id);
        assert_eq!(data.len() as u64, file.size_bytes);
        records.extend(parse(file_text(&data).as_bytes()));
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
    // The tail is no stream: it needs no slot of the table.
    assert_eq!(tail_page(&logging, 0, 0, MAX_TAIL_CHUNK).1.len(), 5);
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
    assert!(!tail_page(&logging, 0, 0, MAX_TAIL_CHUNK).1.is_empty());
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

#[cfg(feature = "lz4")]
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
fn clear_mutates_and_download_reads() {
    let actions = <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 8> as Action>::ACTIONS;
    let access = |id| {
        actions
            .iter()
            .find(|action| action.id == id)
            .unwrap()
            .access
    };
    assert_eq!(access(action::CLEAR), ActionAccess::Mutate);
    assert_eq!(access(action::DOWNLOAD), ActionAccess::Read);
}

#[test]
fn bad_arguments_are_refused() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
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

#[cfg(feature = "lz4")]
fn write_history(rig: &Rig, logging: &TestLogging<'_>, batches: u64) -> u64 {
    let mut next = 1;
    for _ in 0..batches {
        log_many(logging, 6, next);
        next += 6;
        settle(rig, logging);
    }
    next - 1
}

#[cfg(feature = "lz4")]
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
    assert_eq!(tailed(&logging, 0, 0).len(), 8);
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

#[cfg(feature = "lz4")]
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
fn clear_is_busy_while_a_download_is_open_and_changes_nothing() {
    for downloads in [1, 2] {
        let rig = Rig::new();
        let logging = rig.logging(CONFIG);
        many_segments(&rig, &logging);
        let stored = number(&logging, status_field::STORED_RECORDS);
        let history = number(&logging, status_field::HISTORY);
        let oldest = number(&logging, status_field::OLDEST_SEQUENCE);

        let mut streams = Vec::new();
        for _ in 0..downloads {
            streams.push(stream_of(invoke(&logging, action::DOWNLOAD, &[])));
        }
        let pinned = logging.pinned_segments();
        assert!(pinned > 0);

        // Refused, repeatedly, with the archive, the downloads and the pins intact.
        for _ in 0..3 {
            assert_eq!(
                invoke(&logging, action::CLEAR, &[]).err(),
                Some(ActionError::Busy)
            );
        }
        assert_eq!(logging.open_streams(), downloads);
        assert_eq!(logging.pinned_segments(), pinned);
        assert!(streams.iter().all(|stream| !is_gone(&logging, stream.id)));
        assert_eq!(number(&logging, status_field::STORED_RECORDS), stored);
        assert_eq!(number(&logging, status_field::HISTORY), history);
        assert_eq!(number(&logging, status_field::OLDEST_SEQUENCE), oldest);
        // The download is still readable, whole.
        for stream in &streams {
            let data = read_all(&logging, stream, 1);
            assert_eq!(data.len() as u64, stream.files[0].size_bytes);
        }
        assert!(issues(&logging).is_empty());
    }
}

#[test]
fn clear_works_once_the_last_download_is_closed() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let first = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let second = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let service = logging.service();

    block_on(service.close_stream(first.id)).unwrap();
    assert_eq!(
        invoke(&logging, action::CLEAR, &[]).err(),
        Some(ActionError::Busy)
    );
    block_on(service.close_stream(second.id)).unwrap();
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
    assert_eq!((logging.open_streams(), logging.pinned_segments()), (0, 0));
    // The download table is free again.
    let again = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    block_on(service.close_stream(again.id)).unwrap();
}

#[test]
fn clear_works_after_an_idle_download_expired_before_the_task_noticed() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let download = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    // Not yet expired: refused.
    rig.advance(STREAM_TIMEOUT_MS - 1_000);
    assert_eq!(
        invoke(&logging, action::CLEAR, &[]).err(),
        Some(ActionError::Busy)
    );
    // Expired, and the logging task has not run to sweep it: the clear does.
    rig.advance(2_000);
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    assert!(is_gone(&logging, download.id));
    assert_eq!((logging.open_streams(), logging.pinned_segments()), (0, 0));
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
}

#[test]
fn a_read_keeps_a_download_alive_and_so_keeps_clear_waiting() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let download = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    for _ in 0..4 {
        rig.advance(STREAM_TIMEOUT_MS * 6 / 10);
        let mut chunk = [0u8; 16];
        block_on(logging.service().read_stream(download.id, 1, 0, &mut chunk)).unwrap();
        assert_eq!(
            invoke(&logging, action::CLEAR, &[]).err(),
            Some(ActionError::Busy)
        );
    }
    block_on(logging.service().close_stream(download.id)).unwrap();
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
}

#[test]
fn clear_works_after_the_link_that_held_a_download_was_lost() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let download = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    logging.service().link_lost();
    // Whether or not the task has run since.
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    assert!(is_gone(&logging, download.id));
    assert_eq!((logging.open_streams(), logging.pinned_segments()), (0, 0));
}

#[test]
fn a_download_and_a_clear_started_together_do_not_interleave() {
    use embassy_futures::join::join;
    for download_first in [true, false] {
        let rig = Rig::new();
        let logging = rig.logging(CONFIG);
        many_segments(&rig, &logging);
        let before = number(&logging, status_field::STORED_RECORDS).unwrap();
        assert!(before > 0);
        let service = logging.service();
        let registry = (&service,);
        let id = <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 8> as Service>::ID;
        let download = registry.invoke(id, action::DOWNLOAD, &[]);
        let clear = registry.invoke(id, action::CLEAR, &[]);
        let (download, clear) = if download_first {
            block_on(join(download, clear))
        } else {
            let (clear, download) = block_on(join(clear, download));
            (download, clear)
        };
        let download = match download.unwrap() {
            ActionResult::Stream(info) => info,
            other => panic!("{other:?}"),
        };
        if download_first {
            // The download got the archive first: the clear waits and nothing is lost.
            assert_eq!(clear.err(), Some(ActionError::Busy));
            assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(before));
            assert_eq!(download.files.len(), logging.pins());
            assert!(!download.files.is_empty());
            block_on(logging.service().close_stream(download.id)).unwrap();
        } else {
            // The clear came first: the download then sees the empty archive.
            assert!(clear.is_ok());
            assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
            assert!(download.files.is_empty());
            block_on(logging.service().close_stream(download.id)).unwrap();
        }
        assert_eq!((logging.open_streams(), logging.pinned_segments()), (0, 0));
    }
}

#[test]
fn concurrent_download_attempts_are_bounded_and_block_the_clear_until_closed() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let first = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let second = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    for _ in 0..3 {
        assert_eq!(
            invoke(&logging, action::DOWNLOAD, &[]).err(),
            Some(ActionError::Busy)
        );
    }
    assert_eq!(
        invoke(&logging, action::CLEAR, &[]).err(),
        Some(ActionError::Busy)
    );
    assert_eq!(logging.open_streams(), 2);
    let service = logging.service();
    block_on(service.close_stream(first.id)).unwrap();
    block_on(service.close_stream(second.id)).unwrap();
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
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
    let other = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    assert_eq!(logging.open_streams(), 2);

    logging.service().link_lost();
    block_on(logging.step());
    assert_eq!((logging.open_streams(), logging.pins()), (0, 0));
    assert_eq!(logging.pinned_segments(), 0);
    assert!(is_gone(&logging, download.id) && is_gone(&logging, other.id));
}

#[test]
fn closing_streams_in_any_order_leaves_the_other_one_intact() {
    for close_first_one_first in [true, false] {
        let rig = Rig::new();
        let logging = rig.logging(CONFIG);
        many_segments(&rig, &logging);
        let first = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
        let second = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
        let pinned = logging.pinned_segments();
        assert_eq!(pinned, first.files.len());
        let expected = read_all(&logging, &first, 1);
        let service = logging.service();

        if close_first_one_first {
            block_on(service.close_stream(first.id)).unwrap();
            assert_eq!(logging.pinned_segments(), pinned);
            assert!(!read_all(&logging, &second, 1).is_empty());
            block_on(service.close_stream(second.id)).unwrap();
        } else {
            block_on(service.close_stream(second.id)).unwrap();
            assert_eq!(logging.pinned_segments(), pinned);
            assert_eq!(read_all(&logging, &first, 1), expected);
            block_on(service.close_stream(first.id)).unwrap();
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
        assert_eq!(
            invoke(&logging, action::CLEAR, &[]).err(),
            Some(ActionError::Busy)
        );
        block_on(logging.service().close_stream(download.id)).unwrap();
        assert!(
            invoke(&logging, action::CLEAR, &[]).is_ok(),
            "round {round}"
        );
        assert!(
            invoke(&logging, action::CLEAR, &[]).is_ok(),
            "round {round}"
        );
        assert_eq!(logging.pinned_segments(), 0);
        assert!(is_gone(&logging, download.id));
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
        for _ in 0..60 {
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

// The RAM tail: the `TailLogs` request.

/// A page of the tail through the registry, the way the controller asks for it.
fn tail_page(
    logging: &TestLogging<'_>,
    session: u32,
    after: u64,
    room: usize,
) -> (TailPage, Vec<LogRecord>) {
    let service = logging.service();
    let registry = (&service,);
    let mut buf = std::vec![0u8; room];
    let page = registry
        .tail_logs(
            <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 8> as Service>::ID,
            TailRequest { session, after },
            &mut buf,
        )
        .unwrap();
    let text = std::str::from_utf8(&buf[..page.len]).unwrap();
    assert!(text.is_empty() || text.ends_with('\n'), "whole lines only");
    (page, parse(text.as_bytes()))
}

/// The records of the pages of `session` after `after`, until an empty page.
fn tailed(logging: &TestLogging<'_>, session: u32, after: u64) -> Vec<LogRecord> {
    let mut all = Vec::new();
    let mut cursor = after;
    let mut session = session;
    for _ in 0..100 {
        let (page, records) = tail_page(logging, session, cursor, MAX_TAIL_CHUNK);
        assert!(!page.reset, "unexpected reset");
        if records.is_empty() {
            return all;
        }
        // The session of the first page is the one to follow.
        session = page.session;
        cursor = page.cursor;
        all.extend(records);
    }
    panic!("the tail does not end");
}

/// The least page the service serves.
const MIN_PAGE: usize = segment::text::MIN_WITHIN_LEN;

fn sequences(records: &[LogRecord]) -> Vec<u64> {
    records.iter().map(|record| record.sequence).collect()
}

#[test]
fn the_first_page_is_the_oldest_records_held_in_order_with_the_session_of_the_boot() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_session(7);
    log_many(&logging, 20, 1);
    let (page, records) = tail_page(&logging, 0, 0, MAX_TAIL_CHUNK);
    assert_eq!(page.session, 7);
    assert_eq!((page.reset, page.gap), (false, 0));
    let capacity = logging.sink().tail_capacity() as u64;
    assert_eq!(
        (page.oldest_sequence, page.newest_sequence),
        (21 - capacity, 20)
    );
    // Whatever fits one page, from the oldest record held.
    assert_eq!(records[0].sequence, 21 - capacity);
    assert_eq!(page.cursor, records.last().unwrap().sequence);
    assert!(
        records[0]
            .message
            .contains(&std::format!("record {} ", 21 - capacity))
    );
    // The pages, followed to the end, are exactly the ring.
    assert_eq!(
        sequences(&tailed(&logging, 7, 0)),
        (21 - capacity..=20).collect::<Vec<_>>()
    );
    assert_eq!(logging.sink().tail_session(), 7);
}

#[test]
fn a_session_is_never_zero() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log(&logging, "one");
    assert_eq!(tail_page(&logging, 0, 0, MAX_TAIL_CHUNK).0.session, 1);
    logging.set_session(0);
    assert_eq!(logging.sink().tail_session(), 1);
}

#[test]
fn following_returns_each_record_once_and_an_idle_poll_is_an_empty_page() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_session(3);
    let (idle, records) = tail_page(&logging, 0, 0, MAX_TAIL_CHUNK);
    assert!(records.is_empty());
    assert_eq!(
        (idle.session, idle.len, idle.cursor, idle.reset),
        (3, 0, 0, false)
    );
    assert_eq!((idle.oldest_sequence, idle.newest_sequence), (1, 0));
    let mut seen = 0;
    let mut all = Vec::new();
    for round in 0..6 {
        log_many(&logging, 2 + round, round * 10);
        for record in tailed(&logging, 3, seen) {
            assert!(record.sequence > seen, "duplicate {}", record.sequence);
            seen = record.sequence;
            all.push(record);
        }
        let (empty, records) = tail_page(&logging, 3, seen, MAX_TAIL_CHUNK);
        assert!(records.is_empty());
        assert_eq!((empty.cursor, empty.reset, empty.gap), (seen, false, 0));
    }
    assert_eq!(sequences(&all), (1..=all.len() as u64).collect::<Vec<_>>());
    assert_eq!(all.len(), (0..6).map(|r| 2 + r).sum::<u64>() as usize);
}

#[test]
fn a_page_carries_at_most_its_buffer_and_the_next_one_continues() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_session(3);
    for n in 0..8 {
        log(&logging, &std::format!("{n} {}", "y".repeat(100)));
    }
    let first = tail_page(&logging, 0, 0, MAX_TAIL_CHUNK);
    let line = segment::text::line_len(first.1.last().unwrap());
    assert!(line * 2 > MIN_PAGE);
    let mut cursor = 0;
    let mut pages = 0;
    let mut seen = Vec::new();
    loop {
        // Room for two lines and a bit.
        let (page, records) = tail_page(&logging, 3, cursor, line * 2 + line / 2);
        if records.is_empty() {
            break;
        }
        assert!(records.len() <= 2, "{}", records.len());
        seen.extend(sequences(&records));
        cursor = page.cursor;
        pages += 1;
    }
    assert_eq!(seen, (1..=8).collect::<Vec<_>>());
    assert!(pages >= 4, "{pages}");
}

#[test]
fn an_overwritten_cursor_is_a_gap_and_the_page_continues_at_the_oldest_record() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_session(3);
    log_many(&logging, 30, 1);
    let capacity = logging.sink().tail_capacity() as u64;
    let (page, records) = tail_page(&logging, 3, 2, MAX_TAIL_CHUNK);
    assert!(!page.reset);
    assert_eq!(records[0].sequence, 30 - capacity + 1);
    // Records 3.. up to the oldest held are gone, and the page says so.
    assert_eq!(page.gap, 30 - capacity - 2);
    // Following on from the page there is no gap.
    let (next, _) = tail_page(&logging, 3, page.cursor, MAX_TAIL_CHUNK);
    assert_eq!(next.gap, 0);
}

#[test]
fn another_boot_or_a_cursor_from_the_future_is_a_reset_with_the_start_of_the_snapshot() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_session(5);
    log_many(&logging, 4, 1);
    let (page, records) = tail_page(&logging, 4, 3, MAX_TAIL_CHUNK);
    assert_eq!((page.session, page.reset), (5, true));
    assert_eq!(sequences(&records), [1, 2, 3, 4]);
    let (page, records) = tail_page(&logging, 5, 99, MAX_TAIL_CHUNK);
    assert!(page.reset);
    assert_eq!(sequences(&records), [1, 2, 3, 4]);
    let empty = rig.logging(CONFIG);
    empty.set_session(6);
    let (page, records) = tail_page(&empty, 5, 3, MAX_TAIL_CHUNK);
    assert!(records.is_empty());
    assert_eq!((page.session, page.reset, page.cursor), (6, true, 0));
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
        let _ = tail_page(&logging, 0, after, MAX_TAIL_CHUNK);
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
    assert_eq!(tailed(&logging, 0, 0).len(), 3);

    // A medium that fails.
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    step(&rig, &logging);
    rig.internal.0.borrow_mut().fail_after(0);
    log_many(&logging, 3, 1);
    step(&rig, &logging);
    step(&rig, &logging);
    assert_eq!(tailed(&logging, 0, 0).len(), 3);

    // The archive is pinned by a download.
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let download = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    log_many(&logging, 2, 1_000);
    assert!(
        tailed(&logging, 0, 0)
            .last()
            .unwrap()
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
    let records = tailed(&logging, 0, 0);
    assert_eq!(records.len(), 8);
    assert_eq!(records.last().unwrap().message.as_str(), "burst 99");
    // The tail has its own sequence: the archive copy of a dropped record
    // does not exist, the tail copy does.
    assert_eq!(records.last().unwrap().sequence, 100);
}

#[test]
fn clear_empties_the_archive_and_neither_the_tail_nor_its_session() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    logging.set_session(2);
    log_many(&logging, 4, 1);
    let before = tail_page(&logging, 2, 0, MAX_TAIL_CHUNK);
    settle(&rig, &logging);
    assert!(invoke(&logging, action::CLEAR, &[]).is_ok());
    assert_eq!(number(&logging, status_field::STORED_RECORDS), Some(0));
    let after = tail_page(&logging, 2, 0, MAX_TAIL_CHUNK);
    assert_eq!(sequences(&after.1), [1, 2, 3, 4]);
    assert_eq!(sequences(&before.1), sequences(&after.1));
    assert_eq!((after.0.session, after.0.reset), (2, false));
    assert_eq!(logging.sink().tail_session(), 2);
}

#[test]
fn the_tail_needs_no_stream_and_leaves_the_download_table_alone() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    many_segments(&rig, &logging);
    let first = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    let second = stream_of(invoke(&logging, action::DOWNLOAD, &[]));
    assert_eq!(
        invoke(&logging, action::DOWNLOAD, &[]).err(),
        Some(ActionError::Busy)
    );
    // Both downloads are open and the tail answers, however often.
    for _ in 0..10 {
        assert_eq!(tailed(&logging, 0, 0).len(), 8);
    }
    assert_eq!(
        (logging.pins(), logging.open_streams()),
        (first.files.len() + second.files.len(), 2)
    );
}

#[test]
fn the_tail_is_not_an_action_and_its_old_id_stays_unused() {
    let actions = <LoggingService<'_, NoopRawMutex, &Clock, Test, 64, 8> as Action>::ACTIONS;
    assert!(actions.iter().all(|action| action.id != 4));
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    assert_eq!(
        invoke(&logging, 4, &[]).err(),
        Some(ActionError::UnknownAction)
    );
}

#[test]
fn a_buffer_too_small_for_a_line_is_refused() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    log(&logging, "one");
    let service = logging.service();
    let mut buf = [0u8; 16];
    assert_eq!(
        service.tail_logs(
            TailRequest {
                session: 0,
                after: 0
            },
            &mut buf
        ),
        Err(ActionError::InvalidArgument)
    );
    // A buffer bigger than a page is used up to a page.
    let mut big = [0u8; MAX_TAIL_CHUNK * 2];
    let page = service
        .tail_logs(
            TailRequest {
                session: 0,
                after: 0,
            },
            &mut big,
        )
        .unwrap();
    assert!(page.len > 0 && page.len <= MAX_TAIL_CHUNK);
}

#[test]
fn the_worst_case_line_is_shortened_whole_and_every_page_makes_progress() {
    let rig = Rig::new();
    let logging = rig.logging(CONFIG);
    let message = "\u{1}".repeat(MAX_MESSAGE_LEN);
    let target = "\u{2}".repeat(MAX_TARGET_LEN);
    for _ in 0..3 {
        assert!(
            logging
                .sink()
                .push(log::Level::Error, &target, format_args!("{message}"))
        );
    }
    let mut cursor = 0;
    let mut session = 0;
    let mut lines = 0;
    for _ in 0..3 {
        let (page, records) = tail_page(&logging, session, cursor, MAX_TAIL_CHUNK);
        session = page.session;
        assert!(page.len <= MAX_TAIL_CHUNK);
        assert_eq!(records.len(), 1, "one oversized line fills a page");
        // Shortened, flagged, still a valid record of control characters.
        assert!(records[0].truncated);
        assert!(records[0].message.len() < MAX_MESSAGE_LEN);
        assert!(records[0].message.chars().all(|c| c == '\u{1}'));
        assert!(page.cursor > cursor);
        cursor = page.cursor;
        lines += 1;
    }
    assert_eq!(lines, 3);
    assert!(
        tail_page(&logging, session, cursor, MAX_TAIL_CHUNK)
            .1
            .is_empty()
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
    assert!(tailed(&logging, 0, 0).is_empty());
    log(&logging, "kept");
    let kept = tailed(&logging, 0, 0);
    assert_eq!(texts(&kept), ["kept"]);
    // The tail sequence is its own: the archive assigns another one later.
    settle(&rig, &logging);
    assert_eq!(texts(&read(&logging, 0, 10)), ["kept"]);
}

#[test]
fn the_tail_survives_a_reboot_only_as_a_new_session() {
    let rig = Rig::new();
    let session = {
        let logging = rig.logging(CONFIG);
        logging.set_session(11);
        log_many(&logging, 3, 1);
        tail_page(&logging, 0, 0, MAX_TAIL_CHUNK).0.session
    };
    let logging = rig.logging(CONFIG);
    logging.set_session(12);
    // The client reconnects with the old session: it must start over.
    let (page, records) = tail_page(&logging, session, 3, MAX_TAIL_CHUNK);
    assert!(records.is_empty());
    assert_eq!((page.session, page.reset), (12, true));
}

// What the ESP32-C3 log keeps: a 128 KiB partition, two format header sectors,
// 8 KiB segments, plain text. The numbers are measured on the emulated medium
// (`cargo test -p rohi-logging retention -- --nocapture` prints them); they
// depend on the flush pattern because a batch is the unit of a write.
const C3_CONFIG: LoggingConfig = LoggingConfig {
    retention_bytes: 128 * 1024,
    segment_bytes: 8 * 1024,
    compression: Compression::None,
    flush_interval_ms: 1_000,
    ..CONFIG
};

// A message that makes a whole persisted line `line_len` bytes long.
fn message_for(line_len: usize, logging: &TestLogging<'_>) -> std::string::String {
    assert!(log(logging, "x"));
    let record = logging.sink().try_recv().unwrap();
    let mut line = std::string::String::new();
    crate::segment::text::write_line(&mut line, &record).unwrap();
    // The line ends with a newline and holds one character of message.
    "x".repeat(line_len - (line.len() - 1))
}

struct Retained {
    records: usize,
    payload: usize,
    segments: usize,
}

fn retained(logging: &TestLogging<'_>) -> Retained {
    let records = logging.archive(0, u64::MAX).expect("the archive decodes");
    let payload = records
        .iter()
        .map(|record| {
            let mut line = std::string::String::new();
            crate::segment::text::write_line(&mut line, record).unwrap();
            line.len()
        })
        .sum();
    Retained {
        records: records.len(),
        payload,
        segments: number(logging, status_field::SEGMENTS).unwrap() as usize,
    }
}

fn fill_c3(line_len: usize, flush_every: u64) -> (Rig, Retained) {
    let rig = Rig::new();
    let logging = rig.logging(C3_CONFIG);
    let message = message_for(line_len, &logging);
    let mut written = 0u64;
    // Three times what the ring holds.
    while written * line_len as u64 / 128 / 1024 < 3 {
        for _ in 0..flush_every {
            assert!(log(&logging, &message));
            written += 1;
        }
        settle(&rig, &logging);
    }
    let kept = retained(&logging);
    (rig, kept)
}

#[test]
fn the_esp32c3_log_retention_is_measured() {
    std::println!("line_len flush_every segments records payload_bytes (of 122880 in 15 segments)");
    for line_len in [80usize, 100, 120, 160] {
        for flush_every in [1u64, 6, 20, 64] {
            let (rig, kept) = fill_c3(line_len, flush_every);
            std::println!(
                "{line_len:>8} {flush_every:>11} {:>8} {:>7} {:>13}",
                kept.segments,
                kept.records,
                kept.payload
            );
            // The ring holds 15 segments of 8 KiB; a segment seals once a batch of
            // up to 1 KiB would not fit. Whatever the flush pattern, at least
            // 95 KiB of the 120 KiB are kept as text (98 KiB at the least, measured).
            assert!(
                kept.payload > 95 * 1024,
                "{line_len} {flush_every}: {}",
                kept.payload
            );
            assert!(
                kept.segments >= 13,
                "{line_len} {flush_every}: {}",
                kept.segments
            );

            // A reboot keeps what was there.
            let again = rig.logging(C3_CONFIG);
            step(&rig, &again);
            let after = retained(&again);
            assert_eq!(after.records, kept.records, "{line_len} {flush_every}");
        }
    }
}
