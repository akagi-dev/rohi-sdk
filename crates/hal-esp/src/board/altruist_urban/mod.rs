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
//! Altruist Urban board support.
//!
//! This module is the single place that names GPIO pins and configures the
//! buses. The API is chip independent; the Cargo feature `esp32c3` or `esp32c6`
//! (exactly one, forwarded by the firmware) selects the backend module
//! (`esp32c3.rs` or `esp32c6.rs`) that owns the pin map and the chip specific
//! peripheral types. Drivers and firmware tasks never name pins.
//!
//! ```text
//! let (hardware, runtime) = Hardware::split(peripherals);
//! let board = AltruistUrban::new(hardware)?;
//! // board.i2c, board.sds011_uart, board.microphone go to the sensor drivers;
//! // runtime carries the timer, the radios and the flash for the runtime.
//! ```
//!
//! The compiled version of this sequence is
//! `firmwares/altruist-urban/src/bin/main.rs`; the examples in `examples/` do
//! the same through `rohi_examples::run`. The code needs a chip feature and the
//! matching bare-metal target, so it cannot be a doctest.
//!
//! [`Hardware::split`] divides the chip peripherals into the board hardware and
//! the [`Runtime`] peripherals (timer group 0 and the software interrupt of
//! `esp-rtos`, the Wi-Fi and Bluetooth radios, the flash).
//! [`AltruistUrban::new`] configures the buses without talking to any sensor
//! and returns the configured I2C bus (BMx280 and SCD4x), the SDS011 UART
//! ([`SDS011_BAUD_RATE`], 9600 baud) and the I2S microphone, ready to be
//! handed to the sensor drivers.
//!
//! # Pin map
//!
//! | Function | ESP32-C3 | ESP32-C6 |
//! |----------|----------|----------|
//! | I2C0 SDA / SCL | GPIO3 / GPIO0 | GPIO3 / GPIO2 |
//! | SDS011 UART1 TX / RX | GPIO10 / GPIO1 | GPIO5 / GPIO4 |
//! | I2S BCLK / WS / DIN | GPIO7 / GPIO6 / GPIO8 | GPIO10 / GPIO1 / GPIO11 |
//!
//! The ESP32-C3 map was checked by the maintainer against the Altruist Urban
//! pinout; upstream publishes no schematic of this legacy variant. The ESP32-C6
//! map comes from the Altruist Urban main board schematic (ESP32-C6-WROOM-1-N8,
//! "GPIO Map" sheet). Neither map is recorded as verified on hardware (every
//! board entry of `support-matrix/matrix.toml` has `hardware_verification =
//! "not-verified"`). The schematic
//! names the SDS011 nets `PM_TX` (GPIO5) and `PM_RX` (GPIO4) without a
//! viewpoint; they are taken from the ESP32-C6 side, so swap them if the sensor
//! stays silent. GPIO4 and GPIO5 are strapping pins, but they only select the
//! JTAG source when the `JTAG_SEL_ENABLE` eFuse is burned, so the SDS011 does
//! not affect booting. The C6 board also wires a button (GPIO7), a
//! data-logging LED (GPIO0) and the CN4 expansion connector that the firmware
//! does not drive. The firmware drives no sensor power or control pins.
//!
//! A board revision with a different pin map must get its own module instead of
//! an edit of these tables. The same tables, the verification status per board
//! and capability, and the board guides are in
//! <https://rohi.akagi.dev/platform/boards.html> and the support matrix
//! <https://rohi.akagi.dev/support/overview.html>.
//!
//! # Microphone
//!
//! The I2S microphone runs at the noise sample rate of `rohi-hal`
//! ([`SAMPLE_RATE_HZ`]), mono, 32-bit Philips frames. The DMA ring buffer is
//! 4 KiB (64 ms, 16 chunks of 256 bytes); the noise task must drain it within
//! that time or the DMA transfer fails, which [`I2sMicrophone`] reports as an
//! error and restarts on the next read.
//!
//! # Errors
//!
//! [`AltruistUrban::new`] returns [`Error`] when a bus configuration is
//! rejected by `esp-hal` or the microphone transfer cannot be prepared.
//!
//! Hardware production documentation:
//! * <https://github.com/airalab/hardware/tree/main/Altruist>
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

/// SDS011 UART baud rate (9600 baud).
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

/// Board initialization failure, returned by [`AltruistUrban::new`].
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
///
/// Created once by [`AltruistUrban::new`]; the fields are moved into the sensor
/// tasks (see `firmwares/altruist-urban/src/tasks/sensors.rs`).
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
    ///
    /// The I2C bus runs at the `esp-hal` default of 100 kHz with the hardware
    /// bus timeout at its maximum; the UART runs at [`SDS011_BAUD_RATE`] with an
    /// RX FIFO threshold of 10 bytes; the I2S receiver runs mono, 32-bit Philips
    /// frames at [`SAMPLE_RATE_HZ`]. The I2S DMA transfer starts on the first
    /// read of the microphone, not here.
    ///
    /// # Errors
    ///
    /// [`Error::I2c`], [`Error::Uart`] or [`Error::I2s`] if `esp-hal` rejects the
    /// configuration; [`Error::Microphone`] if the microphone cannot be prepared.
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
