///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2018-2026 Robonomics Network <research@robonomics.network>
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
#![no_std]

//! Hardware-independent sensor primitives for Robonomics Open Hardware devices.
//!
//! The crate defines strongly typed units, per-sensor measurements, sensor /
//! device / uplink state and a [`MeasurementStore`] producing immutable
//! telemetry snapshots. It depends on no MCU HAL, async runtime, transport or
//! serialization format, never allocates, and is fully testable on a regular
//! host.
//!
//! Data flow:
//!
//! ```text
//! Sensor drivers -> SensorEvent -> MeasurementStore
//!                -> MeasurementsSnapshot -> Connectivity / BLE / diagnostics
//! ```
//!
//! # Observations, not interpretation
//!
//! Measurements are grouped by the physical sensor that produced them. If
//! BME280 and SCD4x both report temperature, both values stay available
//! independently (`bmx280.temperature` and `scd4x.temperature`). The crate
//! never averages, fuses or selects between sensors.
//!
//! # Time
//!
//! Time is supplied by the caller as a [`MonotonicTimestamp`]; the crate never
//! reads a clock and does not need UTC.

/// Monotonic time.
pub mod time;

/// Typed measurement units.
pub mod units;

/// Measurements grouped by physical sensor, and snapshots.
pub mod measurements;

/// Sensor identity, state, errors and events.
pub mod sensor;

/// Device state.
pub mod device;

/// Transport-neutral uplink types.
pub mod uplink;

/// Measurement store.
pub mod store;

pub use device::DeviceState;
pub use measurements::{
    Ags3871Measurements, Bmx280Measurements, FreshnessPolicy, Measurement, MeasurementState,
    Measurements, MeasurementsSnapshot, NoiseMeasurements, RadSensMeasurements, Scd4xMeasurements,
    Sds011Measurements, Zmod4510Measurements,
};
pub use sensor::{
    Ags3871Update, Bmx280Update, MeasurementUpdate, NoiseUpdate, RadSensUpdate, Scd4xUpdate,
    Sds011Update, SensorErrorCode, SensorEvent, SensorKind, SensorState, SensorStatus,
    SensorStatuses, Zmod4510Update,
};
pub use store::MeasurementStore;
pub use time::MonotonicTimestamp;
pub use units::*;
pub use uplink::{ActiveUplink, UplinkMode, UplinkState};
