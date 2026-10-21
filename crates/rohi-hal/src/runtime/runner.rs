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
//! Translates driver results into the `rohi-sensor-core` lifecycle:
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

use core::fmt::Debug;

use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_hal_async::delay::DelayNs;
use rohi_sensor_core::{MeasurementUpdate, SensorErrorCode, SensorKind, SensorState};

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

    /// Delay between two calls of [`SensorDriver::sample`], in milliseconds.
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

    /// Perform one probe attempt or one sampling attempt, including the
    /// following wait.
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
                self.detected = true;
                self.phase = Phase::Running;
                self.failures = 0;
                self.backoff.reset();
                self.reporter.set_state(if D::WARMS_UP {
                    SensorState::WarmingUp
                } else {
                    SensorState::Ready
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
        self.delay.delay_ms(self.driver.poll_interval_ms()).await;

        match self.driver.sample(self.reporter.clock()).await {
            Ok(Some(update)) => {
                self.failures = 0;
                self.backoff.reset();
                self.reporter.set_state(SensorState::Ready);
                self.reporter.measurement(update);
            }
            Ok(None) => {}
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
    use crate::testutil::{FixedClock, NoDelay, block_on};
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use rohi_sensor_core::{Bmx280Update, Celsius, MonotonicTimestamp, Pascal, SensorEvent};

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

        fn poll_interval_ms(&self) -> u32 {
            1000
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
            [
                SensorState::Initializing,
                SensorState::Ready,
                SensorState::Degraded,
                SensorState::Ready
            ]
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
                SensorState::Ready,
                SensorState::Degraded,
                SensorState::Error,
                SensorState::Initializing,
                SensorState::Error,
                SensorState::Initializing,
                SensorState::Ready,
            ]
        );
        assert!(matches!(events.last(), Some(SensorEvent::Measurements(_))));
    }
}
