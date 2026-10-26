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

use minicbor::{CborLen, Decode, Encode};
use rohi_config::ConfigSection;
use rohi_service::{ConfigField, FieldError, ManagedConfig, Value, ValueKind};

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

/// Where the user says the device is.
///
/// Persistent ABI: the section ID is derived from the struct name
/// `LocationConfig`, the encoding is a CBOR array pinned by a golden test.
#[derive(Clone, Copy, Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[cbor(array)]
#[config(version = 1)]
pub struct LocationConfig {
    /// Latitude in decimal degrees, -90 to 90 (WGS84).
    #[n(0)]
    pub latitude: f64,
    /// Longitude in decimal degrees, -180 to 180 (WGS84).
    #[n(1)]
    pub longitude: f64,
    /// Height above mean sea level in metres.
    #[n(2)]
    pub height_m: Option<f64>,
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
    const VERSION: u16 = <Self as ConfigSection>::VERSION;
    const FIELDS: &'static [ConfigField] = &[
        ConfigField::new(field::LATITUDE, "latitude", ValueKind::F64)
            .describe("Latitude, WGS84")
            .unit("deg")
            .range(-90.0, 90.0),
        ConfigField::new(field::LONGITUDE, "longitude", ValueKind::F64)
            .describe("Longitude, WGS84")
            .unit("deg")
            .range(-180.0, 180.0),
        ConfigField::new(field::HEIGHT_M, "height_m", ValueKind::F64)
            .describe("Height above mean sea level")
            .unit("m")
            .range(MIN_HEIGHT_M, MAX_HEIGHT_M)
            .optional(),
    ];

    /// Not a position: latitude and longitude must both be given, otherwise
    /// a first `set height_m=...` would store "0, 0".
    fn defaults() -> Self {
        Self {
            latitude: f64::NAN,
            longitude: f64::NAN,
            height_m: None,
        }
    }

    fn read_field(&self, id: u32) -> Result<Option<Value>, FieldError> {
        match id {
            field::LATITUDE => Ok(Some(Value::F64(self.latitude))),
            field::LONGITUDE => Ok(Some(Value::F64(self.longitude))),
            field::HEIGHT_M => Ok(self.height_m.map(Value::F64)),
            _ => Err(FieldError::UnknownField),
        }
    }

    fn set_field(&mut self, id: u32, value: Value) -> Result<(), FieldError> {
        let number = value.as_f64().ok_or(FieldError::WrongKind)?;
        match id {
            field::LATITUDE => self.latitude = number,
            field::LONGITUDE => self.longitude = number,
            field::HEIGHT_M => self.height_m = Some(number),
            _ => return Err(FieldError::UnknownField),
        }
        Ok(())
    }

    fn unset_field(&mut self, id: u32) -> Result<(), FieldError> {
        match id {
            field::HEIGHT_M => {
                self.height_m = None;
                Ok(())
            }
            field::LATITUDE | field::LONGITUDE => Err(FieldError::NotOptional),
            _ => Err(FieldError::UnknownField),
        }
    }

    fn validate(&self) -> Result<(), FieldError> {
        if self.is_valid() {
            Ok(())
        } else {
            Err(FieldError::Invalid)
        }
    }
}
