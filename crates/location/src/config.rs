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
//! The persistent `location` section and its generic config interface.

use rohi_config::{Codec, FieldError, Value, ValueKind};
use rohi_service::Config;

use crate::{Location, LocationSourceKind};

/// Where the user says the device is (effective configuration).
///
/// `LocationConfig::default()` is no position: latitude and longitude are
/// `NaN` and the section is not valid on its own, so a first `height_m=...`
/// never stores "0, 0". What is stored are the explicit user overrides of it,
/// [`LocationOverrides`].
#[derive(Clone, Copy, Debug, PartialEq, Config)]
#[config(
    name = "location",
    overrides = LocationOverrides,
    version = 1,
    validate = validate_section
)]
pub struct LocationConfig {
    /// Latitude in decimal degrees, -90 to 90 (WGS84).
    #[config(id = 1, with = Coordinate, unit = Degrees, min = -90.0, max = 90.0)]
    pub latitude: f64,
    /// Longitude in decimal degrees, -180 to 180 (WGS84).
    #[config(id = 2, with = Coordinate, unit = Degrees, min = -180.0, max = 180.0)]
    pub longitude: f64,
    /// Height above mean sea level in metres.
    #[config(id = 3, with = Metres, unit = Meters, min = MIN_HEIGHT_M, max = MAX_HEIGHT_M)]
    pub height_m: Option<f64>,
}

impl Default for LocationConfig {
    fn default() -> Self {
        Self {
            latitude: f64::NAN,
            longitude: f64::NAN,
            height_m: None,
        }
    }
}

/// Lowest height accepted: the Dead Sea shore is about -430 m.
const MIN_HEIGHT_M: f64 = -1_000.0;
/// Highest height accepted: above any place a station is mounted.
const MAX_HEIGHT_M: f64 = 10_000.0;

impl LocationConfig {
    /// Whether the coordinates are finite and in range.
    pub fn is_valid(&self) -> bool {
        self.latitude.is_finite()
            && (-90.0..=90.0).contains(&self.latitude)
            && self.longitude.is_finite()
            && (-180.0..=180.0).contains(&self.longitude)
            && self
                .height_m
                .is_none_or(|height| (MIN_HEIGHT_M..=MAX_HEIGHT_M).contains(&height))
    }

    /// Whether nothing is given: the device has no configured position. A
    /// height alone is not a position and is not unset.
    pub fn is_unset(&self) -> bool {
        self.latitude.is_nan() && self.longitude.is_nan() && self.height_m.is_none()
    }

    /// The configured position as a [`Location`].
    pub const fn location(&self) -> Location {
        Location {
            latitude: self.latitude,
            longitude: self.longitude,
            height_m: self.height_m,
            source: LocationSourceKind::Configured,
        }
    }
}

/// A coordinate in degrees, held as `f64` and shown as `f32`.
///
/// A coordinate has no value until it is given: the default is `NaN`, which
/// reads as "no value", so that a first `height_m=...` never stores "0, 0".
struct Coordinate;

impl Codec<f64> for Coordinate {
    const KIND: ValueKind = ValueKind::F32;

    fn encode(degrees: &f64) -> Result<Option<Value>, FieldError> {
        Ok((!degrees.is_nan()).then_some(Value::F32(*degrees as f32)))
    }

    fn decode(value: &Value) -> Result<f64, FieldError> {
        Ok(f64::from(value.as_f32().ok_or(FieldError::WrongKind)?))
    }
}

/// A height in metres, held as `f64` and shown as `f32`.
struct Metres;

impl Codec<f64> for Metres {
    const KIND: ValueKind = ValueKind::F32;

    fn encode(metres: &f64) -> Result<Option<Value>, FieldError> {
        Ok(Some(Value::F32(*metres as f32)))
    }

    fn decode(value: &Value) -> Result<f64, FieldError> {
        Ok(f64::from(value.as_f32().ok_or(FieldError::WrongKind)?))
    }
}

/// A section without coordinates is valid: the device does not know where
/// it is. A single coordinate, or one out of range, is not.
fn validate_section(config: &LocationConfig) -> Result<(), FieldError> {
    if config.is_unset() || config.is_valid() {
        Ok(())
    } else {
        Err(FieldError::Invalid)
    }
}
