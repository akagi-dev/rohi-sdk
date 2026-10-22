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
//! Altruist Urban (ESP32-C6) pin map and peripheral bundle.
//!
//! All GPIO assignments of the board live in this file; drivers never name
//! pins. Source: the Altruist Urban main board schematic (ESP32-C6-WROOM-1-N8,
//! "GPIO Map" sheet, 2025-11-24) and `Docs/CN4_PINOUT.md` of
//! https://github.com/airalab/hardware/tree/main/Altruist/Urban
//!
//! # Pin map
//!
//! | Function          | Pin    |
//! |-------------------|--------|
//! | I2C0 SDA          | GPIO3  |
//! | I2C0 SCL          | GPIO2  |
//! | SDS011 UART1 TX   | GPIO5  |
//! | SDS011 UART1 RX   | GPIO4  |
//! | I2S WS            | GPIO1  |
//! | I2S BCLK          | GPIO10 |
//! | I2S DIN           | GPIO11 |
//!
//! The schematic names the SDS011 nets `PM_TX` (GPIO5) and `PM_RX` (GPIO4)
//! without stating the viewpoint; they are taken from the ESP32-C6 side.
//! **Unverified on hardware**: swap `uart_tx` / `uart_rx` if the SDS011 stays
//! silent. GPIO4 (MTMS) and GPIO5 (MTDI) are strapping pins, but they only
//! select the JTAG signal source and are sampled only when the
//! `JTAG_SEL_ENABLE` eFuse is burned, so the SDS011 does not affect booting.
//!
//! Not driven by the firmware: BUTTON (GPIO7), DATALOG LED (GPIO0), USB
//! D-/D+ (GPIO12/GPIO13) and the CN4 expansion connector (GPIO6, 15, 18, 19,
//! 21, 23; GPIO20 and GPIO22 are reserved for the LoRa Meshtastic module).

use esp_hal::peripherals::{
    DMA_CH0, GPIO1, GPIO2, GPIO3, GPIO4, GPIO5, GPIO10, GPIO11, I2C0, I2S0, Peripherals, UART1,
};

use super::Runtime;

/// Peripherals used by the board, with the pins assigned by this file.
pub struct Hardware {
    pub(super) i2c: I2C0<'static>,
    pub(super) i2c_sda: GPIO3<'static>,
    pub(super) i2c_scl: GPIO2<'static>,
    pub(super) uart: UART1<'static>,
    pub(super) uart_tx: GPIO5<'static>,
    pub(super) uart_rx: GPIO4<'static>,
    pub(super) i2s: I2S0<'static>,
    pub(super) i2s_dma: DMA_CH0<'static>,
    pub(super) i2s_bclk: GPIO10<'static>,
    pub(super) i2s_ws: GPIO1<'static>,
    pub(super) i2s_din: GPIO11<'static>,
}

impl Hardware {
    /// Split the chip peripherals into board hardware and runtime peripherals.
    pub fn split(peripherals: Peripherals) -> (Self, Runtime) {
        (
            Self {
                i2c: peripherals.I2C0,
                i2c_sda: peripherals.GPIO3,
                i2c_scl: peripherals.GPIO2,
                uart: peripherals.UART1,
                uart_tx: peripherals.GPIO5,
                uart_rx: peripherals.GPIO4,
                i2s: peripherals.I2S0,
                i2s_dma: peripherals.DMA_CH0,
                i2s_bclk: peripherals.GPIO10,
                i2s_ws: peripherals.GPIO1,
                i2s_din: peripherals.GPIO11,
            },
            Runtime {
                timg0: peripherals.TIMG0,
                sw_interrupt: peripherals.FROM_CPU_INTR0,
                wifi: peripherals.WIFI,
                flash: peripherals.FLASH,
            },
        )
    }
}
