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
//! Deferred device reboot.
//!
//! The control `Reboot` request must still deliver its
//! response, so [`request`] only signals [`reboot_task`], which waits for
//! [`GRACE`] before resetting the chip.

use embassy_sync::signal::Signal;
use embassy_time::{Duration, Timer};
use esp_hal::system::software_reset;
use log::warn;

use crate::tasks::RawMutex;

/// Time left for the response to reach the client before the reset.
const GRACE: Duration = Duration::from_millis(1500);

static REQUESTED: Signal<RawMutex, ()> = Signal::new();

/// Reboots the device shortly: after [`GRACE`], not before this call returns.
pub fn request() {
    REQUESTED.signal(());
}

/// Resets the chip once a reboot was requested.
#[embassy_executor::task]
pub async fn reboot_task() {
    REQUESTED.wait().await;
    warn!("Rebooting in {} ms", GRACE.as_millis());
    Timer::after(GRACE).await;
    software_reset()
}
