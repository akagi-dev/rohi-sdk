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
//! The sensors in the generic service pipeline.
//!
//! Every measurement of a present sensor is a status field named
//! `<sensor>.<measurement>` in the canonical unit of its type, absent until
//! the sensor delivered a value.

use embassy_sync::blocking_mutex::raw::RawMutex;
use heapless::Vec;
use rohi_service::{
    FieldDescriptor, Issue, IssueSeverity, MAX_ISSUES, ManagedService, ServiceHealth, ServiceState,
    ServiceSummary, StatusWriter, ValueKind, WriteError,
};

use crate::measurements::{Measurement, MeasurementsSnapshot};
use crate::runtime::MeasurementHandle;
use crate::sensor::{SensorKind, SensorState};
use crate::units::{
    Aqi, Celsius, Decibels, MicrogramsPerCubicMeter, Pascal, Ppb, Ppm, PulseCount, RadiationLevel,
    RelativeHumidity,
};

/// A typed unit value in the canonical unit of its measurement type.
trait Canonical: Copy {
    fn canonical(self) -> f64;
}

macro_rules! canonical {
    ($($unit:ty),+) => {$(
        impl Canonical for $unit {
            fn canonical(self) -> f64 {
                f64::from(self.0)
            }
        }
    )+};
}

canonical!(
    Celsius,
    RelativeHumidity,
    Pascal,
    Ppm,
    Ppb,
    Decibels,
    MicrogramsPerCubicMeter,
    Aqi,
    RadiationLevel,
    PulseCount
);

macro_rules! measurement_fields {
    ($($id:expr, $name:expr, $unit:expr, $description:expr;)+) => {
        &[$(
            FieldDescriptor::new($id, $name, ValueKind::F64)
                .describe($description)
                .unit($unit),
        )+]
    };
}

/// Status fields of the `sensors` service: `<sensor>.<measurement>`.
const FIELDS: &[FieldDescriptor] = measurement_fields! {
    1, "bmx280.temperature", "C", "Temperature";
    2, "bmx280.humidity", "%", "Relative humidity";
    3, "bmx280.pressure", "Pa", "Pressure";
    4, "scd4x.co2", "ppm", "CO2 concentration";
    5, "scd4x.temperature", "C", "Temperature";
    6, "scd4x.humidity", "%", "Relative humidity";
    7, "sds011.pm2_5", "ug/m3", "PM2.5 mass concentration";
    8, "sds011.pm10", "ug/m3", "PM10 mass concentration";
    9, "microphone.average", "dB", "Average sound level";
    10, "microphone.maximum", "dB", "Maximum sound level";
    11, "zmod4510.o3", "ppb", "Ozone concentration";
    12, "zmod4510.no2", "ppb", "Nitrogen dioxide concentration";
    13, "zmod4510.aqi_fast", "", "Fast air quality index";
    14, "zmod4510.aqi_epa", "", "EPA air quality index";
    15, "ags3871.co", "ppm", "Carbon monoxide concentration";
    16, "radsens.radiation", "uSv/h", "Ionizing radiation dose rate";
    17, "radsens.pulses", "", "Detected radiation pulses";
};

fn put<T: Canonical>(
    out: &mut impl StatusWriter,
    id: u32,
    measurement: &Measurement<T>,
) -> Result<(), WriteError> {
    match measurement.value {
        Some(value) => out.f64(id, value.canonical()),
        None => Ok(()),
    }
}

fn write(out: &mut impl StatusWriter, snapshot: &MeasurementsSnapshot) -> Result<(), WriteError> {
    let m = &snapshot.measurements;
    if let Some(g) = &m.bmx280 {
        put(out, 1, &g.temperature)?;
        if let Some(humidity) = &g.humidity {
            put(out, 2, humidity)?;
        }
        put(out, 3, &g.pressure)?;
    }
    if let Some(g) = &m.scd4x {
        put(out, 4, &g.co2)?;
        put(out, 5, &g.temperature)?;
        put(out, 6, &g.humidity)?;
    }
    if let Some(g) = &m.sds011 {
        put(out, 7, &g.pm25)?;
        put(out, 8, &g.pm10)?;
    }
    if let Some(g) = &m.microphone {
        put(out, 9, &g.average)?;
        put(out, 10, &g.maximum)?;
    }
    if let Some(g) = &m.zmod4510 {
        put(out, 11, &g.o3)?;
        put(out, 12, &g.no2)?;
        put(out, 13, &g.fast_aqi)?;
        put(out, 14, &g.epa_aqi)?;
    }
    if let Some(g) = &m.ags3871 {
        put(out, 15, &g.co)?;
    }
    if let Some(g) = &m.radsens {
        put(out, 16, &g.radiation)?;
        put(out, 17, &g.pulses)?;
    }
    Ok(())
}

fn sensor_name(kind: SensorKind) -> &'static str {
    match kind {
        SensorKind::Bmx280 => "bmx280",
        SensorKind::Scd4x => "scd4x",
        SensorKind::Sds011 => "sds011",
        SensorKind::Microphone => "microphone",
        SensorKind::Zmod4510 => "zmod4510",
        SensorKind::Ags3871 => "ags3871",
        SensorKind::RadSens => "radsens",
    }
}

fn issue_code(kind: SensorKind, failed: bool) -> &'static str {
    match (kind, failed) {
        (SensorKind::Bmx280, true) => "bmx280_error",
        (SensorKind::Bmx280, false) => "bmx280_degraded",
        (SensorKind::Scd4x, true) => "scd4x_error",
        (SensorKind::Scd4x, false) => "scd4x_degraded",
        (SensorKind::Sds011, true) => "sds011_error",
        (SensorKind::Sds011, false) => "sds011_degraded",
        (SensorKind::Microphone, true) => "microphone_error",
        (SensorKind::Microphone, false) => "microphone_degraded",
        (SensorKind::Zmod4510, true) => "zmod4510_error",
        (SensorKind::Zmod4510, false) => "zmod4510_degraded",
        (SensorKind::Ags3871, true) => "ags3871_error",
        (SensorKind::Ags3871, false) => "ags3871_degraded",
        (SensorKind::RadSens, true) => "radsens_error",
        (SensorKind::RadSens, false) => "radsens_degraded",
    }
}

fn summary(snapshot: &MeasurementsSnapshot) -> ServiceSummary {
    let present = || {
        snapshot
            .sensor_statuses
            .iter()
            .filter(|status| status.state != SensorState::NotDetected)
    };
    let total = present().count();
    if total == 0 {
        return ServiceSummary::new(ServiceState::Disabled, ServiceHealth::Unknown, false);
    }
    let failed = present()
        .filter(|status| status.state == SensorState::Error)
        .count();
    let degraded = present()
        .filter(|status| status.state == SensorState::Degraded)
        .count();
    let ready = present().any(|status| status.state == SensorState::Ready);
    let starting = present().all(|status| {
        matches!(
            status.state,
            SensorState::Initializing | SensorState::WarmingUp
        )
    });
    if starting {
        return ServiceSummary::new(ServiceState::Starting, ServiceHealth::Unknown, false);
    }
    let health = if failed == total {
        ServiceHealth::Error
    } else if failed + degraded > 0 {
        ServiceHealth::Degraded
    } else {
        ServiceHealth::Ok
    };
    ServiceSummary::new(ServiceState::Running, health, ready)
}

/// The measurement runtime as a `sensors` service of the status plane.
pub struct SensorsService<'a, M: RawMutex>(pub MeasurementHandle<'a, M>);

impl<M: RawMutex> ManagedService for SensorsService<'_, M> {
    const NAME: &'static str = "sensors";
    const VERSION: u16 = 1;
    const STATUS_SCHEMA: &'static [FieldDescriptor] = FIELDS;
    // A device without a working sensor still serves its other roles.
    const ESSENTIAL: bool = false;

    async fn summary(&self) -> ServiceSummary {
        match self.0.snapshot().await {
            Ok(snapshot) => summary(&snapshot),
            // The measurement owner did not answer.
            Err(_) => ServiceSummary::new(ServiceState::Running, ServiceHealth::Unknown, false),
        }
    }

    async fn issues(&self, out: &mut Vec<Issue, MAX_ISSUES>) {
        let Ok(snapshot) = self.0.snapshot().await else {
            return;
        };
        for status in snapshot.sensor_statuses.iter() {
            let failed = status.state == SensorState::Error;
            if !failed && status.state != SensorState::Degraded {
                continue;
            }
            let issue = Issue {
                severity: if failed {
                    IssueSeverity::Error
                } else {
                    IssueSeverity::Warning
                },
                code: issue_code(status.kind, failed),
                message: sensor_name(status.kind),
                age_ms: None,
            };
            if out.push(issue).is_err() {
                break;
            }
        }
    }

    async fn status(&self, out: &mut impl StatusWriter) -> Result<(), WriteError> {
        match self.0.snapshot().await {
            Ok(snapshot) => write(out, &snapshot),
            Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Bmx280Update, MeasurementStore, MeasurementUpdate, MonotonicTimestamp, SensorEvent,
    };

    #[derive(Default)]
    struct Collect(std::vec::Vec<(u32, f64)>);

    impl StatusWriter for Collect {
        fn put(&mut self, id: u32, value: rohi_service::Value) -> Result<(), WriteError> {
            self.0.push((id, value.as_f64().unwrap()));
            Ok(())
        }
    }

    fn snapshot() -> MeasurementsSnapshot {
        let mut store = MeasurementStore::new();
        store.apply(SensorEvent::Measurements(MeasurementUpdate::Bmx280(
            Bmx280Update::bme280(
                Celsius(21.5),
                RelativeHumidity(45.0),
                Pascal(100_800.0),
                MonotonicTimestamp::from_millis(100),
            ),
        )));
        store.snapshot(MonotonicTimestamp::from_millis(600))
    }

    #[test]
    fn measurements_are_status_fields_of_the_present_sensors() {
        let mut out = Collect::default();
        write(&mut out, &snapshot()).unwrap();
        assert_eq!(out.0, [(1, 21.5), (2, 45.0), (3, 100_800.0)]);
    }

    #[test]
    fn a_ready_sensor_makes_the_service_ready() {
        let summary = summary(&snapshot());
        assert_eq!(summary.state, ServiceState::Running);
        assert_eq!(summary.health, ServiceHealth::Ok);
        assert!(summary.ready);
    }

    #[test]
    fn a_device_without_sensors_is_disabled() {
        let empty = MeasurementStore::new().snapshot(MonotonicTimestamp::from_millis(1));
        assert_eq!(summary(&empty).state, ServiceState::Disabled);
    }

    #[test]
    fn field_ids_and_names_are_unique_and_fit_the_protocol() {
        let mut ids: std::vec::Vec<_> = FIELDS.iter().map(|field| field.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), FIELDS.len());
        assert!(FIELDS.len() <= rohi_service::MAX_FIELDS);
        assert!(FIELDS.iter().all(|field| field.name.len() <= 24));
    }
}
