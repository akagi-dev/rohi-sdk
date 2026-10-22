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

//! ESP32 platform glue of `rohi-hal`: peripheral drivers for the
//! hardware-independent sensor drivers and the board support of ROHI devices.
//!
//! The chip is selected by exactly one Cargo feature, `esp32c3` or
//! `esp32c6`. Without a chip feature the crate is empty, so it stays buildable
//! on a regular host together with the portable crates. The firmware is the
//! final chip selector and forwards its own feature here.

#[cfg(all(feature = "esp32c3", feature = "esp32c6"))]
compile_error!(
    "select exactly one ESP target: features `esp32c3` and `esp32c6` are mutually exclusive"
);

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod audio;
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod clock;

/// Robonomics Open Hardware devices collection.
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub mod board;

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use audio::{Error as MicrophoneError, I2sMicrophone};
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use clock::EmbassyClock;
