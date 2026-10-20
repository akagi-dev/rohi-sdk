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

use embassy_time::Delay;
use esp_hal::Async;
//use esp_hal::i2c::master::I2c;
use esp_hal::peripherals::{GPIO1, GPIO10, UART1};
use esp_hal::uart::{self, RxConfig, Uart};
use log::{info, warn};
use sds011::{SDS011, sensor_state::Polling};

use crate::sensor::*;

/// Air-quality sensor board Altruist.
///
/// - ESP32-C3FH4 — high-performance 32-bit single-core RISC-V CPU, up to 160 MHz
/// - 384 KB ROM, 400 KB SRAM (including 16 KB cache), 8 KB SRAM in RTC, 4 MB Flash
/// - Wi-Fi 2.4 GHz, IEEE 802.11 b/g/n-compliant, BLE
///
pub struct Altruist {
    pub sensors: Sensors,
}

/// Altruist board hardware configuration. Please fill it up with peripherals items.
pub struct Hardware {
    pub uart1: UART1<'static>,
    pub uart1_tx: GPIO10<'static>,
    pub uart1_rx: GPIO1<'static>,
}

impl Altruist {
    /// Initialize board hardware and interfaces.
    pub async fn new(hardware: Hardware) -> Self {
        let config = uart::Config::default()
            .with_baudrate(9600)
            .with_rx(RxConfig::default().with_fifo_full_threshold(10u16));
        let uart1 = Uart::new(hardware.uart1, config)
            .unwrap()
            .with_tx(hardware.uart1_tx)
            .with_rx(hardware.uart1_rx)
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

        /*
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
        */

        Self {
            sensors: Sensors { sds011 },
        }
    }
}

/// Altruist board sensors.
///
/// - Air-quality sensor: SDS011 laser-based particulate-matter sensor (PM2.5 / PM10);
/// - Noise sensor: ICS-43434 digital MEMS microphone for ambient-noise monitoring;
/// - Environmental sensor: BME280 for atmospheric pressure, humidity, and temperature.
///
/// This structi implements [`Sensor`] interface to access sensors data.
///
pub struct Sensors {
    sds011: Option<SDS011<Uart<'static, Async>, Polling>>,
    // TODO: use embedded-devices implementation when it ready
    //bme280: Option<BME280<I2c<'static, Async>>>,
}

impl ParticulateMatter for Sensors {
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

/*
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
*/
