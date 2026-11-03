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

//! The measurement store task on the ESP: [`crate::tasks::store::run`] with
//! the wireless heap in the periodic log.

use embassy_executor::Spawner;
use log::info;
use rohi_sdk::esp::hal::EmbassyClock;
use rohi_sdk::esp::net::wireless_heap;

use crate::tasks::store;

/// Starts the store task. Spawn it before the sensors that report to it.
pub fn start(spawner: Spawner) {
    spawner.spawn(store_task().unwrap());
}

fn log_heap() {
    let heap = wireless_heap();
    info!(
        "wireless heap: {} B free of {} B, minimum free {:?} B",
        heap.free, heap.size, heap.min_free
    );
}

/// Applies sensor events to the store and answers snapshot requests.
#[embassy_executor::task]
pub async fn store_task() {
    store::run(EmbassyClock, |_| {}, log_heap).await
}
