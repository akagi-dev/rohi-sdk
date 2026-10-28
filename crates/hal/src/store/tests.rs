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
///////////////////////////////////////////////////////////////////////////////
use core::mem::size_of;

use super::*;
use crate::measurements::MeasurementState as MS;
use crate::sensor::{
    Ags3871Update, Bmx280Update, NoiseUpdate, RadSensUpdate, Scd4xUpdate, Sds011Update,
    SensorStatus, Zmod4510Update,
};
use crate::units::*;

fn ts(ms: u64) -> MonotonicTimestamp {
    MonotonicTimestamp::from_millis(ms)
}

fn bme280(temperature: f32, at: u64) -> SensorEvent {
    SensorEvent::Measurements(MeasurementUpdate::Bmx280(Bmx280Update::bme280(
        Celsius(temperature),
        RelativeHumidity(45.0),
        Pascal(100_200.0),
        ts(at),
    )))
}

fn scd4x(temperature: f32, at: u64) -> SensorEvent {
    SensorEvent::Measurements(MeasurementUpdate::Scd4x(Scd4xUpdate {
        co2: Ppm(530.0),
        temperature: Celsius(temperature),
        humidity: RelativeHumidity(47.0),
        timestamp: ts(at),
    }))
}

fn state(sensor: SensorKind, state: SensorState, at: u64) -> SensorEvent {
    SensorEvent::StateChanged {
        sensor,
        state,
        timestamp: ts(at),
    }
}

fn error(sensor: SensorKind, error: SensorErrorCode, at: u64) -> SensorEvent {
    SensorEvent::Error {
        sensor,
        error,
        timestamp: ts(at),
    }
}

#[test]
fn empty_store_has_no_sensors() {
    let snap = MeasurementStore::new().snapshot(ts(0));
    assert_eq!(snap.measurements, Measurements::new());
    assert_eq!(snap.generated_at, ts(0));
    for status in snap.sensor_statuses.iter() {
        assert_eq!(status.state, SensorState::NotDetected);
    }
}

#[test]
fn basic_measurement_update() {
    let mut store = MeasurementStore::new();
    store.apply(bme280(22.1, 100));
    let snap = store.snapshot(ts(100));

    let bmx = snap.measurements.bmx280.unwrap();
    assert_eq!(bmx.temperature, Measurement::valid(Celsius(22.1), ts(100)));
    assert_eq!(
        bmx.humidity,
        Some(Measurement::valid(RelativeHumidity(45.0), ts(100)))
    );
    assert_eq!(bmx.pressure.value, Some(Pascal(100_200.0)));
    let status = snap.sensor_statuses.get(SensorKind::Bmx280);
    assert_eq!(status.state, SensorState::Ready);
    assert_eq!(status.last_success_at, Some(ts(100)));
}

#[test]
fn independent_sources_are_not_merged() {
    let mut store = MeasurementStore::new();
    store.apply(bme280(22.1, 100));
    store.apply(scd4x(22.7, 110));
    let snap = store.snapshot(ts(120));

    // Provenance: every value is reachable through its own sensor.
    let bmx = snap.measurements.bmx280.unwrap();
    let scd = snap.measurements.scd4x.unwrap();
    assert_eq!(bmx.temperature.value, Some(Celsius(22.1)));
    assert_eq!(scd.temperature.value, Some(Celsius(22.7)));
    assert_eq!(scd.co2.value, Some(Ppm(530.0)));
    assert_eq!(scd.humidity.value, Some(RelativeHumidity(47.0)));

    // No synthetic value: the snapshot holds exactly the six observed
    // temperature / humidity / pressure / CO2 cells and no combined value.
    let mut cells = 0;
    let mut copy = snap.measurements;
    for sensor in SensorKind::ALL {
        copy.each(sensor, &mut |_| cells += 1);
    }
    assert_eq!(cells, 6);
    assert_ne!(bmx.temperature.value, Some(Celsius(22.4)));
    assert_ne!(scd.temperature.value, Some(Celsius(22.4)));
}

#[test]
fn absent_sensor_is_none() {
    let mut store = MeasurementStore::new();
    store.apply(bme280(22.1, 0));
    let snap = store.snapshot(ts(0));
    assert!(snap.measurements.sds011.is_none());
    assert!(!snap.measurements.contains(SensorKind::Sds011));
    assert_eq!(
        snap.sensor_statuses.get(SensorKind::Sds011).state,
        SensorState::NotDetected
    );
}

#[test]
fn not_detected_removes_measurements() {
    let mut store = MeasurementStore::new();
    store.apply(bme280(22.1, 0));
    store.apply(state(SensorKind::Bmx280, SensorState::NotDetected, 10));
    let snap = store.snapshot(ts(10));
    assert!(snap.measurements.bmx280.is_none());
}

#[test]
fn bmp280_has_no_humidity() {
    let mut store = MeasurementStore::new();
    store.apply(SensorEvent::Measurements(MeasurementUpdate::Bmx280(
        Bmx280Update::bmp280(Celsius(20.0), Pascal(99_000.0), ts(1)),
    )));
    let bmx = store.snapshot(ts(1)).measurements.bmx280.unwrap();
    assert!(bmx.humidity.is_none());
    assert!(bmx.temperature.is_valid());
    assert!(bmx.pressure.is_valid());
}

#[test]
fn bme280_has_humidity() {
    let mut store = MeasurementStore::new();
    store.apply(bme280(20.0, 1));
    let bmx = store.snapshot(ts(1)).measurements.bmx280.unwrap();
    assert!(bmx.humidity.is_some());
}

#[test]
fn warm_up_is_not_valid() {
    let mut store = MeasurementStore::new();
    store.apply(state(SensorKind::Ags3871, SensorState::WarmingUp, 0));
    let snap = store.snapshot(ts(5));

    assert_eq!(
        snap.sensor_statuses.get(SensorKind::Ags3871).state,
        SensorState::WarmingUp
    );
    let co = snap.measurements.ags3871.unwrap().co;
    assert_eq!(co.state, MS::WarmingUp);
    assert_eq!(co.value, None);
    assert!(!co.is_valid());
}

#[test]
fn warm_up_finished_without_data_is_unavailable() {
    let mut store = MeasurementStore::new();
    store.apply(state(SensorKind::Ags3871, SensorState::WarmingUp, 0));
    store.apply(state(SensorKind::Ags3871, SensorState::Ready, 10));
    let co = store.snapshot(ts(10)).measurements.ags3871.unwrap().co;
    assert_eq!(co.state, MS::Unavailable);
}

#[test]
fn initializing_creates_unavailable_measurements() {
    let mut store = MeasurementStore::new();
    store.apply(state(SensorKind::Sds011, SensorState::Initializing, 0));
    let sds = store.snapshot(ts(0)).measurements.sds011.unwrap();
    assert_eq!(sds.pm25.state, MS::Unavailable);
    assert_eq!(sds.pm10.state, MS::Unavailable);
}

#[test]
fn stale_transition() {
    let mut store = MeasurementStore::new();
    store.set_freshness_policy(SensorKind::Bmx280, FreshnessPolicy::new(1_000));
    store.apply(bme280(22.1, 0));

    let fresh = store.snapshot(ts(1_000)).measurements.bmx280.unwrap();
    assert_eq!(fresh.temperature.state, MS::Valid);

    let stale = store.snapshot(ts(1_001)).measurements.bmx280.unwrap();
    assert_eq!(stale.temperature.state, MS::Stale);
    assert_eq!(stale.pressure.state, MS::Stale);
    assert_eq!(stale.humidity.unwrap().state, MS::Stale);
    // The value and its time survive staleness.
    assert_eq!(stale.temperature.value, Some(Celsius(22.1)));
    assert_eq!(stale.temperature.updated_at, Some(ts(0)));
}

#[test]
fn snapshot_does_not_modify_store() {
    let mut store = MeasurementStore::new();
    store.set_freshness_policy(SensorKind::Bmx280, FreshnessPolicy::new(10));
    store.apply(bme280(22.1, 0));
    let _ = store.snapshot(ts(1_000));
    let early = store.snapshot(ts(5)).measurements.bmx280.unwrap();
    assert_eq!(early.temperature.state, MS::Valid);
}

#[test]
fn freshness_policy_is_per_sensor() {
    let mut store = MeasurementStore::new();
    store.set_freshness_policy(SensorKind::Bmx280, FreshnessPolicy::new(100));
    store.set_freshness_policy(SensorKind::Scd4x, FreshnessPolicy::new(10_000));
    assert_eq!(
        store.freshness_policy(SensorKind::Bmx280),
        FreshnessPolicy::new(100)
    );
    store.apply(bme280(22.1, 0));
    store.apply(scd4x(22.7, 0));
    let snap = store.snapshot(ts(500));
    assert_eq!(
        snap.measurements.bmx280.unwrap().temperature.state,
        MS::Stale
    );
    assert_eq!(
        snap.measurements.scd4x.unwrap().temperature.state,
        MS::Valid
    );
}

#[test]
fn error_preserves_last_valid_value() {
    let mut store = MeasurementStore::new();
    store.apply(bme280(22.1, 100));
    store.apply(error(SensorKind::Bmx280, SensorErrorCode::Bus, 110));
    let snap = store.snapshot(ts(110));

    let t = snap.measurements.bmx280.unwrap().temperature;
    assert_eq!(t.value, Some(Celsius(22.1)));
    assert_eq!(t.state, MS::Error);
    assert_eq!(t.updated_at, Some(ts(100)));

    let status = snap.sensor_statuses.get(SensorKind::Bmx280);
    assert_eq!(status.state, SensorState::Error);
    assert_eq!(status.error_count, 1);
    assert_eq!(status.last_error, Some(SensorErrorCode::Bus));
    assert_eq!(status.last_success_at, Some(ts(100)));
}

#[test]
fn error_without_prior_value_has_no_value() {
    let mut store = MeasurementStore::new();
    store.apply(error(SensorKind::Scd4x, SensorErrorCode::Timeout, 5));
    let scd = store.snapshot(ts(5)).measurements.scd4x.unwrap();
    assert_eq!(scd.co2.value, None);
    assert_eq!(scd.co2.state, MS::Error);
    assert_eq!(scd.co2.updated_at, None);
}

#[test]
fn errors_are_counted() {
    let mut store = MeasurementStore::new();
    for i in 0..3 {
        store.apply(error(SensorKind::Sds011, SensorErrorCode::Crc, i));
    }
    let status = *store
        .snapshot(ts(3))
        .sensor_statuses
        .get(SensorKind::Sds011);
    assert_eq!(status.error_count, 3);
}

#[test]
fn error_does_not_become_stale() {
    let mut store = MeasurementStore::new();
    store.set_freshness_policy(SensorKind::Bmx280, FreshnessPolicy::new(10));
    store.apply(bme280(22.1, 0));
    store.apply(error(SensorKind::Bmx280, SensorErrorCode::Bus, 1));
    let t = store
        .snapshot(ts(1_000))
        .measurements
        .bmx280
        .unwrap()
        .temperature;
    assert_eq!(t.state, MS::Error);
}

#[test]
fn recovery_after_error() {
    let mut store = MeasurementStore::new();
    store.apply(bme280(22.1, 100));
    store.apply(error(SensorKind::Bmx280, SensorErrorCode::Bus, 110));
    store.apply(bme280(22.3, 120));
    let snap = store.snapshot(ts(120));

    let t = snap.measurements.bmx280.unwrap().temperature;
    assert_eq!(t, Measurement::valid(Celsius(22.3), ts(120)));
    let status = snap.sensor_statuses.get(SensorKind::Bmx280);
    assert_eq!(status.state, SensorState::Ready);
    assert_eq!(status.last_error, None);
    assert_eq!(status.error_count, 1);
}

#[test]
fn ready_state_restores_valid_value_after_error() {
    let mut store = MeasurementStore::new();
    store.apply(bme280(22.1, 100));
    store.apply(error(SensorKind::Bmx280, SensorErrorCode::Bus, 110));
    store.apply(state(SensorKind::Bmx280, SensorState::Ready, 120));
    let t = store
        .snapshot(ts(120))
        .measurements
        .bmx280
        .unwrap()
        .temperature;
    assert_eq!(t.state, MS::Valid);
    assert_eq!(t.value, Some(Celsius(22.1)));
}

#[test]
fn update_restores_ready_from_degraded_and_error_atomically() {
    for from in [SensorState::Degraded, SensorState::Error] {
        let mut store = MeasurementStore::new();
        store.apply(bme280(22.1, 0));
        store.apply(state(SensorKind::Bmx280, from, 1));
        store.apply(bme280(22.2, 2));
        let snap = store.snapshot(ts(2));
        assert_eq!(
            snap.sensor_statuses.get(SensorKind::Bmx280).state,
            SensorState::Ready
        );
        let t = snap.measurements.bmx280.unwrap().temperature;
        assert_eq!(t.state, MS::Valid);
        assert_eq!(t.value, Some(Celsius(22.2)));
    }
}

#[test]
fn update_makes_warming_up_sensor_ready_with_fresh_value() {
    let mut store = MeasurementStore::new();
    store.apply(state(SensorKind::Scd4x, SensorState::WarmingUp, 0));
    store.apply(scd4x(21.0, 5));
    let snap = store.snapshot(ts(5));
    assert_eq!(
        snap.sensor_statuses.get(SensorKind::Scd4x).state,
        SensorState::Ready
    );
    let t = snap.measurements.scd4x.unwrap().temperature;
    assert_eq!(t.state, MS::Valid);
    assert_eq!(t.value, Some(Celsius(21.0)));
}

#[test]
fn snapshot_is_independent_of_later_updates() {
    let mut store = MeasurementStore::new();
    store.apply(bme280(22.1, 100));
    let a = store.snapshot(ts(100));

    store.apply(bme280(22.3, 200));
    store.apply(error(SensorKind::Scd4x, SensorErrorCode::Timeout, 210));
    let b = store.snapshot(ts(210));

    assert_eq!(
        a.measurements.bmx280.unwrap().temperature.value,
        Some(Celsius(22.1))
    );
    assert!(a.measurements.scd4x.is_none());
    assert_eq!(a.sensor_statuses.get(SensorKind::Scd4x).error_count, 0);
    assert_eq!(
        b.measurements.bmx280.unwrap().temperature.value,
        Some(Celsius(22.3))
    );
    assert!(b.measurements.scd4x.is_some());
}

#[test]
fn all_urban_sensors_can_be_represented() {
    let mut store = MeasurementStore::new();
    let t = ts(1);
    store.apply(bme280(22.1, 1));
    store.apply(scd4x(22.7, 1));
    store.apply(SensorEvent::Measurements(MeasurementUpdate::Sds011(
        Sds011Update {
            pm25: MicrogramsPerCubicMeter(5.0),
            pm10: MicrogramsPerCubicMeter(9.0),
            timestamp: t,
        },
    )));
    store.apply(SensorEvent::Measurements(MeasurementUpdate::Microphone(
        NoiseUpdate {
            average: Decibels(45.0),
            maximum: Decibels(70.0),
            timestamp: t,
        },
    )));
    store.apply(SensorEvent::Measurements(MeasurementUpdate::Zmod4510(
        Zmod4510Update {
            o3: Ppb(30.0),
            no2: Ppb(12.0),
            fast_aqi: Aqi(40),
            epa_aqi: Aqi(35),
            timestamp: t,
        },
    )));
    store.apply(SensorEvent::Measurements(MeasurementUpdate::Ags3871(
        Ags3871Update {
            co: Ppm(0.4),
            timestamp: t,
        },
    )));
    store.apply(SensorEvent::Measurements(MeasurementUpdate::RadSens(
        RadSensUpdate {
            radiation: RadiationLevel(0.12),
            pulses: PulseCount(7),
            timestamp: t,
        },
    )));

    let snap = store.snapshot(t);
    for sensor in SensorKind::ALL {
        assert!(snap.measurements.contains(sensor));
        assert_eq!(snap.sensor_statuses.get(sensor).state, SensorState::Ready);
    }
    assert_eq!(snap.measurements.sds011.unwrap().pm10.value.unwrap().0, 9.0);
    assert_eq!(
        snap.measurements
            .microphone
            .unwrap()
            .maximum
            .value
            .unwrap()
            .0,
        70.0
    );
    assert_eq!(
        snap.measurements.zmod4510.unwrap().epa_aqi.value,
        Some(Aqi(35))
    );
    assert_eq!(
        snap.measurements.radsens.unwrap().pulses.value,
        Some(PulseCount(7))
    );
}

#[test]
fn type_sizes_stay_bounded() {
    assert!(size_of::<Measurement<Celsius>>() <= 32);
    assert!(size_of::<SensorStatus>() <= 32);
    assert!(size_of::<SensorEvent>() <= 48);
    assert!(size_of::<Measurements>() <= 576);
    assert!(size_of::<MeasurementsSnapshot>() <= 768);
    assert!(size_of::<MeasurementStore>() <= 800);
}
