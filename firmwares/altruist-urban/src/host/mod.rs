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

//! The host platform of the Altruist Urban: the application as an operating
//! system process.
//!
//! Enabled by the `std` feature, which is exclusive with the ESP platform
//! (`esp32c3`, `esp32c6`). The portable crate (configuration owner, bindings,
//! control device, Wi-Fi manager, measurement store, identity, NTP time,
//! telemetry uplink) runs unchanged on Embassy's `platform-std` executor; only
//! the backends that are ESP-specific on the device are host ones here.
//!
//! ```text
//! sensors (sensor-simulator) --SensorDriver--> Runner --> store task --> MeasurementStore
//! FileFlash (rohi-sim) -----------ConfigStorage--> ConfigStore owner
//! SimWifi (rohi-sim) -------------WifiBackend----> WifiManager
//! SimIp<NativeIp> ----------------IpBackend------> NetworkHandle --> NTP, telemetry
//! TcpAcceptor (std-tcp) ----------rohi-control-tcp -+
//! BlueZ peripheral (std-ble) -----rohi-control-ble -+--> Controller --> UrbanDevice
//! ```
//!
//! | Feature | Adds |
//! |---------|------|
//! | `std` | the platform, the binary `altruist-urban`, the development-management interface |
//! | `std-tcp` | the control transport over TCP |
//! | `std-ble` | the control transport over a BlueZ BLE peripheral (Linux) |
//! | `sensor-simulator` | simulated sensors and scenarios; without it no sensor task runs |
//!
//! TCP and BLE are independent and can be combined. What is simulated is the
//! world behind the backends (`rohi-sim`); what is not: CPU timing, RF, the real
//! Wi-Fi protocol, electrical bus behaviour, memory layout and power use.

#[cfg(all(feature = "std", feature = "esp"))]
compile_error!(
    "the `std` platform and the ESP platform exclude each other: build either with `std` or with `esp32c3` / `esp32c6`"
);

#[cfg(all(feature = "std-ble", not(target_os = "linux")))]
compile_error!("the `std-ble` feature needs BlueZ and is only available on Linux");

pub mod app;
pub mod args;
pub mod control;
pub mod logging;
pub mod process;
#[cfg(feature = "sensor-simulator")]
pub mod sensors;

pub use app::{main, run};
