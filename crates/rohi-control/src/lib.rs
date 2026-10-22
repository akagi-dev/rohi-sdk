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

//! Platform-independent ROHI control dispatcher.
//!
//! ```text
//! request bytes -> Controller -> ControlServices (firmware adapter)
//!                     |              measurements / config / Wi-Fi
//! response bytes <----+
//! ```
//!
//! [`Controller::handle`] decodes one protobuf [`Request`](rohi_control_proto::control::Request),
//! checks capabilities and the [`AuthorizationPolicy`], calls the
//! [`ControlServices`] and encodes exactly one response. It never allocates,
//! never panics on malformed input and reports every failure as a typed
//! protocol error. The crate knows nothing about transports (BLE) or
//! platforms (ESP): those live behind [`ControlServices`] and in the
//! transport crates.

#[cfg(test)]
extern crate std;

mod capabilities;
mod controller;
mod mapping;
mod policy;
mod services;

#[cfg(test)]
mod tests;

pub use capabilities::Capabilities;
pub use controller::Controller;
pub use policy::{
    AuthorizationPolicy, LinkSecurity, OpenPolicy, Privilege, RequireAuthenticated,
    RequireEncryption,
};
pub use services::{ControlServices, DeviceInfo, ServiceError};
