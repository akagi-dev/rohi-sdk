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

//! The simulated world of a ROHI application on `std`.
//!
//! # Overview
//!
//! `rohi-sim` supplies what a ROHI application needs when it runs as a host
//! process without the hardware around it: configuration flash in a file,
//! a Wi-Fi link, faults above the sockets of the host and, with the `sensors`
//! feature, simulated sensors. What is simulated is the *world* behind the
//! portable backend traits; the application and its services are the real
//! ones and never see this crate. Nothing here is part of the device control
//! protocol: the simulation is steered through its own [`management`] interface
//! and a [`scenario`] file.
//!
//! The crate is for development and tests, `std` only, and has no application
//! logic. Products depend on `rohi-sdk` and reach it as `rohi_sdk::sim` (features `sim`
//! and, for the simulated sensors, `sim-sensors`); the Altruist Urban does so from
//! `firmwares/altruist-urban`, see [Run Altruist Urban on std](https://rohi.akagi.dev/guides/altruist-urban-std.html).
//!
//! ```text
//! simulated sensors --SensorDriver--> Runner --> store task --> MeasurementStore
//! FileFlash (NOR rules) -------------ConfigStorage--> ConfigStore owner
//! SimWifi ---------------------------WifiBackend----> WifiManager
//! SimIp(NativeIp) -------------------IpBackend------> NetworkHandle
//!        ▲ World: sensor values, access points, IP faults ◄── management / scenario
//! ```
//!
//! # Quick Example
//!
//! A scenario is a TOML file; its events are commands of the management
//! language.
//!
//! ```
//! use rohi_sim::command::{Command, Field};
//! use rohi_sim::scenario::Scenario;
//!
//! let scenario = Scenario::parse(
//!     r#"
//!     [[events]]
//!     at = "5s"
//!     command = "set co2 1500"
//!     "#,
//! )
//! .unwrap();
//! assert_eq!(scenario.events[0].at_ms, 5_000);
//! assert_eq!(scenario.events[0].command, Command::Set(Field::Co2, 1500.0));
//! ```
//!
//! # Core Concepts
//!
//! | Concept | Meaning |
//! |---------|---------|
//! | [`world::World`] | The shared state the backends read: sensor values and faults, access points, IP faults |
//! | [`command::Command`] | One text command: `set co2 1200`, `fail scd4x`, `network down`, `storage power-loss program 64` |
//! | [`scenario::Scenario`] | Initial state and a timeline of commands, from a TOML file |
//! | [`management`] | The line-based TCP interface that executes commands while the device runs |
//! | [`storage::FileFlash`] | NOR flash with the geometry of the configuration partition, backed by a file |
//! | [`wifi::SimWifi`] | The `WifiBackend` over the simulated access points |
//! | [`ip::SimIp`] | The `IpBackend` wrapper applying the IP faults of the world |
//! | `sensors` | Simulated drivers for the real sensor runner (feature `sensors`) |
//!
//! # Architecture and Integration
//!
//! The application composes these backends where a device composes the ESP ones
//! (`firmwares/altruist-urban/src/host`). Simulated measurements reach clients through the ordinary
//! `SensorDriver → Runner → MeasurementStore → Status` pipeline and the control
//! protocol; no simulator-specific branch exists in the services.
//!
//! The world is a process-wide singleton ([`world::init`]): a process simulates
//! one device, and several devices are several processes.
//!
//! # API Navigation
//!
//! | Item | Use it to |
//! |------|-----------|
//! | [`world::init`], [`world::world`] | create and reach the simulated world |
//! | [`scenario::Scenario`] | load a scenario file |
//! | [`command::Command`] | parse and describe commands |
//! | [`management::start`], [`management::Hooks`] | serve the management interface |
//! | [`storage::FileFlash`] | open the persistent configuration flash |
//! | [`wifi::SimWifi`], [`ip::SimIp`] | provide the network backends |
//!
//! # Guarantees and Limitations
//!
//! * **Not simulated:** CPU timing, RF and the real Wi-Fi and BLE protocols,
//!   I2C/UART/DMA electrical behaviour, memory layout and power consumption.
//! * **Repeatability:** with the same scenario, seed and configuration the
//!   simulated sensor sequence is the same.
//! * **Persistence:** every flash write reaches the file at once, so a process
//!   that stops leaves what a power loss would; the real configuration store
//!   recovers at the next start.
//! * **Security:** the management interface has no authentication. Bind it to
//!   the loopback address; a non-loopback bind is an explicit unsafe choice of the
//!   application.
//! * **Targets:** host-tested on Linux (`cargo test -p rohi-sim`); not hardware.

pub mod command;
pub mod ip;
pub mod management;
pub mod scenario;
#[cfg(feature = "sensors")]
pub mod sensors;
pub mod storage;
pub mod wifi;
pub mod world;
