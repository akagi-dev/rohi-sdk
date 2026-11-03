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

//! Synchronized wall-clock time for Robonomics Open Hardware.
//!
//! # Overview
//!
//! The firmware has **one** source of wall-clock time and synchronization
//! state, the [`TimeService`]. Components that need a real-world timestamp
//! (persistent logs, datalog, diagnostics, measurement timestamps) ask it.
//! They never contact an NTP server, manage a clock chip, keep clock offsets
//! or decide whether a timestamp is trustworthy.
//!
//! This crate is the portable core: the service, the validation policy, the
//! [`TimeSource`] trait and the background [`Synchronizer`]. It is `no_std`,
//! does not allocate and has no Cargo features. The NTP source and the `time`
//! configuration section (the NTP servers) are in
//! [`rohi-time-ntp`](../rohi_time_ntp/index.html).
//!
//! # Quick Example
//!
//! A service on a clock, one source delivering a sample, and the reading
//! side. On a device the clock is `rohi_hal_esp::EmbassyClock` and the source
//! is `rohi_time_ntp::NtpTimeSource`; here both are tiny host stand-ins.
//!
//! ```
//! use core::cell::Cell;
//! use core::time::Duration;
//!
//! use embassy_futures::block_on;
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use rohi_hal::MonotonicTimestamp;
//! use rohi_hal::runtime::Clock;
//! use rohi_time::{
//!     SyncFailure, TimeConfig, TimeSample, TimeService, TimeSource, TimeSourceKind,
//!     TimeSyncState, UnixTime, sync_once,
//! };
//!
//! // The monotonic clock; the example moves it by hand.
//! struct Uptime(Cell<u64>);
//! impl Clock for Uptime {
//!     fn now(&self) -> MonotonicTimestamp {
//!         MonotonicTimestamp::from_millis(self.0.get())
//!     }
//! }
//!
//! // A source that always answers the same time.
//! struct FixedSource(UnixTime);
//! impl TimeSource for FixedSource {
//!     type Error = SyncFailure;
//!     fn kind(&self) -> TimeSourceKind {
//!         TimeSourceKind::External
//!     }
//!     async fn sample(&mut self) -> Result<TimeSample, SyncFailure> {
//!         Ok(TimeSample { unix_time: self.0, uncertainty: Some(Duration::from_millis(40)) })
//!     }
//! }
//!
//! // Samples before the floor (the firmware build time) are implausible.
//! let floor = UnixTime::from_secs(1_767_225_600);
//! let clock = Uptime(Cell::new(10_000));
//! let time = TimeService::<_, NoopRawMutex>::new(clock, TimeConfig::new(floor));
//!
//! // Unsynchronized: no wall-clock time, and never a made-up date.
//! assert_eq!(time.now(), None);
//! assert_eq!(time.status().state, TimeSyncState::Unsynchronized);
//!
//! // One synchronization attempt, normally done by `Synchronizer`.
//! let mut source = FixedSource(UnixTime::from_millis(1_780_000_000_250));
//! block_on(sync_once(&time, &mut source)).unwrap();
//! assert_eq!(time.now(), Some(UnixTime::from_millis(1_780_000_000_250)));
//! assert_eq!(time.status().state, TimeSyncState::Synchronized);
//! ```
//!
//! # Core Concepts
//!
//! ```text
//!    NtpTimeSource            future RTC source         external time
//!    (rohi-time-ntp)          (hardware RTC)
//!         │  DNS + UDP              │  I²C / SPI
//!         └────────────┬────────────┴─────────────┘
//!                      ▼   TimeSample { unix_time, uncertainty }
//!               Synchronizer        background task, backoff, resync
//!                      ▼
//!               ┌─────────────┐   validation (plausibility window)
//!               │ TimeService │   anchor = absolute time + monotonic instant
//!               └──────┬──────┘   state, failure counters
//!                      │  now() = anchor + monotonic elapsed
//!         ┌────────────┼────────────┐
//!         ▼            ▼            ▼
//!      logging       datalog     control plane (`time` service)
//! ```
//!
//! Only the NTP source exists today; the RTC and external kinds are reserved
//! by [`TimeSourceKind`] and no driver or caller exists in this repository.
//! Logging and datalog consumers are likewise not written yet.
//!
//! * **Sample**: a [`TimeSample`], an absolute time plus an optional
//!   uncertainty, produced by a [`TimeSource`].
//! * **Anchor**: the accepted sample together with the monotonic instant it
//!   was accepted at. [`TimeService::now`] is `anchor + monotonic elapsed`.
//! * **Plausibility**: a sample is accepted only inside the window and
//!   uncertainty limit of [`TimeConfig`]; otherwise it is rejected as
//!   [`SyncFailure::Implausible`] and the anchor is kept.
//! * **State**: [`TimeSyncState`], see below.
//!
//! ## Monotonic time versus wall-clock time
//!
//! These are different concepts with different types; there is no ambiguous
//! `timestamp()`.
//!
//! | | Monotonic time | Wall-clock time |
//! |---|----------------|-----------------|
//! | Type | [`rohi_hal::MonotonicTimestamp`] (milliseconds since boot, supplied by a [`rohi_hal::runtime::Clock`]) | [`UnixTime`] (Unix milliseconds, UTC) |
//! | Available | always, from boot | only after a valid sample; otherwise `None` |
//! | Can jump | never | **yes**, forward or backward, when a sample corrects it |
//! | Use for | deadlines, timeouts, intervals, retries, scheduling, elapsed time, ordering events in a boot | absolute timestamps only |
//!
//! **Wall-clock time may step.** A newly accepted sample replaces the anchor
//! at once. The clock is not slewed and not made monotonic. Never use
//! wall-clock time to measure durations or order events; use the monotonic
//! clock. A future work item may add smoothing.
//!
//! ## Synchronization states
//!
//! | [`TimeSyncState`] | Meaning |
//! |-------------------|---------|
//! | [`Unsynchronized`](TimeSyncState::Unsynchronized) | no valid sample yet; `now()` is `None` |
//! | [`Synchronized`](TimeSyncState::Synchronized) | a valid sample was accepted within the stale threshold |
//! | [`Stale`](TimeSyncState::Stale) | the anchor is older than [`TimeConfig::stale_after`] (default 6 h); `now()` still works from the last anchor and may have drifted |
//!
//! [`TimeSourceKind`] (`Ntp`, `Rtc`, `External`) appears in diagnostics only;
//! consumers do not normally branch on it. The latest valid sample of any
//! source wins; the policy is deliberately not generalized yet, but the
//! public API carries no NTP assumption.
//!
//! # Architecture & Integration
//!
//! | Where | What |
//! |-------|------|
//! | Altruist Urban firmware (`firmwares/altruist-urban`, module `time`) | Owns one `static` [`TimeService`] on `rohi_hal_esp::EmbassyClock` with a critical-section mutex; the floor is the firmware build time (`BUILD_TIMESTAMP`, set by `build.rs`). The `ntp_task` runs a [`Synchronizer`] over an `NtpTimeSource` and restarts it when the `time` config section changes. Components read the time through the firmware's `time::service()`. |
//! | [`rohi-time-ntp`](../rohi_time_ntp/index.html) | The NTP [`TimeSource`] and the persistent `time` section (the servers). |
//! | `rohi-connectivity` | Needs UTC to create telemetry frames; see [`rohi-connectivity`](../rohi_connectivity/index.html). |
//! | Control plane | [`TimeService`] is the read-only `time` status service ([`rohi_service`]). |
//!
//! Firmware startup never waits for time: run the [`Synchronizer`] in its own
//! task and everything else works unsynchronized. Persistent logging will
//! consume [`TimeService::now`] and attach a wall timestamp only when it is
//! `Some`; it must keep working before the first synchronization.
//!
//! # API Navigation
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`service`] | [`TimeService`], validation ([`TimeConfig`]), the `time` status service: fields, source values and issues, resource use |
//! | [`source`] | the [`TimeSource`] trait, [`TimeSample`], [`SyncFailure`], [`TimeSourceKind`] |
//! | [`sync`] | [`Synchronizer`], [`SyncSchedule`], [`sync_once`]: background synchronization and backoff |
//! | [`status`] | [`TimeSyncState`], [`TimeStatus`], [`TimeSnapshot`] |
//! | [`timestamp`] | [`UnixTime`] |
//!
//! Consumers use [`TimeService::now`], [`TimeService::status`] and
//! [`TimeService::snapshot`]; sources implement [`TimeSource`]; firmware wires
//! them with [`Synchronizer`].
//!
//! # Guarantees & Limitations
//!
//! - [`TimeService::now`] is `None` until the first valid sample was
//!   accepted. Uptime is never reported as Unix time and no placeholder date
//!   (such as 1970-01-01) is ever invented.
//! - Reading the time is cheap: it performs no network or hardware access and
//!   no allocation, and adds the monotonic time elapsed since the accepted
//!   sample to that sample.
//! - An invalid, implausible or failed sample never replaces a valid anchor.
//! - Failures are only recorded (counters, the category of the last failure);
//!   they never panic.
//! - **Synchronization is not authentication.** Plain NTP is not
//!   cryptographically protected, so the time must not be an input to
//!   security-critical decisions. [`TimeStatus`] reports synchronization
//!   quality, not authenticity. Authenticated network time (NTS) is out of
//!   scope.
//! - Resources are static and bounded: the anchor and the counters are a few
//!   words behind a mutex; see [`service`] for the status service.
//! - Not in scope: a hardware RTC driver, persisted last-known time, clock
//!   slewing, time zones and daylight saving, authenticated NTP (NTS),
//!   GPS/PTP, an NTP server.
//!
//! ## Validation level
//!
//! Everything runs on the host without Internet access: a fake clock and
//! fake sources here (`cargo test -p rohi-time`), and a scripted NTP server
//! over `rohi_net::testing::FakeIp` in
//! [`rohi-time-ntp`](../rohi_time_ntp/index.html) (valid and malformed
//! answers, kiss-o'-death, timeouts, DNS failure, unreachable server, era
//! rollover, implausible time, network loss and recovery). The firmware
//! builds for ESP32 targets in CI, but time synchronization over Wi-Fi is
//! **not verified on hardware**; see the [support
//! matrix](https://rohi.akagi.dev/support/matrix.html) and [hardware testing
//! status](https://rohi.akagi.dev/validation/hardware-testing.html).

#[cfg(test)]
extern crate std;

pub mod service;
pub mod source;
pub mod status;
pub mod sync;
pub mod timestamp;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod testutil;

pub use service::{
    DEFAULT_HORIZON, DEFAULT_MAX_UNCERTAINTY, DEFAULT_STALE_AFTER, TimeConfig, TimeService, issue,
    source_value, status_field,
};
pub use source::{SyncFailure, TimeSample, TimeSource, TimeSourceKind};
pub use status::{TimeSnapshot, TimeStatus, TimeSyncState};
pub use sync::{SyncSchedule, Synchronizer, sync_once};
pub use timestamp::UnixTime;
