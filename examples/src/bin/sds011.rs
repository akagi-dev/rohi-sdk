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
///////////////////////////////////////////////////////////////////////////////
#![no_std]
#![no_main]

//! SDS011 example: prints PM2.5 and PM10 once per duty cycle (about 2 minutes).

use altruist_urban::tasks::sensors::sds011_task;
use esp_backtrace as _;
use rohi_examples::{print_events_task, run};

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    run(|spawner, board| {
        spawner.spawn(print_events_task().unwrap());
        spawner.spawn(sds011_task(board.sds011_uart).unwrap());
    })
}
