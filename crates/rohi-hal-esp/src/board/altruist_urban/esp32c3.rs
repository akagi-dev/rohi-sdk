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
//! Altruist Urban (ESP32-C3) pin map and peripheral bundle.
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
//! Altruist Urban C3 board revision: the Urban board built around the
//! ESP32-C3 module this firmware targets (`riscv32imc-unknown-none-elf`).
//!
//! Verified against: the Altruist Urban pinout, checked by the maintainer
//! (WP2.1, akagi-dev/rohi-sdk#9) against the hardware design published in the
//! production documentation linked above. The firmware drives no sensor power
//! or control pin, so none is part of this map. A different board revision
//! with another pin map must get its own module, not an edit of this table.

use esp_hal::peripherals::{
    DMA_CH0, GPIO0, GPIO1, GPIO3, GPIO6, GPIO7, GPIO8, GPIO10, I2C0, I2S0, Peripherals, UART1,
};

use super::Runtime;

/// Peripherals used by the board, with the pins assigned by this file.
pub struct Hardware {
    pub(super) i2c: I2C0<'static>,
    pub(super) i2c_sda: GPIO3<'static>,
    pub(super) i2c_scl: GPIO0<'static>,
    pub(super) uart: UART1<'static>,
    pub(super) uart_tx: GPIO10<'static>,
    pub(super) uart_rx: GPIO1<'static>,
    pub(super) i2s: I2S0<'static>,
    pub(super) i2s_dma: DMA_CH0<'static>,
    pub(super) i2s_bclk: GPIO7<'static>,
    pub(super) i2s_ws: GPIO6<'static>,
    pub(super) i2s_din: GPIO8<'static>,
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
                timg0: peripherals.TIMG0,
                sw_interrupt: peripherals.FROM_CPU_INTR0,
                wifi: peripherals.WIFI,
                bt: peripherals.BT,
                flash: peripherals.FLASH,
            },
        )
    }
}
