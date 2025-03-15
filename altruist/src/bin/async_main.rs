///////////////////////////////////////////////////////////////////////////////
//
//  Copyright 2025 Akagi Engineering <research@akagi.dev>
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
use esp_hal::clock::CpuClock;
use log::info;

use embassy_executor::Spawner;
use embassy_time::{Delay, Timer};
use esp_hal::uart::{Config, Uart};
use sds011::SDS011;

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_println::logger::init_logger_from_env();

    let timer0 = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(timer0.alarm0);
    info!("Embassy init done");

    let _ = spawner;

    let config = Config::default()
        .with_baudrate(9600)
        .with_rx_fifo_full_threshold(10);
    let mut uart1 = Uart::new(peripherals.UART1, config)
        .unwrap()
        .with_tx(peripherals.GPIO10)
        .with_rx(peripherals.GPIO1)
        .into_async();
    info!("Uart created");

    let sds011 = SDS011::new(&mut uart1, sds011::Config::default());
    info!("SDS011 > created");

    let mut sds011 = sds011.init(&mut Delay).await.unwrap();
    info!("SDS011 > version {} ID {}", sds011.version(), sds011.id());

    loop {
        let dust = sds011.measure(&mut Delay).await.unwrap();
        info!("SDS011 > measure: {}", dust);
        Timer::after_secs(10).await;
    }
}
