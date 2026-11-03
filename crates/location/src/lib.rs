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

//! Where the device is.
//!
//! # Overview
//!
//! `rohi-location` answers one question for the rest of the firmware: **where
//! is the device?** Today the answer is what the user stored in the `location`
//! config section; a GNSS receiver can publish a measured fix through the same
//! seam later (there is no GNSS runtime in this repository yet). Telemetry puts
//! the answer into every frame.
//!
//! A location is **optional**. A device that was never told where it is has
//! none; nothing invents one, and nothing substitutes `0, 0`. Coordinates are
//! WGS84 decimal degrees.
//!
//! The crate is `no_std`, does not allocate and has no Cargo features. It
//! depends on [`rohi_config`] and [`rohi_service`] and on `embassy-sync` for
//! the generic mutex that guards [`LocationShared`].
//!
//! # Quick Example
//!
//! A configured position, a GNSS fix that overrides it, and the fall back when
//! the fix is lost. The type parameter of [`LocationShared`] is the
//! [`RawMutex`] of the firmware; the host example uses `NoopRawMutex`.
//!
//! ```
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use rohi_location::{
//!     Location, LocationConfig, LocationShared, LocationSource, LocationSourceKind,
//! };
//!
//! let shared = LocationShared::<NoopRawMutex>::new();
//! assert!(shared.location().is_none()); // nothing is known yet
//!
//! // What the configuration owner does after loading or committing the section.
//! let configured = LocationConfig {
//!     latitude: 52.52,
//!     longitude: 13.405,
//!     height_m: Some(34.0),
//! };
//! shared.publish_configured(Some(&configured));
//! let here = shared.current().unwrap();
//! assert_eq!(here.source, LocationSourceKind::Configured);
//!
//! // A GNSS runtime publishes a fix; it wins while it is published.
//! shared.publish_fix(Some(Location {
//!     latitude: 52.5201,
//!     longitude: 13.4051,
//!     height_m: None,
//!     source: LocationSourceKind::Gps,
//! }));
//! assert_eq!(shared.current().unwrap().source, LocationSourceKind::Gps);
//!
//! // The receiver lost the fix: back to the configured position.
//! shared.publish_fix(None);
//! assert_eq!(shared.current(), Some(here));
//! ```
//!
//! # Core Concepts
//!
//! * **Configured location**: the user's `location` section,
//!   [`LocationConfig`], with its persisted overrides [`LocationOverrides`]
//!   (fields, ranges, validation and persistent ABI are on
//!   [`LocationConfig`]).
//! * **Fix**: a position measured by a GNSS receiver, published with
//!   [`LocationShared::publish_fix`].
//! * **Current location**: [`LocationShared::current`] returns the fix while
//!   there is one, otherwise the configured location, otherwise `None`.
//! * **Consumers** read a [`Location`] through the [`LocationSource`] trait and
//!   never depend on the provider. [`NoLocation`] is a source that never
//!   knows.
//!
//! ## Sources and priority
//!
//! | Source | Published by | When it wins |
//! |--------|--------------|--------------|
//! | `gps` ([`LocationSourceKind::Gps`]) | a GNSS runtime with [`LocationShared::publish_fix`]`(Some(..))` | while it publishes a fix |
//! | `configured` ([`LocationSourceKind::Configured`]) | the configuration owner with [`LocationShared::publish_configured`] | when there is no fix |
//!
//! There is no expiry in this crate: the GNSS runtime knows what a stale fix
//! is and must publish `None` as soon as the fix is lost, which makes the
//! device fall back to the configured position. The priority rule is covered
//! by host unit tests; no GNSS receiver is supported yet.
//!
//! ## Setting the location
//!
//! The location is an ordinary config section called `location`:
//!
//! ```text
//! $ rohi config location latitude=52.52 longitude=13.405 height_m=34
//! latitude   52.52 deg
//! longitude  13.405 deg
//! height_m   34 m
//!
//! $ rohi config location                    # the effective configuration; nothing set shows <unset>
//! latitude   52.52 deg
//! longitude  13.405 deg
//! height_m   34 m
//!
//! $ rohi status location
//! Service:  location
//! State:    running
//! Health:   ok
//! Ready:    yes
//! Since:    3s
//!
//! latitude   52.52 deg
//! longitude  13.405 deg
//! height_m   34 m
//! source     configured
//! ```
//!
//! The change takes effect at once, without a restart: the firmware's
//! configuration owner task hands every commit or removal to
//! [`LocationShared::publish_configured`] in the commit step, like the other
//! runtime sections ([durable
//! mutation](../rohi_sdk/cancellation/index.html#durable-mutation)). A request
//! cancelled before it was queued changed nothing; cancelled afterwards, the
//! operation is logically completed and the shared location agrees with the
//! stored section.
//!
//! # Architecture & Integration
//!
//! ```text
//!   user (config section `location`) ──┐
//!                                      ├─► LocationShared ──► LocationSource ──► telemetry frames (GPS entry)
//!   GNSS receiver (future)  ───────────┘         │
//!                                                └─► LocationService (status, `rohi status location`)
//! ```
//!
//! | Neighbour | Relation |
//! |-----------|----------|
//! | [`rohi_config`], [`rohi_service`] | [`LocationConfig`] is a [`rohi_service::Config`] section; [`LocationService`] is a status-plane [`rohi_service::Service`]. |
//! | [`rohi-connectivity`](../rohi_connectivity/index.html) | Reads the location through [`LocationSource`] for every telemetry frame. |
//! | Altruist Urban firmware | Owns one `static` [`LocationShared`] (module `location`); the configuration owner publishes the stored section at boot and after every commit (module `config`), the connectivity task reads it, the control registries list the section and the service (module `control`). |
//!
//! Adding a GNSS source means publishing its fix to the same
//! [`LocationShared`].
//!
//! ## Telemetry
//!
//! `rohi-connectivity` reads the location when it creates a frame and adds the
//! `sensor.v1.GPS` entry as the **first** entry of `Urban.public`: WGS84
//! degrees, height `0` when the height is not known (as the schema defines).
//!
//! * The location is **context, not a measurement**. It is not counted as
//!   emitted, and a snapshot without a valid measurement is still not sent,
//!   even if a location is known.
//! * The location is public, **unencrypted** telemetry. Anyone who can read the
//!   frames sees where the device is. Remove the overrides
//!   (`rohi config reset location`) to stop sending it.
//! * Without a location the frame is still sent, without the entry, and counted
//!   in `frames_without_location` of the connectivity diagnostics
//!   (`rohi status connectivity`).
//! * A position that is not a finite number is left out and counted as out of
//!   range by the encoder.
//!
//! ## Connectivity is degraded without a location
//!
//! Telemetry without a position is incomplete, so while telemetry is running
//! and the device has no location, the `connectivity` service reports:
//!
//! ```text
//! $ rohi status connectivity
//! Service:  connectivity
//! State:    running
//! Health:   degraded
//! Ready:    yes
//! ...
//! Issues:
//!   WARNING  no_location  (12s)
//!            No location is set; telemetry is sent without it
//! ```
//!
//! `ready` stays `yes` because frames are delivered. The device health follows
//! the default policy and becomes `degraded`; `rohi status` shows it. Disabled
//! telemetry is not degraded by a missing location. The fix is one command:
//! `rohi config location latitude=.. longitude=..`.
//!
//! # API Navigation
//!
//! Everything is re-exported at the root; the only public modules are the
//! constant modules [`issue`], [`source`] and [`status_field`].
//!
//! | Item | Role |
//! |------|------|
//! | [`LocationConfig`], [`LocationOverrides`] | The `location` config section and its persisted overrides: fields, ranges, validation and persistent ABI. |
//! | [`LocationShared`] | The shared current location with its two sources and their priority. |
//! | [`Location`], [`LocationSourceKind`] | A position and who says so. |
//! | [`LocationSource`], [`NoLocation`] | What consumers read; a source that never knows. |
//! | [`LocationService`], [`issue`], [`source`], [`status_field`] | The `location` service of the status plane and its issue, enum and field ids. |
//!
//! The persistent ABI is described on [`LocationConfig`] and in the
//! [persistent ABI
//! reference](https://docs.rs/rohi-config). Services,
//! config sections and the status plane are described in [`rohi_service`].
//!
//! # Guarantees & Limitations
//!
//! * **Validation**: [`LocationShared::publish_configured`] treats an invalid
//!   section (non-finite or out of range) as removed: a wrong position is
//!   worse than none. Ranges: latitude -90…90, longitude -180…180, height
//!   -1000…10000 m.
//! * **No allocation, no waiting**: [`LocationShared`] is a small `Copy` state
//!   behind a [blocking mutex](embassy_sync::blocking_mutex::Mutex); reading
//!   and publishing are short critical sections and nothing awaits.
//! * **Accuracy**: the control plane carries `f32` (about one to two metres);
//!   the stored overrides and [`Location`] are `f64`.
//! * **Persistent ABI**: section `LocationConfig` version 1; the section ID,
//!   the CBOR map and the status field and issue ids are pinned by tests.
//! * **Privacy**: a published location is sent unencrypted in telemetry.
//! * **Validation level**: host-tested only (`cargo test -p rohi-location`).
//!   There is no GNSS hardware support and nothing in this crate is
//!   hardware-specific; see the [support
//!   matrix](https://rohi.akagi.dev/support/matrix.html).
//!
//! [`RawMutex`]: embassy_sync::blocking_mutex::raw::RawMutex

mod config;
mod service;
mod shared;

pub use config::{LocationConfig, LocationOverrides};
pub use service::{LocationService, issue, source, status_field};
pub use shared::{Location, LocationShared, LocationSource, LocationSourceKind, NoLocation};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
