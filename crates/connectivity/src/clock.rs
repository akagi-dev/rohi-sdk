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
//! Time and entropy boundaries of the telemetry service.

use core::fmt;

use embassy_sync::blocking_mutex::raw::RawMutex;
use rohi_hal::runtime::Clock;
use rohi_time::TimeService;

/// Size of the nonce of a telemetry envelope, in bytes.
pub const NONCE_LEN: usize = rohi_connectivity_proto::limits::NONCE_LEN;

/// The UTC time is not available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockError;

impl fmt::Display for ClockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UTC time is not synchronized")
    }
}

impl core::error::Error for ClockError {}

/// Source of Unix UTC time.
///
/// Telemetry timestamps are UTC milliseconds. Monotonic time (boot time,
/// `embassy_time::Instant`, [`rohi_hal::MonotonicTimestamp`]) is never UTC and
/// must not be returned here: a clock that is not synchronized reports
/// [`ClockError`].
pub trait UtcClock {
    /// Milliseconds since the Unix epoch, UTC.
    fn now_unix_ms(&self) -> Result<u64, ClockError>;
}

impl<C: Clock, M: RawMutex> UtcClock for TimeService<C, M> {
    fn now_unix_ms(&self) -> Result<u64, ClockError> {
        self.now().map(|time| time.as_millis()).ok_or(ClockError)
    }
}

/// No entropy could be obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NonceError;

impl fmt::Display for NonceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("no entropy for the nonce")
    }
}

impl core::error::Error for NonceError {}

/// Source of envelope nonces.
///
/// Every new telemetry message gets a new nonce; a retry of the same frame
/// reuses it, so the nonce is requested exactly once per frame, before the
/// frame is created.
pub trait NonceSource {
    /// Fills `dst` with unpredictable bytes.
    fn fill_nonce(&mut self, dst: &mut [u8; NONCE_LEN]) -> Result<(), NonceError>;
}
