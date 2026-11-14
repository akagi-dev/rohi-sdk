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
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented
    )
)]

//! Robonomics Open Hardware Interface (ROHI): transport-independent telemetry.
//!
//! `rohi-telemetry` owns what telemetry **contains**: collecting observations
//! from any number of independent sources, the stable measurement catalog,
//! persistent **packet profiles** and the construction of a **public section**
//! and recipient-**private sections** from one snapshot. It knows no route,
//! endpoint, transport, protobuf type or physical driver; a
//! [`PacketFormat`] implemented elsewhere (`rohi-telemetry-robonomics`)
//! encodes, encrypts and signs the sections into an immutable
//! [`SignedPacket`].
//!
//! ```text
//! sources -> capture() -> TelemetrySnapshot -> prepare(profile) -> PreparedSections
//!                                                   -> PacketFormat::seal -> SignedPacket
//! ```
//!
//! # Quick example
//!
//! Two software sources, one capture, one profile: the public section of the
//! profile holds the selected measurements of the snapshot, nothing else.
//!
//! ```
//! use core::cell::Cell;
//!
//! use embassy_futures::block_on;
//! use embedded_hal_async::delay::DelayNs;
//! use rohi_telemetry::catalog::{BMX280_TEMPERATURE, SDS011_PM10};
//! use rohi_telemetry::{
//!     DataSelection, FormatCapabilities, PacketFormatId, RecipientAddress, Selection, SourceId,
//!     StaticSource, TelemetryConfig, TelemetrySnapshot, capture, prepare,
//! };
//! use rohi_time::{Clock, MonotonicTimestamp};
//!
//! struct Uptime(Cell<u64>);
//! impl Clock for Uptime {
//!     fn now(&self) -> MonotonicTimestamp {
//!         MonotonicTimestamp::from_millis(self.0.get())
//!     }
//! }
//! struct NoDelay;
//! impl DelayNs for NoDelay {
//!     async fn delay_ns(&mut self, _ns: u32) {}
//! }
//! // What a packet format can encode; a real one comes from the format crate.
//! struct AnyFormat;
//! impl FormatCapabilities for AnyFormat {
//!     fn supported(&self, _: PacketFormatId) -> Selection {
//!         rohi_telemetry::catalog::all_cataloged()
//!     }
//!     fn accepts_recipient(&self, _: PacketFormatId, _: &RecipientAddress) -> bool {
//!         true
//!     }
//! }
//!
//! let at = MonotonicTimestamp::from_millis(900);
//! let weather = [(BMX280_TEMPERATURE, 21.5, at)];
//! let dust = [(SDS011_PM10, 7.0, at)];
//! let mut sources = (
//!     StaticSource::new(SourceId(10), &weather),
//!     StaticSource::new(SourceId(11), &dust),
//! );
//! let snapshot: TelemetrySnapshot<8> =
//!     block_on(capture(&mut sources, &Uptime(Cell::new(1_000)), &mut NoDelay, 100));
//!
//! let mut config = TelemetryConfig::<3, 2>::default();
//! config.add_profile("community", PacketFormatId::Robonomics).unwrap();
//! config
//!     .set_public(
//!         "community",
//!         DataSelection::local(Selection::NONE.with(SDS011_PM10)),
//!         &AnyFormat,
//!     )
//!     .unwrap();
//!
//! let profile = config.get("community").unwrap();
//! let sections = prepare(&snapshot, profile, &rohi_telemetry::catalog::all_cataloged()).unwrap();
//! let public: Vec<_> = sections.public.observations().collect();
//! assert_eq!(public.len(), 1);
//! assert_eq!(public[0].source, SourceId(11));
//! ```
//!
//! # Concepts
//!
//! * **Source and measurement identity.** An [`Observation`] is keyed by
//!   `(SourceId, MeasurementId)`: equal quantities of two sensors are two
//!   observations and provenance is never lost. Each observation has its own
//!   acquisition time; the snapshot's `captured_at` is only the capture
//!   boundary. Nothing is averaged, merged or overwritten; a duplicate key
//!   discards the contribution of the offending source.
//! * **Best effort across sources, atomic per source.** [`capture`] bounds
//!   every [`TelemetrySource`] in time; a failing or slow source is reported in
//!   the [`CollectionDiagnostics`] and skipped, a source's observations are
//!   all kept or all discarded.
//! * **Catalog and selections.** The [`catalog`] fixes names, units and
//!   stable bit indices. A [`Selection`] is a 32-byte bitset (`none`, dynamic
//!   `all`, or explicit bits); `all` never includes `location`.
//! * **Packet profiles.** A [`PacketProfile`] is a format, a public selection
//!   and zero or more private sections, one per recipient. A new profile
//!   discloses nothing. See [`profile`] for generations, incarnations and
//!   revisions.
//! * **Capture once, build many.** [`prepare`] applies a profile to an
//!   immutable snapshot without reading anything again and without copying
//!   observations.
//!
//! # Features
//!
//! `sensors` adds `adapters::SensorsTelemetrySource` over `rohi-sensors`, `location`
//! adds `adapters::LocationTelemetrySource` over `rohi-location`. Neither pulls a
//! driver, and the core works with neither.
//!
//! # Guarantees & limitations
//!
//! `no_std`, heapless, statically bounded. No UTC is read: a format that
//! needs wall-clock time decides how to obtain it and fails with
//! [`SealError::TimeUnavailable`] rather than invent one. Remote peers are not
//! sources in v1 and are never relabelled as local.

#[cfg(test)]
extern crate std;

pub mod adapters;
pub mod catalog;
#[cfg(feature = "service")]
pub mod managed;
pub mod observation;
pub mod packet;
pub mod profile;
pub mod section;
pub mod selection;
pub mod snapshot;
pub mod source;

pub use catalog::{CATALOG, MeasurementDescriptor, Unit, ValueKind};
pub use observation::{GeoPoint, MeasurementId, Observation, ObservationValue, SourceId};
pub use packet::{Capabilities, PacketFormat, PacketInfo, SealError, SignedPacket};
pub use profile::{
    FormatCapabilities, PROFILE_ID_LEN, PacketFormatId, PacketProfile, PacketProfileId,
    PrivateSectionPolicy, ProfileError, RecipientAddress, TelemetryConfig,
};
pub use section::{
    PrepareError, PreparedSections, PrivateSectionView, ProfileStamp, SectionView, prepare,
};
pub use selection::{DataSelection, Selection, SelectionError, SourceSelection};
pub use snapshot::{
    CollectionDiagnostics, SourceFailure, SourceOutcome, SourceReport, SourceWriter,
    TelemetrySnapshot, WriteError,
};
pub use source::{SourceError, SourceSet, StaticSource, TelemetrySource, capture, collect_one};
