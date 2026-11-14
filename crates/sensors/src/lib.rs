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

//! Robonomics Open Hardware Interface (ROHI): sensor measurements, state and
//! the managed `sensors` service.
//!
//! `rohi-sensors` owns the hardware-independent model of **local physical
//! sensor observations**: typed units, per-sensor measurements and their
//! freshness, the sensor state and events, the [`MeasurementStore`], the
//! bounded event bus and the snapshot service, and (feature `service`) the
//! read-only `sensors` status service. It is `no_std`, heapless and statically
//! bounded, and works on a host without any sensor driver, ESP, IP or BLE
//! dependency. Sensor drivers live in `rohi-hal`; they emit
//! [`MeasurementUpdate`]s that this crate stores.
//!
//! It does not aggregate sources, publish measurements, select what is
//! disclosed or know any transport: a consumer such as a telemetry service
//! adapts a [`runtime::MeasurementHandle`] to its own source model.
//!
//! # Quick example
//!
//! A sensor event goes into the store, a snapshot comes out. This is what the
//! store task of a firmware does with the events of its sensor tasks:
//!
//! ```
//! use rohi_sensors::{
//!     Bmx280Update, Celsius, MeasurementState, MeasurementStore, MeasurementUpdate, Pascal,
//!     RelativeHumidity, SensorEvent, SensorKind, SensorState,
//! };
//! use rohi_time::MonotonicTimestamp;
//!
//! let mut store = MeasurementStore::new();
//!
//! // A BME280 observation taken one second after boot.
//! let observed_at = MonotonicTimestamp::from_millis(1_000);
//! store.apply(SensorEvent::Measurements(MeasurementUpdate::Bmx280(
//!     Bmx280Update::bme280(Celsius(21.5), RelativeHumidity(45.0), Pascal(100_800.0), observed_at),
//! )));
//!
//! // Snapshots are plain values; freshness is resolved at the time you ask.
//! let snapshot = store.snapshot(MonotonicTimestamp::from_millis(2_000));
//! let bmx280 = snapshot.measurements.bmx280.expect("sensor reported");
//! assert_eq!(bmx280.temperature.value, Some(Celsius(21.5)));
//! assert_eq!(bmx280.temperature.state, MeasurementState::Valid);
//! assert_eq!(snapshot.sensor_statuses.get(SensorKind::Bmx280).state, SensorState::Ready);
//!
//! // A sensor that never reported has no entry at all.
//! assert!(snapshot.measurements.scd4x.is_none());
//! ```
//!
//! # Core concepts
//!
//! * **Observations, not interpretation.** Measurements are grouped by the
//!   physical sensor that produced them. If a BME280 and an SCD4x both report
//!   temperature, both values stay available independently
//!   (`bmx280.temperature` and `scd4x.temperature`). The crate never averages,
//!   fuses, merges or relabels them.
//! * **Typed units.** Every quantity has its own wrapper ([`Celsius`],
//!   [`Pascal`], [`Ppm`], ...), see [`units`].
//! * **Measurement versus sensor state.** A [`Measurement`] carries the last
//!   valid value, its time and a [`MeasurementState`] (`Unavailable`,
//!   `WarmingUp`, `Valid`, `Stale`, `Error`). The runtime state of the sensor
//!   ([`SensorState`]) and its error counters ([`SensorStatus`]) are separate.
//!   An error never erases the last valid value.
//! * **Freshness.** A valid value becomes `Stale` when it is older than the
//!   [`FreshnessPolicy`] of its sensor; this is applied when a snapshot is
//!   produced, so it is correct at request time.
//! * **Monotonic time.** Time is supplied by the caller as a
//!   [`MonotonicTimestamp`](rohi_time::MonotonicTimestamp); nothing depends on
//!   wall-clock time.
//! * **One owner.** The [`MeasurementStore`] has exactly one owner (the store
//!   task), the only component that applies events. Other tasks ask it for
//!   snapshots through a [`runtime::MeasurementHandle`].
//!
//! # Architecture
//!
//! ```text
//! rohi-time    rohi-service
//!     \           /
//!     rohi-sensors   <- units, measurements, events, store, bus, managed service
//!      /        \
//!  rohi-hal   telemetry sources (adapters over a MeasurementHandle)
//!  drivers
//! ```
//!
//! | Module | Role |
//! |--------|------|
//! | [`units`] | Typed units: [`Celsius`], [`RelativeHumidity`], [`Pascal`], [`Ppm`], [`Decibels`], ... and the [`units::Unit`] trait |
//! | [`measurements`] | [`Measurement`], [`MeasurementState`], [`FreshnessPolicy`], the per-sensor groups, [`Measurements`] and [`MeasurementsSnapshot`] |
//! | [`sensor`] | [`SensorKind`], [`SensorState`], [`SensorStatus`], [`SensorErrorCode`], the per-sensor updates and [`SensorEvent`] |
//! | [`store`] | [`MeasurementStore`]: applies events, produces snapshots |
//! | [`runtime`] | [`Reporter`](runtime::Reporter), [`EventBus`](runtime::EventBus), [`MeasurementService`](runtime::MeasurementService), [`SensorStates`](runtime::SensorStates) |
//! | `managed` (feature `service`) | `SensorsService`: the sensors as a `rohi-service` status service |
//!
//! # Canonical measurement names
//!
//! The status fields and the canonical names are `<sensor>.<measurement>` in
//! the canonical unit of the type:
//!
//! | Field | Unit | Field | Unit |
//! |-------|------|-------|------|
//! | `bmx280.temperature` | °C | `zmod4510.o3`, `zmod4510.no2` | ppb |
//! | `bmx280.humidity` | % RH | `zmod4510.aqi_fast`, `zmod4510.aqi_epa` | index |
//! | `bmx280.pressure` | Pa | `ags3871.co` | ppm |
//! | `scd4x.co2` | ppm | `radsens.radiation` | µSv/h |
//! | `scd4x.temperature`, `scd4x.humidity` | °C, % RH | `radsens.pulses` | pulses |
//! | `sds011.pm2_5`, `sds011.pm10` | µg/m³ | `microphone.average`, `microphone.maximum` | dB |
//!
//! Which of them a consumer publishes, and under which stable bit, is decided
//! by that consumer (the telemetry catalog), never by this crate.
//!
//! # The sensors service
//!
//! With the `service` feature, `SensorsService` exposes a
//! [`runtime::MeasurementHandle`] as the generic read-only `sensors` service
//! of `rohi-service`. Every measurement of a present sensor is a status field
//! named `<sensor>.<measurement>` in the canonical unit of its type, absent
//! until the sensor has delivered a value; the status schema has at most 20
//! fields. Every request takes a fresh snapshot, so values and states are
//! correct at request time.
//!
//! ```text
//! $ rohi status sensors
//! Service:  sensors
//! State:    running
//! Health:   ok
//!
//! bmx280.temperature  21.5 C
//! bmx280.humidity     45 %
//! scd4x.co2           640 ppm
//! ```
//!
//! # Guarantees & limitations
//!
//! * **No allocation.** All types have a fixed size; [`MeasurementStore`],
//!   [`Measurements`] and [`MeasurementsSnapshot`] are `Copy`.
//! * **Bounded queues.** The [`runtime::EventBus`] never blocks producers: when
//!   it is full the oldest event is dropped and counted
//!   ([`runtime::EventBus::dropped`]).
//! * **Chronological events.** [`MeasurementStore::apply`] expects events in
//!   chronological order.
//! * **Verification status.** Covered by host tests and doctests only; no
//!   sensor is recorded as hardware-verified in `device-matrix.toml`.

#[cfg(test)]
extern crate std;

pub mod measurements;
pub mod runtime;
pub mod sensor;
pub mod store;
pub mod units;

#[cfg(feature = "service")]
mod managed;

#[cfg(test)]
mod testutil;

#[cfg(feature = "service")]
pub use managed::SensorsService;
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
pub use units::*;
