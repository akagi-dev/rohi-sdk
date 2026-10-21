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
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Executor;
use embassy_sync::mutex::Mutex;
use esp_hal::clock::CpuClock;
use log::{error, info};
use static_cell::StaticCell;

use altruist_urban::config;
use altruist_urban::tasks::sensors::{
    SharedI2cBus, bmx280_task, i2c_handle, noise_task, scd4x_task, sds011_task,
};
use altruist_urban::tasks::store::store_task;
use rohi_hal::board::altruist_c3::{AltruistC3, Hardware};
use rohi_hal::esp::time_driver;

use esp_backtrace as _;

esp_bootloader_esp_idf::esp_app_desc!();

static EXECUTOR: StaticCell<Executor> = StaticCell::new();
static I2C_BUS: StaticCell<SharedI2cBus> = StaticCell::new();

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let (hardware, runtime) = Hardware::split(peripherals);

    time_driver::init(runtime.systimer);
    info!("Embassy execution engine ready");

    // Runtime tasks never touch the configuration flash directly. A missing
    // partition (old partition table) must not stop the sensors.
    let _config = match config::init(runtime.flash) {
        Ok(config) => Some(config),
        Err(error) => {
            error!("Configuration store unavailable: {:?}", error);
            None
        }
    };

    let board = AltruistC3::new(hardware).expect("board initialization failed");
    let AltruistC3 {
        i2c,
        sds011_uart,
        microphone,
    } = board;
    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));

    EXECUTOR.init(Executor::new()).run(|spawner| {
        spawner.spawn(store_task().unwrap());
        spawner.spawn(bmx280_task(i2c_handle(i2c_bus)).unwrap());
        spawner.spawn(scd4x_task(i2c_handle(i2c_bus)).unwrap());
        spawner.spawn(sds011_task(sds011_uart).unwrap());
        spawner.spawn(noise_task(microphone).unwrap());
        info!("Sensor tasks started");
    })
}
