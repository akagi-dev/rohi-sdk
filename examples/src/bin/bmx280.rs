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

//! BMP280 / BME280 example: prints temperature, pressure (and humidity for BME280).

use altruist_urban::tasks::sensors::bmx280_task;
use altruist_urban::tasks::sensors::{SharedI2cBus, i2c_handle};
use embassy_sync::mutex::Mutex;
use esp_backtrace as _;
use rohi_examples::{print_events_task, run};
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    run(|spawner, board| {
        spawner.spawn(print_events_task().unwrap());
        static I2C_BUS: StaticCell<SharedI2cBus> = StaticCell::new();
        let i2c_bus = I2C_BUS.init(Mutex::new(board.i2c));
        spawner.spawn(bmx280_task(i2c_handle(i2c_bus)).unwrap());
    })
}
