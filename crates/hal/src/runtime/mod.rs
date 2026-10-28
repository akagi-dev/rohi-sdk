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
//! Hardware-independent sensor runtime: event queue, reporter, lifecycle
//! runner, backoff and bounded waits.

/// Retry backoff and bounded waits.
pub mod backoff;
/// Bounded sensor event queue with newest-wins overflow.
pub mod bus;
/// Monotonic clock abstraction.
pub mod clock;
/// I2C bus wrapper bounding every transaction in time.
pub mod i2c;
/// Per-sensor event reporter.
pub mod reporter;
/// Generic sensor lifecycle runner.
pub mod runner;
/// On-demand snapshot access to the measurement owner task.
pub mod service;
/// Shared view of sensor runtime states.
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
