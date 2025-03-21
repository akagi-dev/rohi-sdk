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

use embassy_executor::Spawner;
use heapless::String;

use rohi_hal::board::Altruist;

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(72 * 1024);

    let altruist = Altruist::init().await.unwrap();

    let ssid = String::try_from("hello_altruist").unwrap();
    let address = "192.168.42.1/24".parse().unwrap();

    altruist
        .network()
        .with_wifi_ap(altruist.wifi, ssid, address)
        .unwrap()
        .start(&spawner);
}
