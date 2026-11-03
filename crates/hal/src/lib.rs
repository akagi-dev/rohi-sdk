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

//! Robonomics Open Hardware Interface (ROHI): the sensor HAL.
//!
//! `rohi-hal` turns physical sensors into immutable [`MeasurementsSnapshot`]
//! values. It is the portable, `no_std`, allocation-free part of the sensor
//! stack: typed units, per-sensor measurements, sensor state, the
//! [`MeasurementStore`], the sensor runtime (lifecycle runner, event queue,
//! snapshot access) and the sensor drivers. Use it when firmware needs "the
//! latest trustworthy reading of every sensor, with its age and health"
//! without caring which bus the sensor is on or which transport reports it.
//!
//! Nothing here names a chip: drivers use `embedded-hal-async` and
//! `embedded-io-async`, so the whole crate, drivers included, is tested on a
//! regular host. Pins, buses and the clock of a concrete chip live in the
//! platform crate [`rohi-hal-esp`](../rohi_hal_esp/index.html). Drivers know
//! nothing about Wi-Fi, BLE, serialization or any other transport.
//!
//! # Quick example
//!
//! A sensor event goes into the store, a snapshot comes out. This is what the
//! store task of a firmware does with the events of its sensor tasks:
//!
//! ```
//! use rohi_hal::{
//!     Bmx280Update, Celsius, MeasurementState, MeasurementStore, MeasurementUpdate,
//!     MonotonicTimestamp, Pascal, RelativeHumidity, SensorEvent, SensorKind, SensorState,
//! };
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
//! On a device the events come from sensor tasks, see
//! [Architecture & integration](#architecture--integration). The maintained
//! wiring is in `firmwares/altruist-urban` (`src/tasks/sensors.rs` and
//! `src/tasks/store.rs`); one program per sensor is in `examples/`.
//!
//! # Core concepts
//!
//! * **Observations, not interpretation.** Measurements are grouped by the
//!   physical sensor that produced them. If a BME280 and an SCD4x both report
//!   temperature, both values stay available independently
//!   (`bmx280.temperature` and `scd4x.temperature`). The crate never averages,
//!   fuses or selects between sensors.
//! * **Typed units.** Every quantity has its own wrapper ([`Celsius`],
//!   [`Pascal`], [`Ppm`], ...), see [`units`], so values in different units
//!   cannot be mixed by accident.
//! * **Measurement versus sensor state.** A [`Measurement`] carries the last
//!   valid value, its time and a [`MeasurementState`] (`Unavailable`,
//!   `WarmingUp`, `Valid`, `Stale`, `Error`). The runtime state of the sensor
//!   ([`SensorState`]: `NotDetected` ... `Error`) and its error counters
//!   ([`SensorStatus`]) are separate. An error never erases the last valid
//!   value.
//! * **Freshness.** A valid value becomes `Stale` when it is older than the
//!   [`FreshnessPolicy`] of its sensor; this is applied when a snapshot is
//!   produced, so it is correct at request time. See [`measurements`].
//! * **Monotonic time.** Time is supplied by the caller as a
//!   [`MonotonicTimestamp`] (milliseconds since an arbitrary epoch, usually
//!   boot); the crate never reads a clock and does not need UTC. All waits use
//!   monotonic async timers, nothing depends on wall-clock time.
//! * **One owner.** The [`MeasurementStore`] has exactly one owner (the store
//!   task), the only component that applies events. Other tasks ask it for
//!   snapshots through a [`runtime::MeasurementHandle`].
//!
//! # Architecture & integration
//!
//! ```text
//! physical sensor -> driver (sensors) -> Runner (runtime, one task per sensor)
//!   -> SensorEvent -> bounded EventBus -> store task -> MeasurementStore
//!   -> MeasurementsSnapshot -> consumers
//!
//! consumers: log, SensorsService (status plane / BLE control), connectivity
//!            telemetry, other tasks, via runtime::MeasurementHandle
//! ```
//!
//! The dependency direction is firmware (`altruist-urban`) -> `rohi-hal-esp`
//! -> `rohi-hal`. The Altruist Urban firmware starts one Embassy task per
//! sensor, so a failing or slow sensor never delays another one, and one store
//! task that owns the store. I²C sensors share one bus whose transactions are
//! bounded by [`runtime::TimeoutI2c`] (the firmware uses 200 ms). The sensor
//! tasks and the store task meet only in a static [`runtime::EventBus`]
//! (capacity [`runtime::EVENT_QUEUE_CAPACITY`], 16 events) and a static
//! [`runtime::MeasurementService`].
//!
//! | Need | Provided by |
//! |------|-------------|
//! | Sensors on a new board or chip | A board module of [`rohi-hal-esp`](../rohi_hal_esp/index.html) hands out the I²C, UART and I²S peripherals; the drivers here are reused unchanged |
//! | Showing sensor values to a user | [`SensorsService`] exposes the handle as the `sensors` service of [`rohi_service`] (the status plane of the control protocol) |
//! | Sending measurements to a backend | A consumer takes a [`MeasurementsSnapshot`] and encodes it itself; this crate has no serialization or transport |
//! | A new sensor | Implement [`runtime::SensorDriver`], see <https://rohi.akagi.dev/guides/add-sensor.html> |
//!
//! The Altruist Urban firmware runs Embassy on top of `esp-rtos`; its only
//! heap is the fixed radio heap owned by the ESP wireless runtime (see
//! <https://docs.rs/rohi-net-esp>). Sensor and application
//! code of this crate is heapless.
//!
//! # API navigation
//!
//! | Module | Role |
//! |--------|------|
//! | [`units`] | Typed units: [`Celsius`], [`RelativeHumidity`], [`Pascal`], [`Ppm`], [`Decibels`], ... and the [`units::Unit`] trait |
//! | [`measurements`] | [`Measurement`], [`MeasurementState`], [`FreshnessPolicy`], the per-sensor groups, [`Measurements`] and [`MeasurementsSnapshot`] |
//! | [`sensor`] | [`SensorKind`], [`SensorState`], [`SensorStatus`], [`SensorErrorCode`], the per-sensor updates and [`SensorEvent`] |
//! | [`store`] | [`MeasurementStore`]: applies events, produces snapshots |
//! | [`runtime`] | [`Runner`](runtime::Runner), [`Reporter`](runtime::Reporter), [`EventBus`](runtime::EventBus), [`Backoff`](runtime::Backoff), [`TimeoutI2c`](runtime::TimeoutI2c), [`MeasurementService`](runtime::MeasurementService), [`SensorStates`](runtime::SensorStates) |
//! | [`sensors`] | Drivers [`sensors::bmx280`], [`sensors::scd4x`], [`sensors::sds011`], [`sensors::noise`]; the fields of the `sensors` status service |
//! | [`time`] | [`MonotonicTimestamp`] |
//! | [`device`], [`uplink`] | [`DeviceState`] and the transport-neutral [`UplinkMode`], [`ActiveUplink`], [`UplinkState`] vocabulary |
//! | [`SensorsService`] | The sensors as a [`rohi_service`] service |
//!
//! Sensor drivers exist for the BMP280 / BME280, SCD40 / SCD41, SDS011 and an
//! I²S microphone (noise level). [`SensorKind`] also names the ZMOD4510,
//! AGS3871 and RadSens sensors: the model types, the store and the status
//! service cover them, but this crate has **no driver** for them.
//!
//! # Guarantees & limitations
//!
//! * **No allocation.** All types have a fixed size; [`MeasurementStore`],
//!   [`Measurements`] and [`MeasurementsSnapshot`] are `Copy`.
//! * **Bounded queues.** The [`runtime::EventBus`] never blocks producers: when
//!   it is full the oldest event is dropped and counted
//!   ([`runtime::EventBus::dropped`]). Snapshot requests use one request slot
//!   and one response slot.
//! * **Bounded waits.** Every transaction of a [`runtime::TimeoutI2c`], every
//!   SDS011 UART read and every audio read has a time bound; a stuck device
//!   ends in an error, not in a hang.
//! * **Chronological events.** [`MeasurementStore::apply`] expects events in
//!   chronological order.
//! * **Cancellation.** The sensor runner is meant to run to completion; see
//!   [`runtime`] for what can be dropped safely.
//! * **Noise level.** The microphone level is unweighted dB SPL computed from
//!   the nominal sensitivity of an ICS-43434: it is not dBA and not calibrated
//!   per device, see [`sensors::noise`].
//! * **SDS011 and noise coordination is not implemented.** The SDS011 fan is a
//!   noise source and the legacy firmware coordinated it with the noise
//!   measurement; here nothing does. [`runtime::SensorStates`] is a read-only
//!   view of every sensor state that a future policy can use without coupling
//!   the drivers.
//! * **Verification status.** The crate is covered by host tests
//!   (`cargo test -p rohi-hal`: runner timing and recovery, event consistency,
//!   the store, SDS011 sampling and aggregation, BMx280 address probing,
//!   freshness thresholds) and by doctests; the firmware that uses it builds
//!   for ESP32-C3 and ESP32-C6 in CI. **No sensor is recorded as
//!   hardware-verified** in the support matrix (`support-matrix/matrix.toml`);
//!   timing constants such as the SDS011 duty cycle are starting points not yet
//!   confirmed on a device.
//!
//! Further reading: the support matrix
//! (<https://rohi.akagi.dev/support/overview.html>), host testing
//! (<https://rohi.akagi.dev/validation/host-testing.html>), hardware testing
//! status (<https://rohi.akagi.dev/validation/hardware-testing.html>) and the
//! resource budget
//! (<https://rohi.akagi.dev/validation/resource-budget.html#sensor-runtime-rohi-hal>).

#[cfg(test)]
extern crate std;

pub mod device;
pub mod measurements;
pub mod runtime;
pub mod sensor;
mod sensors_service;
pub mod store;
pub mod time;
pub mod units;
pub mod uplink;

pub mod sensors;

#[cfg(test)]
mod testutil;

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
pub use sensors_service::SensorsService;
pub use store::MeasurementStore;
pub use time::MonotonicTimestamp;
pub use units::*;
pub use uplink::{ActiveUplink, UplinkMode, UplinkState};
