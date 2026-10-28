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

use core::time::Duration;

use rohi_hal::MonotonicTimestamp;

use crate::{SyncFailure, TimeSourceKind, UnixTime};

/// Whether wall-clock time is available and how fresh it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSyncState {
    /// No valid sample was accepted yet; wall-clock time is unavailable.
    Unsynchronized,
    /// A valid sample was accepted recently.
    Synchronized,
    /// The last accepted sample is older than the configured stale
    /// threshold. Wall-clock time is still available, calculated from that
    /// sample, but it may have drifted.
    Stale,
}

/// Diagnostic state of the time service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeStatus {
    /// Synchronization state.
    pub state: TimeSyncState,
    /// Source of the current anchor.
    pub source: Option<TimeSourceKind>,
    /// Time since the current anchor was accepted.
    pub last_sync_age: Option<Duration>,
    /// Uncertainty of the current anchor when it was accepted.
    pub uncertainty: Option<Duration>,
    /// Failed attempts since the last accepted sample.
    pub consecutive_failures: u32,
    /// Category of the most recent failed attempt, cleared by the next
    /// accepted sample.
    pub last_failure: Option<SyncFailure>,
}

/// Monotonic and wall-clock time of approximately the same instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSnapshot {
    /// Monotonic time.
    pub monotonic: MonotonicTimestamp,
    /// Wall-clock time, `None` while unsynchronized.
    pub wall_time: Option<UnixTime>,
    /// Synchronization state.
    pub state: TimeSyncState,
}
