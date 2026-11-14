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
//! The measurement catalog: canonical names, kinds, units and stable bits.
//!
//! One registry serves validation, profile selections and the paged catalog
//! Action. The bit of a measurement is its [`MeasurementId`] and is never
//! reused or renumbered; the mapping is pinned by tests.

use crate::observation::MeasurementId;
use crate::selection::Selection;

/// The kind of value a measurement carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ValueKind {
    /// A number.
    Number,
    /// A WGS84 position.
    Location,
}

/// Canonical unit of a numeric measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// Degrees and metres of a position.
    Position,
    /// Degrees Celsius.
    Celsius,
    /// Percent relative humidity.
    Percent,
    /// Pascal.
    Pascal,
    /// Parts per million.
    Ppm,
    /// Parts per billion.
    Ppb,
    /// Micrograms per cubic metre.
    MicrogramsPerCubicMeter,
    /// Decibels.
    Decibels,
    /// Air quality index.
    Index,
    /// Microsieverts per hour.
    MicrosievertsPerHour,
    /// A count of pulses.
    Pulses,
}

/// A catalog entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MeasurementDescriptor {
    /// The stable id, equal to the selection bit.
    pub id: MeasurementId,
    /// The canonical name, `<sensor>.<measurement>` for sensors.
    pub name: &'static str,
    /// The value kind.
    pub kind: ValueKind,
    /// The canonical unit.
    pub unit: Unit,
}

const fn entry(id: u8, name: &'static str, kind: ValueKind, unit: Unit) -> MeasurementDescriptor {
    MeasurementDescriptor {
        id: MeasurementId(id),
        name,
        kind,
        unit,
    }
}

/// `location`: one atomic position, never part of a dynamic `all`.
pub const LOCATION: MeasurementId = MeasurementId(0);
/// `bmx280.temperature`.
pub const BMX280_TEMPERATURE: MeasurementId = MeasurementId(1);
/// `bmx280.humidity`.
pub const BMX280_HUMIDITY: MeasurementId = MeasurementId(2);
/// `bmx280.pressure`.
pub const BMX280_PRESSURE: MeasurementId = MeasurementId(3);
/// `scd4x.co2`.
pub const SCD4X_CO2: MeasurementId = MeasurementId(4);
/// `scd4x.temperature`.
pub const SCD4X_TEMPERATURE: MeasurementId = MeasurementId(5);
/// `scd4x.humidity`.
pub const SCD4X_HUMIDITY: MeasurementId = MeasurementId(6);
/// `sds011.pm2_5`.
pub const SDS011_PM2_5: MeasurementId = MeasurementId(7);
/// `sds011.pm10`.
pub const SDS011_PM10: MeasurementId = MeasurementId(8);
/// `microphone.average`.
pub const MICROPHONE_AVERAGE: MeasurementId = MeasurementId(9);
/// `microphone.maximum`.
pub const MICROPHONE_MAXIMUM: MeasurementId = MeasurementId(10);
/// `zmod4510.o3`.
pub const ZMOD4510_O3: MeasurementId = MeasurementId(11);
/// `zmod4510.no2`.
pub const ZMOD4510_NO2: MeasurementId = MeasurementId(12);
/// `zmod4510.aqi_fast`.
pub const ZMOD4510_AQI_FAST: MeasurementId = MeasurementId(13);
/// `zmod4510.aqi_epa`.
pub const ZMOD4510_AQI_EPA: MeasurementId = MeasurementId(14);
/// `ags3871.co`.
pub const AGS3871_CO: MeasurementId = MeasurementId(15);
/// `radsens.radiation`.
pub const RADSENS_RADIATION: MeasurementId = MeasurementId(16);
/// `radsens.pulses`.
pub const RADSENS_PULSES: MeasurementId = MeasurementId(17);

use ValueKind::{Location, Number};

/// Every measurement ROHI knows, ordered by id. Whether a packet format can
/// encode a measurement is a different question, see [`PacketFormat`](crate::PacketFormat).
pub const CATALOG: &[MeasurementDescriptor] = &[
    entry(0, "location", Location, Unit::Position),
    entry(1, "bmx280.temperature", Number, Unit::Celsius),
    entry(2, "bmx280.humidity", Number, Unit::Percent),
    entry(3, "bmx280.pressure", Number, Unit::Pascal),
    entry(4, "scd4x.co2", Number, Unit::Ppm),
    entry(5, "scd4x.temperature", Number, Unit::Celsius),
    entry(6, "scd4x.humidity", Number, Unit::Percent),
    entry(7, "sds011.pm2_5", Number, Unit::MicrogramsPerCubicMeter),
    entry(8, "sds011.pm10", Number, Unit::MicrogramsPerCubicMeter),
    entry(9, "microphone.average", Number, Unit::Decibels),
    entry(10, "microphone.maximum", Number, Unit::Decibels),
    entry(11, "zmod4510.o3", Number, Unit::Ppb),
    entry(12, "zmod4510.no2", Number, Unit::Ppb),
    entry(13, "zmod4510.aqi_fast", Number, Unit::Index),
    entry(14, "zmod4510.aqi_epa", Number, Unit::Index),
    entry(15, "ags3871.co", Number, Unit::Ppm),
    entry(16, "radsens.radiation", Number, Unit::MicrosievertsPerHour),
    entry(17, "radsens.pulses", Number, Unit::Pulses),
];

/// The descriptor of `id`, if it is cataloged.
pub fn descriptor(id: MeasurementId) -> Option<&'static MeasurementDescriptor> {
    CATALOG.iter().find(|entry| entry.id == id)
}

/// The id of the measurement with canonical `name`.
pub fn id_of(name: &str) -> Option<MeasurementId> {
    CATALOG
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.id)
}

/// The selection of every cataloged measurement.
pub fn all_cataloged() -> Selection {
    CATALOG
        .iter()
        .fold(Selection::NONE, |selection, entry| selection.with(entry.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_dense_and_equal_their_position() {
        for (index, entry) in CATALOG.iter().enumerate() {
            assert_eq!(usize::from(entry.id.0), index);
        }
    }

    #[test]
    fn the_stable_mapping_is_pinned() {
        let pinned = [
            (0, "location"),
            (1, "bmx280.temperature"),
            (2, "bmx280.humidity"),
            (3, "bmx280.pressure"),
            (4, "scd4x.co2"),
            (5, "scd4x.temperature"),
            (6, "scd4x.humidity"),
            (7, "sds011.pm2_5"),
            (8, "sds011.pm10"),
            (9, "microphone.average"),
            (10, "microphone.maximum"),
            (11, "zmod4510.o3"),
            (12, "zmod4510.no2"),
            (13, "zmod4510.aqi_fast"),
            (14, "zmod4510.aqi_epa"),
            (15, "ags3871.co"),
            (16, "radsens.radiation"),
            (17, "radsens.pulses"),
        ];
        assert_eq!(CATALOG.len(), pinned.len());
        for (entry, (id, name)) in CATALOG.iter().zip(pinned) {
            assert_eq!((entry.id.0, entry.name), (id, name));
        }
        assert_eq!(LOCATION.0, 0);
        assert_eq!(RADSENS_PULSES.0, 17);
    }

    #[test]
    fn names_are_unique_and_only_location_is_a_position() {
        for (index, entry) in CATALOG.iter().enumerate() {
            assert!(
                CATALOG[..index]
                    .iter()
                    .all(|other| other.name != entry.name)
            );
            assert_eq!(entry.kind == ValueKind::Location, entry.id == LOCATION);
        }
    }

    #[test]
    fn lookup_by_id_and_name() {
        assert_eq!(id_of("sds011.pm2_5"), Some(SDS011_PM2_5));
        assert_eq!(id_of("sds011"), None);
        assert_eq!(descriptor(MeasurementId(99)), None);
        assert_eq!(descriptor(SCD4X_CO2).unwrap().unit, Unit::Ppm);
    }
}
