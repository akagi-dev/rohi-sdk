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

//! The configuration flash of the ESP: the `rohi-cfg` partition behind the
//! portable [`ConfigOwner`].

use embassy_executor::Spawner;
use esp_hal::peripherals::FLASH;
use log::{error, info};
use rohi_sdk::esp::config::EspConfigStorage;
use rohi_sdk::esp::config::esp::{PartitionError, config_storage, esp_storage::FlashStorage};

use crate::bindings::ConfigRuntime;
use crate::config::{self, ConfigHandle, ConfigOwner};

/// Configuration storage of the Altruist Urban.
pub type Storage = EspConfigStorage<FlashStorage<'static>>;

fn describe(error: PartitionError) -> &'static str {
    match error {
        PartitionError::PartitionTable => "the partition table cannot be read",
        PartitionError::NotFound => "no `rohi-cfg` data partition (wrong partition table?)",
        PartitionError::TooSmall => "the `rohi-cfg` partition is too small for two slots",
        PartitionError::Region(_) => "the `rohi-cfg` partition is not a usable flash region",
    }
}

fn open(flash: FLASH<'static>) -> ConfigOwner<Storage> {
    match config_storage(FlashStorage::new(flash)) {
        Ok(storage) => {
            let region = storage.region();
            info!(
                "Configuration region @ {:#x}+{:#x}",
                region.offset, region.length
            );
            ConfigOwner::open(storage)
        }
        Err(error) => {
            error!("Configuration partition unavailable: {:?}", error);
            ConfigOwner::unavailable(describe(error))
        }
    }
}

/// Owns the store and answers requests, one at a time.
#[embassy_executor::task]
async fn config_task(mut owner: ConfigOwner<Storage>, runtime: ConfigRuntime) {
    owner.run(&runtime).await
}

/// Opens the configuration store on the `rohi-cfg` partition and starts its
/// owner task. The returned handle is the only way to reach the configuration.
///
/// A missing partition or corrupted committed configuration does not stop the
/// firmware, see [`ConfigOwner::open`]. Every section is published to its
/// consumer once before the task runs, see [`ConfigOwner::publish_boot`].
pub fn start(spawner: Spawner, flash: FLASH<'static>, runtime: ConfigRuntime) -> ConfigHandle {
    let mut owner = open(flash);
    owner.publish_boot(&runtime);
    spawner.spawn(config_task(owner, runtime).unwrap());
    config::handle()
}
