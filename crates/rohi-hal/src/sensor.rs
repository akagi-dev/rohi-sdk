///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025 Akagi Engineering <admin@akagi.dev>
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
//! Robonomics sensors collection provide different kinds of measures via same
//! async interface. For example, access sensor data for board instance will
//! looks like:
//!
//! ```rust
//! let board_sensors = ...
//! let temp = board_sensors.temperature().await;
//! println!("{}", temp);
//! ```

/// A Particulate Matter (PM) sensor measures the floating particles in the air.
#[allow(async_fn_in_trait)]
pub trait ParticulateMatter {
    /// A measurement of PM2.5 fine dust pollution.
    async fn pm10(&mut self) -> Option<u16>;

    /// A measurement of PM10 fine dust pollution.
    async fn pm25(&mut self) -> Option<u16>;
}

/// A Humidity sensor measures relative humidity of the environment.
#[allow(async_fn_in_trait)]
pub trait Humidity {
    /// The measured humidity in tenths of a percent.
    async fn humidity(&mut self) -> Option<u16>;
}

/// A Temperature sensor measures temperature of the environment.
#[allow(async_fn_in_trait)]
pub trait Temperature {
    /// The measured temperature in tenths of degrees **Celsius**.
    async fn temperature(&mut self) -> Option<i16>;
}

/// A Pressure sensor measures air / liquid pressure in environment.
#[allow(async_fn_in_trait)]
pub trait Pressure {
    /// The measured pressure in **Pascals**.
    async fn pressure(&mut self) -> Option<u32>;
}
