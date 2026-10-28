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

//! Background synchronization of a [`TimeService`] from a [`TimeSource`].

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
    /// wait until the next one is due.
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

    /// Runs forever.
    pub async fn run(&mut self) -> ! {
        loop {
            self.step().await;
        }
    }
}
