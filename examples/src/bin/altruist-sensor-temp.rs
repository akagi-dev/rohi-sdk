///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025 Akagi Engineering <admin@akagi.dev>
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

use log::info;

use embassy_executor::Spawner;
use embassy_time::Timer;

use rohi_hal::Sensor;
use rohi_hal::board::Altruist;

use esp_backtrace as _;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[embassy_executor::task]
async fn print_temp_task(mut board: Altruist) {
    let mut sensor = Sensor(&mut board);

    loop {
        //info!("Temperature: {:?} C", sensor.temperature().await);
        Timer::after_secs(10).await;
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    let altruist = Altruist::init().await;

    spawner.spawn(print_temp_task(altruist)).ok();
}
