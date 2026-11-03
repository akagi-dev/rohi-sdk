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
#![no_std]

//! Altruist Urban firmware: the portable application and its ESP platform.
//!
//! ```text
//! sensor tasks -> SensorEvent -> EVENTS (bounded) -> store task -> MeasurementStore
//! ```
//!
//! Sensor tasks never touch the `MeasurementStore`; the store task is its
//! only owner. Other tasks read measurements on demand through
//! [`tasks::store::handle`], a bounded snapshot request to that owner.
//!
//! # Platforms
//!
//! The crate is the composition of the Altruist Urban from portable ROHI
//! services and has no hardware dependency of its own. A platform provides the
//! backends and the Embassy tasks around it (a task cannot be generic):
//!
//! ```text
//!                      altruist-urban (this crate, portable)
//!        config owner · bindings · control device · store · Wi-Fi manager
//!        connectivity · NTP time · identity · location · reboot
//!                              |
//!            +-----------------+------------------+
//!            |                                    |
//!   `esp` module (feature esp32c3 / esp32c6)   `altruist-sim` (host)
//!   flash, radio, BLE, sensors, resources      file flash, native Wi-Fi/IP,
//!                                              TCP control, simulated sensors
//! ```
//!
//! Without a chip feature only the portable part is built.

/// Firmware config bindings: product policy and runtime consumers.
pub mod bindings;
/// Persistent configuration service (single owner and handle).
pub mod config;
/// Telemetry uplink.
pub mod connectivity;
/// Local control plane composition.
pub mod control;
/// Device identity runtime.
pub mod identity;
/// The location of the device.
pub mod location;
/// Wi-Fi station runtime.
pub mod net;
/// Deferred device reboot.
pub mod reboot;
/// The sensor runtime.
pub mod tasks;
/// Synchronized wall-clock time.
pub mod time;

/// The ESP platform.
#[cfg(feature = "esp")]
pub mod esp;
