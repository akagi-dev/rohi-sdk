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

//! Persistent configuration of the firmware.
//!
//! The [`ConfigStore`] is opened once at boot on the `rohi-cfg` flash
//! partition and shared through a mutex. Feature code defines its own
//! `ConfigSection` types and reads/writes them through the shared store;
//! nothing outside this module touches the raw configuration flash.

use embassy_sync::mutex::Mutex;
use esp_hal::peripherals::FLASH;
use log::{info, warn};
use rohi_config::{ConfigError, ConfigStore};
use rohi_config_esp::esp::{PartitionError, config_storage, esp_storage::FlashStorage};
use rohi_config_esp::{EspConfigStorage, EspStorageError};
use static_cell::StaticCell;

use crate::tasks::RawMutex;

/// Configuration storage of the Altruist Urban.
pub type Storage = EspConfigStorage<FlashStorage<'static>>;

/// The shared configuration store.
pub type SharedConfig = Mutex<RawMutex, ConfigStore<Storage>>;

static CONFIG: StaticCell<SharedConfig> = StaticCell::new();

/// Failure to bring up the configuration store.
#[derive(Debug)]
pub enum Error {
    /// The `rohi-cfg` partition is missing or unusable (wrong partition table?).
    Partition(PartitionError),
    /// The store could not read the flash region.
    Store(ConfigError<EspStorageError>),
}

/// Initializes the flash backend and opens the configuration store.
///
/// An empty or fully corrupt region opens as an empty configuration; defaults
/// are the responsibility of the feature owning a section.
pub fn init(flash: FLASH<'static>) -> Result<&'static SharedConfig, Error> {
    let storage = config_storage(FlashStorage::new(flash)).map_err(Error::Partition)?;
    let region = storage.region();
    let store = ConfigStore::open(storage).map_err(Error::Store)?;
    match store.generation() {
        Some(generation) => info!(
            "Configuration: {} section(s), generation {} @ {:#x}+{:#x}",
            store.section_count(),
            generation,
            region.offset,
            region.length
        ),
        None => warn!(
            "Configuration: empty @ {:#x}+{:#x}",
            region.offset, region.length
        ),
    }
    Ok(CONFIG.init(Mutex::new(store)))
}
