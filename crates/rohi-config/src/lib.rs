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

//! Extensible, transactional configuration store for Robonomics Open
//! Hardware devices.
//!
//! Configuration is a set of independent typed *sections*, each defined by the
//! feature that owns it. There is no central device configuration struct:
//!
//! ```
//! use rohi_config::{ConfigSection, ConfigStore, MemoryStorage};
//! use serde::{Deserialize, Serialize};
//! use postcard::experimental::max_size::MaxSize;
//!
//! #[derive(Debug, PartialEq, Serialize, Deserialize, MaxSize, ConfigSection)]
//! #[config(name = "example", version = 1)]
//! struct ExampleConfig {
//!     value: u32,
//! }
//!
//! let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
//! store.set(&ExampleConfig { value: 42 }).unwrap();
//! assert_eq!(
//!     store.get::<ExampleConfig>().unwrap(),
//!     Some(ExampleConfig { value: 42 })
//! );
//! ```
//!
//! Section payloads use [postcard](https://docs.rs/postcard); bounded size is
//! enforced through [`postcard::experimental::max_size::MaxSize`]. Storage is
//! abstracted by [`ConfigStorage`] ([`MemoryStorage`] on the host, a flash
//! backend on devices). Updates are A/B transactions; see [`ConfigStore`].
//!
//! The on-flash container format and section ID hash are a persistent ABI,
//! documented in the `config-store` chapter of the SDK book.

#[cfg(test)]
extern crate std;

// Lets the derive macro refer to this crate as `::rohi_config` in its own tests.
extern crate self as rohi_config;

mod crc;
mod error;
mod format;
mod id;
mod memory;
mod output;
mod section;
mod storage;
mod store;

pub use crc::Crc32;
pub use error::{ConfigError, SectionError};
pub use format::{
    COMMIT_MARKER, FORMAT_VERSION, MAX_SECTION_PAYLOAD, MAX_SECTIONS, MAX_SLOT_SIZE,
    SECTION_HEADER_SIZE, SLOT_HEADER_SIZE, SLOT_MAGIC, SectionHeader,
};
pub use id::SectionId;
pub use memory::{MemoryError, MemoryStorage};
pub use output::{decode_all, encode_to_slice};
pub use section::ConfigSection;
pub use storage::ConfigStorage;
pub use store::{ConfigStore, SectionInfo};

/// Derive macro generating the [`ConfigSection`] implementation.
pub use rohi_config_derive::ConfigSection;

pub use postcard;
pub use serde;
