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
//! The [`TimeService`]: the anchored wall-clock time, its validation and its
//! `time` status in the generic service pipeline.
//!
//! # Validation
//!
//! [`TimeConfig`] (named, documented values) decides whether a sample may
//! become the anchor:
//!
//! | Rule | Default |
//! |------|---------|
//! | `not_before` (plausibility floor) | firmware build time (`BUILD_TIMESTAMP`, set by the firmware `build.rs`); [`TimeConfig::new`] takes it as an argument |
//! | `not_after` | floor + [`DEFAULT_HORIZON`] (40 years) |
//! | `max_uncertainty` | [`DEFAULT_MAX_UNCERTAINTY`] (5 s) |
//! | `stale_after` | [`DEFAULT_STALE_AFTER`] (6 h) |
//!
//! A rejected sample is recorded as [`SyncFailure::Implausible`] and the
//! existing anchor is kept.
//!
//! # Control plane
//!
//! [`TimeService`] is the read-only `time` service of the status plane (no
//! secure link needed, not essential for the device). It reports its state
//! and, when synchronized, `unix_time` (ms; absent while unsynchronized),
//! `source`, `sync_age`, `uncertainty` and `consecutive_failures` (see
//! [`status_field`] and [`source_value`]). The time servers are in the `time`
//! config section ([`rohi_time_ntp`](../../rohi_time_ntp/section/index.html)),
//! not repeated in the status.
//!
//! | Sync state | Service state | Health | Ready |
//! |------------|---------------|--------|-------|
//! | `Unsynchronized`, nothing failed yet | `starting` | unknown | no |
//! | `Unsynchronized`, after failures | `running` | `degraded` | no |
//! | `Synchronized` | `running` | `ok` | yes |
//! | `Stale` (usable, may have drifted) | `running` | `degraded` | yes |
//!
//! While the last attempt failed, the active issue is the one of the last
//! failure category ([`issue`]): `network_unavailable`, `resolve_failed`,
//! `timeout`, `invalid_response`, `rejected`, `implausible`,
//! `source_failure`.
//!
//! ```text
//! $ rohi status time
//! Service:  time
//! State:    running
//! Health:   ok
//! Ready:    yes
//! Since:    2h 13m
//!
//! unix_time                1780000000250 ms
//! source                   ntp
//! sync_age                 90000 ms
//! uncertainty              40 ms
//! consecutive_failures     0
//! ```
//!
//! Manual resynchronization is not offered: periodic automatic
//! synchronization is authoritative. See the
//! [desktop application](https://rohi.akagi.dev/use/desktop/status.html) for
//! `rohi status`.
//!
//! # Resources
//!
//! All storage is static and bounded: the anchor and the counters are a few
//! words behind the mutex `M` of the service (a critical-section mutex in the
//! firmware). A source holds its own bounded
//! resources (the NTP source uses one UDP socket per exchange from the
//! `NetResources` pool, see [`rohi_time_ntp`](../../rohi_time_ntp/index.html)).
//! See [Resource limits](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html).

use core::cell::RefCell;
use core::time::Duration;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;
use log::{debug, info, trace, warn};
use rohi_hal::MonotonicTimestamp;
use rohi_hal::runtime::Clock;
use rohi_service::{
    EnumDescriptor, EnumVariant, FieldDescriptor, Issue, IssueDescriptor, IssueSeverity,
    MAX_ISSUES, Service, ServiceHealth, ServiceState, ServiceSummary, Status, StatusWriter, Unit,
    ValueKind, WriteError,
};

use crate::{
    SyncFailure, TimeSample, TimeSnapshot, TimeSourceKind, TimeStatus, TimeSyncState, UnixTime,
};

/// Default age after which the synchronization is reported as
/// [`TimeSyncState::Stale`]: six hours, several missed hourly resyncs.
pub const DEFAULT_STALE_AFTER: Duration = Duration::from_secs(6 * 60 * 60);

/// A clock step of at least this many milliseconds is logged as a warning.
const SIGNIFICANT_STEP_MS: i128 = 1_000;

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
///
/// ```
/// use core::time::Duration;
/// use rohi_time::{DEFAULT_HORIZON, DEFAULT_MAX_UNCERTAINTY, DEFAULT_STALE_AFTER};
/// use rohi_time::{TimeConfig, UnixTime};
///
/// let floor = UnixTime::from_secs(1_767_225_600);
/// let config = TimeConfig::new(floor);
/// assert_eq!(config.not_before, floor);
/// assert_eq!(
///     config.not_after.as_secs(),
///     floor.as_secs() + DEFAULT_HORIZON.as_secs()
/// );
/// assert_eq!(config.stale_after, DEFAULT_STALE_AFTER);
/// assert_eq!(config.max_uncertainty, DEFAULT_MAX_UNCERTAINTY);
/// assert_eq!(DEFAULT_MAX_UNCERTAINTY, Duration::from_secs(5));
/// ```
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
/// All methods take `&self`; place the service in a `static`. `C` is the
/// monotonic [`Clock`], `M` the [`RawMutex`] that guards the anchor and the
/// counters; each call is one short critical section and nothing awaits. See
/// the [crate documentation](crate) for the guarantees, in particular that the
/// wall-clock time may step.
///
/// ```
/// use core::cell::Cell;
/// use embassy_sync::blocking_mutex::raw::NoopRawMutex;
/// use rohi_hal::MonotonicTimestamp;
/// use rohi_hal::runtime::Clock;
/// use rohi_time::{
///     SyncFailure, TimeConfig, TimeSample, TimeService, TimeSourceKind, TimeSyncState,
///     UnixTime,
/// };
///
/// struct Uptime<'a>(&'a Cell<u64>);
/// impl Clock for Uptime<'_> {
///     fn now(&self) -> MonotonicTimestamp {
///         MonotonicTimestamp::from_millis(self.0.get())
///     }
/// }
///
/// let uptime = Cell::new(0);
/// let floor = UnixTime::from_secs(1_767_225_600);
/// let time = TimeService::<_, NoopRawMutex>::new(Uptime(&uptime), TimeConfig::new(floor));
///
/// // An implausible sample (before the floor) is refused and counted.
/// let early = TimeSample { unix_time: UnixTime::from_secs(1_000), uncertainty: None };
/// assert_eq!(time.submit(TimeSourceKind::Ntp, early), Err(SyncFailure::Implausible));
/// assert_eq!(time.now(), None);
/// assert_eq!(time.status().consecutive_failures, 1);
///
/// // A plausible one becomes the anchor; the time then advances with the clock.
/// let sample = TimeSample { unix_time: UnixTime::from_secs(1_780_000_000), uncertainty: None };
/// time.submit(TimeSourceKind::Ntp, sample).unwrap();
/// uptime.set(2_500);
/// assert_eq!(time.now(), Some(UnixTime::from_millis(1_780_000_002_500)));
/// let snapshot = time.snapshot();
/// assert_eq!(snapshot.monotonic.millis, 2_500);
/// assert_eq!(snapshot.state, TimeSyncState::Synchronized);
///
/// // Six hours later without a new sample the time is stale but still usable.
/// uptime.set(2_500 + 6 * 3_600_000 + 1);
/// assert_eq!(time.status().state, TimeSyncState::Stale);
/// assert!(time.now().is_some());
/// ```
#[derive(Service)]
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

    /// Monotonic and wall-clock time of the same instant (one reading of the
    /// clock), plus the state.
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
    /// untouched and counts as a failure ([`SyncFailure::Implausible`]); an
    /// accepted one clears the failure counter and the last failure.
    ///
    /// # Errors
    ///
    /// [`SyncFailure::Implausible`] when the sample is outside
    /// `not_before..=not_after` of the [`TimeConfig`] or its uncertainty
    /// exceeds `max_uncertainty`.
    pub fn submit(&self, source: TimeSourceKind, sample: TimeSample) -> Result<(), SyncFailure> {
        if !self.is_plausible(&sample) {
            warn!(
                target: "time",
                "Rejected implausible time sample (source={:?} uncertainty_ms={:?})",
                source,
                sample.uncertainty.map(|u| u.as_millis())
            );
            self.record_failure(SyncFailure::Implausible);
            return Err(SyncFailure::Implausible);
        }
        let now = self.clock.now();
        let step = self.inner.lock(|inner| {
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
            step
        });
        // Log outside the critical section: the log sink stamps entries with
        // `now()`, which borrows the same cell.
        match step {
            None => info!(target: "time", "Clock synchronized (source={:?})", source),
            Some(delta_ms) if delta_ms.abs() >= SIGNIFICANT_STEP_MS => warn!(
                target: "time",
                "Clock adjusted significantly (source={:?} delta_ms={})",
                source, delta_ms
            ),
            Some(delta_ms) => debug!(
                target: "time",
                "Clock resynchronized (source={:?} delta_ms={})",
                source, delta_ms
            ),
        }
        debug!(
            target: "time",
            "Time anchor set (source={:?} uncertainty_ms={:?})",
            source,
            sample.uncertainty.map(|u| u.as_millis())
        );
        Ok(())
    }

    /// Records a failed synchronization attempt: the failure counter grows
    /// (saturating) and `failure` becomes the last failure. The anchor is
    /// untouched.
    pub fn record_failure(&self, failure: SyncFailure) {
        let failures = self.inner.lock(|inner| {
            let mut inner = inner.borrow_mut();
            inner.consecutive_failures = inner.consecutive_failures.saturating_add(1);
            inner.last_failure = Some(failure);
            inner.consecutive_failures
        });
        // Outside the critical section, see `submit`.
        debug!(
            target: "time",
            "Synchronization failure recorded (reason={:?} count={})",
            failure, failures
        );
    }

    /// Records that sampling was not even attempted because the source is not
    /// available (for example no network). Not counted as a failed attempt.
    pub fn record_unavailable(&self, failure: SyncFailure) {
        let previous = self
            .inner
            .lock(|inner| inner.borrow_mut().last_failure.replace(failure));
        // Polled every few seconds while the network is down: log the change only.
        if previous != Some(failure) {
            trace!(target: "time", "Time source unavailable (reason={:?})", failure);
        }
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

/// Issue codes of the `time` service.
pub mod issue {
    use rohi_service::FIRST_SERVICE_ISSUE as BASE;

    /// The network is unavailable.
    pub const NETWORK_UNAVAILABLE: u32 = BASE;
    /// The time server name was not resolved.
    pub const RESOLVE_FAILED: u32 = BASE + 1;
    /// The time source did not answer.
    pub const TIMEOUT: u32 = BASE + 2;
    /// The answer was invalid.
    pub const INVALID_RESPONSE: u32 = BASE + 3;
    /// The time source refused to serve.
    pub const REJECTED: u32 = BASE + 4;
    /// The sample was not plausible.
    pub const IMPLAUSIBLE: u32 = BASE + 5;
    /// The time source failed.
    pub const SOURCE_FAILURE: u32 = BASE + 6;
}

/// Values of the `source` status field, the wire values of
/// [`TimeSourceKind`].
pub mod source_value {
    /// Network time protocol.
    pub const NTP: u32 = 0;
    /// Battery-backed clock.
    pub const RTC: u32 = 1;
    /// Set by the application.
    pub const EXTERNAL: u32 = 2;
}

/// Field ids of the `time` service status.
pub mod status_field {
    /// Milliseconds since the Unix epoch; absent while unsynchronized.
    pub const UNIX_TIME_MS: u32 = 1;
    /// Source of the current time.
    pub const SOURCE: u32 = 2;
    /// Time since the current time was accepted.
    pub const SYNC_AGE_MS: u32 = 3;
    /// Uncertainty of the current time when it was accepted.
    pub const UNCERTAINTY_MS: u32 = 4;
    /// Failed attempts since the last accepted sample.
    pub const CONSECUTIVE_FAILURES: u32 = 5;
}

fn failure_issue(failure: SyncFailure) -> Issue {
    let code = match failure {
        SyncFailure::NetworkUnavailable => issue::NETWORK_UNAVAILABLE,
        SyncFailure::Resolve => issue::RESOLVE_FAILED,
        SyncFailure::Timeout => issue::TIMEOUT,
        SyncFailure::InvalidResponse => issue::INVALID_RESPONSE,
        SyncFailure::Rejected => issue::REJECTED,
        SyncFailure::Implausible => issue::IMPLAUSIBLE,
        SyncFailure::Source => issue::SOURCE_FAILURE,
    };
    Issue {
        severity: IssueSeverity::Warning,
        code,
        age_ms: None,
    }
}

/// The [`TimeService`] is the `time` service of the status plane.
///
/// Wall-clock time is optional: the device has none until a source delivered
/// a plausible sample, and it is never invented.
impl<C: Clock, M: RawMutex> Status for TimeService<C, M> {
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = &[
        FieldDescriptor::new(status_field::UNIX_TIME_MS, "unix_time", ValueKind::U64)
            .unit(Unit::Ms),
        FieldDescriptor::new(status_field::SOURCE, "source", ValueKind::Enum).with_enum(1),
        FieldDescriptor::new(status_field::SYNC_AGE_MS, "sync_age", ValueKind::U64).unit(Unit::Ms),
        FieldDescriptor::new(status_field::UNCERTAINTY_MS, "uncertainty", ValueKind::U64)
            .unit(Unit::Ms),
        FieldDescriptor::new(
            status_field::CONSECUTIVE_FAILURES,
            "consecutive_failures",
            ValueKind::U64,
        ),
    ];
    const ENUMS: &'static [EnumDescriptor] = &[EnumDescriptor::new(
        1,
        &[
            EnumVariant::new(source_value::NTP, "ntp"),
            EnumVariant::new(source_value::RTC, "rtc"),
            EnumVariant::new(source_value::EXTERNAL, "external"),
        ],
    )];
    const ISSUES: &'static [IssueDescriptor] = &[
        IssueDescriptor::new(issue::NETWORK_UNAVAILABLE, "network_unavailable"),
        IssueDescriptor::new(issue::RESOLVE_FAILED, "resolve_failed"),
        IssueDescriptor::new(issue::TIMEOUT, "timeout"),
        IssueDescriptor::new(issue::INVALID_RESPONSE, "invalid_response"),
        IssueDescriptor::new(issue::REJECTED, "rejected"),
        IssueDescriptor::new(issue::IMPLAUSIBLE, "implausible"),
        IssueDescriptor::new(issue::SOURCE_FAILURE, "source_failure"),
    ];
    const ESSENTIAL: bool = false;

    async fn summary(&self) -> ServiceSummary {
        use ServiceHealth as H;
        use ServiceState as S;
        let status = self.status();
        match status.state {
            TimeSyncState::Synchronized => ServiceSummary::new(S::Running, H::Ok, true),
            // The time is usable but may have drifted.
            TimeSyncState::Stale => ServiceSummary::new(S::Running, H::Degraded, true),
            TimeSyncState::Unsynchronized if status.last_failure.is_some() => {
                ServiceSummary::new(S::Running, H::Degraded, false)
            }
            // Nothing failed yet: still waiting for the first sample.
            TimeSyncState::Unsynchronized => ServiceSummary::new(S::Starting, H::Unknown, false),
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        if let Some(failure) = self.status().last_failure {
            let _ = out.push(failure_issue(failure));
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        let status = self.status();
        if let Some(now) = self.now() {
            out.u64(status_field::UNIX_TIME_MS, now.as_millis())?;
        }
        if let Some(source) = status.source {
            out.variant(
                status_field::SOURCE,
                match source {
                    TimeSourceKind::Ntp => source_value::NTP,
                    TimeSourceKind::Rtc => source_value::RTC,
                    TimeSourceKind::External => source_value::EXTERNAL,
                },
            )?;
        }
        if let Some(age) = status.last_sync_age {
            out.u64(status_field::SYNC_AGE_MS, age.as_millis() as u64)?;
        }
        if let Some(uncertainty) = status.uncertainty {
            out.u64(status_field::UNCERTAINTY_MS, uncertainty.as_millis() as u64)?;
        }
        out.u64(
            status_field::CONSECUTIVE_FAILURES,
            u64::from(status.consecutive_failures),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use embassy_futures::block_on;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_service::{Service, StatusRegistry, Statuses, Value};

    use super::*;
    use crate::testutil::ManualClock;
    use crate::{TimeConfig, TimeService};

    #[test]
    fn an_unsynchronized_device_has_no_time_and_is_starting() {
        let service = TimeService::<_, NoopRawMutex>::new(
            ManualClock::default(),
            TimeConfig::new(crate::UnixTime::from_secs(1_700_000_000)),
        );
        let registry = Statuses::<NoopRawMutex, _>::new((&service,));
        let status =
            block_on(registry.status(TimeService::<ManualClock, NoopRawMutex>::ID, 0)).unwrap();
        assert_eq!(status.summary.state, rohi_service::ServiceState::Starting);
        assert!(!status.summary.ready);
        assert!(status.issues.is_empty());
        // No time and no source: only the failure counter.
        assert_eq!(status.fields.len(), 1);
        assert_eq!(
            status.fields[0],
            (status_field::CONSECUTIVE_FAILURES, Value::U64(0))
        );
    }

    #[test]
    fn the_schema_lists_the_sources() {
        type S = TimeService<ManualClock, NoopRawMutex>;
        assert_eq!(<S as Service>::NAME, "time");
        assert!(<S as Status>::ENUMS[0].contains(source_value::RTC));
        assert_eq!(<S as Status>::STATUS_SCHEMA[1].enum_id, 1);
        const { assert!(!<S as Status>::ESSENTIAL) };
    }
}
