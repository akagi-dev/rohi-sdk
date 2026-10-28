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
//! Mapping of a measurement snapshot to a `core.v1.Message`.
//!
//! Policy:
//!
//! - only [`MeasurementState::Valid`] measurements are emitted: a cached value
//!   of a `WarmingUp`, `Stale` or `Error` measurement is never sent;
//! - one physical sensor is never relabeled as another: a measurement without
//!   an unambiguous representation in the pinned schema is omitted and counted
//!   in [`EncodeStats::omitted_unsupported`];
//! - values that cannot be represented (NaN, infinite, out of range) are
//!   omitted and counted, never clamped to a misleading number (relative
//!   humidity is the exception: sensor noise slightly outside `0..=100 %` is
//!   clamped, the schema defines that range).
//!
//! | Rohi observation | Protocol field | Unit conversion |
//! |---|---|---|
//! | BME280 temperature (`Bmx280`, with humidity channel) | `BME280.temperature` | °C → 0.01 °C |
//! | BME280 humidity | `BME280.humidity` | % → 0.01 % |
//! | BME280 pressure | `BME280.pressure` | Pa → 0.1 Pa |
//! | SDS011 PM2.5 / PM10 | `SDS011.pm25` / `SDS011.pm10` | µg/m³ → 0.1 µg/m³ |
//! | Microphone average / maximum | `ICS43434.noise_avg` / `noise_max` | dB → whole dB |
//! | BMP280 (no humidity channel), SCD4x, ZMOD4510, AGS3871, RadSens | none | omitted |
//!
//! The location of the device (see `rohi-location`) is the `GPS` entry in
//! front of the sensors when one is known: WGS84 degrees, height 0 when it is
//! not known (the schema says so). It is not a measurement: a frame with a
//! location but no valid measurement is still not sent.

use core::fmt;

use heapless::Vec;
use rohi_connectivity_proto::core_v1::{Message, Message_, Meta};
use rohi_connectivity_proto::device_v1::{Urban, UrbanSensor, UrbanSensor_};
use rohi_connectivity_proto::limits::MAX_MESSAGE_LEN;
use rohi_connectivity_proto::sensor_v1::{
    BME280, BME280_, GPS, Humidity, ICS43434, ICS43434_, NoiseLevel, PM10, PM25, Pressure, SDS011,
    SDS011_, Temperature,
};
use rohi_connectivity_proto::{EncodeError as WireError, encode};
use rohi_hal::{
    Bmx280Measurements, Measurement, MeasurementState, MeasurementsSnapshot, NoiseMeasurements,
    Sds011Measurements,
};
use rohi_location::Location;

/// Counters of one encoding.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct EncodeStats {
    /// Measurements written into the message.
    pub emitted: u32,
    /// Measurements of supported sensors that were not `Valid`.
    pub omitted_not_valid: u32,
    /// Measurements without a representation in the pinned schema.
    pub omitted_unsupported: u32,
    /// Valid measurements whose value cannot be represented.
    pub omitted_out_of_range: u32,
    /// Whether the frame carries the location of the device.
    pub location_included: bool,
}

/// The message could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// The snapshot has no measurement to send.
    NothingToSend,
    /// The message does not fit its bound.
    TooLarge,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NothingToSend => "no valid measurement to send",
            Self::TooLarge => "telemetry message too large",
        })
    }
}

impl core::error::Error for EncodeError {}

/// A serialized `core.v1.Message`.
///
/// These exact bytes are what gets signed and wrapped into the envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedMessage {
    bytes: Vec<u8, MAX_MESSAGE_LEN>,
    stats: EncodeStats,
}

impl EncodedMessage {
    /// The serialized message.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// What was emitted and what was omitted.
    pub fn stats(&self) -> EncodeStats {
        self.stats
    }
}

/// Encoder of the Altruist Urban telemetry payload (`device.v1.Urban`).
pub struct UrbanEncoder;

impl UrbanEncoder {
    /// Builds the message of `snapshot`, without serializing it and without a
    /// location.
    pub fn message(
        snapshot: &MeasurementsSnapshot,
        node_id: u64,
        timestamp_ms: u64,
    ) -> Result<(Message, EncodeStats), EncodeError> {
        Self::message_at(snapshot, node_id, timestamp_ms, None)
    }

    /// Builds the message of `snapshot` located at `location`, without
    /// serializing it.
    pub fn message_at(
        snapshot: &MeasurementsSnapshot,
        node_id: u64,
        timestamp_ms: u64,
        location: Option<&Location>,
    ) -> Result<(Message, EncodeStats), EncodeError> {
        let mut builder = Builder::default();
        if let Some(location) = location {
            builder.location(location)?;
        }
        let m = &snapshot.measurements;
        if let Some(bmx) = &m.bmx280 {
            builder.bmx280(bmx)?;
        }
        if let Some(sds) = &m.sds011 {
            builder.sds011(sds)?;
        }
        if let Some(mic) = &m.microphone {
            builder.microphone(mic)?;
        }
        // Every measurement of these sensors is unsupported.
        let unsupported = [
            (m.scd4x.is_some(), 3),
            (m.zmod4510.is_some(), 4),
            (m.ags3871.is_some(), 1),
            (m.radsens.is_some(), 2),
        ];
        for (present, count) in unsupported {
            if present {
                builder.stats.omitted_unsupported += count;
            }
        }
        if builder.stats.emitted == 0 {
            return Err(EncodeError::NothingToSend);
        }

        let mut message = Message::default();
        message.set_metadata(Meta {
            node_id,
            timestamp: timestamp_ms,
        });
        message.payload = Some(Message_::Payload::Urban(Urban {
            public: builder.sensors,
        }));
        Ok((message, builder.stats))
    }

    /// Maps and serializes `snapshot`: the one and only serialization of the
    /// message.
    ///
    /// `timestamp_ms` is the Unix UTC time of the measurement set, in
    /// milliseconds.
    pub fn encode_snapshot(
        snapshot: &MeasurementsSnapshot,
        node_id: u64,
        timestamp_ms: u64,
    ) -> Result<EncodedMessage, EncodeError> {
        Self::encode_snapshot_at(snapshot, node_id, timestamp_ms, None)
    }

    /// Like [`encode_snapshot`](Self::encode_snapshot), with the location of
    /// the device when one is known.
    pub fn encode_snapshot_at(
        snapshot: &MeasurementsSnapshot,
        node_id: u64,
        timestamp_ms: u64,
        location: Option<&Location>,
    ) -> Result<EncodedMessage, EncodeError> {
        let (message, stats) = Self::message_at(snapshot, node_id, timestamp_ms, location)?;
        let mut bytes = Vec::new();
        bytes
            .resize_default(MAX_MESSAGE_LEN)
            .map_err(|_| EncodeError::TooLarge)?;
        let len = encode(&message, &mut bytes)
            .map_err(|WireError::BufferTooSmall| EncodeError::TooLarge)?;
        bytes.truncate(len);
        Ok(EncodedMessage { bytes, stats })
    }
}

#[derive(Default)]
struct Builder {
    sensors: Vec<UrbanSensor, { rohi_connectivity_proto::limits::MAX_SENSORS_PER_MESSAGE }>,
    stats: EncodeStats,
}

impl Builder {
    /// The location is context, not a measurement: it is not counted as
    /// emitted. A position that cannot be represented is left out.
    fn location(&mut self, location: &Location) -> Result<(), EncodeError> {
        let valid = location.latitude.is_finite()
            && location.longitude.is_finite()
            && location.height_m.is_none_or(f64::is_finite);
        if !valid {
            self.stats.omitted_out_of_range += 1;
            return Ok(());
        }
        self.sensors
            .push(UrbanSensor {
                sensor: Some(UrbanSensor_::Sensor::Gps(GPS {
                    lat: location.latitude,
                    lon: location.longitude,
                    height_m: location.height_m.unwrap_or(0.0),
                })),
            })
            .map_err(|_| EncodeError::TooLarge)?;
        self.stats.location_included = true;
        Ok(())
    }

    fn push(&mut self, sensor: UrbanSensor_::Sensor) -> Result<(), EncodeError> {
        self.sensors
            .push(UrbanSensor {
                sensor: Some(sensor),
            })
            .map_err(|_| EncodeError::TooLarge)?;
        self.stats.emitted += 1;
        Ok(())
    }

    /// Emits `cell` if it is `Valid` and its value is representable; counts
    /// the omission otherwise.
    fn emit<T: Copy, V>(
        &mut self,
        cell: &Measurement<T>,
        convert: impl FnOnce(T) -> Option<V>,
        sensor: impl FnOnce(V) -> UrbanSensor_::Sensor,
    ) -> Result<(), EncodeError> {
        let value = match (cell.state, cell.value) {
            (MeasurementState::Valid, Some(value)) => value,
            _ => {
                self.stats.omitted_not_valid += 1;
                return Ok(());
            }
        };
        match convert(value) {
            Some(value) => self.push(sensor(value)),
            None => {
                self.stats.omitted_out_of_range += 1;
                Ok(())
            }
        }
    }

    fn bmx280(&mut self, group: &Bmx280Measurements) -> Result<(), EncodeError> {
        // Only a BME280 has a humidity channel. The protocol has no BMP280:
        // its temperature and pressure are not relabeled as BME280's.
        let Some(humidity) = &group.humidity else {
            self.stats.omitted_unsupported += 2;
            return Ok(());
        };
        let bme280 = |measurement| {
            UrbanSensor_::Sensor::Bme280(BME280 {
                measurement: Some(measurement),
            })
        };
        self.emit(
            &group.temperature,
            |v| centi_celsius(v.0),
            |centi_celsius| {
                bme280(BME280_::Measurement::Temperature(Temperature {
                    centi_celsius,
                }))
            },
        )?;
        self.emit(
            humidity,
            |v| centi_percent(v.0),
            |centi_percent| bme280(BME280_::Measurement::Humidity(Humidity { centi_percent })),
        )?;
        self.emit(
            &group.pressure,
            |v| deci_pascal(v.0),
            |deci_pascal| bme280(BME280_::Measurement::Pressure(Pressure { deci_pascal })),
        )
    }

    fn sds011(&mut self, group: &Sds011Measurements) -> Result<(), EncodeError> {
        let sds011 = |measurement| {
            UrbanSensor_::Sensor::Sds011(SDS011 {
                measurement: Some(measurement),
            })
        };
        self.emit(
            &group.pm25,
            |v| deci_micrograms(v.0),
            |deci_ug_m3| sds011(SDS011_::Measurement::Pm25(PM25 { deci_ug_m3 })),
        )?;
        self.emit(
            &group.pm10,
            |v| deci_micrograms(v.0),
            |deci_ug_m3| sds011(SDS011_::Measurement::Pm10(PM10 { deci_ug_m3 })),
        )
    }

    fn microphone(&mut self, group: &NoiseMeasurements) -> Result<(), EncodeError> {
        let ics43434 = |measurement| {
            UrbanSensor_::Sensor::Ics43434(ICS43434 {
                measurement: Some(measurement),
            })
        };
        self.emit(
            &group.average,
            |v| whole_decibels(v.0),
            |db| ics43434(ICS43434_::Measurement::NoiseAvg(NoiseLevel { db })),
        )?;
        self.emit(
            &group.maximum,
            |v| whole_decibels(v.0),
            |db| ics43434(ICS43434_::Measurement::NoiseMax(NoiseLevel { db })),
        )
    }
}

fn scaled(value: f32, scale: f32) -> Option<f32> {
    value.is_finite().then(|| libm::roundf(value * scale))
}

fn centi_celsius(celsius: f32) -> Option<i32> {
    let v = scaled(celsius, 100.0)?;
    (v.abs() <= 2_000_000.0).then_some(v as i32)
}

fn deci_pascal(pascal: f32) -> Option<u32> {
    let v = scaled(pascal, 10.0)?;
    (0.0..=u32::MAX as f32).contains(&v).then_some(v as u32)
}

fn centi_percent(percent: f32) -> Option<u32> {
    if !percent.is_finite() {
        return None;
    }
    let v = scaled(percent.clamp(0.0, 100.0), 100.0)?;
    Some(v as u32)
}

fn deci_micrograms(ug_m3: f32) -> Option<u32> {
    let v = scaled(ug_m3, 10.0)?;
    (0.0..=u32::MAX as f32).contains(&v).then_some(v as u32)
}

fn whole_decibels(db: f32) -> Option<u32> {
    let v = scaled(db, 1.0)?;
    (0.0..=u32::MAX as f32).contains(&v).then_some(v as u32)
}
