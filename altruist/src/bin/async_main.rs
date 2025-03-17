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

use esp_backtrace as _;
use log::info;

use embassy_executor::Spawner;
use embassy_time::Timer;

use rohi_hal::Sensor;
use rohi_hal::board::Altruist;

#[esp_hal_embassy::main]
async fn main(_spawner: Spawner) {
    esp_println::logger::init_logger_from_env();

    let altruist = Altruist::init().await.unwrap();
    let mut sensor = Sensor::new(altruist);

    loop {
        info!("PM10 measure: {:?}", sensor.pm10().await);
        Timer::after_secs(10).await;
    }
}
