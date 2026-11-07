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

//! The simulated sensors of the host platform, available with the
//! `sensor-simulator` feature.
//!
//! One task per sensor, as on the device: the real [`Runner`] drives a simulated
//! driver of [`rohi_sdk::sim::sensors`] and reports to the same event bus and
//! measurement store as the hardware drivers do. Without the feature no sensor
//! task runs and the `sensors` service reports every sensor as not detected.

use embassy_executor::Spawner;
use embassy_time::Delay;
use log::info;
use rohi_sdk::hal::runtime::{EmbassyClock, Runner};
use rohi_sdk::sim::sensors::{Bmx280, Noise, Observe, RUNNER, Scd4x, Sds011, SimDriver};

use crate::tasks;

async fn run<O: Observe>(seed: Option<u64>) -> ! {
    let driver = SimDriver::<O>::new(seed);
    let mut runner = Runner::new(
        driver,
        tasks::reporter(O::KIND, EmbassyClock),
        Delay,
        RUNNER,
    );
    runner.run().await
}

#[embassy_executor::task]
async fn bmx280_task(seed: Option<u64>) {
    run::<Bmx280>(seed).await
}

#[embassy_executor::task]
async fn scd4x_task(seed: Option<u64>) {
    run::<Scd4x>(seed).await
}

#[embassy_executor::task]
async fn sds011_task(seed: Option<u64>) {
    run::<Sds011>(seed).await
}

#[embassy_executor::task]
async fn noise_task(seed: Option<u64>) {
    run::<Noise>(seed).await
}

/// Starts one task per simulated sensor, like the firmware does.
pub fn start(spawner: Spawner, seed: Option<u64>) {
    crate::tasks::spawn(spawner, "bmx280_task", bmx280_task(seed));
    crate::tasks::spawn(spawner, "scd4x_task", scd4x_task(seed));
    crate::tasks::spawn(spawner, "sds011_task", sds011_task(seed));
    crate::tasks::spawn(spawner, "noise_task", noise_task(seed));
    info!("Sensor tasks started");
}
