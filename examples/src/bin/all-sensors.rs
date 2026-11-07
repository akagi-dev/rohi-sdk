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
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "firmware must not panic: handle the error, see AGENTS.md"
    )
)]

//! Combined hardware test: runs every supported sensor concurrently and prints their events.

use altruist_urban::esp::sensors::{SharedI2cBus, i2c_handle};
use altruist_urban::esp::sensors::{bmx280_task, noise_task, scd4x_task, sds011_task};
use embassy_sync::mutex::Mutex;
use esp_backtrace as _;
use rohi_examples::{print_events_task, run};
use static_cell::StaticCell;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    run(|spawner, board| {
        altruist_urban::tasks::spawn(spawner, "print_events_task", print_events_task());
        static I2C_BUS: StaticCell<SharedI2cBus> = StaticCell::new();
        let i2c_bus = I2C_BUS.init(Mutex::new(board.i2c));
        altruist_urban::tasks::spawn(spawner, "bmx280_task", bmx280_task(i2c_handle(i2c_bus)));
        altruist_urban::tasks::spawn(spawner, "scd4x_task", scd4x_task(i2c_handle(i2c_bus)));
        altruist_urban::tasks::spawn(spawner, "sds011_task", sds011_task(board.sds011_uart));
        altruist_urban::tasks::spawn(spawner, "noise_task", noise_task(board.microphone));
    })
}
