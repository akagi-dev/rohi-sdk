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
//! Generic sensor lifecycle runner.
//!
//! Translates driver results into the `rohi-hal` lifecycle:
//!
//! ```text
//! NotDetected / Initializing -> WarmingUp -> Ready
//!        Ready -> Degraded -> Error -> (re-init) -> Ready
//! ```
//!
//! Sensor absence is not an error: a sensor that was never detected is
//! reported as `NotDetected` and probed again periodically. A sensor that was
//! detected before and then disappears is reported as `Error` and keeps being
//! recovered with exponential backoff, so reconnecting needs no reboot.
//!
//! # Timing
//!
//! Four delays exist and are never combined:
//!
//! * **initial delay** ([`SensorDriver::initial_delay_ms`]): wait before the
//!   very first sample after the sensor was first detected, `0` by default;
//! * **poll interval** ([`SensorDriver::poll_interval_ms`]): wait after a
//!   successful sample before the next one;
//! * **recovery backoff** ([`RunnerConfig`]): wait after a failed sample or
//!   init, growing exponentially up to a bound and reset by the next success.
//!   A retry waits for the backoff only, not for the poll interval on top;
//! * **sensor-internal warm-up**: owned by the driver, inside
//!   [`SensorDriver::sample`] (e.g. the SDS011 fan spin-up).
//!
//! # Events
//!
//! A successful sample is reported by one `Measurements` event; the store
//! derives `Ready` from it, so no observer can see `Ready` with an old value.
//! After a successful init the state is `WarmingUp` ([`SensorDriver::WARMS_UP`])
//! or still `Initializing`, until the first measurement arrives.

use core::fmt::Debug;

use crate::{MeasurementUpdate, SensorErrorCode, SensorKind, SensorState};
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;

use super::backoff::Backoff;
use super::clock::Clock;
use super::reporter::Reporter;

/// Error of a sensor driver, mapped into the domain model.
pub trait DriverError: Debug {
    /// Domain error code.
    fn code(&self) -> SensorErrorCode;

    /// The sensor does not answer on the bus (as opposed to a bus failure).
    fn is_absent(&self) -> bool;
}

/// A sensor driver managed by a [`Runner`].
#[allow(async_fn_in_trait)]
pub trait SensorDriver {
    /// Sensor produced by this driver.
    const KIND: SensorKind;

    /// The sensor needs a warm-up period before the first valid value.
    const WARMS_UP: bool;

    /// Driver error.
    type Error: DriverError;

    /// Probe and configure the sensor. Called again to recover after failures.
    async fn init(&mut self) -> Result<(), Self::Error>;

    /// Wait for and read the next observation. `Ok(None)`: no data yet.
    async fn sample<C: Clock>(
        &mut self,
        clock: &C,
    ) -> Result<Option<MeasurementUpdate>, Self::Error>;

    /// Wait before the first sample after the sensor was first detected, in
    /// milliseconds. Warm-up that belongs to the sensor is the driver's own
    /// business inside [`SensorDriver::sample`] and is not part of this delay.
    fn initial_delay_ms(&self) -> u32 {
        0
    }

    /// Wait after a successful [`SensorDriver::sample`] before the next call,
    /// in milliseconds. Not applied after failures, see [`RunnerConfig`].
    fn poll_interval_ms(&self) -> u32;
}

/// Timing and recovery policy of a [`Runner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerConfig {
    /// Delay between probes while the sensor was never detected.
    pub reprobe_interval_ms: u32,
    /// Consecutive failures after which the state becomes `Error`
    /// (fewer failures report `Degraded`).
    pub error_after: u32,
    /// Consecutive failures after which the driver is re-initialized.
    pub reinit_after: u32,
    /// First retry delay after a failure.
    pub backoff_initial_ms: u32,
    /// Maximum retry delay.
    pub backoff_max_ms: u32,
}

impl RunnerConfig {
    /// Defaults: reprobe every 10 s, `Error` after 3 failures, re-initialize
    /// after 3 failures, retry delay from 1 s up to 30 s.
    pub const DEFAULT: Self = Self {
        reprobe_interval_ms: 10_000,
        error_after: 3,
        reinit_after: 3,
        backoff_initial_ms: 1_000,
        backoff_max_ms: 30_000,
    };
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Detect,
    Running,
}

/// Drives one sensor: probing, periodic sampling and error recovery.
pub struct Runner<'a, D, M, C, T, const N: usize>
where
    D: SensorDriver,
    M: RawMutex,
    C: Clock,
    T: DelayNs,
{
    driver: D,
    reporter: Reporter<'a, M, C, N>,
    delay: T,
    config: RunnerConfig,
    backoff: Backoff,
    phase: Phase,
    failures: u32,
    probes: u32,
    detected: bool,
    initial_delay_pending: bool,
}

impl<'a, D, M, C, T, const N: usize> Runner<'a, D, M, C, T, N>
where
    D: SensorDriver,
    M: RawMutex,
    C: Clock,
    T: DelayNs,
{
    /// Create a runner.
    pub fn new(driver: D, reporter: Reporter<'a, M, C, N>, delay: T, config: RunnerConfig) -> Self {
        Self {
            driver,
            reporter,
            delay,
            backoff: Backoff::new(config.backoff_initial_ms, config.backoff_max_ms),
            config,
            phase: Phase::Detect,
            failures: 0,
            probes: 0,
            detected: false,
            initial_delay_pending: false,
        }
    }

    /// Reporter, e.g. to inspect the last reported state.
    pub fn reporter(&self) -> &Reporter<'a, M, C, N> {
        &self.reporter
    }

    /// Run forever.
    pub async fn run(&mut self) -> ! {
        loop {
            self.step().await;
        }
    }

    /// Perform one probe attempt or one sampling attempt, including the wait
    /// that follows it (poll interval after success, backoff after failure).
    pub async fn step(&mut self) {
        match self.phase {
            Phase::Detect => self.detect().await,
            Phase::Running => self.poll().await,
        }
    }

    async fn detect(&mut self) {
        // Re-probing a sensor that was never found stays quiet.
        if self.detected || self.probes == 0 {
            self.reporter.set_state(SensorState::Initializing);
        }
        self.probes = self.probes.saturating_add(1);

        match self.driver.init().await {
            Ok(()) => {
                self.initial_delay_pending = !self.detected;
                self.detected = true;
                self.phase = Phase::Running;
                self.failures = 0;
                self.reporter.set_state(if D::WARMS_UP {
                    SensorState::WarmingUp
                } else {
                    SensorState::Initializing
                });
            }
            Err(e) if e.is_absent() && !self.detected => {
                log::debug!("[{:?}] not detected", D::KIND);
                self.reporter.set_state(SensorState::NotDetected);
                self.delay.delay_ms(self.config.reprobe_interval_ms).await;
            }
            Err(e) => {
                log::warn!("[{:?}] init failed: {:?}", D::KIND, e);
                self.reporter.error(e.code());
                self.reporter.set_state(SensorState::Error);
                let wait = self.backoff.next_delay_ms();
                self.delay.delay_ms(wait).await;
            }
        }
    }

    async fn poll(&mut self) {
        if core::mem::take(&mut self.initial_delay_pending) {
            let initial_delay = self.driver.initial_delay_ms();
            if initial_delay > 0 {
                self.delay.delay_ms(initial_delay).await;
            }
        }

        match self.driver.sample(self.reporter.clock()).await {
            Ok(Some(update)) => {
                self.failures = 0;
                self.backoff.reset();
                self.reporter.measurement(update);
                self.delay.delay_ms(self.driver.poll_interval_ms()).await;
            }
            Ok(None) => {
                self.delay.delay_ms(self.driver.poll_interval_ms()).await;
            }
            Err(e) => {
                log::warn!("[{:?}] sample failed: {:?}", D::KIND, e);
                self.failures = self.failures.saturating_add(1);
                self.reporter.error(e.code());
                self.reporter
                    .set_state(if self.failures >= self.config.error_after {
                        SensorState::Error
                    } else {
                        SensorState::Degraded
                    });
                if self.failures >= self.config.reinit_after {
                    self.phase = Phase::Detect;
                }
                let wait = self.backoff.next_delay_ms();
                self.delay.delay_ms(wait).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::bus::EventBus;
    use crate::testutil::{FixedClock, NoDelay, RecordingDelay, block_on};
    use crate::{Bmx280Update, Celsius, MonotonicTimestamp, Pascal, SensorEvent};
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;

    // Different from every backoff step, so waits are attributable.
    const POLL_INTERVAL_MS: u32 = 90_000;

    #[derive(Debug, Clone, Copy)]
    enum FakeError {
        Absent,
        Bus,
    }

    impl DriverError for FakeError {
        fn code(&self) -> SensorErrorCode {
            match self {
                FakeError::Absent => SensorErrorCode::Timeout,
                FakeError::Bus => SensorErrorCode::Bus,
            }
        }
        fn is_absent(&self) -> bool {
            matches!(self, FakeError::Absent)
        }
    }

    struct FakeDriver<const I: usize, const S: usize> {
        inits: [Result<(), FakeError>; I],
        samples: [Result<Option<f32>, FakeError>; S],
        initial_delay_ms: u32,
        init_at: usize,
        sample_at: usize,
    }

    impl<const I: usize, const S: usize> SensorDriver for FakeDriver<I, S> {
        const KIND: SensorKind = SensorKind::Bmx280;
        const WARMS_UP: bool = false;
        type Error = FakeError;

        async fn init(&mut self) -> Result<(), FakeError> {
            let r = self.inits[self.init_at.min(I - 1)];
            self.init_at += 1;
            r
        }

        async fn sample<C: Clock>(
            &mut self,
            clock: &C,
        ) -> Result<Option<MeasurementUpdate>, FakeError> {
            let r = self.samples[self.sample_at.min(S - 1)];
            self.sample_at += 1;
            r.map(|v| {
                v.map(|t| {
                    MeasurementUpdate::Bmx280(Bmx280Update::bmp280(
                        Celsius(t),
                        Pascal(100_000.0),
                        clock.now(),
                    ))
                })
            })
        }

        fn initial_delay_ms(&self) -> u32 {
            self.initial_delay_ms
        }

        fn poll_interval_ms(&self) -> u32 {
            POLL_INTERVAL_MS
        }
    }

    type Bus = EventBus<NoopRawMutex, 32>;

    fn drain(bus: &Bus) -> std::vec::Vec<SensorEvent> {
        let mut v = std::vec::Vec::new();
        while let Some(e) = bus.try_receive() {
            v.push(e);
        }
        v
    }

    fn states(events: &[SensorEvent]) -> std::vec::Vec<SensorState> {
        events
            .iter()
            .filter_map(|e| match e {
                SensorEvent::StateChanged { state, .. } => Some(*state),
                _ => None,
            })
            .collect()
    }

    fn runner<'a, const I: usize, const S: usize>(
        bus: &'a Bus,
        driver: FakeDriver<I, S>,
    ) -> Runner<'a, FakeDriver<I, S>, NoopRawMutex, FixedClock, NoDelay, 32> {
        let reporter = Reporter::new(
            SensorKind::Bmx280,
            bus,
            FixedClock(MonotonicTimestamp::from_millis(5)),
        );
        Runner::new(driver, reporter, NoDelay, RunnerConfig::DEFAULT)
    }

    #[test]
    fn absent_sensor_is_not_detected_not_error() {
        let bus = Bus::new();
        let mut r = runner(
            &bus,
            FakeDriver::<1, 1> {
                inits: [Err(FakeError::Absent)],
                samples: [Ok(None)],
                initial_delay_ms: 0,
                init_at: 0,
                sample_at: 0,
            },
        );
        block_on(r.step());
        block_on(r.step());
        let events = drain(&bus);
        assert_eq!(
            states(&events),
            [SensorState::Initializing, SensorState::NotDetected]
        );
        assert!(
            events
                .iter()
                .all(|e| !matches!(e, SensorEvent::Error { .. }))
        );
    }

    #[test]
    fn transient_failure_degrades_then_recovers() {
        let bus = Bus::new();
        let mut r = runner(
            &bus,
            FakeDriver::<1, 3> {
                inits: [Ok(())],
                samples: [Err(FakeError::Bus), Ok(Some(21.0)), Ok(Some(22.0))],
                initial_delay_ms: 0,
                init_at: 0,
                sample_at: 0,
            },
        );
        for _ in 0..4 {
            block_on(r.step());
        }
        let events = drain(&bus);
        assert_eq!(
            states(&events),
            [SensorState::Initializing, SensorState::Degraded]
        );
        let measurements = events
            .iter()
            .filter(|e| matches!(e, SensorEvent::Measurements(_)))
            .count();
        assert_eq!(measurements, 2);
        assert_eq!(r.reporter().state(), SensorState::Ready);
    }

    #[test]
    fn repeated_failures_error_reinit_and_hot_reconnect() {
        let bus = Bus::new();
        let mut r = runner(
            &bus,
            FakeDriver::<3, 4> {
                // Initial detection, then the sensor disappears, then comes back.
                inits: [Ok(()), Err(FakeError::Absent), Ok(())],
                samples: [
                    Err(FakeError::Bus),
                    Err(FakeError::Bus),
                    Err(FakeError::Bus),
                    Ok(Some(20.0)),
                ],
                initial_delay_ms: 0,
                init_at: 0,
                sample_at: 0,
            },
        );
        // init, 3 failed samples, failed re-init (absent), successful re-init, sample.
        for _ in 0..7 {
            block_on(r.step());
        }
        let events = drain(&bus);
        assert_eq!(
            states(&events),
            [
                SensorState::Initializing,
                SensorState::Degraded,
                SensorState::Error,
                SensorState::Initializing,
                SensorState::Error,
                SensorState::Initializing,
            ]
        );
        assert!(matches!(events.last(), Some(SensorEvent::Measurements(_))));
    }

    fn timed_runner<'a, const I: usize, const S: usize>(
        bus: &'a Bus,
        driver: FakeDriver<I, S>,
    ) -> (
        Runner<'a, FakeDriver<I, S>, NoopRawMutex, FixedClock, RecordingDelay, 32>,
        RecordingDelay,
    ) {
        let delay = RecordingDelay::default();
        let reporter = Reporter::new(
            SensorKind::Bmx280,
            bus,
            FixedClock(MonotonicTimestamp::from_millis(5)),
        );
        (
            Runner::new(driver, reporter, delay.clone(), RunnerConfig::DEFAULT),
            delay,
        )
    }

    fn driver<const I: usize, const S: usize>(
        inits: [Result<(), FakeError>; I],
        samples: [Result<Option<f32>, FakeError>; S],
    ) -> FakeDriver<I, S> {
        FakeDriver {
            inits,
            samples,
            initial_delay_ms: 0,
            init_at: 0,
            sample_at: 0,
        }
    }

    #[test]
    fn first_sample_is_not_delayed_and_success_waits_the_poll_interval() {
        let bus = Bus::new();
        let (mut r, delay) = timed_runner(&bus, driver([Ok(())], [Ok(Some(20.0))]));
        block_on(r.step());
        assert!(delay.waits().is_empty());
        block_on(r.step());
        block_on(r.step());
        assert_eq!(delay.waits(), [POLL_INTERVAL_MS, POLL_INTERVAL_MS]);
    }

    #[test]
    fn initial_delay_applies_once_before_the_first_sample() {
        let bus = Bus::new();
        let mut d = driver([Ok(())], [Ok(Some(20.0))]);
        d.initial_delay_ms = 7_000;
        let (mut r, delay) = timed_runner(&bus, d);
        for _ in 0..3 {
            block_on(r.step());
        }
        assert_eq!(delay.waits(), [7_000, POLL_INTERVAL_MS, POLL_INTERVAL_MS]);
    }

    #[test]
    fn failure_waits_the_backoff_only_and_success_resets_it() {
        let bus = Bus::new();
        let (mut r, delay) = timed_runner(
            &bus,
            driver(
                [Ok(())],
                [
                    Err(FakeError::Bus),
                    Err(FakeError::Bus),
                    Ok(Some(20.0)),
                    Err(FakeError::Bus),
                ],
            ),
        );
        for _ in 0..5 {
            block_on(r.step());
        }
        // No failure is followed by the normal poll interval; the first
        // failure after a success starts again from the initial backoff.
        assert_eq!(delay.waits(), [1_000, 2_000, POLL_INTERVAL_MS, 1_000]);
    }

    #[test]
    fn backoff_is_bounded_and_not_reset_by_reinit() {
        let bus = Bus::new();
        let (mut r, delay) = timed_runner(&bus, driver([Ok(())], [Err(FakeError::Bus)]));
        // init, 3 failures, re-init, 3 failures, re-init, 1 failure.
        for _ in 0..10 {
            block_on(r.step());
        }
        assert_eq!(
            delay.waits(),
            [1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000]
        );
    }

    #[test]
    fn not_detected_sensor_is_reprobed_at_its_own_interval() {
        let bus = Bus::new();
        let (mut r, delay) = timed_runner(&bus, driver([Err(FakeError::Absent)], [Ok(None)]));
        block_on(r.step());
        block_on(r.step());
        assert_eq!(
            delay.waits(),
            [
                RunnerConfig::DEFAULT.reprobe_interval_ms,
                RunnerConfig::DEFAULT.reprobe_interval_ms
            ]
        );
    }

    #[test]
    fn success_is_one_measurement_event_and_ready_is_derived() {
        let bus = Bus::new();
        let mut r = runner(&bus, driver([Ok(())], [Ok(Some(21.0))]));
        for _ in 0..4 {
            block_on(r.step());
        }
        let events = drain(&bus);
        // One state change on init, then only measurements: no `Ready` event.
        assert_eq!(states(&events), [SensorState::Initializing]);
        assert_eq!(events.len(), 1 + 3);
        assert!(
            events[1..]
                .iter()
                .all(|e| matches!(e, SensorEvent::Measurements(_)))
        );
        assert_eq!(r.reporter().state(), SensorState::Ready);
    }

    #[test]
    fn reconnected_sensor_waits_for_its_first_measurement_to_be_ready() {
        let bus = Bus::new();
        let mut r = runner(
            &bus,
            driver([Err(FakeError::Absent), Ok(())], [Ok(Some(21.0))]),
        );
        block_on(r.step());
        assert_eq!(r.reporter().state(), SensorState::NotDetected);
        block_on(r.step());
        assert_eq!(r.reporter().state(), SensorState::Initializing);
        block_on(r.step());
        assert_eq!(r.reporter().state(), SensorState::Ready);
    }
}
