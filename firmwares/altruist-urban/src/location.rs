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
//! The location of the device.
//!
//! ```text
//! config owner task ──publish──► LOCATION ──► connectivity task (frames)
//!   (LocationConfig section)         │
//!                                    └────► `location` service (control plane)
//! ```
//!
//! Today the location is what the user stored in the `location` section; the
//! configuration owner task publishes it here at boot and after every commit
//! or removal, in the commit step like the other runtime sections (see
//! [`crate::config`]). A GNSS receiver will publish its fix to the same
//! [`LocationShared`].

use rohi_location::LocationShared;

use crate::tasks::RawMutex;

static LOCATION: LocationShared<RawMutex> = LocationShared::new();

/// The shared current location.
pub fn shared() -> &'static LocationShared<RawMutex> {
    &LOCATION
}
