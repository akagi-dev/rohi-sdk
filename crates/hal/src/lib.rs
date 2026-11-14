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
#![no_std]

//! Robonomics Open Hardware Interface (ROHI): physical sensor drivers.
//!
//! `rohi-hal` is the hardware-facing part of the sensor stack: the drivers of
//! the physical sensors, the lifecycle [`Runner`](runtime::Runner) that
//! schedules acquisition and recovers from failures, and bounded I/O helpers
//! such as [`TimeoutI2c`](runtime::TimeoutI2c). Drivers emit
//! [`rohi_sensors::MeasurementUpdate`]s and the runner publishes them through a
//! [`rohi_sensors::runtime::Reporter`] into a
//! [`rohi_sensors::runtime::EventBus`]. The measurement model, the store, the
//! snapshot service and the `sensors` status service are owned by
//! [`rohi-sensors`](../rohi_sensors/index.html); time, [`Clock`] and
//! [`Backoff`] by [`rohi-time`](../rohi_time/index.html). Nothing here is
//! re-exported from those crates.
//!
//! Nothing here names a chip: drivers use `embedded-hal-async` and
//! `embedded-io-async`, so the whole crate, drivers included, is tested on a
//! regular host. Pins, buses and the clock of a concrete chip live in the
//! platform crate [`rohi-hal-esp`](../rohi_hal_esp/index.html). Drivers know
//! nothing about Wi-Fi, BLE, serialization or any other transport.
//!
//! [`Clock`]: rohi_time::Clock
//! [`Backoff`]: rohi_time::Backoff
//!
//! # Architecture & integration
//!
//! ```text
//! physical sensor -> driver (sensors) -> Runner (runtime, one task per sensor)
//!   -> rohi_sensors::SensorEvent -> bounded EventBus -> store task
//!   -> MeasurementStore -> MeasurementsSnapshot -> consumers
//! ```
//!
//! The dependency direction is firmware (`altruist-urban`) -> `rohi-hal-esp`
//! -> `rohi-hal` -> `rohi-sensors` and `rohi-time`. The Altruist Urban firmware
//! starts one Embassy task per sensor, so a failing or slow sensor never delays
//! another one, and one store task that owns the store. I²C sensors share one
//! bus whose transactions are bounded by [`runtime::TimeoutI2c`] (the firmware
//! uses 200 ms).
//!
//! | Need | Provided by |
//! |------|-------------|
//! | Sensors on a new board or chip | A board module of [`rohi-hal-esp`](../rohi_hal_esp/index.html) hands out the I²C, UART and I²S peripherals; the drivers here are reused unchanged |
//! | Showing sensor values to a user | `rohi_sensors::SensorsService` (feature `service` of `rohi-sensors`) |
//! | Sending measurements to a backend | A consumer takes a `MeasurementsSnapshot` from `rohi-sensors` and encodes it itself |
//! | A new sensor | Implement [`runtime::SensorDriver`], see <https://rohi.akagi.dev/guides/add-sensor.html> |
//!
//! # API navigation
//!
//! | Module | Role |
//! |--------|------|
//! | [`runtime`] | [`Runner`](runtime::Runner), [`SensorDriver`](runtime::SensorDriver), [`RunnerConfig`](runtime::RunnerConfig), [`TimeoutI2c`](runtime::TimeoutI2c) |
//! | [`sensors`] | Drivers [`sensors::bmx280`], [`sensors::scd4x`], [`sensors::sds011`], [`sensors::noise`] |
//!
//! Sensor drivers exist for the BMP280 / BME280, SCD40 / SCD41, SDS011 and an
//! I²S microphone (noise level). `rohi_sensors::SensorKind` also names the
//! ZMOD4510, AGS3871 and RadSens sensors: the model types, the store and the
//! status service cover them, but this crate has **no driver** for them.
//!
//! # Guarantees & limitations
//!
//! * **Bounded waits.** Every transaction of a [`runtime::TimeoutI2c`], every
//!   SDS011 UART read and every audio read has a time bound; a stuck device
//!   ends in an error, not in a hang.
//! * **Cancellation.** The sensor runner is meant to run to completion; see
//!   [`runtime`] for what can be dropped safely.
//! * **Noise level.** The microphone level is unweighted dB SPL computed from
//!   the nominal sensitivity of an ICS-43434: it is not dBA and not calibrated
//!   per device, see [`sensors::noise`].
//! * **SDS011 and noise coordination is not implemented.** The SDS011 fan is a
//!   noise source and the legacy firmware coordinated it with the noise
//!   measurement; here nothing does. `rohi_sensors::runtime::SensorStates` is a
//!   read-only view of every sensor state that a future policy can use without
//!   coupling the drivers.
//! * **Verification status.** The crate is covered by host tests
//!   (`cargo test -p rohi-hal`: runner timing and recovery, SDS011 sampling and
//!   aggregation, BMx280 address probing, freshness thresholds) and by
//!   doctests; the firmware that uses it builds for ESP32-C3 and ESP32-C6 in
//!   CI. **No sensor is recorded as hardware-verified** in the support matrix
//!   (`device-matrix.toml`); timing constants such as the SDS011 duty cycle
//!   are starting points not yet confirmed on a device.
//!
//! Further reading: the support matrix
//! (<https://rohi.akagi.dev/support/overview.html>), host testing
//! (<https://rohi.akagi.dev/validation/host-testing.html>), hardware testing
//! status (<https://rohi.akagi.dev/validation/hardware-testing.html>) and the
//! resource budget
//! (<https://rohi.akagi.dev/validation/resource-budget.html#sensor-runtime-rohi-sensors-rohi-hal>).

#[cfg(test)]
extern crate std;

pub mod runtime;
pub mod sensors;

#[cfg(test)]
mod testutil;
