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
//! Monotonic clock backed by `embassy-time`.

use embassy_time::Instant;
use rohi_hal::MonotonicTimestamp;
use rohi_hal::runtime::Clock;

/// Time since boot, from the Embassy time driver.
///
/// A [`Clock`] returning [`Instant::now`] in milliseconds as a
/// [`MonotonicTimestamp`]. It is monotonic, not wall-clock time, and needs the
/// Embassy time driver to be running (the firmware starts it with `esp-rtos`).
/// Cheap to copy; give one to every [`Reporter`](rohi_hal::runtime::Reporter)
/// and to the store task.
///
/// ```text
/// let reporter = Reporter::new(SensorKind::Bmx280, &EVENTS, EmbassyClock);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct EmbassyClock;

impl Clock for EmbassyClock {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(Instant::now().as_millis())
    }
}
