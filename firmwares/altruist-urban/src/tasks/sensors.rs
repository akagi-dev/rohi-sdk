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
//! One task per sensor. Lightweight I2C sensors share one bus, so a long
//! operation of one sensor never blocks another sensor task.

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_sync::mutex::Mutex;
use embassy_time::Delay;
use esp_hal::Async;
use esp_hal::i2c::master::I2c;
use esp_hal::uart::Uart;
use log::info;
use rohi_hal::SensorKind;
use rohi_hal::runtime::{Reporter, Runner, RunnerConfig, TimeoutI2c};
use rohi_hal::sensors::{bmx280, noise, scd4x, sds011};
use rohi_hal_esp::board::altruist_urban::AltruistUrban;
use rohi_hal_esp::{EmbassyClock, I2sMicrophone};
use static_cell::StaticCell;

use super::{EVENTS, RawMutex, STATES};

/// Upper bound of a single I2C transaction.
const I2C_TRANSACTION_TIMEOUT_MS: u32 = 200;

/// Async mutex around the I2C bus.
pub type SharedI2cBus = Mutex<RawMutex, I2c<'static, Async>>;

/// I2C bus handle of one sensor: shared bus with bounded transactions.
pub type I2cHandle = TimeoutI2c<I2cDevice<'static, RawMutex, I2c<'static, Async>>, Delay>;

/// Create a handle to the shared I2C bus.
pub fn i2c_handle(bus: &'static SharedI2cBus) -> I2cHandle {
    TimeoutI2c::new(I2cDevice::new(bus), Delay, I2C_TRANSACTION_TIMEOUT_MS)
}

static I2C_BUS: StaticCell<SharedI2cBus> = StaticCell::new();

/// Starts the sensor tasks of the Altruist Urban `board`: the I2C sensors share
/// one bus, every sensor has its own task, so a failing or slow sensor never
/// delays another one.
pub fn start(spawner: Spawner, board: AltruistUrban) {
    let AltruistUrban {
        i2c,
        sds011_uart,
        microphone,
    } = board;
    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));
    spawner.spawn(bmx280_task(i2c_handle(i2c_bus)).unwrap());
    spawner.spawn(scd4x_task(i2c_handle(i2c_bus)).unwrap());
    spawner.spawn(sds011_task(sds011_uart).unwrap());
    spawner.spawn(noise_task(microphone).unwrap());
    info!("Sensor tasks started");
}

fn reporter(
    kind: SensorKind,
) -> Reporter<'static, RawMutex, EmbassyClock, { rohi_hal::runtime::EVENT_QUEUE_CAPACITY }> {
    Reporter::new(kind, &EVENTS, EmbassyClock).with_shared_states(&STATES)
}

/// BMP280 / BME280.
#[embassy_executor::task]
pub async fn bmx280_task(i2c: I2cHandle) {
    let driver = bmx280::Bmx280::auto(i2c, Delay);
    let mut runner = Runner::new(
        driver,
        reporter(SensorKind::Bmx280),
        Delay,
        RunnerConfig::DEFAULT,
    );
    runner.run().await
}

/// SCD40 / SCD41.
#[embassy_executor::task]
pub async fn scd4x_task(i2c: I2cHandle) {
    let driver = scd4x::Scd4x::new(i2c, Delay);
    let mut runner = Runner::new(
        driver,
        reporter(SensorKind::Scd4x),
        Delay,
        RunnerConfig::DEFAULT,
    );
    runner.run().await
}

/// SDS011.
#[embassy_executor::task]
pub async fn sds011_task(uart: Uart<'static, Async>) {
    let driver = sds011::Sds011::new(uart, Delay, sds011::Config::DEFAULT);
    let mut runner = Runner::new(
        driver,
        reporter(SensorKind::Sds011),
        Delay,
        RunnerConfig::DEFAULT,
    );
    runner.run().await
}

/// I2S noise microphone.
#[embassy_executor::task]
pub async fn noise_task(microphone: I2sMicrophone) {
    let driver = noise::Noise::new(microphone, Delay, noise::Config::DEFAULT);
    let mut runner = Runner::new(
        driver,
        reporter(SensorKind::Microphone),
        Delay,
        RunnerConfig::DEFAULT,
    );
    runner.run().await
}
