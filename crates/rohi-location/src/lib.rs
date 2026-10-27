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

//! Where the device is.
//!
//! ```text
//!   user (config section `location`) ──┐
//!                                      ├─► LocationShared ──► LocationSource ──► telemetry
//!   GNSS receiver (future)  ───────────┘         │
//!                                                └─► LocationService (status, `rohi status location`)
//! ```
//!
//! Today the only source is the user: the stored [`LocationConfig`] section
//! (`rohi config set location latitude=... longitude=...`). A GNSS receiver
//! will be a second source: it publishes its fix with
//! [`LocationShared::publish_fix`] and **clears it when the fix is lost**, so
//! [`LocationShared::current`] prefers a live fix and falls back to the
//! configured position. Consumers never care where a location came from; they
//! see a [`Location`] and, if they want, its [`LocationSourceKind`].
//!
//! Coordinates are WGS84 decimal degrees. A location is optional: a device
//! that was never told where it is has none, and nothing invents one.

mod config;
mod service;
mod shared;

pub use config::{LocationConfig, field};
pub use service::{LocationService, SERVICE_ID, issue, source, status_field};
pub use shared::{Location, LocationShared, LocationSource, LocationSourceKind, NoLocation};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
