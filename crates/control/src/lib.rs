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

//! Transport- and platform-independent ROHI control dispatcher.
//!
//! ```text
//! request bytes -> Controller -> ControlBackend (firmware)
//!                     |           config, status and action registries
//! response bytes <----+
//! ```
//!
//! [`Controller::handle`] decodes one protobuf [`Request`](rohi_control_proto::control::Request),
//! checks the [`AuthorizationPolicy`], serves it from the registries of
//! [`rohi_service`] and encodes exactly one response. It never allocates,
//! never panics on malformed input and reports every failure as a typed
//! protocol error.
//!
//! The crate knows no device module (Wi-Fi, sensors, telemetry, ...): config
//! sections, services and actions are discovered through the registries, and
//! it knows no transport (BLE) or platform (ESP).

#[cfg(test)]
extern crate std;

mod backend;
mod controller;
mod mapping;
mod policy;

#[cfg(test)]
mod tests;

pub use backend::{ControlBackend, Device, DeviceInfo};
pub use controller::Controller;
pub use policy::{
    AuthorizationPolicy, LinkSecurity, OpenPolicy, Privilege, RequireAuthenticated,
    RequireEncryption,
};
