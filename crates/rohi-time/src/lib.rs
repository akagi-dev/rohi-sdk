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
#![no_std]

//! Synchronized wall-clock time for Robonomics Open Hardware.
//!
//! Monotonic time and wall-clock time are different things:
//!
//! - **Monotonic time** ([`rohi_hal::MonotonicTimestamp`], from the platform
//!   timer) never goes backwards and starts at boot. It is the only clock for
//!   deadlines, timeouts, intervals, retries, scheduling, measuring elapsed
//!   time and ordering events within a boot.
//! - **Wall-clock time** ([`UnixTime`]) is an absolute timestamp. It exists
//!   only after a [`TimeSource`] delivered a trustworthy sample, and it may
//!   *step* when a later sample corrects it.
//!
//! ```text
//!   NTP source     future RTC     external
//!        └──────────────┼──────────────┘
//!                       ▼ TimeSample
//!                 ┌───────────┐
//!                 │TimeService│  validation, anchor, state
//!                 └─────┬─────┘
//!                       │ anchor + monotonic elapsed
//!                       ▼
//!                    now() ─▶ logging, datalog, diagnostics
//! ```
//!
//! Consumers depend on [`TimeService`] only; they never contact a time
//! server or a clock chip. A [`TimeSource`] produces samples, [`Synchronizer`] drives
//! a source in the background with bounded backoff and periodic
//! resynchronization.
//!
//! # Guarantees
//!
//! - [`TimeService::now`] is `None` until the first valid sample was
//!   accepted. Uptime is never reported as Unix time and no placeholder date
//!   is ever invented.
//! - Reading the time performs no I/O and no allocation: it adds the
//!   monotonic time elapsed since the accepted sample to that sample.
//! - An invalid, implausible or failed sample never replaces a valid anchor.
//! - **Wall-clock time may step.** A newly accepted sample replaces the
//!   anchor at once; the time can jump forward or backward. The value is not
//!   smoothed and not made monotonic. Code that needs durations, deadlines or
//!   ordering must use the monotonic clock.
//! - Synchronization is not authentication. Plain NTP is unauthenticated, so
//!   the time must not be used for security decisions; [`TimeStatus`] reports
//!   synchronization quality, not authenticity.

#[cfg(test)]
extern crate std;

mod service;
mod source;
mod status;
pub mod sync;
mod timestamp;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;

pub use service::{
    DEFAULT_HORIZON, DEFAULT_MAX_UNCERTAINTY, DEFAULT_STALE_AFTER, TimeConfig, TimeService,
};
pub use source::{SyncFailure, TimeSample, TimeSource, TimeSourceKind};
pub use status::{TimeSnapshot, TimeStatus, TimeSyncState};
pub use sync::{SyncSchedule, Synchronizer, sync_once};
pub use timestamp::UnixTime;
