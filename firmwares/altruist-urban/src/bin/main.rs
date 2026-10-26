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
use embassy_time::Duration;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use log::{error, info};
use static_cell::StaticCell;

use altruist_urban::config;
use altruist_urban::connectivity::connectivity_task;
use altruist_urban::control::{self, BLE_RESOURCES, ble_task, control_task};
use altruist_urban::identity;
use altruist_urban::net::{self, net_runner_task, wifi_task};
use altruist_urban::resources::WIRELESS_HEAP_SIZE;
use altruist_urban::tasks::sensors::{
    SharedI2cBus, bmx280_task, i2c_handle, noise_task, scd4x_task, sds011_task,
};
use altruist_urban::tasks::store::{self, store_task};
use altruist_urban::time::ntp_task;
use rohi_hal_esp::board::altruist_urban::{AltruistUrban, Hardware};
use rohi_net_esp::{EspWireless, wireless_heap};

use esp_backtrace as _;

esp_bootloader_esp_idf::esp_app_desc!();

static I2C_BUS: StaticCell<SharedI2cBus> = StaticCell::new();

/// Longest wait for the first Wi-Fi connection attempt before sensing starts.
const WIFI_FIRST_ATTEMPT_WAIT: Duration = Duration::from_secs(30);

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let (hardware, runtime) = Hardware::split(peripherals);

    // One owner for the radio: it takes the wireless heap and creates the Wi-Fi
    // and BLE drivers, see `rohi_net_esp`. All sizes come from
    // `altruist_urban::resources`.
    let heap = wireless_heap!(WIRELESS_HEAP_SIZE);
    let mut wireless = EspWireless::new(runtime.wifi, runtime.bt, heap);
    esp_rtos::start(TimerGroup::new(runtime.timg0).timer0, runtime.sw_interrupt);
    info!(
        "Embassy execution engine ready, wireless heap free {} B",
        wireless.heap().free
    );

    // The configuration task is the single owner of the store; everything else
    // uses the handle. A missing partition (old partition table) or corrupted
    // configuration must not stop the sensors.
    let config = config::start(spawner, runtime.flash, net::handle());
    // Identity is provisioned from the host, never generated; a fresh device
    // simply has none and keeps sensing.
    let identity = identity::start(config).await;

    let board = AltruistUrban::new(hardware).expect("board initialization failed");
    let AltruistUrban {
        i2c,
        sds011_uart,
        microphone,
    } = board;

    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));
    spawner.spawn(store_task().unwrap());

    // A Wi-Fi or BLE failure must never stop the sensors: log and carry on.
    let mut wifi_configured = false;
    let mut ip = None;
    match net::start(&mut wireless) {
        Ok((backend, runner, network)) => {
            // `network` is the IP data plane (DNS, TCP, UDP): the time service
            // uses it for NTP and the telemetry uplink for HTTP.
            spawner.spawn(net_runner_task(runner).unwrap());
            spawner.spawn(wifi_task(backend).unwrap());
            spawner.spawn(ntp_task(network, config).unwrap());
            // Telemetry reads snapshots only: it can neither delay nor stop
            // the sensors, whatever the uplink does. It follows configuration
            // changes made over the control link.
            spawner.spawn(connectivity_task(network, store::handle(), config).unwrap());
            wifi_configured = net::handle().is_configured();
            ip = Some(network);
            info!(
                "Wi-Fi started, wireless heap free {} B",
                wireless.heap().free
            );
        }
        Err(error) => error!("Wi-Fi unavailable: {}", error),
    }

    match wireless.ble(control::BLE_CONFIG, BLE_RESOURCES.init(Default::default())) {
        Ok((transport, runner)) => {
            let device = control::device(config, identity, ip);
            spawner.spawn(ble_task(runner).unwrap());
            spawner.spawn(control_task(transport, device).unwrap());
            info!(
                "BLE control started, wireless heap free {} B",
                wireless.heap().free
            );
        }
        Err(error) => error!("BLE unavailable: {}", error),
    }

    // Sensing starts last: bringing up the radio, and the first Wi-Fi
    // association in particular, keeps the core busy long enough to overrun
    // the microphone DMA ring.
    if wifi_configured {
        net::wait_first_attempt(WIFI_FIRST_ATTEMPT_WAIT).await;
    }
    spawner.spawn(bmx280_task(i2c_handle(i2c_bus)).unwrap());
    spawner.spawn(scd4x_task(i2c_handle(i2c_bus)).unwrap());
    spawner.spawn(sds011_task(sds011_uart).unwrap());
    spawner.spawn(noise_task(microphone).unwrap());
    info!("Sensor tasks started");

    core::future::pending().await
}
