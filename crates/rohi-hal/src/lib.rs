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

//! Robonomics Open Hardware abstraction layer.
//!
//! This crate introduces Rust support for devices designed and assembled as
//! part of Robonomics Open Hardware Initiative (ROHI). Details available at
//! https://robonomics.network/devices/
//!
//! Sensor drivers publish typed `rohi-sensor-core` events and know nothing
//! about transports. Everything except [`board`] and [`esp`] is hardware
//! independent and testable on a regular host; those two require the `esp`
//! feature.

#[cfg(test)]
extern crate std;

/// Hardware-independent sensor runtime.
pub mod runtime;

/// Sensor drivers.
pub mod sensors;

/// Robonomics Open Hardware devices collection.
#[cfg(feature = "esp")]
pub mod board;

/// ESP32 peripheral glue for the hardware-independent drivers.
#[cfg(feature = "esp")]
pub mod esp;

#[cfg(test)]
mod testutil;
