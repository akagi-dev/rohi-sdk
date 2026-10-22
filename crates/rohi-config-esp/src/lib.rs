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

//! ESP32 flash backend for [`rohi-config`](rohi_config).
//!
//! [`EspConfigStorage`] implements [`ConfigStorage`](rohi_config::ConfigStorage)
//! on top of an `embedded-storage` NOR flash restricted to an explicit
//! [`FlashRegion`]. It handles bounds checking, erase-sector and write-word
//! alignment and maps platform failures to [`EspStorageError`]; it knows
//! nothing about sections, CBOR documents, CRCs or generations.
//!
//! With the `esp` feature the [`esp`] module resolves the region from the
//! `rohi-cfg` data partition of the device partition table and provides the
//! ESP32-C3 flash driver constructor.
//!
//! ```text
//! ESP32 flash
//! +----------------------------+
//! | bootloader / firmware / ...|
//! +----------------------------+
//! | rohi-cfg partition         |  <- FlashRegion
//! |   slot A | slot B          |     (see `layout`)
//! +----------------------------+
//! ```

#[cfg(test)]
extern crate std;

mod error;
#[cfg(feature = "esp")]
pub mod esp;
pub mod layout;
mod region;
mod storage;

pub use error::EspStorageError;
pub use region::FlashRegion;
pub use storage::EspConfigStorage;
