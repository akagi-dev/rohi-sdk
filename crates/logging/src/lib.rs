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

//! Persistent logging for ROHI devices.
//!
//! # Overview
//!
//! `rohi-logging` is the producer side of persistent logging: it turns the
//! events of the standard `log` facade into bounded, owned [`LogRecord`]s and
//! queues them for a logging task without ever blocking the caller. Storage,
//! rotation and the `logging` service are built on top of it.
//!
//! The crate is `no_std`, does not allocate and has no Cargo features.
//!
//! # Quick Example
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
//! | [`LogRecord`] | one event as owned, bounded data |
//! | [`LogLevel`] | `off`, `error`, `warn`, `info` or `debug` |
//! | [`LogClock`] | where the monotonic and wall-clock stamps come from |
//!
//! # Architecture and Integration
//!
//! ```text
//! info!(target: "wifi", ..) ──► LogSink::push ──► bounded queue ──► logging task
//!    level filter, format, stamp, try_send          (single consumer)
//! ```
//!
//! # API Navigation
//!
//! | Item | Use it to |
//! |------|-----------|
//! | [`LogSink`] | register as the `log` backend (with `CriticalSectionRawMutex`) or push events directly |
//! | [`LogRecord`] | read what the logging task receives |
//! | [`LogClock`] | stamp records; implemented for `rohi_time::TimeService` |
//!
//! # Guarantees and Limitations
//!
//! - **Non-blocking:** [`LogSink::push`] never waits, allocates or panics. A full
//!   queue drops the new record and counts it in [`LogSink::dropped`].
//! - **Bounds:** a record keeps at most [`MAX_TARGET_LEN`] bytes of target and
//!   [`MAX_MESSAGE_LEN`] bytes of message; longer text is cut at a character
//!   boundary and the record is flagged `truncated`.
//! - **No recursion:** events with a target starting with [`INTERNAL_TARGET`]
//!   are not persisted.
//! - **Not verified on hardware.**

mod clock;
mod config;
mod level;
mod record;
pub mod segment;
mod sink;

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod config_tests;

#[cfg(test)]
mod tests;

pub use clock::LogClock;
pub use config::*;
pub use level::LogLevel;
pub use record::{LogRecord, MAX_MESSAGE_LEN, MAX_TARGET_LEN};
pub use sink::{INTERNAL_TARGET, LogSink};
