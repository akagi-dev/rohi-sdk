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
//! Measurements organized by the physical sensor that produced them.
//!
//! Observations of the same physical quantity from different sensors are never
//! merged: BME280 and SCD4x temperatures are independent fields. The model
//! records observations, it does not interpret them.

use crate::sensor::{SensorKind, SensorStatuses};
use crate::time::MonotonicTimestamp;
use crate::units::{
    Aqi, Celsius, Decibels, MicrogramsPerCubicMeter, Pascal, Ppb, Ppm, PulseCount, RadiationLevel,
    RelativeHumidity,
};

/// State of a single measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MeasurementState {
    /// No usable measurement is available.
    #[default]
    Unavailable,
    /// The sensor is warming up.
    WarmingUp,
    /// The value is a recent successful observation.
    Valid,
    /// The value is a successful observation, but older than the freshness
    /// policy allows.
    Stale,
    /// The sensor reported an error. The last valid value, if any, is kept.
    Error,
}

/// A measurement of a single quantity.
///
/// `value` and `updated_at` describe the last valid observation and are kept
/// while the state is [`MeasurementState::Error`] or
/// [`MeasurementState::WarmingUp`], which allows telling "never measured"
/// apart from "measured, but the sensor currently fails".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement<T> {
    /// Last valid value, if any.
    pub value: Option<T>,
    /// Current state of the measurement.
    pub state: MeasurementState,
    /// Time of the last valid observation, if any.
    pub updated_at: Option<MonotonicTimestamp>,
}

impl<T> Default for Measurement<T> {
    fn default() -> Self {
        Self::unavailable()
    }
}

impl<T> Measurement<T> {
    /// A measurement that was never observed.
    pub const fn unavailable() -> Self {
        Self {
            value: None,
            state: MeasurementState::Unavailable,
            updated_at: None,
        }
    }

    /// A valid measurement taken at `at`.
    pub const fn valid(value: T, at: MonotonicTimestamp) -> Self {
        Self {
            value: Some(value),
            state: MeasurementState::Valid,
            updated_at: Some(at),
        }
    }

    /// Whether the state is [`MeasurementState::Valid`].
    pub fn is_valid(&self) -> bool {
        self.state == MeasurementState::Valid
    }
}

/// Type-erased access to the state of a [`Measurement`].
pub(crate) trait Cell {
    fn state(&self) -> MeasurementState;
    fn set_state(&mut self, state: MeasurementState);
    fn has_value(&self) -> bool;
    fn updated_at(&self) -> Option<MonotonicTimestamp>;
}

impl<T> Cell for Measurement<T> {
    fn state(&self) -> MeasurementState {
        self.state
    }

    fn set_state(&mut self, state: MeasurementState) {
        self.state = state;
    }

    fn has_value(&self) -> bool {
        self.value.is_some()
    }

    fn updated_at(&self) -> Option<MonotonicTimestamp> {
        self.updated_at
    }
}

/// Measurements of one physical sensor.
pub(crate) trait Group {
    fn each(&mut self, f: &mut dyn FnMut(&mut dyn Cell));
}

/// Time after which a [`MeasurementState::Valid`] measurement becomes
/// [`MeasurementState::Stale`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreshnessPolicy {
    /// Maximum age of a valid measurement, in milliseconds.
    pub stale_after_ms: u64,
}

impl FreshnessPolicy {
    /// Create a policy.
    pub const fn new(stale_after_ms: u64) -> Self {
        Self { stale_after_ms }
    }

    /// Default policy for a sensor: several times its measurement cadence, so
    /// normal operation never shows `Stale`. The WP2 sensors are checked
    /// against their driver cadences by tests in `rohi-hal`; thresholds can
    /// still be tuned through [`crate::MeasurementStore::set_freshness_policy`].
    pub const fn default_for(sensor: SensorKind) -> Self {
        match sensor {
            // Every 10 s.
            SensorKind::Bmx280 => Self::new(60_000),
            // Every 5 s.
            SensorKind::Scd4x => Self::new(30_000),
            // Every ~2.5 min: 30 s warm-up, sampling window, 90 s sleep.
            SensorKind::Sds011 => Self::new(300_000),
            // A result every 10 s window.
            SensorKind::Microphone => Self::new(30_000),
            SensorKind::Zmod4510 => Self::new(120_000),
            SensorKind::Ags3871 => Self::new(120_000),
            SensorKind::RadSens => Self::new(60_000),
        }
    }

    pub(crate) fn is_stale(&self, updated_at: MonotonicTimestamp, now: MonotonicTimestamp) -> bool {
        now.saturating_millis_since(updated_at) > self.stale_after_ms
    }
}

/// BMP280 / BME280 measurements.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Bmx280Measurements {
    /// Temperature.
    pub temperature: Measurement<Celsius>,
    /// Relative humidity. `None` unless the sensor is a BME280: the BMP280
    /// has no humidity channel.
    pub humidity: Option<Measurement<RelativeHumidity>>,
    /// Pressure.
    pub pressure: Measurement<Pascal>,
}

impl Group for Bmx280Measurements {
    fn each(&mut self, f: &mut dyn FnMut(&mut dyn Cell)) {
        f(&mut self.temperature);
        if let Some(humidity) = &mut self.humidity {
            f(humidity);
        }
        f(&mut self.pressure);
    }
}

/// SCD4x measurements.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Scd4xMeasurements {
    /// CO2 concentration.
    pub co2: Measurement<Ppm>,
    /// Temperature.
    pub temperature: Measurement<Celsius>,
    /// Relative humidity.
    pub humidity: Measurement<RelativeHumidity>,
}

impl Group for Scd4xMeasurements {
    fn each(&mut self, f: &mut dyn FnMut(&mut dyn Cell)) {
        f(&mut self.co2);
        f(&mut self.temperature);
        f(&mut self.humidity);
    }
}

/// SDS011 measurements.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Sds011Measurements {
    /// PM2.5 concentration.
    pub pm25: Measurement<MicrogramsPerCubicMeter>,
    /// PM10 concentration.
    pub pm10: Measurement<MicrogramsPerCubicMeter>,
}

impl Group for Sds011Measurements {
    fn each(&mut self, f: &mut dyn FnMut(&mut dyn Cell)) {
        f(&mut self.pm25);
        f(&mut self.pm10);
    }
}

/// Microphone noise measurements.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NoiseMeasurements {
    /// Average sound level, dB SPL, unweighted (not dBA).
    pub average: Measurement<Decibels>,
    /// Maximum sound level, dB SPL, unweighted (not dBA).
    pub maximum: Measurement<Decibels>,
}

impl Group for NoiseMeasurements {
    fn each(&mut self, f: &mut dyn FnMut(&mut dyn Cell)) {
        f(&mut self.average);
        f(&mut self.maximum);
    }
}

/// ZMOD4510 measurements.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Zmod4510Measurements {
    /// Ozone concentration.
    pub o3: Measurement<Ppb>,
    /// Nitrogen dioxide concentration.
    pub no2: Measurement<Ppb>,
    /// Fast AQI.
    pub fast_aqi: Measurement<Aqi>,
    /// EPA AQI.
    pub epa_aqi: Measurement<Aqi>,
}

impl Group for Zmod4510Measurements {
    fn each(&mut self, f: &mut dyn FnMut(&mut dyn Cell)) {
        f(&mut self.o3);
        f(&mut self.no2);
        f(&mut self.fast_aqi);
        f(&mut self.epa_aqi);
    }
}

/// AGS3871 measurements.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Ags3871Measurements {
    /// Carbon monoxide concentration.
    pub co: Measurement<Ppm>,
}

impl Group for Ags3871Measurements {
    fn each(&mut self, f: &mut dyn FnMut(&mut dyn Cell)) {
        f(&mut self.co);
    }
}

/// RadSens measurements.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct RadSensMeasurements {
    /// Radiation dose rate.
    pub radiation: Measurement<RadiationLevel>,
    /// Detected pulses.
    pub pulses: Measurement<PulseCount>,
}

impl Group for RadSensMeasurements {
    fn each(&mut self, f: &mut dyn FnMut(&mut dyn Cell)) {
        f(&mut self.radiation);
        f(&mut self.pulses);
    }
}

/// Measurements of all supported sensors, grouped by physical source.
///
/// A sensor that is not detected has no entry (`None`). The structure has a
/// fixed size and never allocates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Measurements {
    /// BMP280 / BME280.
    pub bmx280: Option<Bmx280Measurements>,
    /// SCD4x.
    pub scd4x: Option<Scd4xMeasurements>,
    /// SDS011.
    pub sds011: Option<Sds011Measurements>,
    /// Microphone.
    pub microphone: Option<NoiseMeasurements>,
    /// ZMOD4510.
    pub zmod4510: Option<Zmod4510Measurements>,
    /// AGS3871.
    pub ags3871: Option<Ags3871Measurements>,
    /// RadSens.
    pub radsens: Option<RadSensMeasurements>,
}

impl Measurements {
    /// Empty measurements: no sensor is present.
    pub const fn new() -> Self {
        Self {
            bmx280: None,
            scd4x: None,
            sds011: None,
            microphone: None,
            zmod4510: None,
            ags3871: None,
            radsens: None,
        }
    }

    /// Whether measurements of the sensor are present.
    pub const fn contains(&self, sensor: SensorKind) -> bool {
        match sensor {
            SensorKind::Bmx280 => self.bmx280.is_some(),
            SensorKind::Scd4x => self.scd4x.is_some(),
            SensorKind::Sds011 => self.sds011.is_some(),
            SensorKind::Microphone => self.microphone.is_some(),
            SensorKind::Zmod4510 => self.zmod4510.is_some(),
            SensorKind::Ags3871 => self.ags3871.is_some(),
            SensorKind::RadSens => self.radsens.is_some(),
        }
    }

    /// Create an empty group for the sensor unless one exists.
    pub(crate) fn ensure(&mut self, sensor: SensorKind) {
        match sensor {
            SensorKind::Bmx280 => {
                self.bmx280.get_or_insert_with(Default::default);
            }
            SensorKind::Scd4x => {
                self.scd4x.get_or_insert_with(Default::default);
            }
            SensorKind::Sds011 => {
                self.sds011.get_or_insert_with(Default::default);
            }
            SensorKind::Microphone => {
                self.microphone.get_or_insert_with(Default::default);
            }
            SensorKind::Zmod4510 => {
                self.zmod4510.get_or_insert_with(Default::default);
            }
            SensorKind::Ags3871 => {
                self.ags3871.get_or_insert_with(Default::default);
            }
            SensorKind::RadSens => {
                self.radsens.get_or_insert_with(Default::default);
            }
        }
    }

    /// Remove the group of the sensor.
    pub(crate) fn clear(&mut self, sensor: SensorKind) {
        match sensor {
            SensorKind::Bmx280 => self.bmx280 = None,
            SensorKind::Scd4x => self.scd4x = None,
            SensorKind::Sds011 => self.sds011 = None,
            SensorKind::Microphone => self.microphone = None,
            SensorKind::Zmod4510 => self.zmod4510 = None,
            SensorKind::Ags3871 => self.ags3871 = None,
            SensorKind::RadSens => self.radsens = None,
        }
    }

    /// Visit every measurement of the sensor, if present.
    pub(crate) fn each(&mut self, sensor: SensorKind, f: &mut dyn FnMut(&mut dyn Cell)) {
        match sensor {
            SensorKind::Bmx280 => self.bmx280.as_mut().map(|g| g.each(f)),
            SensorKind::Scd4x => self.scd4x.as_mut().map(|g| g.each(f)),
            SensorKind::Sds011 => self.sds011.as_mut().map(|g| g.each(f)),
            SensorKind::Microphone => self.microphone.as_mut().map(|g| g.each(f)),
            SensorKind::Zmod4510 => self.zmod4510.as_mut().map(|g| g.each(f)),
            SensorKind::Ags3871 => self.ags3871.as_mut().map(|g| g.each(f)),
            SensorKind::RadSens => self.radsens.as_mut().map(|g| g.each(f)),
        };
    }
}

/// Immutable point-in-time view of all sensor observations and states.
///
/// A snapshot is a plain value: it does not change when the store that
/// produced it is updated later. It never contains values calculated from
/// several sensors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeasurementsSnapshot {
    /// Measurements of every detected sensor, freshness resolved at
    /// `generated_at`.
    pub measurements: Measurements,
    /// Status of every supported sensor.
    pub sensor_statuses: SensorStatuses,
    /// Time the snapshot was generated.
    pub generated_at: MonotonicTimestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_measurement_is_unavailable() {
        let m = Measurement::<Celsius>::default();
        assert_eq!(m.value, None);
        assert_eq!(m.state, MeasurementState::Unavailable);
        assert_eq!(m.updated_at, None);
    }

    #[test]
    fn valid_measurement() {
        let m = Measurement::valid(Celsius(22.1), MonotonicTimestamp::from_millis(5));
        assert!(m.is_valid());
        assert_eq!(m.value, Some(Celsius(22.1)));
        assert_eq!(m.updated_at, Some(MonotonicTimestamp::from_millis(5)));
    }

    #[test]
    fn freshness_policy_boundary() {
        let policy = FreshnessPolicy::new(1_000);
        let t0 = MonotonicTimestamp::from_millis(0);
        assert!(!policy.is_stale(t0, MonotonicTimestamp::from_millis(1_000)));
        assert!(policy.is_stale(t0, MonotonicTimestamp::from_millis(1_001)));
        // Timestamps from the future are never stale.
        let later = MonotonicTimestamp::from_millis(5_000);
        assert!(!policy.is_stale(later, t0));
    }

    #[test]
    fn bmx280_group_without_humidity_has_two_cells() {
        let mut group = Bmx280Measurements::default();
        let mut count = 0;
        group.each(&mut |_| count += 1);
        assert_eq!(count, 2);
        group.humidity = Some(Measurement::unavailable());
        count = 0;
        group.each(&mut |_| count += 1);
        assert_eq!(count, 3);
    }
}
