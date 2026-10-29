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
//! Measurement store: records sensor observations and produces snapshots.

use crate::measurements::{
    Ags3871Measurements, Bmx280Measurements, FreshnessPolicy, Measurement, MeasurementState,
    Measurements, MeasurementsSnapshot, NoiseMeasurements, RadSensMeasurements, Scd4xMeasurements,
    Sds011Measurements, Zmod4510Measurements,
};
use crate::sensor::{
    MeasurementUpdate, SensorErrorCode, SensorEvent, SensorKind, SensorState, SensorStatuses,
};
use crate::time::MonotonicTimestamp;

/// Records the latest observation of every physical sensor.
///
/// The store receives [`SensorEvent`]s, keeps measurements per sensor, tracks
/// sensor state and errors, and produces immutable [`MeasurementsSnapshot`]s.
///
/// It does not average measurements, select a preferred source, calculate a
/// consensus or hide one sensor behind another. It contains no async or
/// runtime code and never allocates; sharing it between tasks is up to the
/// caller.
///
/// Events must be applied in chronological order.
#[derive(Debug, Clone, Copy)]
pub struct MeasurementStore {
    measurements: Measurements,
    statuses: SensorStatuses,
    policies: [FreshnessPolicy; SensorKind::COUNT],
}

impl Default for MeasurementStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MeasurementStore {
    /// Create an empty store: no sensor is detected.
    pub const fn new() -> Self {
        let mut policies = [FreshnessPolicy::new(0); SensorKind::COUNT];
        let mut i = 0;
        while i < SensorKind::COUNT {
            policies[i] = FreshnessPolicy::default_for(SensorKind::ALL[i]);
            i += 1;
        }
        Self {
            measurements: Measurements::new(),
            statuses: SensorStatuses::new(),
            policies,
        }
    }

    /// Override the freshness policy of a sensor.
    pub fn set_freshness_policy(&mut self, sensor: SensorKind, policy: FreshnessPolicy) {
        self.policies[sensor.index()] = policy;
    }

    /// Freshness policy of a sensor.
    pub const fn freshness_policy(&self, sensor: SensorKind) -> FreshnessPolicy {
        self.policies[sensor.index()]
    }

    /// Apply a sensor event.
    ///
    /// - A measurement update stores the new values as valid and makes the
    ///   sensor `Ready` in the same step, whatever its previous state (also
    ///   `Degraded` or `Error`): fresh values and `Ready` are never observable
    ///   apart.
    /// - A state change updates the sensor state. `NotDetected` removes the
    ///   sensor measurements. `Initializing` and `WarmingUp` mark the
    ///   measurements unavailable / warming up, and `Ready` restores
    ///   measurements to valid if a value exists. `Error` marks them as
    ///   errored.
    /// - An error marks the measurements as errored and counts the error.
    ///
    /// The last valid value and its time are never discarded by an error or
    /// a state change, only by `NotDetected`.
    pub fn apply(&mut self, event: SensorEvent) {
        match event {
            SensorEvent::Measurements(update) => self.apply_update(update),
            SensorEvent::StateChanged { sensor, state, .. } => self.change_state(sensor, state),
            SensorEvent::Error { sensor, error, .. } => self.record_error(sensor, error),
        }
    }

    /// Produce an immutable snapshot at `now`.
    ///
    /// Valid measurements older than the sensor freshness policy are reported
    /// as stale in the snapshot; the store itself is not modified.
    pub fn snapshot(&self, now: MonotonicTimestamp) -> MeasurementsSnapshot {
        let mut measurements = self.measurements;
        for sensor in SensorKind::ALL {
            let policy = self.policies[sensor.index()];
            measurements.each(sensor, &mut |cell| {
                if cell.state() != MeasurementState::Valid {
                    return;
                }
                if cell.updated_at().is_some_and(|at| policy.is_stale(at, now)) {
                    cell.set_state(MeasurementState::Stale);
                }
            });
        }
        MeasurementsSnapshot {
            measurements,
            sensor_statuses: self.statuses,
            generated_at: now,
        }
    }

    fn apply_update(&mut self, update: MeasurementUpdate) {
        let at = update.timestamp();
        let m = &mut self.measurements;
        match update {
            MeasurementUpdate::Bmx280(u) => {
                let g = m.bmx280.get_or_insert_with(Bmx280Measurements::default);
                g.temperature = Measurement::valid(u.temperature, at);
                g.pressure = Measurement::valid(u.pressure, at);
                if let Some(humidity) = u.humidity {
                    g.humidity = Some(Measurement::valid(humidity, at));
                }
            }
            MeasurementUpdate::Scd4x(u) => {
                let g = m.scd4x.get_or_insert_with(Scd4xMeasurements::default);
                g.co2 = Measurement::valid(u.co2, at);
                g.temperature = Measurement::valid(u.temperature, at);
                g.humidity = Measurement::valid(u.humidity, at);
            }
            MeasurementUpdate::Sds011(u) => {
                let g = m.sds011.get_or_insert_with(Sds011Measurements::default);
                g.pm25 = Measurement::valid(u.pm25, at);
                g.pm10 = Measurement::valid(u.pm10, at);
            }
            MeasurementUpdate::Microphone(u) => {
                let g = m.microphone.get_or_insert_with(NoiseMeasurements::default);
                g.average = Measurement::valid(u.average, at);
                g.maximum = Measurement::valid(u.maximum, at);
            }
            MeasurementUpdate::Zmod4510(u) => {
                let g = m.zmod4510.get_or_insert_with(Zmod4510Measurements::default);
                g.o3 = Measurement::valid(u.o3, at);
                g.no2 = Measurement::valid(u.no2, at);
                g.fast_aqi = Measurement::valid(u.fast_aqi, at);
                g.epa_aqi = Measurement::valid(u.epa_aqi, at);
            }
            MeasurementUpdate::Ags3871(u) => {
                let g = m.ags3871.get_or_insert_with(Ags3871Measurements::default);
                g.co = Measurement::valid(u.co, at);
            }
            MeasurementUpdate::RadSens(u) => {
                let g = m.radsens.get_or_insert_with(RadSensMeasurements::default);
                g.radiation = Measurement::valid(u.radiation, at);
                g.pulses = Measurement::valid(u.pulses, at);
            }
        }

        let status = self.statuses.get_mut(update.sensor());
        status.state = SensorState::Ready;
        status.last_success_at = Some(at);
        status.last_error = None;
    }

    fn change_state(&mut self, sensor: SensorKind, state: SensorState) {
        self.statuses.get_mut(sensor).state = state;
        match state {
            SensorState::NotDetected => self.measurements.clear(sensor),
            SensorState::Initializing => {
                self.set_all(sensor, |_| Some(MeasurementState::Unavailable));
            }
            SensorState::WarmingUp => {
                self.set_all(sensor, |_| Some(MeasurementState::WarmingUp));
            }
            SensorState::Ready => self.set_all(sensor, |cell| match cell.state() {
                MeasurementState::Valid | MeasurementState::Stale => None,
                _ if cell.has_value() => Some(MeasurementState::Valid),
                _ => Some(MeasurementState::Unavailable),
            }),
            SensorState::Degraded => self.measurements.ensure(sensor),
            SensorState::Error => self.set_all(sensor, |_| Some(MeasurementState::Error)),
        }
    }

    fn record_error(&mut self, sensor: SensorKind, error: SensorErrorCode) {
        let status = self.statuses.get_mut(sensor);
        status.state = SensorState::Error;
        status.error_count = status.error_count.saturating_add(1);
        status.last_error = Some(error);
        self.set_all(sensor, |_| Some(MeasurementState::Error));
    }

    /// Create the sensor group if needed and set the state of every
    /// measurement for which `f` returns a new state.
    fn set_all(
        &mut self,
        sensor: SensorKind,
        mut f: impl FnMut(&dyn crate::measurements::Cell) -> Option<MeasurementState>,
    ) {
        self.measurements.ensure(sensor);
        self.measurements.each(sensor, &mut |cell| {
            if let Some(state) = f(cell) {
                cell.set_state(state);
            }
        });
    }
}

#[cfg(test)]
mod tests;
