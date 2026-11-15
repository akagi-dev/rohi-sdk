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

//! The one SPI flash of the device and the partitions that live on it.
//!
//! ```text
//! FLASH peripheral ─► FlashStorage ─► SharedFlash ─┬─► rohi-cfg  config owner task
//!                                                  └─► rohi-log  logging task
//! ```
//!
//! [`init`] takes the peripheral once, reads the partition table and returns a
//! [`Flash`] handle with the two regions. The handle only serializes
//! operations; what a region may be used for is decided by its owner, which
//! never names the other's bytes.

use esp_hal::peripherals::FLASH;
use log::{error, info};
use rohi_sdk::esp::config::esp::{
    PartitionError, esp_storage::FlashStorage, log_partition_region, partition_region,
};
use rohi_sdk::esp::config::{FlashCell, FlashRegion, SharedFlash};
use static_cell::StaticCell;

use crate::tasks::RawMutex;

/// The flash driver behind its lock.
static SHARED: StaticCell<FlashCell<RawMutex, FlashStorage<'static>>> = StaticCell::new();

/// A handle to the device flash.
pub type Flash = SharedFlash<RawMutex, FlashStorage<'static>>;

/// Where the partitions are: each lookup can fail independently, and a missing
/// partition only takes its owner out of service.
pub struct Partitions {
    /// The `rohi-cfg` configuration region.
    pub config: Result<FlashRegion, PartitionError>,
    /// The `rohi-log` persistent log region.
    pub log: Result<FlashRegion, PartitionError>,
}

/// Takes the flash peripheral, finds the partitions and shares the driver.
/// Call once at boot.
pub fn init(peripheral: FLASH<'static>) -> (Flash, Partitions) {
    let mut driver = FlashStorage::new(peripheral);
    let partitions = Partitions {
        config: partition_region(&mut driver),
        log: log_partition_region(&mut driver),
    };
    match &partitions.log {
        Ok(region) => info!("Log partition @ {:#x}+{:#x}", region.offset, region.length),
        Err(error) => error!("Log partition unavailable: {:?}", error),
    }
    let cell = SHARED.init(Flash::cell(driver));
    #[cfg(feature = "crash-inject")]
    cell.lock(|flash| {
        // The lock and the driver are held, as in the middle of a write.
        let _driver = flash.borrow_mut();
        super::inject::at("flash");
    });
    (Flash::new(cell), partitions)
}
