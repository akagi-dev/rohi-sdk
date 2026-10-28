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
//! The persistent `location` section and its generic config interface.

use rohi_config::ConfigOverrides;
use rohi_service::{Bounds, ConfigField, FieldError, ManagedConfig, Unit, Value, ValueKind};

use crate::{Location, LocationSourceKind};

/// Field ids of the `location` config section.
pub mod field {
    /// Latitude, degrees.
    pub const LATITUDE: u32 = 1;
    /// Longitude, degrees.
    pub const LONGITUDE: u32 = 2;
    /// Height above mean sea level, metres.
    pub const HEIGHT_M: u32 = 3;
}

/// Where the user says the device is (effective configuration).
///
/// `LocationConfig::default()` is no position: latitude and longitude are
/// `NaN` and the section is not valid on its own, so a first `height_m=...`
/// never stores "0, 0". What is stored are the explicit user overrides of it,
/// [`LocationOverrides`].
#[derive(Clone, Copy, Debug, PartialEq, ConfigOverrides)]
#[config(overrides = LocationOverrides, version = 1)]
pub struct LocationConfig {
    /// Latitude in decimal degrees, -90 to 90 (WGS84).
    #[config(id = 1)]
    pub latitude: f64,
    /// Longitude in decimal degrees, -180 to 180 (WGS84).
    #[config(id = 2)]
    pub longitude: f64,
    /// Height above mean sea level in metres.
    #[config(id = 3)]
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

impl ManagedConfig for LocationConfig {
    const NAME: &'static str = "location";
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(field::LATITUDE, "latitude", ValueKind::F32)
            .unit(Unit::Degrees)
            .bounds(Bounds::F32(-90.0, 90.0)),
        ConfigField::new(field::LONGITUDE, "longitude", ValueKind::F32)
            .unit(Unit::Degrees)
            .bounds(Bounds::F32(-180.0, 180.0)),
        ConfigField::new(field::HEIGHT_M, "height_m", ValueKind::F32)
            .unit(Unit::Meters)
            .bounds(Bounds::F32(MIN_HEIGHT_M as f32, MAX_HEIGHT_M as f32))
            .optional(),
    ];

    /// Latitude and longitude have no value until they are given: the
    /// default is not a position.
    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        let coordinate = |degrees: f64| (!degrees.is_nan()).then_some(Value::F32(degrees as f32));
        match id {
            field::LATITUDE => Ok(coordinate(self.latitude)),
            field::LONGITUDE => Ok(coordinate(self.longitude)),
            field::HEIGHT_M => Ok(self.height_m.map(|height| Value::F32(height as f32))),
            _ => Err(FieldError::UnknownField),
        }
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        let number = f64::from(value.as_f32().ok_or(FieldError::WrongKind)?);
        match id {
            field::LATITUDE => self.latitude = number,
            field::LONGITUDE => self.longitude = number,
            field::HEIGHT_M => self.height_m = Some(number),
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn field_is_set(&self, id: u32) -> Result<bool, FieldError> {
        Ok(self.read_field(id)?.is_some())
    }

    fn set_none(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            field::HEIGHT_M => {
                self.height_m = None;
                Ok(())
            }
            field::LATITUDE | field::LONGITUDE => Err(FieldError::NotOptional),
            _ => Err(FieldError::UnknownField),
        }
    }

    /// A section without coordinates is valid: the device does not know where
    /// it is. A single coordinate, or one out of range, is not.
    fn validate(&self) -> Result<(), FieldError> {
        if self.is_unset() || self.is_valid() {
            Ok(())
        } else {
            Err(FieldError::Invalid)
        }
    }
}
