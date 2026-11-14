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

//! The Robonomics Connectivity Protocol as a packet format of ROHI telemetry.
//!
//! `rohi-telemetry-robonomics` implements [`rohi_telemetry::PacketFormat`]: it
//! maps the public and private sections of a profile to the protocol's
//! `device.v1.Urban` message, encrypts every private section to its recipient
//! and signs the complete `core.v1.Message` into a `crypto.v1.SignedEnvelope`.
//! The vendored schema, the bounded `micropb` bindings and the wire helpers
//! are in [`wire`].

#[cfg(test)]
extern crate std;

pub mod crypto;
pub mod encoder;
pub mod format;
pub mod wire;

pub use format::{ClockError, DeviceKey, Entropy, EntropyError, RobonomicsFormat, UtcClock};
