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
//! Sensor bus traits. Board implements this traits to access sensor measures.

/// A Particulate Matter (PM) sensor measures the floating particles in the air.
pub(crate) trait ParticulateMatter {
    /// A measurement of PM2.5 fine dust pollution.
    async fn pm10(&mut self) -> Option<u16>;

    /// A measurement of PM10 fine dust pollution.
    async fn pm25(&mut self) -> Option<u16>;
}

/// A Humidity sensor measures relative humidity of the environment.
pub(crate) trait Humidity {
    /// The measured humidity in tenths of a percent.
    async fn humidity(&mut self) -> Option<u16>;
}

/// A Temperature sensors measures temperature of the environment.
pub(crate) trait Temperature {
    /// The measured temperature in tenths of degrees **Celsius**.
    async fn temperature(&mut self) -> Option<i16>;
}

/// A Pressure sensors measures air / liquid pressure in environment.
pub(crate) trait Pressure {
    /// The measured pressure in **Pascals**.
    async fn pressure(&mut self) -> Option<u32>;
}
