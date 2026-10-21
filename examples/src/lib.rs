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

//! Shared setup of the hardware examples.
//!
//! Every example starts the sensor tasks of the Altruist Urban firmware it
//! demonstrates and prints the typed [`SensorEvent`]s they publish.

use altruist_urban::tasks::EVENTS;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use log::info;
use rohi_hal::board::altruist_c3::{AltruistC3, Hardware};
use rohi_sensor_core::SensorEvent;

/// Initialize logging, heap, the Embassy runtime and the board.
pub fn init() -> AltruistC3 {
    esp_println::logger::init_logger_from_env();
    esp_alloc::heap_allocator!(#[unsafe(link_section = ".dram2_uninit")] size: 66320);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let (hardware, runtime) = Hardware::split(peripherals);

    let timg0 = TimerGroup::new(runtime.timg0);
    esp_rtos::start(timg0.timer0, runtime.sw_interrupt0);
    info!("Embassy execution engine ready");

    AltruistC3::new(hardware).expect("board initialization failed")
}

/// Print every sensor event: measurements, state changes and errors.
#[embassy_executor::task]
pub async fn print_events_task() {
    loop {
        match EVENTS.receive().await {
            SensorEvent::Measurements(update) => info!("{:?}", update),
            SensorEvent::StateChanged {
                sensor,
                state,
                timestamp,
            } => info!(
                "[{} ms] {:?} state -> {:?}",
                timestamp.millis, sensor, state
            ),
            SensorEvent::Error {
                sensor,
                error,
                timestamp,
            } => info!("[{} ms] {:?} error {:?}", timestamp.millis, sensor, error),
        }
    }
}
