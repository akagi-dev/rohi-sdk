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

//! Platform-neutral flash storage primitives for ROHI.
//!
//! # Overview
//!
//! This `no_std` crate supplies shared flash access, checked region addressing,
//! and sector-based media. It does not allocate or prescribe a persistent format.
//!
//! # Quick Example
//!
//! ```
//! use rohi_storage::FlashRegion;
//! let region = FlashRegion::new(0x1000, 0x100);
//! assert_eq!(region.absolute(0x10, 4), Some(0x1010));
//! assert_eq!(region.absolute(0xff, 2), None);
//! ```
//!
//! # Core Concepts
//!
//! [`SharedFlash`] serializes operations on one flash device; [`FlashRegion`]
//! checks addresses within a window. [`Medium`] provides erased sectors and
//! byte-granularity access, implemented by [`FlashMedium`] and [`RamMedium`].
//!
//! # Architecture and Integration
//!
//! Configuration owns A/B slots and validation. Logging owns its segment
//! format, retention and recovery policy. Both can share one physical device
//! through [`SharedFlash`], while using separate bounded regions.
//!
//! # API Navigation
//!
//! | Item | Use |
//! |------|-----|
//! | [`FlashRegion`] | Check region-relative ranges |
//! | [`SharedFlash`] | Serialize access to a NOR driver |
//! | [`Medium`] | Access erasable sectors |
//! | [`FlashMedium`] | Adapt a NOR driver to a medium |
//! | [`RamMedium`] | Exercise medium operations and injected faults |
//!
//! # Guarantees and Limitations
//!
//! Shared handles lock for each driver operation, not a whole transaction.
//! Callers must assign non-overlapping regions and coordinate transactions on
//! the same region. Driver operations are blocking. The flash adapter uses a
//! fixed-size scratch buffer; the RAM medium stores its entire contents.
//! Hardware operation is not verified by this crate's host tests.

#![no_std]

pub mod crc;
mod flash;
mod medium;
mod region;
pub mod ring;
mod shared;

pub use crc::Crc32;
pub use flash::{FlashMedium, FlashMediumError};
pub use medium::{Medium, RamError, RamMedium, SECTOR_SIZE};
pub use region::FlashRegion;
pub use shared::{FlashCell, SharedFlash};
