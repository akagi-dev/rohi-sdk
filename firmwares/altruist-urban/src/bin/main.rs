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
#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
use embassy_sync::mutex::Mutex;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use log::{error, info};
use static_cell::StaticCell;

use altruist_urban::config;
use altruist_urban::net::{self, net_runner_task, wifi_task};
use altruist_urban::tasks::sensors::{
    SharedI2cBus, bmx280_task, i2c_handle, noise_task, scd4x_task, sds011_task,
};
use altruist_urban::tasks::store::store_task;
use rohi_hal_esp::board::altruist_urban::{AltruistUrban, Hardware};
use rohi_net_esp::{EspWifi, init_radio_heap, radio_heap_free};

use esp_backtrace as _;

esp_bootloader_esp_idf::esp_app_desc!();

static I2C_BUS: StaticCell<SharedI2cBus> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let (hardware, runtime) = Hardware::split(peripherals);

    // The radio drivers are the only users of the heap; see `rohi_net_esp`.
    init_radio_heap();
    esp_rtos::start(TimerGroup::new(runtime.timg0).timer0, runtime.sw_interrupt);
    info!(
        "Embassy execution engine ready, radio heap free {} B",
        radio_heap_free()
    );

    // The configuration task is the single owner of the store; everything else
    // uses the handle. A missing partition (old partition table) or corrupted
    // configuration must not stop the sensors.
    let config = config::start(spawner, runtime.flash);

    let board = AltruistUrban::new(hardware).expect("board initialization failed");
    let AltruistUrban {
        i2c,
        sds011_uart,
        microphone,
    } = board;

    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));
    spawner.spawn(store_task().unwrap());
    spawner.spawn(bmx280_task(i2c_handle(i2c_bus)).unwrap());
    spawner.spawn(scd4x_task(i2c_handle(i2c_bus)).unwrap());
    spawner.spawn(sds011_task(sds011_uart).unwrap());
    spawner.spawn(noise_task(microphone).unwrap());
    info!("Sensor tasks started");

    // A Wi-Fi failure must never stop the sensors: log and carry on.
    match EspWifi::new(runtime.wifi) {
        Ok((backend, runner)) => {
            spawner.spawn(net_runner_task(runner).unwrap());
            spawner.spawn(wifi_task(backend).unwrap());
            net::apply_stored_config(config).await;
            info!("Wi-Fi started, radio heap free {} B", radio_heap_free());
        }
        Err(error) => error!("Wi-Fi unavailable: {}", error),
    }

    core::future::pending().await
}
