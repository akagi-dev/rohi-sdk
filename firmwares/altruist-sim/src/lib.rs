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

//! Native Altruist Urban simulator.
//!
//! `altruist-sim` runs the ROHI application of the Altruist Urban as a host
//! process. It is a *platform*, not a different application: the portable
//! `altruist-urban` crate (configuration owner, bindings, control device,
//! Wi-Fi manager, measurement store, identity, NTP time, telemetry uplink)
//! runs unchanged on Embassy's `platform-std` executor, and only the backends
//! that are ESP-specific on the device are native here.
//!
//! ```text
//! simulated sensors --SensorDriver--> Runner --> store task --> MeasurementStore
//! FileFlash (NOR rules) -------------ConfigStorage--> ConfigStore owner
//! NativeWifi ------------------------WifiBackend----> WifiManager
//! NativeIp (host sockets) -----------IpBackend------> NetworkHandle --> NTP, telemetry
//! TcpTransport ----------------------BleTransport---> ControlBle --> Controller --> device
//! ```
//!
//! What is simulated is the *world* behind those seams, see [`world`]; the
//! scenario file ([`scenario`]) and the management interface ([`management`])
//! change it while the device runs. What is not simulated: CPU timing, RF and
//! the real Wi-Fi/BLE protocols, I2C/UART/DMA electrical behavior, exact
//! memory layout and power consumption.

pub mod app;
pub mod args;
pub mod command;
pub mod control;
pub mod diagnostics;
mod io;
pub mod management;
pub mod network;
pub mod scenario;
pub mod sensors;
pub mod storage;
pub mod wifi;
pub mod world;

pub use app::main;
