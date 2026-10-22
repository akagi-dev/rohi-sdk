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

//! Altruist Urban firmware: sensor runtime.
//!
//! ```text
//! sensor tasks -> SensorEvent -> EVENTS (bounded) -> store task -> MeasurementStore
//! ```
//!
//! Sensor tasks never touch the `MeasurementStore`; the store task is its
//! only owner.

#[cfg(all(feature = "esp32c3", feature = "esp32c6"))]
compile_error!(
    "select exactly one ESP target: features `esp32c3` and `esp32c6` are mutually exclusive"
);

#[cfg(not(any(feature = "esp32c3", feature = "esp32c6")))]
compile_error!(
    "select an ESP target: use `cargo check-esp32c3` / `cargo check-esp32c6` or enable feature `esp32c3` / `esp32c6`"
);

/// Persistent configuration service (single owner task and handle).
pub mod config;
pub mod net;

/// Embassy tasks.
pub mod tasks;
