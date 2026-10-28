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
//! Typed measurement units.
//!
//! Every physical quantity has its own wrapper so values in different units
//! cannot be mixed by accident. Wrappers are `repr(transparent)` and hold the
//! plain numeric value; fixed-point encoding for constrained transports is the
//! concern of the future protocol adapters, not of this crate.

use core::fmt;

/// A unit of measurement.
pub trait Unit {
    /// Short human readable unit symbol.
    const SYMBOL: &'static str;
}

macro_rules! unit {
    ($(#[$meta:meta])* $name:ident($ty:ty), $symbol:literal) => {
        $(#[$meta])*
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
        pub struct $name(pub $ty);

        impl Unit for $name {
            const SYMBOL: &'static str = $symbol;
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{} {}", self.0, Self::SYMBOL)
            }
        }
    };
}

unit!(
    /// Temperature in degrees Celsius.
    Celsius(f32), "°C"
);
unit!(
    /// Relative humidity in percent.
    RelativeHumidity(f32), "%"
);
unit!(
    /// Pressure in Pascals.
    Pascal(f32), "Pa"
);
unit!(
    /// Gas concentration in parts per million.
    Ppm(f32), "ppm"
);
unit!(
    /// Gas concentration in parts per billion.
    Ppb(f32), "ppb"
);
unit!(
    /// Sound level in decibels.
    Decibels(f32), "dB"
);
unit!(
    /// Particulate matter mass concentration in micrograms per cubic meter.
    MicrogramsPerCubicMeter(f32), "µg/m³"
);
unit!(
    /// Air quality index value.
    Aqi(u16), "AQI"
);
unit!(
    /// Ionizing radiation dose rate in microsieverts per hour.
    RadiationLevel(f32), "µSv/h"
);
unit!(
    /// Number of detected radiation pulses.
    PulseCount(u32), "pulses"
);

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::string::ToString;

    #[test]
    fn symbols() {
        assert_eq!(Celsius::SYMBOL, "°C");
        assert_eq!(Pascal::SYMBOL, "Pa");
        assert_eq!(MicrogramsPerCubicMeter::SYMBOL, "µg/m³");
    }

    #[test]
    fn display_includes_unit() {
        assert_eq!(Celsius(22.5).to_string(), "22.5 °C");
        assert_eq!(Ppm(530.0).to_string(), "530 ppm");
        assert_eq!(Aqi(42).to_string(), "42 AQI");
    }

    #[test]
    fn transparent_layout() {
        assert_eq!(core::mem::size_of::<Celsius>(), core::mem::size_of::<f32>());
        assert_eq!(core::mem::size_of::<Aqi>(), core::mem::size_of::<u16>());
    }
}
