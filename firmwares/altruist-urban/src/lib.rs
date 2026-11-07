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
#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "firmware must not panic: handle the error, see AGENTS.md"
    )
)]

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
//!        connectivity · NTP time · identity · location · reboot · logging
//!                              |
//!            +-----------------+------------------+
//!            |                                    |
//!   `esp` module (feature esp32c3 / esp32c6)   `host` module (feature std)
//!   flash, radio, BLE, sensors, resources      file flash, simulated Wi-Fi, host IP,
//!                                              TCP and BlueZ BLE control, simulated sensors
//! ```
//!
//! Without a chip feature only the portable part is built.

pub mod bindings;
pub mod config;
pub mod connectivity;
pub mod control;
pub mod identity;
pub mod location;
pub mod logging;
pub mod net;
pub mod reboot;
pub mod tasks;
pub mod time;

#[cfg(feature = "esp")]
pub mod esp;

#[cfg(feature = "std")]
pub mod host;
