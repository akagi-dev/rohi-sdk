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
//! Portable code versus platform code.
//!
//! The SDK separates **portable** code from **platform** code so that most of
//! it can be tested on a developer machine and reused on another MCU.
//!
//! | Category | Crates | Properties |
//! |----------|--------|------------|
//! | Portable | [`rohi-hal`](../../rohi_hal/index.html), [`rohi-config`](../../rohi_config/index.html), [`rohi-config-derive`](../../rohi_config_derive/index.html), [`rohi-service`](../../rohi_service/index.html), [`rohi-service-derive`](../../rohi_service_derive/index.html), [`rohi-net`](../../rohi_net/index.html), [`rohi-identity`](../../rohi_identity/index.html), [`rohi-location`](../../rohi_location/index.html), [`rohi-time`](../../rohi_time/index.html), [`rohi-time-ntp`](../../rohi_time_ntp/index.html), [`rohi-connectivity-proto`](../../rohi_connectivity_proto/index.html), [`rohi-connectivity`](../../rohi_connectivity/index.html), [`rohi-control-proto`](../../rohi_control_proto/index.html), [`rohi-control`](../../rohi_control/index.html), [`rohi-control-ble`](../../rohi_control_ble/index.html) | `no_std`, no allocation, no MCU HAL, builds and tests on the host (the derive crates are proc-macros that run at build time) |
//! | ESP-specific | [`rohi-hal-esp`](../../rohi_hal_esp/index.html), [`rohi-config-esp`](../../rohi_config_esp/index.html), [`rohi-net-esp`](../../rohi_net_esp/index.html) | `no_std`, depend on `esp-hal` / `esp-radio` / `esp-storage` / `embassy-net` / TrouBLE; empty without a chip feature, so they stay buildable on the host |
//! | Product | `altruist-urban` (`firmwares/altruist-urban`), `examples` | choose the chip, size the resources, spawn the tasks |
//! | Desktop | [`rohi`](../../rohi/index.html) (`tools/rohi`) | `std`; talks to a device over BLE |
//!
//! # How portability is achieved
//!
//! - **Traits at the seams.** `SensorDriver` + `embedded-hal(-async)` for
//!   sensors; `ConfigStorage` for flash; `WifiBackend`, `IpBackend` and
//!   `BleTransport` for the radio and IP stack; `ControlBackend` and the
//!   [`rohi-service`](../../rohi_service/index.html) registries for the control
//!   plane. Portable code is generic over them and ships a fake or in-memory
//!   implementation for tests (`MemoryStorage`, `rohi_net::testing::FakeIp`
//!   behind the `testing` feature of [`rohi-net`](../../rohi_net/index.html),
//!   mocks in the test modules).
//! - **ROHI types in public APIs.** Vendor types and errors are converted into
//!   ROHI types inside the platform crate; a portable signature never mentions
//!   `esp-*` or `embassy-net`. `NetworkHandle` is generic over `IpBackend`,
//!   and higher layers should depend on the standard `embedded-nal-async` /
//!   `embedded-io-async` traits.
//! - **Time and clocks are injected.** Portable code takes a `Clock` /
//!   `DelayNs`; nothing reads a hardware timer or wall clock.
//! - **One chip feature at the edge.** The chip (`esp32c3` or `esp32c6`) is
//!   selected only by the firmware's features and forwarded to the platform
//!   crates; it never leaks into portable crates. Exactly one chip feature must
//!   be enabled by a final build; see
//!   [Targets and features](https://rohi.akagi.dev/reference/targets-and-features.html).
//!
//! # What can and cannot be tested on the host
//!
//! Portable crates are fully testable on the host, including timing (mock
//! delays), reconnect logic (mock backends), cancellation (dropped futures) and
//! storage faults (`MemoryStorage` fault injection). Platform crates are only
//! **compile-checked** for both chips; their behaviour needs hardware. The
//! host commands cover the portable crates and the desktop tools
//! (`default-members`); firmware and examples are built per chip. See
//! [Host testing](https://rohi.akagi.dev/validation/host-testing.html) and
//! [Hardware testing](https://rohi.akagi.dev/validation/hardware-testing.html).
//!
//! # Porting to another platform
//!
//! Implement the seam traits in a new platform crate (for example
//! `rohi-hal-<mcu>`, `rohi-net-<mcu>`), keep vendor types inside, and add a
//! firmware or board module that composes them. The portable crates should need
//! no change; if one does, the seam was leaking a platform detail and should be
//! fixed there.
