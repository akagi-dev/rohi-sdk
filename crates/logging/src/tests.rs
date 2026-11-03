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
use core::cell::Cell;
use core::sync::atomic::{AtomicU64, Ordering};

use embassy_futures::block_on;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use log::Log;
use rohi_hal::MonotonicTimestamp;
use rohi_hal::runtime::Clock;
use rohi_time::{TimeConfig, TimeSample, TimeService, TimeSourceKind, UnixTime};

use crate::*;

struct Manual {
    monotonic: Cell<u64>,
    wall: Cell<Option<UnixTime>>,
}

impl Manual {
    fn new() -> Self {
        Self {
            monotonic: Cell::new(0),
            wall: Cell::new(None),
        }
    }
}

impl LogClock for &Manual {
    fn monotonic_ms(&self) -> u64 {
        self.monotonic.get()
    }

    fn wall_time(&self) -> Option<UnixTime> {
        self.wall.get()
    }
}

type Sink<'a, const N: usize> = LogSink<NoopRawMutex, &'a Manual, N>;

fn push(sink: &Sink<'_, 4>, level: log::Level, message: &str) -> bool {
    sink.push(level, "test", format_args!("{message}"))
}

#[test]
fn the_level_decides_what_is_kept() {
    for (level, kept) in [
        (LogLevel::Off, [false, false, false, false]),
        (LogLevel::Error, [true, false, false, false]),
        (LogLevel::Warn, [true, true, false, false]),
        (LogLevel::Info, [true, true, true, false]),
        (LogLevel::Debug, [true, true, true, true]),
    ] {
        for (log_level, expected) in [
            log::Level::Error,
            log::Level::Warn,
            log::Level::Info,
            log::Level::Debug,
        ]
        .into_iter()
        .zip(kept)
        {
            assert_eq!(level.keeps(log_level), expected, "{level:?} {log_level}");
        }
        assert!(!level.keeps(log::Level::Trace));
    }
}

#[test]
fn the_default_level_is_warn_and_numbers_round_trip() {
    assert_eq!(LogLevel::default(), LogLevel::Warn);
    for value in 0..=4 {
        assert_eq!(LogLevel::from_u8(value).unwrap() as u8, value);
    }
    assert_eq!(LogLevel::from_u8(5), None);
}

#[test]
fn a_level_change_applies_to_the_next_call_without_a_restart() {
    let clock = Manual::new();
    let sink = Sink::<4>::new(&clock, LogLevel::Warn);
    assert!(!push(&sink, log::Level::Debug, "before"));
    sink.set_level(LogLevel::Debug);
    assert!(push(&sink, log::Level::Debug, "after"));
    sink.set_level(LogLevel::Off);
    assert!(!push(&sink, log::Level::Error, "off"));
    assert_eq!(sink.try_recv().unwrap().message.as_str(), "after");
    assert!(sink.try_recv().is_none());
}

#[test]
fn a_disabled_level_neither_formats_nor_queues() {
    struct Panics;
    impl core::fmt::Display for Panics {
        fn fmt(&self, _: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            panic!("formatted a filtered record");
        }
    }
    let clock = Manual::new();
    let sink = Sink::<4>::new(&clock, LogLevel::Warn);
    assert!(!sink.push(log::Level::Debug, "test", format_args!("{}", Panics)));
    assert_eq!(sink.queued(), 0);
    assert_eq!(sink.dropped(), 0);
}

#[test]
fn a_full_queue_drops_the_new_record_and_counts_it() {
    let clock = Manual::new();
    let sink = Sink::<4>::new(&clock, LogLevel::Info);
    for index in 0..4 {
        assert!(sink.push(log::Level::Info, "test", format_args!("{index}")));
    }
    assert!(!push(&sink, log::Level::Error, "lost"));
    assert!(!push(&sink, log::Level::Error, "lost"));
    assert_eq!(sink.dropped(), 2);
    // The oldest records survive, in order.
    let kept: std::vec::Vec<_> = core::iter::from_fn(|| sink.try_recv())
        .map(|record| record.message)
        .collect();
    assert_eq!(kept, ["0", "1", "2", "3"]);
    assert!(push(&sink, log::Level::Info, "again"));
}

#[test]
fn a_record_carries_the_time_of_the_event() {
    let clock = Manual::new();
    let sink = Sink::<4>::new(&clock, LogLevel::Info);
    clock.monotonic.set(1_000);
    push(&sink, log::Level::Info, "first");
    clock.monotonic.set(2_000);
    clock.wall.set(Some(UnixTime::from_secs(1_800_000_000)));
    push(&sink, log::Level::Warn, "second");
    // Time moves on before the task takes the records: the stamps stay.
    clock.monotonic.set(9_000);
    clock.wall.set(None);

    let first = sink.try_recv().unwrap();
    assert_eq!((first.monotonic_ms, first.wall_time), (1_000, None));
    let second = sink.try_recv().unwrap();
    assert_eq!(second.monotonic_ms, 2_000);
    assert_eq!(second.wall_time, Some(UnixTime::from_secs(1_800_000_000)));
    assert_eq!(second.level, LogLevel::Warn);
    assert_eq!(second.sequence, 0);
}

#[test]
fn long_text_is_cut_at_a_character_boundary_and_flagged() {
    let record = LogRecord::new(
        LogLevel::Info,
        "ok",
        format_args!("{}", "é".repeat(MAX_MESSAGE_LEN)),
        0,
        None,
    );
    assert!(record.truncated);
    assert_eq!(record.message.len(), MAX_MESSAGE_LEN);
    assert!(record.message.chars().all(|c| c == 'é'));
    assert_eq!(record.target.as_str(), "ok");

    let long_target = "t".repeat(MAX_TARGET_LEN + 5);
    let record = LogRecord::new(LogLevel::Info, &long_target, format_args!("m"), 0, None);
    assert!(record.truncated);
    assert_eq!(record.target.len(), MAX_TARGET_LEN);

    let record = LogRecord::new(LogLevel::Info, "t", format_args!("fits"), 0, None);
    assert!(!record.truncated);
}

#[test]
fn a_failing_display_keeps_what_was_written() {
    struct Fails;
    impl core::fmt::Display for Fails {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("partial")?;
            Err(core::fmt::Error)
        }
    }
    let record = LogRecord::new(LogLevel::Info, "t", format_args!("{Fails}"), 0, None);
    assert_eq!(record.message.as_str(), "partial");
}

#[test]
fn the_logging_task_never_feeds_itself() {
    let clock = Manual::new();
    let sink = Sink::<4>::new(&clock, LogLevel::Debug);
    assert!(!sink.push(
        log::Level::Error,
        "rohi_logging::storage",
        format_args!("write failed")
    ));
    assert_eq!(sink.queued(), 0);
    assert_eq!(sink.dropped(), 0);
}

#[test]
fn the_task_waits_for_records() {
    let clock = Manual::new();
    let sink = Sink::<4>::new(&clock, LogLevel::Info);
    push(&sink, log::Level::Info, "queued");
    assert_eq!(block_on(sink.recv()).message.as_str(), "queued");
}

struct Atomic(AtomicU64);

impl LogClock for Atomic {
    fn monotonic_ms(&self) -> u64 {
        self.0.fetch_add(1, Ordering::Relaxed)
    }

    fn wall_time(&self) -> Option<UnixTime> {
        None
    }
}

#[test]
fn the_sink_is_a_log_backend() {
    let sink: &'static LogSink<CriticalSectionRawMutex, Atomic, 4> = std::boxed::Box::leak(
        std::boxed::Box::new(LogSink::new(Atomic(AtomicU64::new(10)), LogLevel::Info)),
    );
    let metadata = |level| log::Metadata::builder().level(level).target("wifi").build();
    assert!(sink.enabled(&metadata(log::Level::Warn)));
    assert!(!sink.enabled(&metadata(log::Level::Debug)));

    sink.log(
        &log::Record::builder()
            .level(log::Level::Warn)
            .target("wifi")
            .args(format_args!("connected to {}", "home"))
            .build(),
    );
    let record = sink.try_recv().unwrap();
    assert_eq!(record.target.as_str(), "wifi");
    assert_eq!(record.message.as_str(), "connected to home");
    assert_eq!(record.monotonic_ms, 10);
}

struct Embassy(Cell<u64>);

impl Clock for Embassy {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp {
            millis: self.0.get(),
        }
    }
}

#[test]
fn wall_time_comes_from_the_time_service() {
    let time = TimeService::<_, NoopRawMutex>::new(
        Embassy(Cell::new(5_000)),
        TimeConfig::new(UnixTime::from_secs(1_767_225_600)),
    );
    assert_eq!(time.monotonic_ms(), 5_000);
    assert_eq!(LogClock::wall_time(&time), None);

    time.submit(
        TimeSourceKind::Ntp,
        TimeSample {
            unix_time: UnixTime::from_secs(1_800_000_000),
            uncertainty: None,
        },
    )
    .unwrap();
    assert!(LogClock::wall_time(&time).is_some());
}
