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
//! Noise level measurement from a digital MEMS microphone (I2S).
//!
//! Raw audio is never retained: samples are processed in small fixed-size
//! chunks and only running statistics are kept.
//!
//! # Method
//!
//! * **Sample rate**: [`SAMPLE_RATE_HZ`] (16 kHz), mono, 24-bit samples
//!   right-justified in an `i32`.
//! * **DC removal**: first-order high-pass (`y[n] = x[n] - x[n-1] + y[n-1] *
//!   (1 - 2^-8)`, about 10 Hz corner), in integer arithmetic.
//! * **Sub-window**: every [`SUB_WINDOW_MS`] (125 ms, "fast" time interval)
//!   the mean square of the filtered samples gives one sound level.
//! * **Measurement window**: [`WINDOW_MS`] (10 s, 80 sub-windows).
//!   *Average* is the energy average of the window (equivalent continuous
//!   level, `Leq`), *maximum* is the highest sub-window level.
//! * **Units**: dB SPL, unweighted (no A-weighting):
//!   `level = 10 * log10(mean_square) - 20 * log10(2^23) + CALIBRATION_OFFSET_DB`.
//!   The offset assumes an ICS-43434 microphone (sensitivity -26 dBFS at
//!   94 dB SPL), so full scale is 120 dB SPL (`94 + 26`) and the measurable floor
//!   is about -18 dB SPL (one LSB), far below the microphone noise floor.
//!   Adjust [`Config::calibration_offset_db`] for other microphones.
//!
//! CPU load is a handful of integer operations per sample plus one logarithm
//! per sub-window.

use core::fmt::Debug;

use embedded_hal_async::delay::DelayNs;
use rohi_sensor_core::{Decibels, MeasurementUpdate, NoiseUpdate, SensorErrorCode, SensorKind};

use crate::runtime::{Clock, DriverError, SensorDriver, with_timeout};

/// Audio sample rate.
pub const SAMPLE_RATE_HZ: u32 = 16_000;
/// Level calculation interval.
pub const SUB_WINDOW_MS: u32 = 125;
/// Measurement window.
pub const WINDOW_MS: u32 = 10_000;
/// Samples processed per read; the only audio buffer (1 KiB of stack).
pub const CHUNK_SAMPLES: usize = 256;
/// Bound of a single read of the audio source.
pub const READ_TIMEOUT_MS: u32 = 1_000;
/// Offset converting dBFS to dB SPL for an ICS-43434.
pub const CALIBRATION_OFFSET_DB: f32 = 120.0;

/// Full scale of 24-bit samples in dB: `20 * log10(2^23)`.
const FULL_SCALE_DB: f32 = 138.47;

/// Noise measurement parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Config {
    /// Samples per sub-window.
    pub sub_window_samples: u32,
    /// Sub-windows per measurement window.
    pub sub_windows_per_window: u32,
    /// dBFS to dB SPL conversion offset.
    pub calibration_offset_db: f32,
}

impl Config {
    /// Default parameters, see the module documentation.
    pub const DEFAULT: Self = Self {
        sub_window_samples: SAMPLE_RATE_HZ / 1000 * SUB_WINDOW_MS,
        sub_windows_per_window: WINDOW_MS / SUB_WINDOW_MS,
        calibration_offset_db: CALIBRATION_OFFSET_DB,
    };
}

impl Default for Config {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Result of one measurement window, in dB SPL.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseLevels {
    /// Energy-average level of the window.
    pub average: f32,
    /// Highest sub-window level.
    pub maximum: f32,
}

/// Streaming level meter holding only running statistics.
#[derive(Debug, Clone, Copy)]
pub struct LevelMeter {
    config: Config,
    primed: bool,
    prev_x: i64,
    prev_y: i64,
    sum_sq: u64,
    sub_samples: u32,
    energy_sum: f64,
    max_mean_sq: f32,
    sub_windows: u32,
}

impl LevelMeter {
    /// Create a meter.
    pub const fn new(config: Config) -> Self {
        Self {
            config,
            primed: false,
            prev_x: 0,
            prev_y: 0,
            sum_sq: 0,
            sub_samples: 0,
            energy_sum: 0.0,
            max_mean_sq: 0.0,
            sub_windows: 0,
        }
    }

    fn level_db(&self, mean_square: f64) -> f32 {
        // At least one LSB of power, so silence yields a finite floor.
        let ms = if mean_square < 1.0 { 1.0 } else { mean_square };
        10.0 * libm::log10f(ms as f32) - FULL_SCALE_DB + self.config.calibration_offset_db
    }

    /// Process samples; returns the window result once a window completes.
    pub fn process(&mut self, samples: &[i32]) -> Option<NoiseLevels> {
        let mut result = None;
        for &sample in samples {
            let x = sample as i64;
            if !self.primed {
                // Start the high-pass at the first sample to avoid a DC step.
                self.prev_x = x;
                self.primed = true;
            }
            let y = x - self.prev_x + self.prev_y - (self.prev_y >> 8);
            self.prev_x = x;
            self.prev_y = y;
            // |y| < 2^33, so y * y < 2^66 would overflow; clamp to 24 bit range
            // (the DC blocker can overshoot to about twice the input range).
            let y = y.clamp(-(1 << 24), 1 << 24);
            self.sum_sq += (y * y) as u64;
            self.sub_samples += 1;

            if self.sub_samples == self.config.sub_window_samples {
                let mean_sq = self.sum_sq as f64 / self.sub_samples as f64;
                self.sum_sq = 0;
                self.sub_samples = 0;
                self.energy_sum += mean_sq;
                self.max_mean_sq = self.max_mean_sq.max(mean_sq as f32);
                self.sub_windows += 1;

                if self.sub_windows == self.config.sub_windows_per_window {
                    result = Some(NoiseLevels {
                        average: self.level_db(self.energy_sum / self.sub_windows as f64),
                        maximum: self.level_db(self.max_mean_sq as f64),
                    });
                    self.energy_sum = 0.0;
                    self.max_mean_sq = 0.0;
                    self.sub_windows = 0;
                }
            }
        }
        result
    }
}

/// Source of audio samples: signed 24-bit values right-justified in `i32`.
#[allow(async_fn_in_trait)]
pub trait AudioSource {
    /// Source error.
    type Error: Debug;

    /// Fill `out` with at most `out.len()` samples, returning how many.
    async fn read(&mut self, out: &mut [i32]) -> Result<usize, Self::Error>;
}

/// Noise driver error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error<E> {
    /// The audio source failed.
    Source(E),
    /// The audio source delivered no samples in time.
    NoData,
}

impl<E: Debug> DriverError for Error<E> {
    fn code(&self) -> SensorErrorCode {
        match self {
            Error::Source(_) => SensorErrorCode::Bus,
            Error::NoData => SensorErrorCode::Timeout,
        }
    }

    fn is_absent(&self) -> bool {
        matches!(self, Error::NoData)
    }
}

/// Noise driver.
pub struct Noise<S, D> {
    source: S,
    delay: D,
    meter: LevelMeter,
}

impl<S: AudioSource, D: DelayNs> Noise<S, D> {
    /// Create a driver around an audio source.
    pub fn new(source: S, delay: D, config: Config) -> Self {
        Self {
            source,
            delay,
            meter: LevelMeter::new(config),
        }
    }

    async fn read_chunk(&mut self, chunk: &mut [i32]) -> Result<usize, Error<S::Error>> {
        match with_timeout(&mut self.delay, READ_TIMEOUT_MS, self.source.read(chunk)).await {
            Err(_) => Err(Error::NoData),
            Ok(Err(e)) => Err(Error::Source(e)),
            Ok(Ok(0)) => Err(Error::NoData),
            Ok(Ok(n)) => Ok(n),
        }
    }

    /// Acquire samples until one measurement window is complete.
    pub async fn measure_window(&mut self) -> Result<NoiseLevels, Error<S::Error>> {
        let mut chunk = [0i32; CHUNK_SAMPLES];
        loop {
            let n = self.read_chunk(&mut chunk).await?;
            if let Some(levels) = self.meter.process(&chunk[..n]) {
                return Ok(levels);
            }
        }
    }
}

impl<S: AudioSource, D: DelayNs> SensorDriver for Noise<S, D> {
    const KIND: SensorKind = SensorKind::Microphone;
    const WARMS_UP: bool = false;
    type Error = Error<S::Error>;

    async fn init(&mut self) -> Result<(), Self::Error> {
        // Presence check: the microphone must deliver samples.
        let mut chunk = [0i32; CHUNK_SAMPLES];
        self.read_chunk(&mut chunk).await?;
        self.meter = LevelMeter::new(self.meter.config);
        Ok(())
    }

    async fn sample<C: Clock>(
        &mut self,
        clock: &C,
    ) -> Result<Option<MeasurementUpdate>, Self::Error> {
        let levels = self.measure_window().await?;
        Ok(Some(MeasurementUpdate::Microphone(NoiseUpdate {
            average: Decibels(levels.average),
            maximum: Decibels(levels.maximum),
            timestamp: clock.now(),
        })))
    }

    fn poll_interval_ms(&self) -> u32 {
        // Acquisition is continuous; the measurement window sets the cadence.
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{FixedClock, NoDelay, block_on};
    use rohi_sensor_core::MonotonicTimestamp;
    use std::vec::Vec;

    fn small_config() -> Config {
        Config {
            sub_window_samples: 1_000,
            sub_windows_per_window: 4,
            calibration_offset_db: CALIBRATION_OFFSET_DB,
        }
    }

    fn sine(amplitude: f64, n: usize) -> Vec<i32> {
        (0..n)
            .map(|i| {
                let t = i as f64 / SAMPLE_RATE_HZ as f64;
                (amplitude * (2.0 * core::f64::consts::PI * 1_000.0 * t).sin()) as i32
            })
            .collect()
    }

    #[test]
    fn sine_level_matches_expected_spl() {
        // Peak at -6.02 dBFS => RMS -9.03 dBFS => 110.97 dB SPL.
        let mut meter = LevelMeter::new(small_config());
        let samples = sine((1i64 << 22) as f64, 4_000);
        let levels = meter.process(&samples).expect("window complete");
        assert!((levels.average - 110.97).abs() < 0.3, "{levels:?}");
        assert!((levels.maximum - 110.97).abs() < 0.3, "{levels:?}");
    }

    #[test]
    fn silence_and_dc_are_at_the_floor() {
        let mut meter = LevelMeter::new(small_config());
        let levels = meter.process(&[0; 4_000]).unwrap();
        assert!(levels.average < -17.0, "{levels:?}");

        let mut meter = LevelMeter::new(small_config());
        let levels = meter.process(&[1_000_000; 4_000]).unwrap();
        assert!(levels.average < 40.0, "{levels:?}");
    }

    #[test]
    fn maximum_reflects_loud_burst_and_average_is_energy_mean() {
        let mut meter = LevelMeter::new(small_config());
        let mut samples = sine(10_000.0, 3_000);
        samples.extend(sine((1i64 << 22) as f64, 1_000));
        let levels = meter.process(&samples).unwrap();
        assert!(levels.maximum > levels.average + 3.0, "{levels:?}");
        assert!((levels.maximum - 110.97).abs() < 0.5, "{levels:?}");
    }

    #[test]
    fn window_restarts_after_completion() {
        let mut meter = LevelMeter::new(small_config());
        assert!(meter.process(&sine(1_000.0, 3_999)).is_none());
        assert!(meter.process(&[0]).is_some());
        assert!(meter.process(&sine(1_000.0, 3_999)).is_none());
    }

    #[test]
    fn chunked_processing_equals_one_shot() {
        let samples = sine(500_000.0, 4_000);
        let mut one = LevelMeter::new(small_config());
        let expected = one.process(&samples).unwrap();
        let mut chunked = LevelMeter::new(small_config());
        let mut got = None;
        for chunk in samples.chunks(CHUNK_SAMPLES) {
            got = got.or(chunked.process(chunk));
        }
        assert_eq!(got, Some(expected));
    }

    struct SineSource {
        phase: usize,
        fail: bool,
        silent: bool,
    }

    impl AudioSource for SineSource {
        type Error = ();

        async fn read(&mut self, out: &mut [i32]) -> Result<usize, ()> {
            if self.fail {
                self.fail = false;
                return Err(());
            }
            if self.silent {
                return core::future::pending().await;
            }
            let data = sine(1_000_000.0, self.phase + out.len());
            out.copy_from_slice(&data[self.phase..]);
            self.phase += out.len();
            Ok(out.len())
        }
    }

    #[test]
    fn driver_publishes_window_levels() {
        let source = SineSource {
            phase: 0,
            fail: false,
            silent: false,
        };
        let mut noise = Noise::new(source, NoDelay, small_config());
        let clock = FixedClock(MonotonicTimestamp::from_millis(77));
        let Some(MeasurementUpdate::Microphone(u)) = block_on(noise.sample(&clock)).unwrap() else {
            panic!("expected noise update")
        };
        assert!(u.average.0 <= u.maximum.0 + 0.01);
        assert!(u.average.0 > 90.0);
        assert_eq!(u.timestamp, MonotonicTimestamp::from_millis(77));
    }

    #[test]
    fn silent_source_is_absent_and_errors_are_mapped() {
        let source = SineSource {
            phase: 0,
            fail: false,
            silent: true,
        };
        let mut noise = Noise::new(source, NoDelay, small_config());
        let err = block_on(noise.init()).unwrap_err();
        assert_eq!(err, Error::NoData);
        assert!(err.is_absent());

        let source = SineSource {
            phase: 0,
            fail: true,
            silent: false,
        };
        let mut noise = Noise::new(source, NoDelay, small_config());
        let err = block_on(noise.measure_window()).unwrap_err();
        assert_eq!(err, Error::Source(()));
        assert_eq!(err.code(), SensorErrorCode::Bus);
        assert!(block_on(noise.measure_window()).is_ok());
    }
}
