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

//! ESP32 Wi-Fi backend of [`rohi-net`](rohi_net).
//!
//! Implements [`WifiBackend`](rohi_net::WifiBackend) on top of `esp-radio` and
//! `embassy-net`; vendor types never leave this crate. The chip is selected by
//! exactly one Cargo feature, `esp32c3` or `esp32c6`, forwarded by the final
//! firmware. Without a chip feature the crate is empty, so it stays buildable
//! on a regular host.

#[cfg(all(feature = "esp32c3", feature = "esp32c6"))]
compile_error!(
    "select exactly one ESP target: features `esp32c3` and `esp32c6` are mutually exclusive"
);

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod wifi;

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use wifi::{EspWifi, NetRunner, RADIO_HEAP_SIZE, init_radio_heap, radio_heap_free};
