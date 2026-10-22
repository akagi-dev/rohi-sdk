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

//! ROHI control protocol over BLE.
//!
//! Portable binding between a [`BleTransport`](rohi_net::BleTransport) and the
//! [`rohi_control::Controller`]:
//!
//! - [`uuid`]: the ROHI Control Service and its RX/TX characteristics;
//! - [`frame`]: request/response framing with fragmentation and reassembly,
//!   independent of protobuf and bounded at compile time;
//! - [`ControlBle`]: the control loop (receive, reassemble, dispatch,
//!   fragment, send) and disconnect cleanup.
//!
//! The crate contains no platform code. The ESP GATT server lives in
//! `rohi-net-esp` and only exposes the generic transport.

#[cfg(test)]
extern crate std;

pub mod frame;
mod service;
pub mod uuid;

#[cfg(test)]
mod tests;

pub use service::ControlBle;
