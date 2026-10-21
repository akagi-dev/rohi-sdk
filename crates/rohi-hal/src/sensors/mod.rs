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
//! Sensor drivers publishing typed `rohi-sensor-core` observations.

/// BMP280 / BME280 temperature, pressure and humidity sensor.
pub mod bmx280;
/// Noise level measurement from an I2S microphone.
pub mod noise;
/// SCD40 / SCD41 CO2, temperature and humidity sensor.
pub mod scd4x;
/// SDS011 particulate matter sensor.
pub mod sds011;
