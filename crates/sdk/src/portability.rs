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
//! | Portable | [`rohi-sensors`](../../rohi_sensors/index.html), [`rohi-hal`](../../rohi_hal/index.html), [`rohi-config`](../../rohi_config/index.html), [`rohi-config-derive`](../../rohi_config_derive/index.html), [`rohi-service`](../../rohi_service/index.html), [`rohi-service-derive`](../../rohi_service_derive/index.html), [`rohi-net`](../../rohi_net/index.html), [`rohi-identity`](../../rohi_identity/index.html), [`rohi-location`](../../rohi_location/index.html), [`rohi-system`](../../rohi_system/index.html), [`rohi-time`](../../rohi_time/index.html), [`rohi-time-ntp`](../../rohi_time_ntp/index.html), [`rohi-connectivity-proto`](../../rohi_connectivity_proto/index.html), [`rohi-connectivity`](../../rohi_connectivity/index.html), [`rohi-control-proto`](../../rohi_control_proto/index.html), [`rohi-control`](../../rohi_control/index.html), [`rohi-control-ble`](../../rohi_control_ble/index.html) | `no_std`, no allocation, no MCU HAL, builds and tests on the host (the derive crates are proc-macros that run at build time) |
//! | ESP-specific | [`rohi-hal-esp`](../../rohi_hal_esp/index.html), [`rohi-config-esp`](../../rohi_config_esp/index.html), [`rohi-net-esp`](../../rohi_net_esp/index.html) | `no_std`, depend on `esp-hal` / `esp-radio` / `esp-storage` / `embassy-net` / TrouBLE; empty without a chip feature, so they stay buildable on the host |
//! | Product | `altruist-urban` (`firmwares/altruist-urban`), `examples` | the portable application plus its platform: per chip feature the ESP platform (choose the chip, size the resources, spawn the tasks) or, with `std`, the host platform (`altruist-urban`) |
//! | Host platform | [`rohi-net-std`](../../rohi_net_std/index.html), [`rohi-control-tcp`](../../rohi_control_tcp/index.html) (`no_std`), `rohi-sim` | `std`; host DNS, TCP and UDP, the BlueZ BLE peripheral, the TCP control transport and the simulated world; never depend on ESP crates |
//! | Desktop | [`rohi`](../../rohi/index.html) (`tools/rohi`) | `std`; talks to a device over BLE, or over TCP to a device that serves it, such as the `std` Altruist Urban |
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
//!   [Targets and features](#targets-and-features).
//!
//! # What can and cannot be tested on the host
//!
//! Portable crates are fully testable on the host, including timing (mock
//! delays), reconnect logic (mock backends), cancellation (dropped futures) and
//! storage faults (`MemoryStorage` fault injection). Platform crates are only
//! **compile-checked** for both chips; their behaviour needs hardware. The
//! host commands cover the portable crates, the desktop tools and the portable
//! part of `altruist-urban` (`default-members`); the examples and the ESP
//! platform are built per chip. The `std` platform (`--features std,std-tcp,sensor-simulator`)
//! runs the composed application on the host,
//! so cancellation, restart and failure flows of the whole stack are tested
//! without hardware. See
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
//!
//! # Targets and features
//!
//! | Target | Chip | Cargo feature | Rust target | Commands |
//! |--------|------|---------------|-------------|----------|
//! | Host | developer machine | none | the host triple | `cargo test`, `cargo clippy`, `cargo doc` |
//! | ESP32-C3 | ESP32-C3 | `esp32c3` | `riscv32imc-unknown-none-elf` | `cargo check-esp32c3`, `cargo build-esp32c3` |
//! | ESP32-C6 | ESP32-C6 | `esp32c6` | `riscv32imac-unknown-none-elf` | `cargo check-esp32c6`, `cargo build-esp32c6` |
//!
//! The `check-*` and `build-*` aliases are defined in `.cargo/config.toml` and
//! expand to `cargo [check|build --release] -p altruist-urban --no-default-features
//! --features <chip> --target <triple>`. CI additionally runs
//! `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings` on the
//! host and clippy for the ESP packages per chip.
//!
//! **One chip at a time.** Never validate with `--all-features` or
//! `--workspace`: `esp32c3` and `esp32c6` are mutually exclusive. The firmware
//! features forward to chip-specific `esp-hal` features, and the build scripts
//! and `compile_error!`s reject both (or none) at once. Check each chip in its
//! own invocation. The repository has no default embedded target: a plain
//! `cargo` command builds for the host and the chip is always explicit.
//!
//! **Host default members.** `cargo test` and `cargo clippy` at the root run on
//! `default-members`: `crates/*`, `tools/*` and `firmwares/altruist-urban`. The platform
//! crates compile on the host without a chip feature (they are empty), so they
//! are covered; `altruist-urban` is covered through its portable part (no
//! platform feature). Its `std` platform and end-to-end tests need features,
//! `cargo test -p altruist-urban --features std,std-tcp,sensor-simulator`; the
//! `std` and ESP platforms exclude each other. ESP firmware and examples are
//! built per chip only.
//!
//! | Crate | Feature | Meaning |
//! |-------|---------|---------|
//! | `altruist-urban` | `esp32c3` / `esp32c6` | chip selection, forwarded to `rohi-sdk` and every `esp-*` dependency |
//! | `rohi-sdk` | `esp32c3` / `esp32c6` | chip selection, forwarded to every `*-esp` crate; implies `esp` |
//! | `rohi-sdk` | `config`, `service`, `time`, `sensors`, `hal`, `net`, `ntp`, `identity`, `location`, `system`, `control`, `ble`, `connectivity`, `esp`, `heap-stats`, `std` | re-export areas, see the crate page; all off by default |
//! | `rohi-hal-esp`, `rohi-config-esp`, `rohi-net-esp` | `esp32c3` / `esp32c6` | chip selection; no feature means an empty crate |
//! | `rohi-net-esp` | `heap-stats` | track the minimum free wireless heap (small runtime overhead); enabled by the firmware |
//! | `rohi-net` | `testing` | in-memory `IpBackend` (`rohi_net::testing`) for host tests; not for firmware |
//!
//! **Network stack features.** `embassy-net` is built with `tcp`, `udp`, `dns`,
//! `dhcpv4` and `medium-ethernet` (the Ethernet medium is what `esp-radio`
//! uses). Platform and network protocol features are enabled only when a
//! current SDK component uses them; a change that needs another feature (IPv6,
//! mDNS, ...) enables it together with the component and documents why.
//!
//! **Toolchain.** `rust-toolchain.toml` pins the Rust toolchain, `rust-src`,
//! `rustfmt`, `clippy` and both RISC-V targets; CI uses the same toolchain.
//! `protoc` is needed to build `rohi-control-proto`, `libdbus` and `pkg-config`
//! to build `tools/rohi` on Linux; the Nix development shell provides all of
//! them.
