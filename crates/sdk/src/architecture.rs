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
//! The layered dependency model of the SDK and its data flows.
//!
//! The SDK is layered: portable crates define typed models, contracts and
//! logic; thin `*-esp` platform crates implement those contracts on the ESP32;
//! the firmware composes them and owns the product decisions (resource sizes,
//! which features run). The [crate map](crate#crate-map) lists every crate and
//! the allowed dependency direction.
//!
//! ```text
//!                    firmwares/altruist-urban   (product: portable composition; esp/ module: ESP tasks, resources)
//!                                      │ composes
//!           ┌──────────────────────┬──────────────────────────┬──────────────────────────────┐
//!  portable │ sensors              │ configuration, services  │ networking, time, control    │
//!  crates   │ rohi-hal             │ rohi-config              │ rohi-net                     │
//!           │   drivers, Runner,   │   ConfigStore,           │   WifiManager, WifiHandle,   │
//!           │   MeasurementStore,  │   ConfigSection          │   NetworkHandle, IpBackend,  │
//!           │   MeasurementHandle  │ rohi-service             │   BleTransport               │
//!           │                      │   Config, Status, Action │ rohi-time, rohi-time-ntp     │
//!           │                      │   registries             │ rohi-connectivity (+ proto)  │
//!           │                      │ rohi-identity            │ rohi-control-ble             │
//!           │                      │ rohi-location            │   -> rohi-control            │
//!           │                      │                          │   -> rohi-control-proto      │
//!           ├──────────────────────┼──────────────────────────┼──────────────────────────────┤
//!  platform │ rohi-hal-esp         │ rohi-config-esp          │ rohi-net-esp                 │
//!  crates   │   board pins, I²C,   │   flash region,          │   EspWireless, EspWifi,      │
//!  (ESP)    │   UART, I²S, clock   │   esp-storage            │   EspIp, EspBle              │
//!           └──────────────────────┴──────────────────────────┴──────────────────────────────┘
//!                                    │ platform crates are built on
//!                  Embassy  ─  esp-rtos  ─  esp-hal / esp-radio / embassy-net / TrouBLE
//! ```
//!
//! In the picture `->` means "depends on".
//!
//! # Platforms: ESP and `std`
//!
//! A product is the portable application on one platform. The ESP platform
//! implements the seams on the ESP32; the **`std` platform** implements the
//! same seams on a host operating system, so the same application runs without
//! hardware as the process `altruist-urban` (the `std` feature of
//! `altruist-urban`). It is a platform implementation, not a different
//! application or protocol stack.
//!
//! ```text
//!                         portable ROHI SDK
//!             (rohi-hal, -config, -net, -control, ... and the
//!              portable part of `altruist-urban`)
//!                                │
//!                   ┌────────────┴─────────────┐
//!                   ▼                          ▼
//!              ESP platform               native platform
//!   rohi-*-esp, `altruist-urban::esp`      rohi-net-std, rohi-sim, `altruist-urban::host`
//!   esp-rtos, esp-hal, esp-radio           Embassy platform-std, host sockets, BlueZ
//!                   │                          │
//!             altruist-urban              altruist-urban (feature `std`)
//! ```
//!
//! | Seam | ESP platform | Native platform |
//! |------|--------------|-----------------|
//! | `SensorDriver` | I²C / UART / I²S drivers | simulated drivers fed by a scenario |
//! | `ConfigStorage` | `EspConfigStorage` on the `rohi-cfg` partition | `FileFlash` of `rohi-sim`, a file with NOR rules |
//! | `WifiBackend` | `EspWifi` | `SimWifi` of `rohi-sim` over simulated access points |
//! | `IpBackend` | `EspIp` on `embassy-net` | `NativeIp` of `rohi-net-std` on host sockets, behind the faults of `SimIp` |
//! | `BleTransport` | `EspBle` | `BluezBle` of `rohi-net-std` (Linux, feature `std-ble`) |
//! | Control over TCP | none | `ControlTcp` of `rohi-control-tcp` over `TcpAcceptor` (feature `std-tcp`) |
//!
//! BLE and TCP are independent transports of the same `Controller`; TCP is not
//! BLE framing over a socket. The `std` platform never depends on an ESP crate
//! and the two platforms exclude each other at build time. It simulates logic,
//! not hardware: CPU timing, RF, electrical bus behaviour and DMA remain
//! hardware-validation concerns. See
//! [Run Altruist Urban on std](https://rohi.akagi.dev/guides/altruist-urban-std.html).
//!
//! # Rules that keep this picture true
//!
//! 1. **Dependencies point from platform and product towards portable
//!    crates.** `rohi-net` never depends on `rohi-net-esp` or on
//!    `rohi-control*`; `rohi-hal` never depends on `rohi-net`; portable crates
//!    never depend on `esp-*` crates. See [`portability`](crate::portability).
//! 2. **Feature crates own their own config sections.**
//!    [`rohi-config`](../../rohi_config/index.html) never learns the set of
//!    application sections:
//!    [`rohi-net`](../../rohi_net/index.html) owns `WifiConfig`, `NetworkConfig`
//!    and `BleSettings` (each the effective configuration, persisted as
//!    generated overrides), [`rohi-identity`](../../rohi_identity/index.html) owns
//!    the `Identity` section, [`rohi-location`](../../rohi_location/index.html)
//!    owns `LocationConfig`, [`rohi-time-ntp`](../../rohi_time_ntp/index.html)
//!    owns `NtpServersConfig` and
//!    [`rohi-connectivity`](../../rohi_connectivity/index.html) owns
//!    `ConnectivityConfig`. A section type lives next to the code that
//!    interprets it, and the firmware only wires the section to its runtime.
//!    See [Add a ConfigSection](https://rohi.akagi.dev/guides/add-config-section.html).
//! 3. **Stateful services have one owner task** and are reached through
//!    bounded handles: see [`ownership`](crate::ownership).
//! 4. **Platform types never cross a portable API.** Vendor types and errors
//!    are converted into ROHI types inside the platform crate.
//! 5. **The product, not the platform crate, decides how much RAM the wireless
//!    stack takes**: see [`memory`](crate::memory).
//!
//! # Data flows
//!
//! Sensor data. Sensor tasks never touch the store; the store task is its only
//! owner. Other components read measurements on demand through a
//! `MeasurementHandle`, a bounded snapshot request to that owner. The control
//! plane reads them through the `SensorsService` of
//! [`rohi-hal`](../../rohi_hal/index.html), the telemetry service through its
//! snapshot source:
//!
//! ```text
//! sensor tasks ─► SensorEvent ─► EVENTS (bounded) ─► store task ─► MeasurementStore
//!                                                        ▲                │
//!                                        MeasurementHandle (snapshot)     │
//!                                                        │                │
//!                  SensorsService (status) ──► rohi-control ─► BLE ─► rohi client
//!                  ConnectivityService ──► signed frames ─► NetworkHandle (HTTP)
//! ```
//!
//! Control requests. The transport only moves bounded chunks, the dispatcher
//! knows no module; everything it serves comes from the registries of
//! [`rohi-service`](../../rohi_service/index.html):
//!
//! ```text
//! BLE write ─► rohi-net-esp (EspBle, bounded RX) ─► rohi-control-ble (reassemble) ─► Controller
//!           ◄─ bounded TX ◄─ fragment ◄─ response ◄─────────────────────────────────────┘
//!                                            │ ControlBackend (registries of rohi-service)
//!                   config sections (ConfigHandle) · services (MeasurementHandle, WifiHandle, ...)
//! ```
//!
//! Configuration. A committed section reaches its runtime in the same step as
//! the commit (see [`cancellation`](crate::cancellation#durable-mutation)),
//! and the application then uses the network through the data plane:
//!
//! ```text
//! ConfigHandle::set(WifiConfig) ─► config owner task ─ commit ─► publish desired Wi-Fi config ─► WifiManager
//!                                                                                                  │
//!                                           application ◄── NetworkHandle (DNS, TCP) ◄── EspIp ◄───┘ (link + DHCP)
//! ```
//!
//! `ConfigHandle` is the handle of the configuration owner task of the
//! firmware (`firmwares/altruist-urban/src/config.rs`).
//!
//! # Where does new code go?
//!
//! See the table in the [crate map](crate#where-does-new-code-go).
