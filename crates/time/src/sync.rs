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
//! Background synchronization of a [`TimeService`] from a [`TimeSource`].
//!
//! [`Synchronizer`] runs in one task (in the Altruist Urban firmware
//! `altruist_urban::time::ntp_task`):
//!
//! - **Network down** ([`TimeSource::is_available`] is `false`): polls every
//!   [`unavailable_poll_ms`](SyncSchedule::unavailable_poll_ms) (2 s),
//!   **without counting a failure** (the status shows the
//!   `network_unavailable` issue, see
//!   [`TimeService::record_unavailable`]); the first sample is taken at the
//!   first poll after the network returns. The network appearing and
//!   disappearing at runtime is expected.
//! - **Failure** (DNS, timeout, unreachable, invalid response,
//!   kiss-o'-death, implausible time): recorded
//!   ([`TimeStatus::consecutive_failures`](crate::TimeStatus), [`TimeStatus::last_failure`](crate::TimeStatus)) and
//!   retried with bounded exponential backoff, 15 s doubling up to 10
//!   minutes (no jitter). The anchor is untouched; there is no panic.
//! - **Success**: the backoff is reset, the failure counters are cleared and
//!   the next sample is taken after [`resync_ms`](SyncSchedule::resync_ms)
//!   (1 hour). Oscillator drift is thereby corrected, while polling stays
//!   well within NTP etiquette (RFC 4330: never more often than every 15 s).
//!
//! The intervals are [`SyncSchedule`] fields; [`SyncSchedule::DEFAULT`] holds
//! the values above.
//!
//! # Cancellation
//!
//! Every wait is an `await` of the source or of the delay, so dropping the
//! future of [`Synchronizer::run`], [`Synchronizer::step`] or [`sync_once`] at
//! any point is safe: the [`TimeService`] is only updated after the source
//! returned, in one step, and it is never left half-updated. The firmware
//! cancels the task this way to restart it with new NTP servers. A new
//! [`Synchronizer`] starts with a fresh backoff and samples at once.
//!
//! ```
//! use core::cell::Cell;
//!
//! use embassy_futures::block_on;
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use embedded_hal_async::delay::DelayNs;
//! use rohi_hal::MonotonicTimestamp;
//! use rohi_hal::runtime::Clock;
//! use rohi_time::{
//!     SyncFailure, SyncSchedule, Synchronizer, TimeConfig, TimeSample, TimeService, TimeSource,
//!     TimeSourceKind, UnixTime,
//! };
//!
//! struct Uptime;
//! impl Clock for Uptime {
//!     fn now(&self) -> MonotonicTimestamp {
//!         MonotonicTimestamp::from_millis(0)
//!     }
//! }
//!
//! // A delay that returns at once and remembers the last wait.
//! struct Recorder<'a>(&'a Cell<u32>);
//! impl DelayNs for Recorder<'_> {
//!     async fn delay_ns(&mut self, _ns: u32) {}
//!     async fn delay_ms(&mut self, ms: u32) {
//!         self.0.set(ms);
//!     }
//! }
//!
//! // A server that never answers.
//! struct Silent;
//! impl TimeSource for Silent {
//!     type Error = SyncFailure;
//!     fn kind(&self) -> TimeSourceKind {
//!         TimeSourceKind::Ntp
//!     }
//!     async fn sample(&mut self) -> Result<TimeSample, SyncFailure> {
//!         Err(SyncFailure::Timeout)
//!     }
//! }
//!
//! let time = TimeService::<_, NoopRawMutex>::new(
//!     Uptime,
//!     TimeConfig::new(UnixTime::from_secs(1_767_225_600)),
//! );
//! let waited = Cell::new(0);
//! let mut sync = Synchronizer::new(&time, Silent, Recorder(&waited), SyncSchedule::DEFAULT);
//!
//! // Failures are recorded and retried after 15 s, 30 s, 60 s, ...
//! block_on(sync.step());
//! assert_eq!(waited.get(), 15_000);
//! block_on(sync.step());
//! assert_eq!(waited.get(), 30_000);
//! assert_eq!(time.status().consecutive_failures, 2);
//! assert_eq!(time.status().last_failure, Some(SyncFailure::Timeout));
//! assert_eq!(time.now(), None);
//! ```

use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use rohi_hal::runtime::{Backoff, Clock};

use crate::{SyncFailure, TimeService, TimeSource};

/// When to sample a source.
///
/// Every interval is a named, bounded value; there is no implicit polling
/// rate. NTP servers expect clients to poll rarely (RFC 4330: never more often
/// than every 15 s), so the retry floor is 15 s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncSchedule {
    /// First delay after a failed attempt.
    pub initial_retry_ms: u32,
    /// Largest delay after repeated failures; the delay doubles up to this
    /// bound.
    pub max_retry_ms: u32,
    /// Delay between two successful samples.
    pub resync_ms: u32,
    /// How often to look at a source that is not available (no network).
    pub unavailable_poll_ms: u32,
}

impl SyncSchedule {
    /// 15 s first retry, up to 10 minutes, hourly resynchronization, a
    /// 2 s availability check.
    pub const DEFAULT: Self = Self {
        initial_retry_ms: 15_000,
        max_retry_ms: 10 * 60 * 1_000,
        resync_ms: 60 * 60 * 1_000,
        unavailable_poll_ms: 2_000,
    };
}

impl Default for SyncSchedule {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Takes one sample of `source` and offers it to `service`.
///
/// A source error is recorded as a failure and the anchor is kept; a
/// plausible sample replaces the anchor. Returns the failure category when no
/// time was accepted.
///
/// If the returned future is dropped while the source is still sampling,
/// nothing is recorded.
///
/// # Errors
///
/// The [`SyncFailure`] the source error maps to, or
/// [`SyncFailure::Implausible`] when the sample was refused by the
/// [`TimeConfig`](crate::TimeConfig) of the service.
pub async fn sync_once<S, C, M>(
    service: &TimeService<C, M>,
    source: &mut S,
) -> Result<(), SyncFailure>
where
    S: TimeSource,
    C: Clock,
    M: RawMutex,
{
    match source.sample().await {
        Ok(sample) => service.submit(source.kind(), sample),
        Err(error) => {
            let failure = error.into();
            log::warn!("time: {:?} source failed: {:?}", source.kind(), failure);
            service.record_failure(failure);
            Err(failure)
        }
    }
}

/// Keeps a [`TimeService`] synchronized from one [`TimeSource`].
///
/// Startup does not wait for it: run [`run`](Self::run) in its own task and
/// the rest of the firmware carries on unsynchronized. Failures never panic;
/// they are recorded in the service, retried with bounded exponential backoff
/// and reset by the next success. A source that is not available (no network)
/// is polled cheaply without counting failures, so the first sample is taken
/// as soon as it appears. After a success the next sample is taken after
/// [`SyncSchedule::resync_ms`].
pub struct Synchronizer<'a, S, C, M: RawMutex, D> {
    service: &'a TimeService<C, M>,
    source: S,
    delay: D,
    schedule: SyncSchedule,
    backoff: Backoff,
}

impl<'a, S, C, M, D> Synchronizer<'a, S, C, M, D>
where
    S: TimeSource,
    C: Clock,
    M: RawMutex,
    D: DelayNs,
{
    /// Synchronizer of `service` from `source`, waiting with `delay`.
    pub fn new(
        service: &'a TimeService<C, M>,
        source: S,
        delay: D,
        schedule: SyncSchedule,
    ) -> Self {
        Self {
            service,
            source,
            delay,
            schedule,
            backoff: Backoff::new(schedule.initial_retry_ms.max(1), schedule.max_retry_ms),
        }
    }

    /// One iteration: sample the source (or note that it is unavailable) and
    /// wait until the next one is due: [`SyncSchedule::resync_ms`] after a
    /// success, the next backoff delay after a failure,
    /// [`SyncSchedule::unavailable_poll_ms`] while the source is unavailable.
    pub async fn step(&mut self) {
        if !self.source.is_available() {
            self.service
                .record_unavailable(SyncFailure::NetworkUnavailable);
            self.delay.delay_ms(self.schedule.unavailable_poll_ms).await;
            return;
        }
        let wait = match sync_once(self.service, &mut self.source).await {
            Ok(()) => {
                self.backoff.reset();
                self.schedule.resync_ms
            }
            Err(_) => self.backoff.next_delay_ms(),
        };
        self.delay.delay_ms(wait).await;
    }

    /// The source, for tests that script it between steps.
    #[cfg(test)]
    pub(crate) fn source_mut_for_test(&mut self) -> &mut S {
        &mut self.source
    }

    /// Runs forever: [`step`](Self::step) in a loop. Drop the future to stop
    /// it.
    pub async fn run(&mut self) -> ! {
        loop {
            self.step().await;
        }
    }
}
