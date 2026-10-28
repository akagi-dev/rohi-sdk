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

//! Shared setup of the hardware examples.
//!
//! Every example starts the sensor tasks of the Altruist Urban firmware it
//! demonstrates and prints the typed [`SensorEvent`]s they publish.

use altruist_urban::tasks::EVENTS;
use embassy_executor::Spawner;
use esp_hal::Async;
use esp_hal::clock::CpuClock;
use esp_hal::i2c::master::I2c;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::uart::Uart;
use esp_rtos::embassy::Executor;
use log::info;
use rohi_hal::SensorEvent;
use rohi_hal_esp::I2sMicrophone;
use rohi_hal_esp::board::altruist_urban::{AltruistUrban, Hardware};
use static_cell::StaticCell;

static EXECUTOR: StaticCell<Executor> = StaticCell::new();

const RTOS_HEAP_SIZE: usize = 16 * 1024;

/// Board peripherals handed to an example.
pub struct Board {
    /// I2C bus of BMx280 and SCD4x.
    pub i2c: I2c<'static, Async>,
    /// UART of the SDS011.
    pub sds011_uart: Uart<'static, Async>,
    /// Noise microphone.
    pub microphone: I2sMicrophone,
}

/// Initialize logging, the Embassy runtime and the board, then run `init` on
/// the executor to spawn the example tasks.
pub fn run(init: impl FnOnce(Spawner, Board)) -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Debug);

    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let (hardware, runtime) = Hardware::split(peripherals);

    // `esp-rtos` provides the Embassy time base and needs a small heap for its task stacks.
    esp_alloc::heap_allocator!(size: RTOS_HEAP_SIZE);
    esp_rtos::start(TimerGroup::new(runtime.timg0).timer0, runtime.sw_interrupt);
    info!("Embassy execution engine ready");

    let board = AltruistUrban::new(hardware).expect("board initialization failed");
    let board = Board {
        i2c: board.i2c,
        sds011_uart: board.sds011_uart,
        microphone: board.microphone,
    };

    EXECUTOR
        .init(Executor::new())
        .run(|spawner| init(spawner, board))
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
