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

use core::cell::RefCell;
use core::time::Duration;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use log::{info, warn};
use rohi_hal::MonotonicTimestamp;
use rohi_hal::runtime::Clock;

use crate::{
    SyncFailure, TimeSample, TimeSnapshot, TimeSourceKind, TimeStatus, TimeSyncState, UnixTime,
};

/// Default age after which the synchronization is reported as
/// [`TimeSyncState::Stale`]: six hours, several missed hourly resyncs.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// Default largest accepted [`TimeSample::uncertainty`]: five seconds.
pub const DEFAULT_MAX_UNCERTAINTY: Duration = Duration::from_secs(5);

/// Default distance between the plausibility floor and ceiling: 40 years.
pub const DEFAULT_HORIZON: Duration = Duration::from_secs(40 * 365 * 24 * 60 * 60);

/// Acceptance and staleness policy of a [`TimeService`].
///
/// A sample is accepted when `not_before <= time <= not_after` and its
/// uncertainty, if known, does not exceed `max_uncertainty`. Anything else is
/// rejected with [`SyncFailure::Implausible`] and the current anchor is kept.
///
/// The floor is the firmware build time on a device without any better
/// absolute time: a device cannot be running before its firmware was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeConfig {
    /// Earliest plausible time.
    pub not_before: UnixTime,
    /// Latest plausible time.
    pub not_after: UnixTime,
    /// Age of the anchor after which the state becomes
    /// [`TimeSyncState::Stale`].
    pub stale_after: Duration,
    /// Largest accepted uncertainty of a sample.
    pub max_uncertainty: Duration,
}

impl TimeConfig {
    /// Policy with the plausibility floor `not_before` and the default
    /// ceiling ([`DEFAULT_HORIZON`] later), stale threshold and uncertainty
    /// limit.
    pub const fn new(not_before: UnixTime) -> Self {
        Self {
            not_before,
            not_after: not_before.saturating_add_millis(DEFAULT_HORIZON.as_secs() * 1_000),
            stale_after: DEFAULT_STALE_AFTER,
            max_uncertainty: DEFAULT_MAX_UNCERTAINTY,
        }
    }
}

#[derive(Clone, Copy)]
struct Anchor {
    unix_time: UnixTime,
    monotonic: MonotonicTimestamp,
    source: TimeSourceKind,
    uncertainty: Option<Duration>,
}

impl Anchor {
    fn age_ms(&self, now: MonotonicTimestamp) -> u64 {
        now.saturating_millis_since(self.monotonic)
    }

    fn wall_time(&self, now: MonotonicTimestamp) -> UnixTime {
        self.unix_time.saturating_add_millis(self.age_ms(now))
    }
}

struct Inner {
    anchor: Option<Anchor>,
    consecutive_failures: u32,
    last_failure: Option<SyncFailure>,
}

/// The single source of wall-clock time of the firmware.
///
/// Sources deliver samples ([`submit`](Self::submit), usually through
/// [`Synchronizer`](crate::Synchronizer)); consumers read the time
/// ([`now`](Self::now)) and the diagnostics ([`status`](Self::status)). The
/// service stores an anchor, the accepted absolute time together with the
/// monotonic instant it was accepted at, and calculates the current time as
/// `anchor + monotonic elapsed`. Reading never touches the network or a
/// hardware clock and never allocates.
///
/// All methods take `&self`; place the service in a `static`. See the
/// [crate documentation](crate) for the guarantees, in particular that the
/// wall-clock time may step.
pub struct TimeService<C, M: RawMutex> {
    clock: C,
    config: TimeConfig,
    inner: Mutex<M, RefCell<Inner>>,
}

impl<C: Clock, M: RawMutex> TimeService<C, M> {
    /// An unsynchronized service reading monotonic time from `clock`.
    pub const fn new(clock: C, config: TimeConfig) -> Self {
        Self {
            clock,
            config,
            inner: Mutex::new(RefCell::new(Inner {
                anchor: None,
                consecutive_failures: 0,
                last_failure: None,
            })),
        }
    }

    /// The acceptance policy.
    pub const fn config(&self) -> &TimeConfig {
        &self.config
    }

    /// Current monotonic time.
    pub fn monotonic_now(&self) -> MonotonicTimestamp {
        self.clock.now()
    }

    /// Current wall-clock time, `None` while no valid sample was accepted.
    ///
    /// Never returns uptime, a placeholder date or a stale guess disguised as
    /// "unknown": a [`Stale`](TimeSyncState::Stale) service still returns the
    /// time calculated from its last anchor.
    pub fn now(&self) -> Option<UnixTime> {
        let now = self.clock.now();
        self.inner
            .lock(|inner| inner.borrow().anchor.map(|anchor| anchor.wall_time(now)))
    }

    /// Best available guess of the current time, for sources that need a
    /// pivot to resolve wire-format ambiguities such as the NTP era.
    ///
    /// It is the wall-clock time when synchronized, otherwise the
    /// plausibility floor plus the uptime. **Not a timestamp**: it must never
    /// be recorded or shown as the time; use [`now`](Self::now) for that.
    pub fn pivot(&self) -> UnixTime {
        self.now().unwrap_or_else(|| {
            self.config
                .not_before
                .saturating_add_millis(self.clock.now().millis)
        })
    }

    /// Monotonic and wall-clock time of the same instant, plus the state.
    pub fn snapshot(&self) -> TimeSnapshot {
        let now = self.clock.now();
        self.inner.lock(|inner| {
            let inner = inner.borrow();
            TimeSnapshot {
                monotonic: now,
                wall_time: inner.anchor.map(|anchor| anchor.wall_time(now)),
                state: self.state_of(&inner, now),
            }
        })
    }

    /// Synchronization diagnostics.
    pub fn status(&self) -> TimeStatus {
        let now = self.clock.now();
        self.inner.lock(|inner| {
            let inner = inner.borrow();
            TimeStatus {
                state: self.state_of(&inner, now),
                source: inner.anchor.map(|anchor| anchor.source),
                last_sync_age: inner
                    .anchor
                    .map(|anchor| Duration::from_millis(anchor.age_ms(now))),
                uncertainty: inner.anchor.and_then(|anchor| anchor.uncertainty),
                consecutive_failures: inner.consecutive_failures,
                last_failure: inner.last_failure,
            }
        })
    }

    /// Validates `sample` and, when it is plausible, makes it the anchor:
    /// the sample is taken to describe the current monotonic instant.
    ///
    /// The previous anchor is replaced at once, so the wall-clock time can
    /// step forward or backward. A rejected sample leaves the anchor
    /// untouched and counts as a failure ([`SyncFailure::Implausible`]).
    pub fn submit(&self, source: TimeSourceKind, sample: TimeSample) -> Result<(), SyncFailure> {
        if !self.is_plausible(&sample) {
            warn!(
                "time: rejected implausible sample from {:?}: {} ms",
                source,
                sample.unix_time.as_millis()
            );
            self.record_failure(SyncFailure::Implausible);
            return Err(SyncFailure::Implausible);
        }
        let now = self.clock.now();
        self.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            let step = inner
                .anchor
                .map(|anchor| anchor.wall_time(now).as_millis() as i128)
                .map(|previous| sample.unix_time.as_millis() as i128 - previous);
            inner.anchor = Some(Anchor {
                unix_time: sample.unix_time,
                monotonic: now,
                source,
                uncertainty: sample.uncertainty,
            });
            inner.consecutive_failures = 0;
            inner.last_failure = None;
            match step {
                Some(step) => info!(
                    "time: synchronized from {:?}, stepped by {} ms",
                    source, step
                ),
                None => info!("time: synchronized from {:?}", source),
            }
        });
        Ok(())
    }

    /// Records a failed synchronization attempt. The anchor is untouched.
    pub fn record_failure(&self, failure: SyncFailure) {
        self.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            inner.consecutive_failures = inner.consecutive_failures.saturating_add(1);
            inner.last_failure = Some(failure);
        });
    }

    /// Records that sampling was not even attempted because the source is not
    /// available (for example no network). Not counted as a failed attempt.
    pub fn record_unavailable(&self, failure: SyncFailure) {
        self.inner
            .lock(|inner| inner.borrow_mut().last_failure = Some(failure));
    }

    fn is_plausible(&self, sample: &TimeSample) -> bool {
        let config = &self.config;
        sample.unix_time >= config.not_before
            && sample.unix_time <= config.not_after
            && sample
                .uncertainty
                .is_none_or(|uncertainty| uncertainty <= config.max_uncertainty)
    }

    fn state_of(&self, inner: &Inner, now: MonotonicTimestamp) -> TimeSyncState {
        match inner.anchor {
            None => TimeSyncState::Unsynchronized,
            Some(anchor)
                if u128::from(anchor.age_ms(now)) > self.config.stale_after.as_millis() =>
            {
                TimeSyncState::Stale
            }
            Some(_) => TimeSyncState::Synchronized,
        }
    }
}
