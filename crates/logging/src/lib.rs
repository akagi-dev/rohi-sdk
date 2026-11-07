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
#![no_std]

//! Persistent logging for ROHI devices: the `log` facade kept in a bounded, rotated, compressed history and exposed as the `logging` service.
//!
//! # Overview
//!
//! `rohi-logging` turns the events of the standard `log` facade into bounded,
//! owned [`LogRecord`]s, queues them without ever blocking the caller, writes
//! them in batches to segments of a [`LogStorage`](storage::LogStorage), and
//! offers the history through the generic service model: a `logging` config
//! section ([`LoggingConfig`]), a status, and the actions `read`, `download` and
//! `clear` ([`LoggingService`]). There is no logging-specific control protocol:
//! `download` and `read` return a [stream](rohi_service::StreamInfo).
//!
//! It provides a ring journal over any erasable medium
//! ([`RingStorage`](storage::RingStorage)), an adapter for NOR flash
//! ([`FlashMedium`](storage::FlashMedium)) and a RAM medium for tests; a host application
//! can implement [`Medium`](storage::Medium) over a file, as `rohi-sim` does.
//! An SD backend is **not** implemented; the [`Backends`] trait is where it
//! would plug in.
//!
//! The crate is `no_std`, does not allocate and has no Cargo features.
//!
//! # Quick Example
//!
//! The producer side: filtering, bounded records, a bounded queue.
//!
//! ```
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use rohi_logging::{LogClock, LogLevel, LogSink};
//! use rohi_time::UnixTime;
//!
//! struct Fixed;
//! impl LogClock for Fixed {
//!     fn monotonic_ms(&self) -> u64 { 1_500 }
//!     fn wall_time(&self) -> Option<UnixTime> { None }
//! }
//!
//! let sink = LogSink::<NoopRawMutex, _, 8>::new(Fixed, LogLevel::Warn);
//! assert!(sink.push(log::Level::Error, "wifi", format_args!("link lost")));
//! assert!(!sink.push(log::Level::Info, "wifi", format_args!("filtered")));
//! let record = sink.try_recv().unwrap();
//! assert_eq!(record.message.as_str(), "link lost");
//! assert_eq!(record.monotonic_ms, 1_500);
//! ```
//!
//! # Core Concepts
//!
//! | Concept | Meaning |
//! |---------|---------|
//! | [`LogSink`] | the bounded queue and runtime level behind the `log` backend |
//! | [`LogRecord`] | one event as owned, bounded data; the sequence is assigned by the logging task |
//! | [`LogLevel`] | `off`, `error`, `warn`, `info` or `debug` |
//! | [`LogClock`] | where the monotonic and wall-clock stamps come from; implemented for `rohi_time::TimeService` |
//! | [`Logging`] | the runtime: queue, storage, status and open streams; its [`run`](Logging::run) is the logging task |
//! | [`LoggingService`] | the `logging` status and actions |
//! | [`LoggingConfig`] | the `logging` config section (`level`, `storage`, `compression`, `retention_bytes`, `segment_bytes`, `flush_interval`) |
//! | [`segment`] | the persistent form: line text, one LZ4 block per batch, a checksummed header per segment |
//! | [`storage`] | the [`LogStorage`](storage::LogStorage) contract and its backends |
//!
//! # Architecture and Integration
//!
//! ```text
//! info!(target: "wifi", ..) ─► log backend ─► LogSink::push ─► bounded queue
//!                                level filter, format, stamp      │
//!                                                                 ▼
//!                      Logging::run (one task) ─► Batch ─► LogStorage ─► segments
//!                                                                 ▲
//! rohi logs ─► logging.read / download / clear ─► LoggingService ─┘ (streams, pins)
//! ```
//!
//! Records of a download are held in sealed segments that the storage will not
//! evict while a stream is open; the producer never waits for that: when the
//! history cannot make room, records are dropped and counted. The platforms of the Altruist Urban
//! (`firmwares/altruist-urban/src/esp/logging.rs` on the device, `host/logging.rs` on the host) install
//! the `log` backend, own the log medium and spawn the task.
//!
//! # API Navigation
//!
//! | Item | Use it to |
//! |------|-----------|
//! | [`LogSink`] | register as the `log` backend (with `CriticalSectionRawMutex`) or push events directly |
//! | [`Logging`], [`Backends`] | build the runtime over a set of storage backends |
//! | [`LoggingService`], [`status_field`], [`action`], [`issue`] | register the service; ids of its status fields, actions and issues |
//! | [`LoggingConfig`], [`LoggingOverrides`] | the config section and its persisted overrides |
//! | [`segment`] | read and write the text and the compressed files |
//! | [`storage`] | implement or use a storage: [`RingStorage`](storage::RingStorage), [`RamLogStorage`](storage::RamLogStorage), [`FlashLogStorage`](storage::FlashLogStorage) |
//!
//! # Guarantees and Limitations
//!
//! - **Non-blocking:** [`LogSink::push`] never waits, allocates or panics. A full
//!   queue drops the new record and counts it in [`LogSink::dropped`]; the
//!   `logging` status reports `dropped_records` and the issue `queue_overflow`.
//! - **Bounds:** a record keeps at most [`MAX_TARGET_LEN`] bytes of target and
//!   [`MAX_MESSAGE_LEN`] bytes of message; longer text is cut at a character
//!   boundary and the record is flagged `truncated`. A batch is at most
//!   [`MAX_BATCH_BYTES`](segment::MAX_BATCH_BYTES) of text.
//! - **No recursion:** events with a target starting with [`INTERNAL_TARGET`]
//!   are not persisted, so a storage failure is never logged into the failing storage.
//! - **Power loss:** a segment is written block by block and sealed with a
//!   checksummed header; recovery reads a torn segment up to its last complete
//!   block, drops what cannot be read and never fails because of the content.
//!   Verified on an emulated NOR flash; not verified on hardware.
//! - **No migration, no fallback:** switching `storage` leaves the old history
//!   where it is; an unavailable backend is reported, not replaced by another.
//! - **Persistent format:** the segment header (`RLG1`, version 1), the ring start
//!   record (`RLS1`, version 1) and the config section (`LoggingConfig`, version 1)
//!   are stored ABI; changing them needs a version.
//! - **Secrets:** persisted text is externally retrievable. Never log secrets.
//! - **Cancellation:** [`Logging::run`] is meant to run for the life of the device;
//!   storage calls are not cancel safe in the middle of a write.
//! - **Targets:** host-tested; the Altruist Urban firmware builds for both chips;
//!   hardware verification: not verified ([support matrix](https://rohi.akagi.dev/support/overview.html)).

mod clock;
mod config;
mod level;
mod record;
pub mod segment;
mod service;
mod sink;
pub mod storage;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod service_tests;

#[cfg(test)]
mod tests;

pub use clock::LogClock;
pub use config::*;
pub use level::LogLevel;
pub use record::{LogRecord, MAX_MESSAGE_LEN, MAX_TARGET_LEN};
pub use service::*;
pub use sink::{INTERNAL_TARGET, LogSink};
