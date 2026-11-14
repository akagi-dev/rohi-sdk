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
//! Maps observations to the sensors of `device.v1.Urban`.
//!
//! Only what the protocol can say correctly is encoded; the rest is rejected
//! when a profile is edited (see [`SUPPORTED`]) and never relabelled:
//!
//! * `bmx280.temperature` and `bmx280.pressure` are `BME280` messages, so
//!   they are only encoded when the same source also reported a humidity,
//!   which only a BME280 has. A BMP280 is not published as a BME280.
//! * the SCD4x and the sensors without a protocol message are not supported.
//! * a position becomes `GPS`; a missing altitude is `0`.

use heapless::Vec;
use rohi_telemetry::catalog::{
    BMX280_HUMIDITY, BMX280_PRESSURE, BMX280_TEMPERATURE, LOCATION, MICROPHONE_AVERAGE,
    MICROPHONE_MAXIMUM, SDS011_PM2_5, SDS011_PM10,
};
use rohi_telemetry::{
    MeasurementId, Observation, ObservationValue, SectionView, Selection, SourceId,
};

use crate::wire::device_v1::{UrbanSensor, UrbanSensor_};
use crate::wire::limits::MAX_SENSORS_PER_MESSAGE;
use crate::wire::sensor_v1::{
    BME280, BME280_, GPS, Humidity, ICS43434, ICS43434_, NoiseLevel, PM10, PM25, Pressure, SDS011,
    SDS011_, Temperature,
};

/// The sensors of one section.
pub type Sensors = Vec<UrbanSensor, MAX_SENSORS_PER_MESSAGE>;

/// The measurements the protocol can encode correctly.
pub fn supported() -> Selection {
    [
        LOCATION,
        BMX280_TEMPERATURE,
        BMX280_HUMIDITY,
        BMX280_PRESSURE,
        SDS011_PM2_5,
        SDS011_PM10,
        MICROPHONE_AVERAGE,
        MICROPHONE_MAXIMUM,
    ]
    .into_iter()
    .fold(Selection::NONE, |selection, id| selection.with(id))
}

/// Why a section cannot be encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// The section contains a measurement the protocol cannot encode.
    Unsupported(MeasurementId),
    /// More sensors than a message holds.
    TooLarge,
}

/// Encodes the observations of `view`. Values that cannot be represented
/// (out of range) are omitted; relative humidity is limited to 0-100 %.
///
/// # Errors
///
/// [`EncodeError`]; nothing is returned on error.
pub fn encode_section<const N: usize>(view: &SectionView<'_, N>) -> Result<Sensors, EncodeError> {
    let mut sensors = Sensors::new();
    let mut push = |sensor: UrbanSensor_::Sensor| {
        sensors
            .push(UrbanSensor {
                sensor: Some(sensor),
            })
            .map_err(|_| EncodeError::TooLarge)
    };
    for observation in view.observations() {
        if let Some(sensor) = sensor_of(observation, view)? {
            push(sensor)?;
        }
    }
    Ok(sensors)
}

fn has_humidity<const N: usize>(view: &SectionView<'_, N>, source: SourceId) -> bool {
    view.snapshot().get(source, BMX280_HUMIDITY).is_some()
}

fn sensor_of<const N: usize>(
    observation: &Observation,
    view: &SectionView<'_, N>,
) -> Result<Option<UrbanSensor_::Sensor>, EncodeError> {
    let number = |value: &ObservationValue| match value {
        ObservationValue::Number(number) => Some(*number),
        ObservationValue::Location(_) => None,
    };
    let bme280 = |measurement| {
        UrbanSensor_::Sensor::Bme280(BME280 {
            measurement: Some(measurement),
        })
    };
    let sds011 = |measurement| {
        UrbanSensor_::Sensor::Sds011(SDS011 {
            measurement: Some(measurement),
        })
    };
    let ics43434 = |measurement| {
        UrbanSensor_::Sensor::Ics43434(ICS43434 {
            measurement: Some(measurement),
        })
    };
    let id = observation.measurement;
    if id == LOCATION {
        let ObservationValue::Location(point) = observation.value else {
            return Ok(None);
        };
        return Ok(Some(UrbanSensor_::Sensor::Gps(GPS {
            lat: point.latitude,
            lon: point.longitude,
            height_m: point.altitude_m.unwrap_or(0.0),
        })));
    }
    let Some(value) = number(&observation.value) else {
        return Ok(None);
    };
    Ok(match id {
        // Only a BME280 has humidity: the others are not relabelled.
        BMX280_TEMPERATURE if has_humidity(view, observation.source) => {
            centi_celsius(value).map(|centi_celsius| {
                bme280(BME280_::Measurement::Temperature(Temperature {
                    centi_celsius,
                }))
            })
        }
        BMX280_HUMIDITY => centi_percent(value).map(|centi_percent| {
            bme280(BME280_::Measurement::Humidity(Humidity { centi_percent }))
        }),
        BMX280_PRESSURE if has_humidity(view, observation.source) => deci(value)
            .map(|deci_pascal| bme280(BME280_::Measurement::Pressure(Pressure { deci_pascal }))),
        BMX280_TEMPERATURE | BMX280_PRESSURE => None,
        SDS011_PM2_5 => {
            deci(value).map(|deci_ug_m3| sds011(SDS011_::Measurement::Pm25(PM25 { deci_ug_m3 })))
        }
        SDS011_PM10 => {
            deci(value).map(|deci_ug_m3| sds011(SDS011_::Measurement::Pm10(PM10 { deci_ug_m3 })))
        }
        MICROPHONE_AVERAGE => {
            whole(value).map(|db| ics43434(ICS43434_::Measurement::NoiseAvg(NoiseLevel { db })))
        }
        MICROPHONE_MAXIMUM => {
            whole(value).map(|db| ics43434(ICS43434_::Measurement::NoiseMax(NoiseLevel { db })))
        }
        other => return Err(EncodeError::Unsupported(other)),
    })
}

fn scaled(value: f32, scale: f32) -> Option<f32> {
    value.is_finite().then(|| libm::roundf(value * scale))
}

fn centi_celsius(celsius: f32) -> Option<i32> {
    let v = scaled(celsius, 100.0)?;
    (v.abs() <= 2_000_000.0).then_some(v as i32)
}

fn centi_percent(percent: f32) -> Option<u32> {
    let v = scaled(percent.clamp(0.0, 100.0), 100.0)?;
    Some(v as u32)
}

fn deci(value: f32) -> Option<u32> {
    let v = scaled(value, 10.0)?;
    (0.0..=u32::MAX as f32).contains(&v).then_some(v as u32)
}

fn whole(value: f32) -> Option<u32> {
    let v = scaled(value, 1.0)?;
    (0.0..=u32::MAX as f32).contains(&v).then_some(v as u32)
}

#[cfg(test)]
mod tests;
