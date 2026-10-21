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
//! Altruist Urban (ESP32-C3) board support.
//!
//! All GPIO assignments of the board live in this file; drivers never name
//! pins. Hardware production documentation:
//! * https://github.com/airalab/hardware/tree/main/Altruist
//!
//! Device: <https://robonomics.network/devices/altruist/>
//!
//! # Pin map
//!
//! | Function          | Pin    |
//! |-------------------|--------|
//! | I2C0 SDA          | GPIO3  |
//! | I2C0 SCL          | GPIO0  |
//! | SDS011 UART1 TX   | GPIO10 |
//! | SDS011 UART1 RX   | GPIO1  |
//! | I2S BCLK          | GPIO7  |
//! | I2S WS            | GPIO6  |
//! | I2S DIN           | GPIO8  |
//!
//! PLACEHOLDER pins are not verified against the hardware design and must be
//! replaced with the real assignment before use on a device.

use esp_hal::Async;
use esp_hal::dma_rx_stream_buffer;
use esp_hal::i2c::master::{BusTimeout, Config as I2cConfig, I2c};
use esp_hal::i2s::master::{Channels, DataFormat, I2s, TdmConfig};
use esp_hal::peripherals::{
    DMA_CH0, GPIO0, GPIO1, GPIO3, GPIO6, GPIO7, GPIO8, GPIO10, I2C0, I2S0, Peripherals, SYSTIMER,
    UART1,
};
use esp_hal::time::Rate;
use esp_hal::uart::{self, RxConfig, Uart};

use crate::esp::I2sMicrophone;
use crate::sensors::noise::SAMPLE_RATE_HZ;

/// SDS011 UART baud rate.
pub const SDS011_BAUD_RATE: u32 = 9_600;

/// I2S DMA ring buffer size in bytes.
const I2S_DMA_BUFFER_BYTES: usize = I2S_DMA_CHUNK_BYTES * 8;
/// I2S DMA descriptor chunk size in bytes.
const I2S_DMA_CHUNK_BYTES: usize = 256;

/// Peripherals used by the board, with the pins assigned by this file.
pub struct Hardware {
    i2c: I2C0<'static>,
    i2c_sda: GPIO3<'static>,
    i2c_scl: GPIO0<'static>,
    uart: UART1<'static>,
    uart_tx: GPIO10<'static>,
    uart_rx: GPIO1<'static>,
    i2s: I2S0<'static>,
    i2s_dma: DMA_CH0<'static>,
    i2s_bclk: GPIO7<'static>,
    i2s_ws: GPIO6<'static>,
    i2s_din: GPIO8<'static>,
}

/// Peripherals left for the runtime after the board took its share.
pub struct Runtime {
    /// System timer used as the Embassy time base, see [`crate::esp::time_driver`].
    pub systimer: SYSTIMER<'static>,
}

impl Hardware {
    /// Split the chip peripherals into board hardware and runtime peripherals.
    pub fn split(peripherals: Peripherals) -> (Self, Runtime) {
        (
            Self {
                i2c: peripherals.I2C0,
                i2c_sda: peripherals.GPIO3,
                i2c_scl: peripherals.GPIO0,
                uart: peripherals.UART1,
                uart_tx: peripherals.GPIO10,
                uart_rx: peripherals.GPIO1,
                i2s: peripherals.I2S0,
                i2s_dma: peripherals.DMA_CH0,
                i2s_bclk: peripherals.GPIO7,
                i2s_ws: peripherals.GPIO6,
                i2s_din: peripherals.GPIO8,
            },
            Runtime {
                systimer: peripherals.SYSTIMER,
            },
        )
    }
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
    Microphone(crate::esp::audio::Error),
}

/// Board peripherals, ready to be handed to sensor drivers.
pub struct AltruistC3 {
    /// I2C bus of BMx280 and SCD4x.
    pub i2c: I2c<'static, Async>,
    /// UART of the SDS011.
    pub sds011_uart: Uart<'static, Async>,
    /// Noise microphone.
    pub microphone: I2sMicrophone,
}

impl AltruistC3 {
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
