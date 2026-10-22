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
//! Altruist Urban board support.
//!
//! The board is exposed by a chip independent API; the Cargo feature
//! `esp32c3` or `esp32c6` selects the backend module that owns the pin map and
//! the chip specific peripheral types. Drivers never name pins.
//! Hardware production documentation:
//! * https://github.com/airalab/hardware/tree/main/Altruist
//!
//! Device: <https://robonomics.network/devices/altruist/>

#[cfg(feature = "esp32c3")]
mod esp32c3;
#[cfg(feature = "esp32c6")]
mod esp32c6;

#[cfg(feature = "esp32c3")]
use esp32c3 as chip;
#[cfg(feature = "esp32c6")]
use esp32c6 as chip;

use esp_hal::Async;
use esp_hal::dma_rx_stream_buffer;
use esp_hal::i2c::master::{BusTimeout, Config as I2cConfig, I2c};
use esp_hal::i2s::master::{Channels, DataFormat, I2s, TdmConfig};
use esp_hal::peripherals::{BT, FLASH, FROM_CPU_INTR0, TIMG0, WIFI};
use esp_hal::time::Rate;
use esp_hal::uart::{self, RxConfig, Uart};
use rohi_hal::sensors::noise::SAMPLE_RATE_HZ;

use crate::I2sMicrophone;

pub use chip::Hardware;

/// SDS011 UART baud rate.
pub const SDS011_BAUD_RATE: u32 = 9_600;

/// I2S DMA ring buffer size in bytes: 4 KiB, 64 ms of 16 kHz 32-bit mono.
/// The noise task must drain it within that time or the DMA stops.
const I2S_DMA_BUFFER_BYTES: usize = I2S_DMA_CHUNK_BYTES * 16;
/// I2S DMA descriptor chunk size in bytes.
const I2S_DMA_CHUNK_BYTES: usize = 256;

/// Peripherals left for the runtime after the board took its share.
pub struct Runtime {
    /// Timer group 0, the scheduler time base of `esp-rtos` (also the Embassy time base).
    pub timg0: TIMG0<'static>,
    /// Software interrupt used by the `esp-rtos` scheduler.
    pub sw_interrupt: FROM_CPU_INTR0<'static>,
    /// Wi-Fi radio, handed to the network backend.
    pub wifi: WIFI<'static>,
    /// Bluetooth radio, handed to the BLE backend.
    pub bt: BT<'static>,
    /// SPI flash, handed to the configuration store.
    pub flash: FLASH<'static>,
}

/// Board initialization failure.
#[derive(Debug)]
pub enum Error {
    /// I2C configuration is invalid.
    I2c(esp_hal::i2c::master::ConfigError),
    /// UART configuration is invalid.
    Uart(esp_hal::uart::ConfigError),
    /// I2S configuration is invalid.
    I2s(esp_hal::i2s::master::ConfigError),
    /// The I2S DMA transfer could not be started.
    Microphone(crate::audio::Error),
}

/// Board peripherals, ready to be handed to sensor drivers.
pub struct AltruistUrban {
    /// I2C bus of BMx280 and SCD4x.
    pub i2c: I2c<'static, Async>,
    /// UART of the SDS011.
    pub sds011_uart: Uart<'static, Async>,
    /// Noise microphone.
    pub microphone: I2sMicrophone,
}

impl AltruistUrban {
    /// Configure buses and peripherals. Does not talk to any sensor.
    pub fn new(hardware: Hardware) -> Result<Self, Error> {
        let i2c = I2c::new(
            hardware.i2c,
            I2cConfig::default().with_timeout(BusTimeout::Maximum),
        )
        .map_err(Error::I2c)?
        .with_sda(hardware.i2c_sda)
        .with_scl(hardware.i2c_scl)
        .into_async();

        let uart_config = uart::Config::default()
            .with_baudrate(SDS011_BAUD_RATE)
            .with_rx(RxConfig::default().with_fifo_full_threshold(10u16));
        let sds011_uart = Uart::new(hardware.uart, uart_config)
            .map_err(Error::Uart)?
            .with_tx(hardware.uart_tx)
            .with_rx(hardware.uart_rx)
            .into_async();

        let i2s_config = TdmConfig::new_tdm_philips()
            .with_sample_rate(Rate::from_hz(SAMPLE_RATE_HZ))
            .with_data_format(DataFormat::Data32Channel32)
            .with_channels(Channels::MONO);
        let i2s = I2s::new(hardware.i2s, hardware.i2s_dma, i2s_config)
            .map_err(Error::I2s)?
            .into_async();
        let rx = i2s
            .i2s_rx
            .with_bclk(hardware.i2s_bclk)
            .with_ws(hardware.i2s_ws)
            .with_din(hardware.i2s_din)
            .build();
        let buffer = dma_rx_stream_buffer!(I2S_DMA_BUFFER_BYTES, I2S_DMA_CHUNK_BYTES);
        let microphone = I2sMicrophone::new(rx, buffer).map_err(Error::Microphone)?;

        Ok(Self {
            i2c,
            sds011_uart,
            microphone,
        })
    }
}
