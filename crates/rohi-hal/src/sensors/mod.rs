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

/// BMP280 / BME280 temperature, pressure and humidity sensor.
pub mod bmx280;
/// Noise level measurement from an I2S microphone.
pub mod noise;
/// SCD40 / SCD41 CO2, temperature and humidity sensor.
pub mod scd4x;
/// SDS011 particulate matter sensor.
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
