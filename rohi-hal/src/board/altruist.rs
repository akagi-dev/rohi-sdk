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
//! Altruist Air Quality Sensor hardware abstraction layer.
//!
//! Hardware production documentation available on GitHub:
//! * https://github.com/airalab/hardware/tree/main/Altruist
//!
//! Device available to buy on official web site:
//! * https://robonomics.network/devices/altruist/
//!

use bme280::i2c::AsyncBME280 as BME280;
use embassy_time::Delay;
use esp_hal::Async;
use esp_hal::clock::CpuClock;
use esp_hal::i2c::master::I2c;
use esp_hal::rng::Rng;
use esp_hal::uart::{self, Uart};
use log::{info, warn};
use sds011::{SDS011, sensor_state::Polling};

use rohi_net::{Network, wifi::Wifi};

use crate::sensor::bus::*;

/// Altruist hardware instance.
pub struct Altruist {
    sds011: Option<SDS011<Uart<'static, Async>, Polling>>,
    bme280: Option<BME280<I2c<'static, Async>>>,
    rng: Rng,
    pub wifi: Wifi,
}

impl Altruist {
    /// Initialize peripherial devices.
    pub async fn init() -> Result<Self, Error> {
        let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
        let peripherals = esp_hal::init(config);

        let timer0 = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER);
        esp_hal_embassy::init(timer0.alarm0);
        info!("[Altruist] Embassy execution engine ready");

        let config = uart::Config::default()
            .with_baudrate(9600)
            .with_rx_fifo_full_threshold(10);
        let uart1 = Uart::new(peripherals.UART1, config)
            .unwrap()
            .with_tx(peripherals.GPIO10)
            .with_rx(peripherals.GPIO1)
            .into_async();

        // Create SDS011 instance and save it in case of successful init.
        let sds011_device = SDS011::new(uart1, sds011::Config::default());
        let sds011 = match sds011_device.init(&mut Delay).await {
            Ok(sds011) => {
                info!(
                    "[Altruist] SDS011 version {}, ID {}",
                    sds011.version(),
                    sds011.id()
                );
                Some(sds011)
            }
            Err(e) => {
                warn!("[Altruist] SDS011 init failure: {}", e);
                None
            }
        };

        let i2c = I2c::new(peripherals.I2C0, Default::default())
            .unwrap()
            .with_sda(peripherals.GPIO3)
            .with_scl(peripherals.GPIO0)
            .into_async();

        // Create BME280 instance and save it in case of successful init.
        let mut bme280_device = BME280::new_primary(i2c);
        let bme280 = match bme280_device.init(&mut Delay).await {
            Ok(_) => {
                info!("[Altruist] BME280 init complete");
                Some(bme280_device)
            }
            Err(e) => {
                warn!("[Altruist] BME280 init failure: {:?}", e);
                None
            }
        };

        let rng = esp_hal::rng::Rng::new(peripherals.RNG);

        // Create Wifi HAL
        let wifi = Wifi::new(
            peripherals.WIFI,
            peripherals.TIMG0,
            peripherals.RADIO_CLK,
            rng.clone(),
        );

        Ok(Self {
            sds011,
            bme280,
            wifi,
            rng,
        })
    }

    pub fn network(&self) -> Network {
        Network::new(self.rng.clone())
    }
}

/// Hardware related errors.
#[derive(Debug)]
pub enum Error {
    WifiError,
}

impl ParticulateMatter for Altruist {
    async fn pm10(&mut self) -> Option<u16> {
        if let Some(sds011) = &mut self.sds011 {
            let data = sds011.measure(&mut Delay).await.unwrap();
            Some(data.pm10())
        } else {
            None
        }
    }

    async fn pm25(&mut self) -> Option<u16> {
        if let Some(sds011) = &mut self.sds011 {
            let data = sds011.measure(&mut Delay).await.unwrap();
            Some(data.pm25())
        } else {
            None
        }
    }
}

impl Temperature for Altruist {
    async fn temperature(&mut self) -> Option<i16> {
        if let Some(bme280) = &mut self.bme280 {
            let data = bme280.measure(&mut Delay).await.unwrap();
            let temp_tenths = data.temperature * 10.0;
            Some(temp_tenths as i16)
        } else {
            None
        }
    }
}

impl Pressure for Altruist {
    async fn pressure(&mut self) -> Option<u32> {
        if let Some(bme280) = &mut self.bme280 {
            let data = bme280.measure(&mut Delay).await.unwrap();
            Some(data.pressure as u32)
        } else {
            None
        }
    }
}
