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

//! The ESP platform of the Altruist Urban: the backends, resource budget and
//! Embassy tasks around the portable application of this crate.
//!
//! Everything here needs a chip feature (`esp32c3` or `esp32c6`); the rest of
//! the crate builds without one and runs on the host simulator too.

#[cfg(all(feature = "esp32c3", feature = "esp32c6"))]
compile_error!(
    "select exactly one ESP target: enable only one of the `esp32c3` and `esp32c6` features"
);

#[cfg(not(any(feature = "esp32c3", feature = "esp32c6")))]
compile_error!(
    "select an ESP target: enable the `esp32c3` or `esp32c6` feature (use `cargo check-esp32c3` / `cargo check-esp32c6`)"
);

pub mod config;
pub mod connectivity;
pub mod control;
pub mod net;
pub mod reboot;
pub mod resources;
pub mod sensors;
pub mod store;
pub mod time;
