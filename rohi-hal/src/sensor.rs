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
//! let board = ...
//! let sensor = Sensor::new(board);
//! let temp = sensor.temperature().await;
//! println!("{}", temp);
//!

pub(crate) mod bus;
use bus::*;

/// Generic sensor data interface.
pub struct Sensor<'a, T>(pub &'a mut T);

impl<T: ParticulateMatter> Sensor<'_, T> {
    /// A measurement of PM2.5 fine dust pollution.
    pub async fn pm10(&mut self) -> Option<u16> {
        self.0.pm10().await
    }

    /// A measurement of PM10 fine dust pollution.
    pub async fn pm25(&mut self) -> Option<u16> {
        self.0.pm25().await
    }
}

impl<T: Humidity> Sensor<'_, T> {
    /// The measured humidity in tenths of a percent.
    pub async fn humidity(&mut self) -> Option<u16> {
        self.0.humidity().await
    }
}

impl<T: Temperature> Sensor<'_, T> {
    /// The measured temperature in tenths of degrees **Celsius**.
    pub async fn temperature(&mut self) -> Option<i16> {
        self.0.temperature().await
    }
}

impl<T: Pressure> Sensor<'_, T> {
    /// The measured pressure in **Pascals**.
    pub async fn pressure(&mut self) -> Option<u32> {
        self.0.pressure().await
    }
}
