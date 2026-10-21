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
//! Sensor identity, runtime state, errors and events.

use crate::time::MonotonicTimestamp;
use crate::units::{
    Aqi, Celsius, Decibels, MicrogramsPerCubicMeter, Pascal, Ppb, Ppm, PulseCount, RadiationLevel,
    RelativeHumidity,
};

/// Supported Altruist Urban sensors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensorKind {
    /// BMP280 / BME280 pressure, temperature and (BME280 only) humidity sensor.
    Bmx280,
    /// SCD4x CO2, temperature and humidity sensor.
    Scd4x,
    /// SDS011 particulate matter sensor.
    Sds011,
    /// Noise microphone.
    Microphone,
    /// ZMOD4510 O3 / NO2 sensor.
    Zmod4510,
    /// AGS3871 CO sensor.
    Ags3871,
    /// RadSens radiation sensor.
    RadSens,
}

impl SensorKind {
    /// Number of supported sensors.
    pub const COUNT: usize = 7;

    /// All supported sensors.
    pub const ALL: [SensorKind; Self::COUNT] = [
        SensorKind::Bmx280,
        SensorKind::Scd4x,
        SensorKind::Sds011,
        SensorKind::Microphone,
        SensorKind::Zmod4510,
        SensorKind::Ags3871,
        SensorKind::RadSens,
    ];

    /// Stable index of the sensor, suitable for fixed-size storage.
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Runtime state of a sensor, separate from its measurement values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SensorState {
    /// The sensor is not present.
    #[default]
    NotDetected,
    /// The sensor is being initialized.
    Initializing,
    /// The sensor is warming up and does not provide valid values yet.
    WarmingUp,
    /// The sensor works normally.
    Ready,
    /// The sensor works with reduced quality.
    Degraded,
    /// The sensor failed.
    Error,
}

/// Domain-level sensor error. Driver specific errors are mapped into these
/// codes outside of this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SensorErrorCode {
    /// The sensor did not answer in time.
    Timeout,
    /// Bus communication failed.
    Bus,
    /// A checksum did not match.
    Crc,
    /// The sensor returned invalid data.
    InvalidData,
    /// The sensor has no data ready yet.
    NotReady,
    /// Calibration data is missing or invalid.
    Calibration,
    /// Unclassified error.
    Unknown,
}

/// Runtime status of a sensor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorStatus {
    /// The sensor.
    pub kind: SensorKind,
    /// Current state.
    pub state: SensorState,
    /// Time of the last successful measurement update.
    pub last_success_at: Option<MonotonicTimestamp>,
    /// Total number of reported errors.
    pub error_count: u32,
    /// Last reported error, cleared by the next successful update.
    pub last_error: Option<SensorErrorCode>,
}

impl SensorStatus {
    /// Initial status of a sensor: not detected.
    pub const fn new(kind: SensorKind) -> Self {
        Self {
            kind,
            state: SensorState::NotDetected,
            last_success_at: None,
            error_count: 0,
            last_error: None,
        }
    }
}

/// Statuses of all supported sensors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorStatuses([SensorStatus; SensorKind::COUNT]);

impl Default for SensorStatuses {
    fn default() -> Self {
        Self::new()
    }
}

impl SensorStatuses {
    /// All sensors are not detected.
    pub const fn new() -> Self {
        let mut statuses = [SensorStatus::new(SensorKind::Bmx280); SensorKind::COUNT];
        let mut i = 0;
        while i < SensorKind::COUNT {
            statuses[i] = SensorStatus::new(SensorKind::ALL[i]);
            i += 1;
        }
        Self(statuses)
    }

    /// Status of a sensor.
    pub const fn get(&self, sensor: SensorKind) -> &SensorStatus {
        &self.0[sensor.index()]
    }

    pub(crate) fn get_mut(&mut self, sensor: SensorKind) -> &mut SensorStatus {
        &mut self.0[sensor.index()]
    }

    /// Iterate over all sensor statuses.
    pub fn iter(&self) -> impl Iterator<Item = &SensorStatus> {
        self.0.iter()
    }
}

/// BMP280 / BME280 observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bmx280Update {
    /// Temperature.
    pub temperature: Celsius,
    /// Relative humidity, `None` for BMP280.
    pub humidity: Option<RelativeHumidity>,
    /// Pressure.
    pub pressure: Pascal,
    /// Time of the observation.
    pub timestamp: MonotonicTimestamp,
}

impl Bmx280Update {
    /// BMP280 observation, which has no humidity.
    pub const fn bmp280(
        temperature: Celsius,
        pressure: Pascal,
        timestamp: MonotonicTimestamp,
    ) -> Self {
        Self {
            temperature,
            humidity: None,
            pressure,
            timestamp,
        }
    }

    /// BME280 observation.
    pub const fn bme280(
        temperature: Celsius,
        humidity: RelativeHumidity,
        pressure: Pascal,
        timestamp: MonotonicTimestamp,
    ) -> Self {
        Self {
            temperature,
            humidity: Some(humidity),
            pressure,
            timestamp,
        }
    }
}

/// SCD4x observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scd4xUpdate {
    /// CO2 concentration.
    pub co2: Ppm,
    /// Temperature.
    pub temperature: Celsius,
    /// Relative humidity.
    pub humidity: RelativeHumidity,
    /// Time of the observation.
    pub timestamp: MonotonicTimestamp,
}

/// SDS011 observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sds011Update {
    /// PM2.5 concentration.
    pub pm25: MicrogramsPerCubicMeter,
    /// PM10 concentration.
    pub pm10: MicrogramsPerCubicMeter,
    /// Time of the observation.
    pub timestamp: MonotonicTimestamp,
}

/// Microphone noise observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseUpdate {
    /// Average (equivalent continuous) sound level of the window, in dB SPL
    /// without frequency weighting; not dBA.
    pub average: Decibels,
    /// Highest short-interval sound level of the window, in dB SPL without
    /// frequency weighting; not dBA.
    pub maximum: Decibels,
    /// Time of the observation.
    pub timestamp: MonotonicTimestamp,
}

/// ZMOD4510 observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zmod4510Update {
    /// Ozone concentration.
    pub o3: Ppb,
    /// Nitrogen dioxide concentration.
    pub no2: Ppb,
    /// Fast AQI.
    pub fast_aqi: Aqi,
    /// EPA AQI.
    pub epa_aqi: Aqi,
    /// Time of the observation.
    pub timestamp: MonotonicTimestamp,
}

/// AGS3871 observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ags3871Update {
    /// Carbon monoxide concentration.
    pub co: Ppm,
    /// Time of the observation.
    pub timestamp: MonotonicTimestamp,
}

/// RadSens observation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RadSensUpdate {
    /// Radiation dose rate.
    pub radiation: RadiationLevel,
    /// Detected pulses.
    pub pulses: PulseCount,
    /// Time of the observation.
    pub timestamp: MonotonicTimestamp,
}

/// A successful observation of one physical sensor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MeasurementUpdate {
    /// BMP280 / BME280.
    Bmx280(Bmx280Update),
    /// SCD4x.
    Scd4x(Scd4xUpdate),
    /// SDS011.
    Sds011(Sds011Update),
    /// Microphone.
    Microphone(NoiseUpdate),
    /// ZMOD4510.
    Zmod4510(Zmod4510Update),
    /// AGS3871.
    Ags3871(Ags3871Update),
    /// RadSens.
    RadSens(RadSensUpdate),
}

impl MeasurementUpdate {
    /// Sensor that produced the update.
    pub const fn sensor(&self) -> SensorKind {
        match self {
            MeasurementUpdate::Bmx280(_) => SensorKind::Bmx280,
            MeasurementUpdate::Scd4x(_) => SensorKind::Scd4x,
            MeasurementUpdate::Sds011(_) => SensorKind::Sds011,
            MeasurementUpdate::Microphone(_) => SensorKind::Microphone,
            MeasurementUpdate::Zmod4510(_) => SensorKind::Zmod4510,
            MeasurementUpdate::Ags3871(_) => SensorKind::Ags3871,
            MeasurementUpdate::RadSens(_) => SensorKind::RadSens,
        }
    }

    /// Time of the observation.
    pub const fn timestamp(&self) -> MonotonicTimestamp {
        match self {
            MeasurementUpdate::Bmx280(u) => u.timestamp,
            MeasurementUpdate::Scd4x(u) => u.timestamp,
            MeasurementUpdate::Sds011(u) => u.timestamp,
            MeasurementUpdate::Microphone(u) => u.timestamp,
            MeasurementUpdate::Zmod4510(u) => u.timestamp,
            MeasurementUpdate::Ags3871(u) => u.timestamp,
            MeasurementUpdate::RadSens(u) => u.timestamp,
        }
    }
}

/// An event published by a sensor task and consumed by the
/// [`MeasurementStore`](crate::MeasurementStore).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SensorEvent {
    /// A new successful observation.
    Measurements(MeasurementUpdate),
    /// The runtime state of a sensor changed.
    StateChanged {
        /// The sensor.
        sensor: SensorKind,
        /// New state.
        state: SensorState,
        /// Time of the change.
        timestamp: MonotonicTimestamp,
    },
    /// A sensor operation failed.
    Error {
        /// The sensor.
        sensor: SensorKind,
        /// Error code.
        error: SensorErrorCode,
        /// Time of the error.
        timestamp: MonotonicTimestamp,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_indices_are_dense_and_match_all() {
        for (i, kind) in SensorKind::ALL.iter().enumerate() {
            assert_eq!(kind.index(), i);
        }
    }

    #[test]
    fn statuses_start_not_detected() {
        let statuses = SensorStatuses::new();
        for kind in SensorKind::ALL {
            let status = statuses.get(kind);
            assert_eq!(status.kind, kind);
            assert_eq!(status.state, SensorState::NotDetected);
            assert_eq!(status.error_count, 0);
        }
        assert_eq!(statuses.iter().count(), SensorKind::COUNT);
    }

    #[test]
    fn bmp280_has_no_humidity_and_bme280_has() {
        let t = MonotonicTimestamp::from_millis(0);
        assert!(
            Bmx280Update::bmp280(Celsius(20.0), Pascal(100_000.0), t)
                .humidity
                .is_none()
        );
        assert!(
            Bmx280Update::bme280(Celsius(20.0), RelativeHumidity(40.0), Pascal(100_000.0), t)
                .humidity
                .is_some()
        );
    }

    #[test]
    fn update_sensor_and_timestamp() {
        let t = MonotonicTimestamp::from_millis(42);
        let update = MeasurementUpdate::Ags3871(Ags3871Update {
            co: Ppm(1.0),
            timestamp: t,
        });
        assert_eq!(update.sensor(), SensorKind::Ags3871);
        assert_eq!(update.timestamp(), t);
    }
}
