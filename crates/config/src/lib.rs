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
//! Hardware independent, transactional configuration store of the Robonomics
//! Open Hardware Interface (ROHI) SDK.
//!
//! # Overview
//!
//! Configuration is a set of **independent typed sections**, each defined by
//! the feature crate that owns it; there is no central `DeviceConfig`. This
//! crate persists the sections as CBOR in two A/B flash slots without knowing
//! anything about them, so that after a power loss at any point either the
//! previous or the new configuration is loadable. Use it to define a persisted
//! section ([`ConfigSection`]), to split a user configurable section into its
//! effective value and persisted overrides ([`Override`], [`ConfigOverrides`],
//! [`materialize`]), and to store sections ([`ConfigStore`] over a
//! [`ConfigStorage`]).
//!
//! The store only persists bytes. How users see and change a section
//! (`rohi config`, atomic patches of overrides, secret fields) is a separate,
//! generic layer in [`rohi_service`](../rohi_service/index.html), which this
//! crate does not depend on.
//!
//! # Quick example
//!
//! A hand written section is a `minicbor` type with `#[derive(ConfigSection)]`
//! (this is how `Identity` of [`rohi_identity`](../rohi_identity/index.html) is
//! defined). The store is generic over the storage; [`MemoryStorage`] emulates
//! flash on the host:
//!
//! ```
//! use minicbor::{CborLen, Decode, Encode};
//! use rohi_config::{ConfigSection, ConfigStore, MemoryStorage};
//!
//! #[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
//! #[cbor(array)]
//! #[config(version = 1)]
//! struct ExampleConfig {
//!     #[n(0)]
//!     value: u32,
//! }
//!
//! let mut store = ConfigStore::open(MemoryStorage::<16384>::new()).unwrap();
//! store.set(&ExampleConfig { value: 42 }).unwrap();
//! assert_eq!(
//!     store.get::<ExampleConfig>().unwrap(),
//!     Some(ExampleConfig { value: 42 })
//! );
//!
//! // The flash content survives a reboot: reopen the same storage.
//! let mut store = ConfigStore::open(store.into_storage()).unwrap();
//! assert_eq!(store.get::<ExampleConfig>().unwrap().unwrap().value, 42);
//!
//! // `remove` deletes one section, `clear` all of them.
//! store.remove::<ExampleConfig>().unwrap();
//! assert_eq!(store.get::<ExampleConfig>().unwrap(), None);
//! ```
//!
//! The caller never handles section IDs, record framing, CRCs, slots or
//! generations. A user configurable section is split into the effective typed
//! config and its persisted overrides, see [`ConfigOverrides`].
//!
//! # Core concepts
//!
//! - **Section.** A typed value with a stable [`SectionId`] and a schema
//!   `VERSION`, see [`ConfigSection`]. The ID is the FNV-1a 64-bit hash of
//!   the Rust struct name unless an explicit `id` is given, so renaming a
//!   persisted type is a storage ABI change. Its payload is a `minicbor`
//!   document whose size is taken from [`minicbor::CborLen`]; there is no
//!   per-section size declaration.
//! - **Store.** [`ConfigStore`] keeps at most [`MAX_SECTIONS`] sections of at
//!   most [`MAX_SECTION_PAYLOAD`] encoded bytes each in two slots of at most
//!   [`MAX_SLOT_SIZE`] bytes. Every update writes the whole new document into
//!   the inactive slot, verifies it and programs the commit word last.
//!   Sections with unknown IDs (for example of an older firmware) are copied
//!   byte for byte. Corrupt storage is never reported as empty storage.
//! - **Overrides.** A managed section is persisted as the fields the user set
//!   explicitly. The effective configuration is `C::default()`, then the
//!   firmware overrides, then the user overrides, see [`materialize`].
//! - **Codecs.** [`Codec`], [`Standard`], [`Text`] and [`Variant`] convert
//!   typed fields to the generic [`Value`] that the control plane uses;
//!   [`cbor`] holds bounded `heapless` codecs for persisted types.
//!
//! # Architecture and integration
//!
//! ```text
//! feature crate
//!     │ typed section ─ #[derive(ConfigSection)] or #[derive(Config)] (overrides)
//!     ▼
//! minicbor payload ─► SectionRecord [id, version, payload]
//!     ▼
//! ConfigPayload { format, generation, records } ─► SlotEnvelope { payload, CRC-32 } + commit word
//!     ▼
//! ConfigStore: A/B flash transaction over a ConfigStorage
//!     ├─ MemoryStorage      host tests and examples (this crate)
//!     └─ EspConfigStorage   ESP32 flash region (rohi-config-esp)
//! ```
//!
//! | Crate | Responsibility |
//! |-------|----------------|
//! | `rohi-config` (this crate) | sections, overrides, codecs, [`ConfigStore`], [`ConfigStorage`], [`MemoryStorage`], the slot format |
//! | [`rohi_config_derive`] | `#[derive(ConfigSection)]` and `#[derive(Config)]` (overrides and the `rohi_service::Config` implementation) |
//! | [`rohi_config_esp`](../rohi_config_esp/index.html) | ESP32 flash backend (`EspConfigStorage`), bounded flash region, flash layout, and the firmware task that owns the store at runtime |
//! | [`rohi_service`](../rohi_service/index.html) | the generic section layer: patches, secrets, `Section`, registries; consumes [`ConfigOverrides`] and [`HasOverrides`] |
//!
//! Device identity is itself a section of this store (see
//! [`rohi_identity`](../rohi_identity/index.html)). The store is a plain,
//! synchronous value; the firmware that owns it at runtime serializes access
//! through a configuration task, described in the
//! [`rohi_config_esp`](../rohi_config_esp/index.html) crate documentation.
//!
//! # API navigation
//!
//! | Item | Role |
//! |------|------|
//! | [`ConfigStore`] | the A/B transactional store: `open`, `try_open`, `format`, `get`, `set`, `remove`, `clear`, raw access, diagnostics |
//! | [`ConfigOpenState`], [`OpenFailure`] | how the flash looked at open, and the storage handed back by a failed open |
//! | [`ConfigError`], [`SectionError`], [`Corruption`] | error model, with the full table on [`ConfigError`] |
//! | [`ConfigSection`], [`SectionId`] | a typed persisted section and its stable 64-bit ID |
//! | [`Override`], [`ConfigOverrides`], [`HasOverrides`], [`materialize`] | explicit per-field overrides and the effective configuration |
//! | [`ConfigStorage`], [`MemoryStorage`] | the NOR flash abstraction and its host emulation with fault injection |
//! | [`Codec`], [`Standard`], [`Text`], [`Variant`], [`Value`], [`FieldError`] | field conversion used by managed configs |
//! | [`decode_exact`], [`encode_to_slice`], [`cbor`] | CBOR helpers for section payloads |
//! | [`COMMIT_WORD`], [`FORMAT_VERSION`], [`MAX_SECTIONS`], [`MAX_SECTION_PAYLOAD`], [`MAX_SLOT_SIZE`] | container format constants and limits |
//!
//! # Guarantees and limitations
//!
//! - **Power loss.** An update never modifies the active slot; either the
//!   previous or the completely committed new configuration is loadable. The
//!   host tests inject write and erase faults at every byte position with
//!   [`MemoryStorage`]. Whether a particular flash chip honours the NOR
//!   programming model that [`ConfigStorage`] requires is not checked here.
//! - **Persistent ABI.** Field numbers, the explicit `#[cbor(array)]` /
//!   `#[cbor(map)]` representation, strict decode, section IDs and versions,
//!   the container format and unknown-section preservation are persistent ABI,
//!   documented in this crate and in the
//!   [`rohi-sdk` versioning rules](https://docs.rs/rohi-sdk/latest/rohi_sdk/versioning/index.html);
//!   the recipe is the
//!   [add a ConfigSection guide](https://rohi.akagi.dev/guides/add-config-section.html).
//!   Decoding is strict: trailing bytes are an error
//!   ([`SectionError::TrailingBytes`]), and a stored version other than
//!   `VERSION` is rejected unless the section implements a migration.
//! - **Resources.** No heap and no dynamic CBOR tree. A commit streams CBOR
//!   into flash and copies unrelated records in 64 byte chunks, so there is
//!   never a second copy of the configuration in RAM; [`ConfigStore::get`]
//!   decodes through a [`MAX_SECTION_PAYLOAD`] byte stack buffer. The flash
//!   region and the other limits are the constants of this crate and of
//!   `rohi-config-esp`, see also the
//!   [`rohi-sdk` memory model](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html).
//! - **Geometry.** The storage must have a power-of-two erase size and a write
//!   size of 1, 2 or 4 bytes, and hold two slots of at least 64 bytes;
//!   otherwise [`ConfigError::InvalidStorage`] is returned.
//! - **Secrets.** Secret fields are stored as ordinary CBOR in the section;
//!   this crate does not encrypt flash.
//! - **Scope.** The format is CBOR only. Control over BLE or Wi-Fi, OTA and
//!   JSON configuration are not part of this crate.
//! - **Features.** `std` (off by default) adds the field codec of
//!   `std::string::String` for tests of crates that have `std`; the crate is
//!   `#![no_std]` without it.

#[cfg(any(test, feature = "std"))]
extern crate std;

// Lets the derive macro refer to this crate as `::rohi_config` in its own tests.
extern crate self as rohi_config;

pub mod cbor;
mod codec;
mod crc;
mod error;
mod field_codec;
mod format;
mod id;
mod memory;
mod overrides;
mod section;
mod slot;
mod storage;
mod store;
mod value;
mod writer;

pub use codec::{decode_exact, encode_to_slice};
pub use crc::Crc32;
pub use error::{ConfigError, Corruption, DecodeFailure, DecodeKind, EncodeError, SectionError};
pub use field_codec::{Codec, Standard, Text, TextValue, Variant, VariantValue};
pub use format::{
    COMMIT_WORD, COMMIT_WORD_SIZE, FORMAT_VERSION, MAX_SECTION_PAYLOAD, MAX_SECTIONS, MAX_SLOT_SIZE,
};
pub use id::SectionId;
pub use memory::{MemoryError, MemoryStorage};
pub use overrides::ConfigOverrides;
pub use overrides::{HasOverrides, Override, RedactedOverride, materialize};
pub use section::ConfigSection;
pub use storage::ConfigStorage;
pub use store::{ConfigOpenState, ConfigStore, OpenFailure, SectionInfo};
pub use value::{ADDRESS_LEN, FieldError, MAX_VALUE_LEN, Value, ValueKind};

/// Derive macro generating the [`ConfigSection`] implementation of a private
/// persisted section. The managed config derive, `Config`, is in
/// [`rohi_config_derive::Config`] and re-exported by
/// `rohi-service`, next to the trait it implements.
pub use rohi_config_derive::ConfigSection;

pub use minicbor;
