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
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use embassy_executor::Spawner;
use embassy_time::Duration;
use esp_hal::clock::CpuClock;
use esp_hal::timer::timg::TimerGroup;
use log::{error, info};

use altruist_urban::bindings::ConfigRuntime;
use altruist_urban::esp::reboot::reboot_task;
use altruist_urban::esp::resources::WIRELESS_HEAP_SIZE;
use altruist_urban::esp::{
    config, connectivity, control, crash, flash, logging, net, sensors, store, time,
};
use altruist_urban::{identity, net as wifi, tasks};
use rohi_sdk::esp::hal::board::altruist_urban::{AltruistUrban, Hardware};
use rohi_sdk::esp::net::{EspWireless, wireless_heap};

esp_bootloader_esp_idf::esp_app_desc!();

/// The panic path: the panic goes to the retained crash slot, the chip resets,
/// and the next boot writes the record to the log (`altruist_urban::esp::crash`).
#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    rohi_sdk::esp::crash::on_panic(info)
}

/// Longest wait for the first Wi-Fi connection attempt before sensing starts.
const WIFI_FIRST_ATTEMPT_WAIT: Duration = Duration::from_secs(30);

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // First of all: why the chip started, before anything can panic again.
    let reset = rohi_sdk::esp::crash::init();
    logging::init_logger();
    crash::log_reset(reset);
    #[cfg(feature = "crash-inject")]
    altruist_urban::esp::inject::at("early");
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);
    let (hardware, runtime) = Hardware::split(peripherals);

    // One owner for the radio: it takes the wireless heap and creates the Wi-Fi
    // and BLE drivers, see `rohi_sdk::esp::net`. All sizes come from
    // `altruist_urban::esp::resources`.
    let heap = wireless_heap!(WIRELESS_HEAP_SIZE);
    let mut wireless = EspWireless::new(runtime.wifi, runtime.bt, heap);
    esp_rtos::start(TimerGroup::new(runtime.timg0).timer0, runtime.sw_interrupt);
    info!("Embassy execution engine ready");

    // The configuration task is the single owner of the store; everything else
    // uses the handle. A missing partition (old partition table) or corrupted
    // configuration must not stop the sensors. It publishes every section to
    // its consumer before they start.
    let (flash, partitions) = flash::init(runtime.flash);
    // The log partition is the log task's alone; boot messages wait in its queue
    // until it runs.
    logging::start(spawner, flash, partitions.log);
    let config = config::start(
        spawner,
        flash,
        partitions.config,
        ConfigRuntime::new(wifi::handle(), logging::apply),
    );
    // Identity is provisioned from the host, never generated; a fresh device
    // simply has none and keeps sensing.
    let identity = identity::start(config);
    altruist_urban::tasks::spawn(spawner, "reboot_task", reboot_task());

    // Without the board there is no sensing, but configuration, Wi-Fi and the
    // control link still work: the device stays reachable.
    let board = match AltruistUrban::new(hardware) {
        Ok(board) => Some(board),
        Err(error) => {
            error!("Board initialization failed, sensors disabled: {:?}", error);
            None
        }
    };
    store::start(spawner);

    // A Wi-Fi or BLE failure must never stop the sensors: log and carry on.
    let mut wifi_configured = false;
    let mut ip = None;
    match net::start(spawner, &mut wireless) {
        Ok(network) => {
            // `network` is the IP data plane (DNS, TCP, UDP): the time service
            // uses it for NTP and the telemetry uplink for HTTP. Telemetry
            // follows the configuration changes made over the control link.
            time::start(spawner, network);
            connectivity::start(spawner, network, tasks::store::handle());
            wifi_configured = wifi::handle().is_configured();
            ip = Some(network);
            info!(
                "Wi-Fi started, wireless heap free {} B",
                wireless.heap().free
            );
        }
        Err(error) => error!("Wi-Fi unavailable: {}", error),
    }

    control::start(spawner, &mut wireless, config, identity, ip);

    // Sensing starts last: bringing up the radio, and the first Wi-Fi
    // association in particular, keeps the core busy long enough to overrun
    // the microphone DMA ring.
    if wifi_configured {
        wifi::wait_first_attempt(WIFI_FIRST_ATTEMPT_WAIT).await;
    }
    if let Some(board) = board {
        sensors::start(spawner, board);
    }

    #[cfg(feature = "crash-inject")]
    altruist_urban::esp::inject::running().await;

    core::future::pending().await
}
