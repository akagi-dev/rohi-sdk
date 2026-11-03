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
///////////////////////////////////////////////////////////////////////////////
//! Hardware-independent sensor runtime: event queue, reporter, lifecycle
//! runner, backoff and bounded waits.
//!
//! Every sensor runs in its own task around a [`Runner`], which maps the
//! results of a [`SensorDriver`] into the sensor lifecycle and publishes
//! [`SensorEvent`](crate::SensorEvent)s through a [`Reporter`] into one
//! [`EventBus`]. A single store task consumes the bus and owns the
//! [`MeasurementStore`](crate::MeasurementStore); other tasks get snapshots from
//! it through a [`MeasurementHandle`]. Lightweight I²C sensors share one bus;
//! wrapped in [`TimeoutI2c`] every transaction is bounded in time (the Altruist
//! Urban firmware uses 200 ms), so a stuck device cannot block the bus forever.
//!
//! ```text
//! sensor task N: Runner { SensorDriver, Reporter } --SensorEvent--> EventBus
//!                                                                    |
//! store task:  loop { select(EventBus::receive, MeasurementService::accept) }
//!                |-- event   -> MeasurementStore::apply(event)
//!                `-- request -> request.respond(store.snapshot(now))
//!
//! any other task: MeasurementHandle::snapshot().await
//! ```
//!
//! `firmwares/altruist-urban/src/tasks/` is the maintained wiring of these
//! pieces; the doctest of [`Runner`] shows the pieces on a host.
//!
//! The capacities are fixed at compile time: [`EVENT_QUEUE_CAPACITY`] events
//! in the bus (the oldest is dropped when full, see [`bus`]) and one request
//! and one response slot in the snapshot service.
//!
//! | Module | Role |
//! |--------|------|
//! | [`runner`] | [`Runner`]: lifecycle states, timing semantics (initial delay, poll interval, recovery backoff, warm-up), recovery |
//! | [`reporter`] | [`Reporter`]: publishes the events of one sensor |
//! | [`bus`] | [`EventBus`]: bounded queue, overflow policy, the three event kinds |
//! | [`service`] | [`MeasurementHandle`]: on-demand snapshots from the store owner task |
//! | [`states`] | [`SensorStates`]: read-only view of every sensor state |
//! | [`backoff`] | [`Backoff`] and [`with_timeout`] |
//! | [`i2c`] | [`TimeoutI2c`] |
//! | [`clock`] | [`Clock`]: source of monotonic timestamps |
//!
//! # Cancellation
//!
//! The sensor runner is meant to run to completion. [`Runner::step`] may be
//! dropped (for example at shutdown), but a sensor transaction in flight is
//! then cut short and the next step repeats the interrupted attempt; the
//! warm-up wait is not lost when a step is dropped. [`EventBus::receive`] and
//! the owner side [`MeasurementService::accept`] are cancel-safe;
//! [`MeasurementHandle::snapshot`] can be dropped at any point.

pub mod backoff;
pub mod bus;
pub mod clock;
pub mod i2c;
pub mod reporter;
pub mod runner;
pub mod service;
pub mod states;

pub use backoff::{Backoff, TimedOut, with_timeout};
pub use bus::{EVENT_QUEUE_CAPACITY, EventBus};
pub use clock::Clock;
pub use i2c::{TimeoutError, TimeoutI2c};
pub use reporter::Reporter;
pub use runner::{DriverError, Runner, RunnerConfig, SensorDriver};
pub use service::{
    MeasurementHandle, MeasurementService, MeasurementServiceError, SnapshotRequest,
};
pub use states::SensorStates;
