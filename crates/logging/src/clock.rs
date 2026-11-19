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
//! Where records get their time.

use embassy_sync::blocking_mutex::raw::RawMutex;
use rohi_time::Clock;
use rohi_time::{TimeService, UnixTime};

/// The time a record is stamped with, read at the moment of the event.
///
/// Both methods run on the logging hot path: they must not block, wait or log.
pub trait LogClock {
    /// Milliseconds since boot.
    fn monotonic_ms(&self) -> u64;

    /// Synchronized wall-clock time, `None` until the first valid sample.
    fn wall_time(&self) -> Option<UnixTime>;
}

impl<C: Clock, M: RawMutex> LogClock for TimeService<C, M> {
    fn monotonic_ms(&self) -> u64 {
        self.monotonic_now().millis
    }

    fn wall_time(&self) -> Option<UnixTime> {
        self.now()
    }
}

impl<T: LogClock + ?Sized> LogClock for &T {
    fn monotonic_ms(&self) -> u64 {
        (**self).monotonic_ms()
    }

    fn wall_time(&self) -> Option<UnixTime> {
        (**self).wall_time()
    }
}
