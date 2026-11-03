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
//! Sensor drivers publishing typed `rohi-hal` observations.
//!
//! The Altruist Urban firmware (ESP32-C3 and ESP32-C6) runs all four drivers.
//! Which sensors are present on which board and what is verified is in the
//! support matrix, <https://rohi.akagi.dev/support/overview.html>; as of its
//! last review no driver is recorded as verified on hardware. Each driver is
//! hardware independent (`embedded-hal-async`, `embedded-io-async`),
//! unit tested on a host, implements
//! [`SensorDriver`](crate::runtime::SensorDriver) and publishes typed
//! observations; lifecycle, timing and events are described in
//! [`runtime`](crate::runtime). Adding a driver:
//! <https://rohi.akagi.dev/guides/add-sensor.html>.
//!
//! | Sensor | Module | Bus | Cadence | Notes |
//! |--------|--------|-----|---------|-------|
//! | BMP280 / BME280 | [`bmx280`] | I²C `0x76` / `0x77` | 10 s | Temperature, pressure, humidity (BME280 only) |
//! | SCD40 / SCD41 | [`scd4x`] | I²C `0x62` | 5 s | CO₂, temperature, humidity |
//! | SDS011 | [`sds011`] | UART, 9600 baud | duty cycle, at most 150 s by default | PM2.5, PM10, fan off between cycles, see [`sds011::Config::max_cadence_ms`] |
//! | I²S microphone | [`noise`] | I²S | 5 s window | Noise level, dB SPL |
//!
//! Which pins and buses the sensors are wired to is board support, see
//! [`rohi-hal-esp`](../../rohi_hal_esp/index.html); the UART baud rate and the
//! I²S setup are configured there, not in the drivers. Nothing is averaged or
//! fused across sensors: `bmx280.temperature` and `scd4x.temperature` are
//! separate observations.
//!
//! # Status and the control plane
//!
//! The firmware keeps the strongly typed `rohi-hal` model. Only at the control
//! boundary does it become the generic `sensors` service of
//! [`rohi_service`] ([`SensorsService`](crate::SensorsService) over a
//! [`MeasurementHandle`](crate::runtime::MeasurementHandle)). Every
//! measurement of a present sensor is a status field named
//! `<sensor>.<measurement>` in the **canonical unit** of its type, and is
//! absent until the sensor has delivered a value. Observations of one quantity
//! from different sensors are never merged. Every request takes a fresh
//! snapshot, so values and states are correct at request time.
//!
//! Illustrative output of the desktop `rohi` tool (values are examples):
//!
//! ```text
//! $ rohi status sensors
//! Service:  sensors
//! State:    running
//! Health:   ok
//! Ready:    yes
//! Since:    2h 14m
//!
//! bmx280.temperature  21.5 C
//! bmx280.humidity     45 %
//! bmx280.pressure     100800 Pa
//! scd4x.co2           640 ppm
//! sds011.pm2_5        6.5 ug/m3
//! ```
//!
//! | Field | Unit | Field | Unit |
//! |-------|------|-------|------|
//! | `bmx280.temperature` | °C | `zmod4510.o3`, `zmod4510.no2` | ppb |
//! | `bmx280.humidity` | % RH | `zmod4510.aqi_fast`, `zmod4510.aqi_epa` | index |
//! | `bmx280.pressure` | Pa | `ags3871.co` | ppm |
//! | `scd4x.co2` | ppm | `radsens.radiation` | µSv/h |
//! | `scd4x.temperature`, `scd4x.humidity` | °C, % RH | `radsens.pulses` | pulses |
//! | `sds011.pm2_5`, `sds011.pm10` | µg/m³ | `microphone.average`, `microphone.maximum` | dB |
//!
//! The service state, health and issues are described at
//! [`SensorsService`](crate::SensorsService). The status schema has at most 20 fields
//! (`MAX_FIELDS` of `rohi-service`).

pub mod bmx280;
pub mod noise;
pub mod scd4x;
pub mod sds011;

#[cfg(test)]
mod freshness {
    //! The default freshness policy of `rohi-hal` must outlast the
    //! measurement cadence of every sensor, or normal operation would show
    //! `Stale` values.

    use crate::{Decibels, MicrogramsPerCubicMeter};
    use crate::{
        FreshnessPolicy, MeasurementState, MeasurementStore, MeasurementUpdate, MonotonicTimestamp,
        NoiseUpdate, Sds011Update, SensorEvent, SensorKind,
    };

    use super::{bmx280, noise, scd4x, sds011};

    /// The threshold must exceed the worst healthy cadence by at least this
    /// factor (numerator / denominator) to tolerate scheduling jitter.
    const TOLERANCE: (u64, u64) = (3, 2);

    fn assert_outlasts(sensor: SensorKind, worst_cadence_ms: u32) {
        let stale_after = FreshnessPolicy::default_for(sensor).stale_after_ms;
        let needed = u64::from(worst_cadence_ms) * TOLERANCE.0 / TOLERANCE.1;
        assert!(
            stale_after > needed,
            "{sensor:?}: stale after {stale_after} ms, cadence {worst_cadence_ms} ms needs more than {needed} ms"
        );
    }

    #[test]
    fn default_thresholds_outlast_the_cadence_of_every_wp2_sensor() {
        assert_outlasts(SensorKind::Bmx280, bmx280::MEASUREMENT_INTERVAL_MS);
        assert_outlasts(
            SensorKind::Scd4x,
            scd4x::MEASUREMENT_INTERVAL_MS + scd4x::MAX_READY_POLLS * scd4x::POLL_INTERVAL_MS,
        );
        assert_outlasts(SensorKind::Sds011, sds011::Config::DEFAULT.max_cadence_ms());
        assert_outlasts(SensorKind::Microphone, noise::WINDOW_MS);
    }

    #[test]
    fn sds011_stays_valid_through_a_full_duty_cycle_and_goes_stale_after_failures() {
        let mut store = MeasurementStore::new();
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Sds011(
            Sds011Update {
                pm25: MicrogramsPerCubicMeter(12.0),
                pm10: MicrogramsPerCubicMeter(20.0),
                timestamp: MonotonicTimestamp::from_millis(0),
            },
        )));
        let cycle = u64::from(sds011::Config::DEFAULT.max_cadence_ms());
        let pm25 = |store: &MeasurementStore, at: u64| {
            store
                .snapshot(MonotonicTimestamp::from_millis(at))
                .measurements
                .sds011
                .unwrap()
                .pm25
                .state
        };
        assert_eq!(pm25(&store, cycle), MeasurementState::Valid);
        let limit = FreshnessPolicy::default_for(SensorKind::Sds011).stale_after_ms;
        assert_eq!(pm25(&store, limit + 1), MeasurementState::Stale);
    }

    #[test]
    fn noise_stays_valid_between_windows() {
        let mut store = MeasurementStore::new();
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Microphone(
            NoiseUpdate {
                average: Decibels(40.0),
                maximum: Decibels(55.0),
                timestamp: MonotonicTimestamp::from_millis(0),
            },
        )));
        let at = MonotonicTimestamp::from_millis(u64::from(noise::WINDOW_MS) * 2);
        let state = store
            .snapshot(at)
            .measurements
            .microphone
            .unwrap()
            .average
            .state;
        assert_eq!(state, MeasurementState::Valid);
    }
}
