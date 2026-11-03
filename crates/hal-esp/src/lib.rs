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

//! Robonomics Open Hardware Interface (ROHI): ESP32 platform backend of
//! [`rohi-hal`](rohi_hal).
//!
//! `rohi-hal` contains hardware-independent sensor drivers. This crate adapts
//! `esp-hal` peripherals to the traits those drivers use and owns the board
//! support of ROHI devices: which GPIO is which bus, how the buses are
//! configured, and where the monotonic clock comes from. Use it from firmware
//! that runs on an ESP32-C3 or ESP32-C6; use `rohi-hal` alone when you only
//! need the portable part.
//!
//! # Features and target requirements
//!
//! The chip is selected by **exactly one** Cargo feature, forwarded by the
//! final firmware (the firmware is the only chip selector). Without a chip
//! feature the crate is empty, so it stays buildable on a regular host together
//! with the portable crates.
//!
//! | Feature | Chip | Rust target | Effect |
//! |---------|------|-------------|--------|
//! | `esp32c3` | ESP32-C3 | `riscv32imc-unknown-none-elf` | Enables `esp-hal` and `embassy-time` for the chip and all items below |
//! | `esp32c6` | ESP32-C6 | `riscv32imac-unknown-none-elf` | As above for the ESP32-C6 |
//! | none (default) | any | any | Empty crate, nothing is exported |
//!
//! Enabling both chip features is a `compile_error!`. Validate one chip per
//! invocation, never `--all-features`:
//!
//! ```text
//! cargo doc --no-deps -p rohi-hal-esp --no-default-features --features esp32c3 --target riscv32imc-unknown-none-elf
//! cargo doc --no-deps -p rohi-hal-esp --no-default-features --features esp32c6 --target riscv32imac-unknown-none-elf
//! ```
//!
//! The items documented below only exist with a chip feature. Because
//! doctests cannot be run for a bare-metal target, this crate carries no
//! doctests; the examples below are maintained as real firmware.
//!
//! # Quick example
//!
//! The Altruist Urban firmware initializes the board once and hands the
//! peripherals to the sensor tasks:
//!
//! ```text
//! let (hardware, runtime) = Hardware::split(peripherals);        // esp-hal Peripherals
//! let board = AltruistUrban::new(hardware).expect("board init");  // buses configured
//! // board.i2c            -> BMP280/BME280 and SCD4x drivers (shared bus)
//! // board.sds011_uart    -> SDS011 driver
//! // board.microphone     -> noise driver (AudioSource)
//! // runtime.{timg0, sw_interrupt, wifi, bt, flash} -> esp-rtos, radios, config store
//! ```
//!
//! The complete, compiled code is `firmwares/altruist-urban/src/bin/main.rs`
//! (board initialization) and `firmwares/altruist-urban/src/tasks/sensors.rs`
//! (how each peripheral goes into a `rohi_hal` `Runner`); `examples/` holds one
//! small program per sensor.
//!
//! # Core concepts
//!
//! * **One place for pins.** Pin numbers and bus configuration appear only in
//!   `board::altruist_urban`. `rohi-hal` drivers and firmware tasks never name a
//!   pin.
//! * **Chip independent API, chip specific backend.** The board API is the same
//!   for both chips; the chip feature selects the module that owns the pin map
//!   and the `esp-hal` peripheral types.
//! * **Board revisions get their own module.** A revision with a different pin
//!   map must not edit an existing table.
//! * **Nothing is stored.** The microphone converts DMA data to samples and
//!   hands them on; no audio buffer survives a read.
//!
//! # Architecture & integration
//!
//! ```text
//! firmware (altruist-urban)
//!    |  esp32c3 / esp32c6 feature forwarded to every *-esp crate
//!    v
//! rohi-hal-esp  -- board::altruist_urban::{Hardware, AltruistUrban, Runtime}
//!    |           -- I2sMicrophone  (implements rohi_hal::sensors::noise::AudioSource)
//!    |           -- EmbassyClock   (implements rohi_hal::runtime::Clock)
//!    v
//! rohi-hal  (drivers, Runner, MeasurementStore)      esp-hal / embassy-time
//! ```
//!
//! | Item | Role |
//! |------|------|
//! | `board::altruist_urban` | Pin assignments, bus configuration and the peripheral bundle `AltruistUrban` of the Altruist Urban, per chip feature |
//! | `board::altruist_urban::Runtime` | The peripherals the board does not use (timer group 0 and software interrupt of `esp-rtos`, Wi-Fi, Bluetooth, flash), for the runtime, the radios and the configuration store |
//! | `I2sMicrophone` | I²S microphone [`AudioSource`](rohi_hal::sensors::noise::AudioSource) of the noise driver |
//! | `MicrophoneError` | Error of the microphone |
//! | `EmbassyClock` | [`Clock`](rohi_hal::runtime::Clock) backed by `embassy-time`: time since boot |
//!
//! The firmware passes the remaining peripherals of `Runtime` on to the other
//! platform crates: `runtime.wifi` and `runtime.bt` to the network backend
//! (`rohi-net-esp`), `runtime.flash` to the configuration store
//! (`rohi-config-esp`).
//!
//! # API navigation
//!
//! * `board::altruist_urban`: `Hardware::split`, `AltruistUrban::new`,
//!   `AltruistUrban`, `Runtime`, `Error`, `SDS011_BAUD_RATE`; the pin tables.
//! * `I2sMicrophone` and `MicrophoneError`: audio source.
//! * `EmbassyClock`: monotonic clock.
//! * The portable side of each item: [`rohi_hal::sensors`], [`rohi_hal::runtime`].
//!
//! The items are plain code font here because they only exist with a chip
//! feature; build the docs with one (see above) to get links.
//!
//! # Guarantees & limitations
//!
//! * **Platform requirements.** Rust target and chip feature as in the table
//!   above; `esp-hal` is used with its `unstable` features (workspace setting).
//! * **No sensor traffic at initialization.** `AltruistUrban::new` configures
//!   buses only and talks to no sensor. It fails only with the board
//!   `Error` (invalid bus configuration or a microphone DMA start failure).
//! * **Microphone ring buffer.** The I²S DMA ring is 4 KiB, 64 ms of 16 kHz
//!   32-bit mono audio. The noise task must drain it in time; an undrained ring
//!   makes the DMA stop with an error, which the microphone reports and
//!   recovers from on the next read.
//! * **Pin maps and hardware verification.** The ESP32-C3 map is checked
//!   against the Altruist Urban pinout; the ESP32-C6 map is taken from the
//!   upstream schematic. In the support matrix (`support-matrix/matrix.toml`,
//!   reviewed 2026-11-02) **no board and capability is recorded as verified on
//!   hardware**; both chips are covered by CI target builds and clippy, and the
//!   drivers by host tests in `rohi-hal`. See
//!   <https://rohi.akagi.dev/platform/boards.html>,
//!   <https://rohi.akagi.dev/support/overview.html> and the hardware status in
//!   <https://rohi.akagi.dev/validation/hardware-testing.html>. Adding a board:
//!   <https://rohi.akagi.dev/guides/add-board.html>.

#[cfg(all(feature = "esp32c3", feature = "esp32c6"))]
compile_error!(
    "select exactly one ESP target: features `esp32c3` and `esp32c6` are mutually exclusive"
);

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod audio;
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod clock;

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub mod board;

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use audio::{Error as MicrophoneError, I2sMicrophone};
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use clock::EmbassyClock;
