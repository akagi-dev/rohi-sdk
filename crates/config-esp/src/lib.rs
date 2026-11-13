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
        clippy::unimplemented,
        reason = "firmware must not panic: handle the error, see AGENTS.md"
    )
)]
//! ESP32 flash backend for the `rohi-config` store.
//!
//! # Overview
//!
//! [`rohi_config::ConfigStore`] keeps typed configuration sections in two
//! A/B slots of a [`ConfigStorage`](rohi_config::ConfigStorage). This crate
//! provides that storage for ESP32 flash: [`EspConfigStorage`] wraps any
//! `embedded-storage` NOR flash and restricts it to an explicit
//! [`FlashRegion`], doing bounds checking, erase/write alignment checks and
//! mapping driver failures to [`EspStorageError`]. It knows nothing about
//! sections, CBOR documents, CRCs or generations: that is the store's job.
//!
//! With a chip feature, the `esp` module also finds the `rohi-cfg` data
//! partition in the device partition table and builds the storage on the
//! `esp-storage` driver (`esp::config_storage`). The [`layout`] module holds
//! the partition label and the sector, slot and region sizes of the Altruist
//! Urban partition tables.
//!
//! Use it when firmware needs configuration that survives reboots, power
//! loss and interrupted updates. For another flash technology, implement
//! [`ConfigStorage`](rohi_config::ConfigStorage) yourself instead.
//!
//! # Features and target
//!
//! | Build | What it contains |
//! |-------|------------------|
//! | no chip feature (default, also the host) | [`layout`], [`FlashRegion`], [`EspConfigStorage`], [`EspStorageError`]: pure, generic over `embedded-storage`, host-tested |
//! | `esp32c3` (target `riscv32imc-unknown-none-elf`) | adds the `esp` module and the `esp-storage` driver |
//! | `esp32c6` (target `riscv32imac-unknown-none-elf`) | the same for the ESP32-C6 |
//! | both chip features | `compile_error!`: exactly one chip may be selected |
//!
//! Only the chip-gated `esp` module needs a chip and a bare-metal target; the
//! rest of the crate builds and is tested on the host, which is why this
//! page can use runnable examples. Items of `esp` are written in code font
//! below because they do not exist in a host build.
//!
//! # Quick Example
//!
//! The store over [`EspConfigStorage`], here on a RAM emulation of NOR flash
//! (the hidden lines) so that it runs on the host. On a device the flash is
//! `esp-storage`'s `FlashStorage` and the region comes from the partition
//! table (see below).
//!
//! ```
//! # use embedded_storage::nor_flash::{
//! #     ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
//! # };
//! # #[derive(Debug)]
//! # struct Fail;
//! # impl NorFlashError for Fail {
//! #     fn kind(&self) -> NorFlashErrorKind { NorFlashErrorKind::Other }
//! # }
//! # struct RamFlash([u8; 65536]);
//! # impl ErrorType for RamFlash { type Error = Fail; }
//! # impl ReadNorFlash for RamFlash {
//! #     const READ_SIZE: usize = 4;
//! #     fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Fail> {
//! #         let start = offset as usize;
//! #         bytes.copy_from_slice(self.0.get(start..start + bytes.len()).ok_or(Fail)?);
//! #         Ok(())
//! #     }
//! #     fn capacity(&self) -> usize { self.0.len() }
//! # }
//! # impl NorFlash for RamFlash {
//! #     const WRITE_SIZE: usize = 4;
//! #     const ERASE_SIZE: usize = 4096;
//! #     fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Fail> {
//! #         let start = offset as usize;
//! #         for (cell, byte) in self.0.get_mut(start..start + bytes.len()).ok_or(Fail)?.iter_mut().zip(bytes) {
//! #             *cell &= *byte;
//! #         }
//! #         Ok(())
//! #     }
//! #     fn erase(&mut self, from: u32, to: u32) -> Result<(), Fail> {
//! #         self.0.get_mut(from as usize..to as usize).ok_or(Fail)?.fill(0xFF);
//! #         Ok(())
//! #     }
//! # }
//! # impl MultiwriteNorFlash for RamFlash {}
//! use minicbor::{CborLen, Decode, Encode};
//! use rohi_config::{ConfigOpenState, ConfigSection, ConfigStore};
//! use rohi_config_esp::layout::REGION_SIZE;
//! use rohi_config_esp::{EspConfigStorage, FlashRegion};
//!
//! #[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
//! #[cbor(array)]
//! #[config(version = 1)]
//! struct Brightness {
//!     #[n(0)]
//!     percent: u8,
//! }
//!
//! // 16 KiB (two 8 KiB slots) at an erase-aligned offset of a 64 KiB flash.
//! let region = FlashRegion::new(0x4000, REGION_SIZE);
//! let storage = EspConfigStorage::new(RamFlash([0xFF; 65536]), region).unwrap();
//!
//! let mut store = ConfigStore::open(storage).unwrap();
//! assert_eq!(store.state(), ConfigOpenState::Empty);
//! store.set(&Brightness { percent: 70 }).unwrap();
//!
//! // "Reboot": take the flash back and open the store again.
//! let flash = store.into_storage().into_inner();
//! let mut store = ConfigStore::open(EspConfigStorage::new(flash, region).unwrap()).unwrap();
//! assert_eq!(store.get::<Brightness>().unwrap(), Some(Brightness { percent: 70 }));
//! assert_eq!(store.state(), ConfigOpenState::Valid);
//! ```
//!
//! On a device (chip feature), `esp::config_storage` does the partition
//! lookup and builds the storage in one call. A complete, maintained example
//! that persists and reads back sections on real flash is
//! `examples/src/bin/config.rs`; the firmware wiring is
//! `firmwares/altruist-urban/src/esp/config.rs`.
//!
//! # Core Concepts
//!
//! - **Region.** A [`FlashRegion`] is the only part of flash the storage may
//!   touch. All offsets seen by the store are relative to the region start;
//!   [`FlashRegion::absolute`] translates them with overflow-safe arithmetic.
//! - **A/B slots.** The store splits the region into two slots, writes every
//!   update as a complete new document into the inactive slot, verifies it
//!   and programs the commit word last, so an interrupted update leaves the
//!   previous configuration intact. Slot size is half of the region, rounded
//!   down to the erase size and at most [`rohi_config::MAX_SLOT_SIZE`]; for
//!   the 16 KiB Altruist Urban region that is 8 KiB per slot. The
//!   transaction, the open states (`Empty`, `Valid`, `Recovered`) and the
//!   errors are documented in [`rohi_config`].
//! - **Alignment.** Writes must be aligned to the flash `WRITE_SIZE` and
//!   erases to `ERASE_SIZE`, both in offset and length; otherwise
//!   [`EspStorageError::InvalidAlignment`] is returned and the flash is not
//!   touched. Reads may have any offset and length.
//! - **Rewritable words.** The store programs a commit word a second time to
//!   invalidate a slot before erasing it, so the flash must implement
//!   [`MultiwriteNorFlash`](embedded_storage::nor_flash::MultiwriteNorFlash).
//!   `esp_storage::FlashStorage` does; flash encryption would break that
//!   assumption and is not supported by this crate.
//!
//! # Architecture & Integration
//!
//! ```text
//! typed sections   ConfigStore<S>                 rohi-config  (format, A/B, CRC, recovery)
//!                      │  ConfigStorage
//!                  EspConfigStorage<F>            this crate   (region, bounds, alignment)
//!                      │  embedded-storage NorFlash + MultiwriteNorFlash
//!                  esp_storage::FlashStorage      chip feature (ROM flash routines)
//!
//! Flash of the Altruist Urban (partitions-esp32c3.csv / partitions-esp32c6.csv)
//!   0x09000  nvs        0x6000
//!   0x0f000  phy_init   0x1000
//!   0x10000  rohi-cfg   0x4000   <- FlashRegion: slot A (8 KiB) | slot B (8 KiB)
//!   0x20000  rohi-log   0x40000  <- persistent log ring (rohi-logging)
//!   0x60000  factory    0x3A0000 (ESP32-C3, 4 MB) / 0x7A0000 (ESP32-C6, 8 MB)
//! ```
//!
//! | Crate | Role |
//! |-------|------|
//! | [`rohi_config`] | the store and the [`ConfigStorage`](rohi_config::ConfigStorage) trait this crate implements |
//! | `esp-storage`, `esp-bootloader-esp-idf` | flash driver and partition table parser (chip features only; `esp-storage` is re-exported as `esp::esp_storage`) |
//! | `firmwares/altruist-urban` | owns the one store at runtime and ships the partition tables |
//!
//! ## Runtime ownership in the Altruist Urban firmware
//!
//! The firmware (the portable `firmwares/altruist-urban/src/config.rs`, opened on the flash by
//! `src/esp/config.rs`) opens the store
//! once at boot and hands it to a single configuration task. Everything else
//! talks to the task through a cloneable `ConfigHandle` (`get`, `set`,
//! `remove`, `status`) and a bounded request channel, so there is no global
//! mutable store and nothing else touches the configuration flash.
//!
//! - If the partition is missing or committed configuration is corrupt, the
//!   firmware keeps running without persistent configuration and every
//!   request fails with `ServiceError::Unavailable`; corrupt storage is never
//!   treated as empty. `status()` reports the store's current state, so
//!   `Recovered` becomes `Valid` after the next successful update.
//! - After a commit the owner task publishes the effective Wi-Fi or network
//!   configuration to its runtime consumer in the same task step, without an
//!   `.await` in between, and once at boot from the stored value.
//! - `ConfigHandle` is section-oriented: there is no "erase everything" on
//!   it. The separate `StorageMaintenance` handle (`config::maintenance()`)
//!   offers `format_storage()` (only for an unavailable store with a present
//!   partition; a healthy store is refused with `ServiceError::StorageHealthy`)
//!   and the privileged `factory_reset()`, which formats the whole region,
//!   including identity and unknown sections.
//!
//! The partition tables are passed to `espflash` by the `cargo run` runner
//! of the repository (`.cargo/config.toml`), so run from the repository root.
//!
//! # API Navigation
//!
//! | Item | Responsibility |
//! |------|----------------|
//! | [`EspConfigStorage`] | [`ConfigStorage`](rohi_config::ConfigStorage) over a [`FlashRegion`] of a [`MultiwriteNorFlash`](embedded_storage::nor_flash::MultiwriteNorFlash) |
//! | [`FlashRegion`] | bounded flash window with overflow-safe region-relative addressing |
//! | [`EspStorageError`] | typed platform errors (`OutOfBounds`, `InvalidAlignment`, `InvalidRegion`, `ReadFailed`, `WriteFailed`, `EraseFailed`) |
//! | [`layout`] | partition label, sector, slot and region sizes, kept in sync with the partition tables by a host test |
//! | `esp::config_storage` | chip feature: partition lookup plus [`EspConfigStorage`] on `esp-storage` |
//! | `esp::partition_region`, `esp::log_partition_region` | chip feature: the `rohi-cfg` and `rohi-log` lookups, as a [`FlashRegion`] |
//! | [`SharedFlash`] | one flash driver behind a lock, shared by the owners of the partitions |
//! | `esp::PartitionError` | chip feature: `PartitionTable`, `NotFound`, `TooSmall`, `Region` |
//! | [`rohi_config::ConfigStore`] | the store itself, documented in [`rohi_config`] |
//!
//! # Guarantees & Limitations
//!
//! - **Errors.** Every failure is an [`EspStorageError`]; bounds, alignment
//!   and region errors are detected before the flash is touched. Driver
//!   errors are mapped without detail to `ReadFailed`, `WriteFailed` and
//!   `EraseFailed`. Power-loss and corruption handling is the store's, see
//!   [`rohi_config`].
//! - **Region requirements.** [`EspConfigStorage::new`] rejects regions that
//!   are empty, not erase-size aligned (offset and length), beyond the flash
//!   capacity or whose end does not fit in `u32`. `esp::config_storage`
//!   additionally requires a data partition (raw type 1) labelled
//!   `rohi-cfg` of at least two sectors; the partition sub-type is not
//!   checked.
//! - **Resource bounds.** No heap allocation. Reads that are not aligned to
//!   the flash read granularity go through a 64-byte stack bounce buffer; the
//!   flash read granularity must be a power of two of at most 64 (checked at
//!   compile time).
//! - **Persistent layout.** The partition label, the 16 KiB region and the
//!   8 KiB slots in [`layout`] mirror the shipped partition tables. Moving or
//!   resizing the partition, or changing the erase size, changes what the
//!   store sees; the on-flash format itself is owned by [`rohi_config`].
//! - **Flash operations block.** Erase and programming are synchronous
//!   blocking calls into the `esp-storage` driver. By default `esp-storage`
//!   runs its flash routines under a critical section (its default
//!   `critical-section` feature), so an update is not a good fit for
//!   timing-critical paths. Moving the configuration task to another
//!   executor does not avoid this; the duration is not measured by this
//!   project.
//! - **Not supported.** Encrypted flash (no multiple word programming) and
//!   wear levelling beyond the A/B slot alternation.
//! - **Validation status.** Region, alignment and read logic are
//!   host-tested with a mock flash, and a host test keeps [`layout`] in sync
//!   with both partition tables. The `esp32c3` and `esp32c6` builds are
//!   built and linted by CI. The on-device self-test
//!   (`examples/src/bin/config.rs`, run with
//!   `cargo run --release -p rohi-examples --features esp32c3 --target riscv32imc-unknown-none-elf --bin example-config`)
//!   checks persistence over repeated resets and the fallback to the
//!   previous generation after the newest slot was corrupted on purpose, but
//!   no run is recorded: the persistent configuration store is
//!   **not hardware-verified** in `device-matrix.toml` and
//!   `docs/validation/hardware-testing.md`.

#[cfg(test)]
extern crate std;

mod error;
#[cfg(all(feature = "esp32c3", feature = "esp32c6"))]
compile_error!(
    "select exactly one ESP target: features `esp32c3` and `esp32c6` are mutually exclusive"
);

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub mod esp;
pub mod layout;
mod region;
mod shared;
mod storage;

pub use error::EspStorageError;
pub use region::FlashRegion;
pub use shared::{FlashCell, SharedFlash};
pub use storage::EspConfigStorage;
