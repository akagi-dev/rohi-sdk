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
//! Typed, bounded observations.

use rohi_time::MonotonicTimestamp;

/// Identity of one source of observations, including the physical instance:
/// two sensors of the same kind are two sources.
///
/// Ids are assigned by the firmware that composes the sources; [`SourceId::LOCAL`]
/// is the group of the device's own sensors when it is composed as one source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(pub u16);

impl SourceId {
    /// The local sensors of the device, collected as one source.
    pub const LOCAL: Self = Self(0);
    /// The location of the device.
    pub const LOCATION: Self = Self(1);
}

/// A stable, never reused measurement id: the bit index of the measurement in
/// a [`Selection`](crate::Selection), see [`catalog`](crate::catalog).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MeasurementId(pub u8);

/// A WGS84 position, an atomic observation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeoPoint {
    /// Latitude in decimal degrees, -90 to 90.
    pub latitude: f64,
    /// Longitude in decimal degrees, -180 to 180.
    pub longitude: f64,
    /// Height above mean sea level in metres, if known.
    pub altitude_m: Option<f64>,
}

impl GeoPoint {
    /// Whether every component is finite and in range.
    pub fn is_valid(&self) -> bool {
        self.latitude.is_finite()
            && self.longitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && (-180.0..=180.0).contains(&self.longitude)
            && self.altitude_m.is_none_or(f64::is_finite)
    }
}

/// The value of an observation. Its kind and unit are fixed by the
/// [`MeasurementDescriptor`](crate::MeasurementDescriptor) of its measurement,
/// not repeated per observation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ObservationValue {
    /// A number in the canonical unit of the measurement.
    Number(f32),
    /// A position.
    Location(GeoPoint),
}

/// One value of one measurement of one source, with its own acquisition time.
///
/// `(source, measurement)` is the key: equal quantities of two sensors stay
/// separate observations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Observation {
    /// Who observed it.
    pub source: SourceId,
    /// What was observed.
    pub measurement: MeasurementId,
    /// The value.
    pub value: ObservationValue,
    /// When the value was acquired, on the monotonic clock. Not a claim about
    /// the capture instant of the snapshot.
    pub observed_at: MonotonicTimestamp,
}
