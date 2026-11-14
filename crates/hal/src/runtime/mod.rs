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
//! Sensor driver runtime: the lifecycle [`Runner`] and bounded hardware I/O.
//!
//! Every sensor runs in its own task around a [`Runner`], which maps the
//! results of a [`SensorDriver`] into the sensor lifecycle and publishes
//! [`SensorEvent`](rohi_sensors::SensorEvent)s through a
//! [`Reporter`](rohi_sensors::runtime::Reporter) into one
//! [`EventBus`](rohi_sensors::runtime::EventBus). The event bus, the store and
//! the snapshot service belong to [`rohi-sensors`](../../rohi_sensors/index.html).
//! Lightweight I²C sensors share one bus; wrapped in [`TimeoutI2c`] every
//! transaction is bounded in time (the Altruist Urban firmware uses 200 ms), so
//! a stuck device cannot block the bus forever.
//!
//! ```text
//! sensor task N: Runner { SensorDriver, Reporter } --SensorEvent--> EventBus
//!                                                                    |
//!                                                  store task (rohi-sensors)
//! ```
//!
//! `firmwares/altruist-urban/src/tasks/` and `src/esp/sensors.rs` are the
//! maintained wiring of these pieces; the doctest of [`Runner`] shows the
//! pieces on a host.
//!
//! | Module | Role |
//! |--------|------|
//! | [`runner`] | [`Runner`]: lifecycle states, timing semantics (initial delay, poll interval, recovery backoff, warm-up), recovery |
//! | [`i2c`] | [`TimeoutI2c`] |
//!
//! The monotonic [`Clock`](rohi_time::Clock), [`Backoff`](rohi_time::Backoff)
//! and [`with_timeout`](rohi_time::with_timeout) are provided by
//! [`rohi-time`](../../rohi_time/index.html).
//!
//! # Cancellation
//!
//! The sensor runner is meant to run to completion. [`Runner::step`] may be
//! dropped (for example at shutdown), but a sensor transaction in flight is
//! then cut short and the next step repeats the interrupted attempt; the
//! warm-up wait is not lost when a step is dropped.

pub mod i2c;
pub mod runner;

pub use i2c::{TimeoutError, TimeoutI2c};
pub use runner::{DriverError, Runner, RunnerConfig, SensorDriver};
