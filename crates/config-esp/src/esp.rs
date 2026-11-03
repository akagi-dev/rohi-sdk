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

//! ESP32 flash driver and configuration partition lookup.
//!
//! [`config_storage`] reads the partition table from flash, finds the data
//! partition labelled [`PARTITION_LABEL`]
//! (`rohi-cfg`; only the label and the data type identify it, the sub-type is
//! not parsed), requires at least two erase sectors and builds an
//! [`EspConfigStorage`] on the `esp-storage` driver. The errors are the typed
//! [`PartitionError`]. `esp-storage`'s `FlashStorage` implements
//! [`MultiwriteNorFlash`](embedded_storage::nor_flash::MultiwriteNorFlash),
//! which the store needs to program a commit word more than once; encrypted
//! flash would not qualify. The storage knows nothing about CBOR, sections or
//! any feature's configuration.
//!
//! Enabled by the `esp32c3` or `esp32c6` feature (exactly one). The expected
//! partition tables are described in [`layout`](crate::layout).

use esp_bootloader_esp_idf::partitions::{
    PARTITION_TABLE_MAX_LEN, PartitionTable, read_partition_table,
};
use esp_storage::FlashStorage;

use crate::layout::{PARTITION_LABEL, SECTOR_SIZE};
use crate::{EspConfigStorage, EspStorageError, FlashRegion};

pub use esp_storage;

const DATA_PARTITION_TYPE: u8 = 1;

/// Failure to locate the configuration partition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartitionError {
    /// The partition table could not be read or is invalid.
    PartitionTable,
    /// No data partition labelled [`PARTITION_LABEL`] exists.
    NotFound,
    /// The partition is too small for two A/B slots.
    TooSmall,
    /// The partition is not usable as a flash region.
    Region(EspStorageError),
}

fn find_region(table: &PartitionTable<'_>) -> Result<FlashRegion, PartitionError> {
    let entry = table
        .iter()
        .find(|entry| {
            // The sub-type is deliberately not parsed: only the label and the
            // data type identify the configuration partition.
            entry.raw_type() == DATA_PARTITION_TYPE && entry.label_as_str() == PARTITION_LABEL
        })
        .ok_or(PartitionError::NotFound)?;
    let region = FlashRegion::new(entry.offset() as usize, entry.len() as usize);
    if region.length < 2 * SECTOR_SIZE {
        return Err(PartitionError::TooSmall);
    }
    Ok(region)
}

/// Resolves the configuration region from the on-flash partition table.
///
/// # Errors
///
/// [`PartitionError::PartitionTable`] if the table cannot be read,
/// [`PartitionError::NotFound`] if there is no data partition labelled
/// `rohi-cfg`, [`PartitionError::TooSmall`] if it is smaller than two erase
/// sectors.
pub fn partition_region(flash: &mut FlashStorage<'_>) -> Result<FlashRegion, PartitionError> {
    let mut buffer = [0u8; PARTITION_TABLE_MAX_LEN];
    let table =
        read_partition_table(flash, &mut buffer).map_err(|_| PartitionError::PartitionTable)?;
    find_region(&table)
}

/// Builds the configuration storage on the `rohi-cfg` partition.
///
/// Takes the flash driver by value: the returned storage owns it. Pass it to
/// [`rohi_config::ConfigStore::open`] (see
/// `firmwares/altruist-urban/src/esp/config.rs` and `examples/src/bin/config.rs`
/// for complete use).
///
/// # Errors
///
/// The errors of [`partition_region`], and [`PartitionError::Region`] if the
/// partition is not a valid [`FlashRegion`] for the flash (for example not
/// erase-sector aligned).
pub fn config_storage(
    mut flash: FlashStorage<'static>,
) -> Result<EspConfigStorage<FlashStorage<'static>>, PartitionError> {
    let region = partition_region(&mut flash)?;
    EspConfigStorage::new(flash, region).map_err(PartitionError::Region)
}
