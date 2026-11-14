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
#![cfg_attr(docsrs, feature(doc_cfg))]

//! ROHI SDK: the umbrella crate, overview, crate map and cross-cutting
//! contracts.
//!
//! This crate has two roles. It documents what the SDK is, how its crates fit
//! together and the rules that hold across crates (ownership, cancellation,
//! memory, portability, versioning). It also re-exports the ROHI crates behind
//! Cargo features (see [Using the umbrella crate](#using-the-umbrella-crate)),
//! so a product depends on `rohi-sdk` alone. The contracts of one component are
//! documented in the crate that implements it; the links below lead there.
//!
//! # Using the umbrella crate
//!
//! Every area is a feature, all off by default. Enabling a feature enables the
//! crates it needs, and each crate is re-exported as a module:
//!
//! | Feature | Module | Crate | Also enables |
//! |---------|--------|-------|--------------|
//! | `config` | `config` | `rohi-config` | |
//! | `service` | `service` | `rohi-service` | `config` |
//! | `hal` (alias `sensors`) | `hal` | `rohi-hal`: typed measurements, sensor drivers (`hal::sensors`), sensor runtime | `service` |
//! | `time` | `time` | `rohi-time` | `hal` |
//! | `net` | `net` | `rohi-net` | `service` |
//! | `ntp` | `ntp` | `rohi-time-ntp` | `time`, `net` |
//! | `identity` | `identity` | `rohi-identity` | `service` |
//! | `location` | `location` | `rohi-location` | `service` |
//! | `logging` | `logging` | `rohi-logging` | `time` |
//! | `system` | `system` | `rohi-system`: the `system` service (reboot, factory reset) | `service` |
//! | `control` | `control` | `rohi-control` | `service` |
//! | `ble` | `ble` | `rohi-control-ble` | `control`, `net` |
//! | `tcp` | `tcp` | `rohi-control-tcp` | `control` |
//! | `host` | `host` | `rohi-net-std` | `net`, `tcp` |
//! | `host-ble` | `host::ble` | BlueZ BLE peripheral of `rohi-net-std` (Linux) | `host`, `ble` |
//! | `sim` | `sim` | `rohi-sim`: simulated world for `std` applications | `host`, `hal`, `config` |
//! | `sim-sensors` | `sim::sensors` | simulated sensor drivers of `rohi-sim` | `sim` |
//! | `connectivity` | `connectivity` | `rohi-connectivity` | `hal`, `identity`, `location`, `net`, `time` |
//! | `embassy-time` | `hal::runtime::EmbassyClock` | the `embassy-time` clock of `rohi-hal` | `hal` |
//! | `esp` | `esp::{hal, config, net}` | `rohi-hal-esp`, `rohi-config-esp`, `rohi-net-esp` | `hal`, `config`, `net` |
//! | `esp32c3`, `esp32c6` | | chip selection of the `esp` crates | `esp` |
//! | `heap-stats` | | wireless heap statistics of `rohi-net-esp` | `esp` |
//! | `std` | | `std` support of `rohi-config` and `rohi-identity` | |
//!
//! ```toml
//! [dependencies]
//! rohi-sdk = { version = "0.5", features = ["service", "hal", "net", "esp"] }
//!
//! [features]
//! esp32c3 = ["rohi-sdk/esp32c3"]
//! esp32c6 = ["rohi-sdk/esp32c6"]
//! ```
//!
//! ```text
//! use rohi_sdk::hal::sensors::bmx280;
//! use rohi_sdk::service::Service;
//! use rohi_sdk::esp::hal::EmbassyClock;
//! ```
//!
//! The derive macros of `rohi-config` and `rohi-service` find the runtime
//! crates through `rohi-sdk` when it is the only ROHI dependency. Third-party
//! crates (`esp-hal`, Embassy, `log`, ...) are not re-exported: the product
//! depends on them directly and selects their chip features itself. Exactly
//! one chip feature must be enabled by a final build, see [`portability`].
//!
//! # What is the ROHI SDK?
//!
//! The Robonomics Open Hardware Interface (ROHI) SDK is a set of Rust
//! libraries, firmware and desktop tools for building IoT devices powered by
//! the [Robonomics](https://robonomics.network/) network. Its platform crates
//! target the ESP32-C3 and ESP32-C6 (RISC-V) and it is built on:
//!
//! ```text
//! Embassy              async executor, time, sync primitives
//!    ↓
//! esp-rtos             scheduler / time base required by the radio
//!    ↓
//! esp-hal / esp-radio  peripherals, Wi-Fi, BLE
//! ```
//!
//! ROHI does not introduce another runtime or RTOS abstraction. It adds its
//! own **SDK contracts** on top: typed sensor measurements, a transactional
//! configuration store, a Wi-Fi control plane, a portable IP data plane, a BLE
//! control protocol, and the rules that make them safe to combine.
//!
//! # What is in the box
//!
//! | Area | What you get | Crates |
//! |------|--------------|--------|
//! | Sensors | Typed measurements, drivers (BMx280, SCD4x, SDS011, noise), runtime, `MeasurementStore` | [`rohi-hal`](../rohi_hal/index.html), [`rohi-hal-esp`](../rohi_hal_esp/index.html) |
//! | Configuration | CBOR, A/B-slot, power-fail-safe store of independent typed sections; users persist only their overrides of the firmware defaults | [`rohi-config`](../rohi_config/index.html), [`rohi-config-esp`](../rohi_config_esp/index.html) |
//! | Networking | Wi-Fi control plane, DNS/TCP/UDP data plane, BLE transport, ESP backend | [`rohi-net`](../rohi_net/index.html), [`rohi-net-esp`](../rohi_net_esp/index.html) |
//! | Services | Config sections, live status (state, health, issues) and actions of every module, discovered by name | [`rohi-service`](../rohi_service/index.html) |
//! | Control | Generic local control protocol over BLE or TCP, `rohi` desktop client | [`rohi-control`](../rohi_control/index.html), [`rohi-control-ble`](../rohi_control_ble/index.html), [`rohi-control-tcp`](../rohi_control_tcp/index.html), [`rohi`](../rohi/index.html) |
//! | Time | Synchronized wall-clock time and its NTP source | [`rohi-time`](../rohi_time/index.html), [`rohi-time-ntp`](../rohi_time_ntp/index.html) |
//! | Location | Where the device is (user-configured; no GNSS runtime exists yet), sent in telemetry | [`rohi-location`](../rohi_location/index.html) |
//! | System | Reboot and factory reset as generic Actions, and why the device started, in the `system` service | [`rohi-system`](../rohi_system/index.html) |
//! | Crash records | A bounded panic record in reset-retained memory, the crash-loop policy, the ESP panic handler | [`rohi-crash`](../rohi_crash/index.html), [`rohi-crash-esp`](../rohi_crash_esp/index.html) |
//! | Identity | Provisioned Ed25519 device identity and signer | [`rohi-identity`](../rohi_identity/index.html) |
//! | Telemetry | Measurement snapshots as signed `connectivity-protocol` frames | [`rohi-connectivity`](../rohi_connectivity/index.html) |
//! | Firmware | Altruist Urban, a complete product built from the above | `firmwares/altruist-urban`, [Boards](https://docs.rs/rohi-hal-esp) |
//! | Host platform | The same application as a host process: simulated sensors, host IP, BLE (BlueZ) and TCP control | [`rohi-net-std`](../rohi_net_std/index.html), [`rohi-control-tcp`](../rohi_control_tcp/index.html), `rohi-sim`, `altruist-urban` (feature `std`) |
//!
//! The desktop client is described in the
//! [desktop application](https://rohi.akagi.dev/use/desktop/index.html) guide
//! and its [status and diagnostics](https://rohi.akagi.dev/use/desktop/status.html)
//! page.
//!
//! # Design principles
//!
//! - **Portable core, thin platform crates.** Everything except the `*-esp`
//!   crates, the firmware and the examples builds and tests on the host; see
//!   [`portability`].
//! - **One owner per stateful service.** Other tasks use bounded handles; see
//!   [`ownership`].
//! - **Bounded and static.** Application code does not allocate; every queue
//!   and buffer has a documented limit chosen by the product; see [`memory`].
//! - **Cancellation is normal.** Public async APIs document their cancellation
//!   semantics; see [`cancellation`].
//! - **Persistent data is an ABI.** See [`versioning`] and the
//!   [`rohi-config`](https://docs.rs/rohi-config).
//!
//! # The layers
//!
//! The SDK is layered: portable crates define typed models, contracts and
//! logic; thin `*-esp` platform crates implement those contracts on the ESP32;
//! the firmware composes them and owns the product decisions (resource sizes,
//! which features run). The complete dependency picture and the rules that keep
//! it true are in [`architecture`].
//!
//! ```text
//! firmware   altruist-urban, examples      composition, tasks, resource budget
//!                │
//! platform   rohi-hal-esp  rohi-config-esp  rohi-net-esp      ESP32-C3 / ESP32-C6
//!                │               │               │
//! portable   rohi-hal  rohi-config  rohi-net  rohi-service  rohi-control*  ...
//!                │
//! runtime    Embassy · esp-rtos · esp-hal / esp-radio / embassy-net / TrouBLE
//! ```
//!
//! # Modules
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`architecture`] | The dependency model, the rules that keep it true, the data flows |
//! | [`portability`] | Portable versus platform code, seams, host testing, porting |
//! | [`ownership`] | One owner per stateful service, state versus command |
//! | [`cancellation`] | Execution model, cancel-safe primitives, durable mutation, what each public async API guarantees, testing cancellation |
//! | [`memory`] | Static bounds, the single wireless heap, the product-owned budget, exhaustion behaviour |
//! | [`versioning`] | Which contracts are versioned, compatibility rules |
//!
//! # Crate map
//!
//! Every production workspace crate, what it is for, whether it is portable and
//! the crates it depends on. "Portable" crates are `no_std`, never allocate and
//! build and test on the host. Besides the proc-macro crates (`std` at build
//! time), the desktop client and the host crates (`rohi-net-std`, `rohi-sim`), every crate is `no_std`; a few have an optional
//! `std` feature for host tools and tests.
//!
//! | Crate | Purpose | Kind | Major public abstractions | Depends on | Does **not** own |
//! |-------|---------|------|---------------------------|------------|------------------|
//! | [`rohi-hal`](../rohi_hal/index.html) | Typed units and measurements, sensor drivers, sensor runtime, `MeasurementStore` | portable | `MeasurementStore`, `MeasurementsSnapshot`, `SensorDriver`, `Runner`, `EventBus`, `MeasurementHandle`, `SensorsService` | `embedded-hal(-async)`, `embedded-io-async`, Embassy sync and futures, `rohi-service` | MCU peripherals, board pins, networking, serialization |
//! | [`rohi-hal-esp`](../rohi_hal_esp/index.html) | ESP32 backend of the HAL: boards, I²C/UART/I²S setup, clock | ESP | `board::altruist_urban::{Hardware, AltruistUrban}`, `I2sMicrophone`, `EmbassyClock` | `rohi-hal`, `esp-hal`, `embassy-time` | Sensor logic, application tasks |
//! | [`rohi-config`](../rohi_config/index.html) | Transactional CBOR configuration store | portable | `ConfigSection`, `ConfigStore`, `ConfigStorage`, `MemoryStorage`, `Override`, `ConfigOverrides`, `HasOverrides`, `materialize` | `rohi-config-derive`, `minicbor`, `heapless`, `zeroize` | Any concrete section, flash access |
//! | [`rohi-config-derive`](../rohi_config_derive/index.html) | `#[derive(ConfigSection)]` and `#[derive(Config)]` | portable (proc-macro) | derive macros | `syn`, `quote` | Runtime behaviour |
//! | [`rohi-service`](../rohi_service/index.html) | The generic service pipeline: `Value`, `Config` sections with atomic patch, `Service` identity, `Status` with state/health/issues, `Action`s, registries | portable | `Config`, `Section`, `ConfigRegistry`, `Service`, `Status`, `StatusRegistry`, `Action`, `ActionRegistry`, `Value` | `rohi-config`, `rohi-config-derive`, `rohi-service-derive`, `heapless`, Embassy sync | Any concrete section or service, the protocol |
//! | [`rohi-service-derive`](../rohi_service_derive/index.html) | `#[derive(Service)]` | portable (proc-macro) | derive macro | `syn`, `quote` | Runtime behaviour |
//! | [`rohi-config-esp`](../rohi_config_esp/index.html) | Flash backend of the store | ESP | `EspConfigStorage`, `FlashRegion`, `layout` | `rohi-config`, `embedded-storage`, `esp-storage` | CBOR, sections, any feature's configuration |
//! | [`rohi-identity`](../rohi_identity/index.html) | Device identity: Ed25519 `Identity` config section, `Secret`, `Signer`, `IdentitySigner` | portable | `Identity`, `Secret`, `Signer`, `IdentitySigner`, `PublicKey`, `Signature` | `rohi-config`, `rohi-service`, `ed25519-zebra` | Provisioning transport, key derivation (done on the host by `rohi`), protocol semantics of signed messages |
//! | [`rohi-net`](../rohi_net/index.html) | Wi-Fi control plane, IP data plane (DNS, TCP, UDP), BLE transport traits, the `WifiConfig`, `NetworkConfig` and `BleSettings` sections and the `wifi`, `network` and `ble` services | portable | `WifiManager`, `WifiHandle`, `WifiBackend`, `NetworkHandle`, `IpBackend`, `UdpSocket`, `NetError`, `BleTransport` | `rohi-config`, `rohi-service`, Embassy sync and futures, `embedded-nal-async`, `embedded-io-async` | Radio drivers, the IP stack, control protocol |
//! | [`rohi-time`](../rohi_time/index.html) | Synchronized wall-clock time: `TimeService`, `TimeSource`, validation, background `Synchronizer` | portable | `TimeService`, `TimeSource`, `TimeSample`, `UnixTime`, `TimeStatus`, `Synchronizer` | `rohi-hal` (monotonic `Clock`, `Backoff`), `rohi-service`, Embassy sync | Network or clock hardware, any concrete source |
//! | [`rohi-time-ntp`](../rohi_time_ntp/index.html) | NTP/SNTP `TimeSource` over the IP data plane | portable | `NtpTimeSource`, `NtpConfig`, `NtpServersConfig` (the `time` config section) | `rohi-time`, `rohi-net`, `rohi-hal`, `rohi-config`, `rohi-service`, `sntpc` | The global concept of time, the UDP stack |
//! | [`rohi-location`](../rohi_location/index.html) | The location of the device (user-configured; no GNSS runtime exists yet): `LocationConfig` section, `LocationShared`, `LocationService` | portable | `Location`, `LocationConfig`, `LocationShared`, `LocationSource`, `LocationService` | `rohi-config`, `rohi-service`, Embassy sync | GNSS hardware, telemetry |
//! | [`rohi-system`](../rohi_system/index.html) | The `system` service: `reboot` and `factory_reset` Actions and the reset and crash status over a `SystemPlatform` the firmware implements | portable | `SystemService`, `SystemPlatform`, `SystemError` | `rohi-service`, `rohi-crash` | Chip reset, flash formatting |
//! | [`rohi-crash`](../rohi_crash/index.html) | The record of a panic for reset-retained memory (magic, version, CRC-32), the crash-loop policy and the reset reason | portable | `Slot`, `Crash`, `Policy`, `Next`, `Reset`, `Report` | none | The panic handler, the memory it lives in, the log |
//! | [`rohi-crash-esp`](../rohi_crash_esp/index.html) | ESP32-C3 and ESP32-C6 panic path: RTC fast memory slot, handler body, watchdog fallback, software reset | ESP | `on_panic`, `init`, `report`, `pending`, `acknowledge` | `rohi-crash`, `esp-hal`, `esp-backtrace`, `esp-println` | Persisting the record to the log (the firmware does it with `rohi-logging`) |
//! | [`rohi-logging`](../rohi_logging/index.html) | Persistent logging: the `log` sink, segment format, flash ring storage over any `Medium`, `LoggingConfig` and the `logging` service | portable | `LogSink`, `LogRecord`, `LogLevel`, `LogClock`, `Logging`, `LoggingService`, `LoggingConfig`, `LogStorage`, `RingStorage` | `rohi-config`, `rohi-service`, `rohi-time`, `rohi-hal`, Embassy sync, `log` | The flash driver, an SD backend |
//! | [`rohi-connectivity-proto`](../rohi_connectivity_proto/index.html) | Bounded bindings of `airalab/connectivity-protocol` (vendored, pinned revision) | portable | `core_v1`, `crypto_v1`, `device_v1`, `sensor_v1`, `limits` | `micropb` | Signing, mapping, transports |
//! | [`rohi-connectivity`](../rohi_connectivity/index.html) | Telemetry: snapshot (+ location) to signed envelope frame, bounded outbox, retry, HTTP transport, `ConnectivityConfig` section, `connectivity` service | portable | `UrbanEncoder`, `EnvelopeBuilder`, `EnvelopeFrame`, `EnvelopeTransport`, `HttpTransport`, `ConnectivityService`, `UtcClock`, `NonceSource`, `FrameSigner` | `rohi-connectivity-proto`, `rohi-hal`, `rohi-identity`, `rohi-location`, `rohi-net`, `rohi-time`, `rohi-config`, `rohi-service` | Private keys, Wi-Fi lifecycle, sensors, backend logic |
//! | [`rohi-net-esp`](../rohi_net_esp/index.html) | ESP backend of `rohi-net` | ESP | `EspWireless`, `EspWifi`, `EspIp`, `EspBle`, `NetResources`, `BleResources`, `wireless_heap!` | `rohi-net`, `esp-radio`, `embassy-net`, TrouBLE | Product resource sizes, control protocol |
//! | [`rohi-control-proto`](../rohi_control_proto/index.html) | Bounded protobuf schema of the control protocol | portable | generated messages, `PROTOCOL_VERSION`, `limits` | `micropb` | Transport, behaviour |
//! | [`rohi-control`](../rohi_control/index.html) | Transport-agnostic control dispatcher and authorization; serves the registries of `rohi-service` | portable | `Controller`, `ControlBackend`, `Device`, `AuthorizationPolicy` | `rohi-control-proto`, `rohi-service` | BLE, stores, the radio, any knowledge of a module (Wi-Fi, sensors, ...) |
//! | [`rohi-control-ble`](../rohi_control_ble/index.html) | Firmware-side (peripheral) GATT binding: UUIDs, framing, fragmentation, control loop; the host tool `rohi` reuses only `frame` and `uuid` | portable | `ControlBle`, `frame` | `rohi-control`, `rohi-control-proto`, `rohi-net` | The BLE stack |
//! | `altruist-urban` (`firmwares/altruist-urban`) | Product composition: the portable application (always) and one platform: ESP (`esp` module, chip features) or host (`host` module, feature `std`, binary `altruist-urban`), resource budget | product | `config`, `net`, `control`, `time`, `connectivity`, `identity`, `location`, `tasks`; with a chip feature `esp::{config, net, control, resources, sensors, ...}`; with `std` `host::{app, args, control, process}` | `rohi-sdk` (the portable and ESP crates above) | Reusable library code |
//! | [`rohi-control-tcp`](../rohi_control_tcp/index.html) | Control protocol over TCP: 4-byte length-prefixed Protobuf records, the serving loop and the `Acceptor` seam (independent of BLE framing) | portable | `ControlTcp`, `Acceptor`, `frame` | `rohi-control`, `rohi-control-proto`, `embedded-io-async` | Sockets, security |
//! | [`rohi-net-std`](../rohi_net_std/index.html) | `std` backend of `rohi-net`: host DNS, TCP and UDP, the TCP acceptor of the control transport, the BlueZ BLE peripheral (feature `ble`, Linux) | host (`std`) | `NativeIp`, `TcpAcceptor`, `runtime`, `ble::BluezBle` | `rohi-net`, `rohi-control-tcp`, `tokio`, `bluer` (feature `ble`) | Wi-Fi, simulation, application logic |
//! | `rohi-sim` (`crates/sim`) | Simulated world of a `std` application: sensors, Wi-Fi, IP faults, NOR flash in a file for the configuration and the log, scenarios, the management interface | host (`std`) | `World`, `Scenario`, `Command`, `FileFlash`, `FileLogFlash`, `SimWifi`, `SimIp`, `management` | `rohi-hal`, `rohi-net`, `rohi-config`, `rohi-net-std`, Embassy | The device control protocol, application logic |
//! | `rohi-examples` (`examples/`) | One hardware example per sensor and a config self-test | product | binaries | `altruist-urban`, `rohi-sdk` | |
//! | [`rohi`](../rohi/index.html) (`tools/rohi`) | Desktop client library and CLI over BLE and TCP | desktop (`std`) | `Device`, `Session`, `BleLink`, `TcpLink` | `rohi-control-ble` (`frame`), `rohi-control-tcp` (`frame`), `rohi-control-proto`, `rohi-net` (BLE payload size), `rohi-identity`, `btleplug` | Anything that runs on the device |
//! | [`rohi-sdk`](crate) | Umbrella crate: feature-gated re-exports of the crates above and this documentation | portable | feature-gated modules (`service`, `hal`, `net`, `esp`, ...) | the crates above, each optional | |
//!
//! The firmware and the examples are built per chip and are not part of the
//! host documentation tree; see
//! [Targets and features](https://docs.rs/rohi-sdk/latest/rohi_sdk/portability/index.html#targets-and-features).
//!
//! ## Allowed dependency direction
//!
//! ```text
//! altruist-urban, examples ──► rohi-sdk (features) ──► rohi-*-esp ──► rohi-* (portable) ──► embedded-* / embassy-sync
//!                                                       └─► never esp-*
//! rohi-hal-esp ──► rohi-hal            rohi-config-esp ──► rohi-config
//! rohi-net-esp ──► rohi-net
//!
//! rohi-sdk ──► every crate below, each behind a feature; nothing depends on rohi-sdk
//! rohi-config ──► rohi-config-derive          (#[derive(ConfigSection)])
//! rohi-service ──► rohi-config, rohi-config-derive, rohi-service-derive
//!                                             (generic model: Value, Config, Status, Action, registries)
//! rohi-hal, rohi-net, rohi-identity, rohi-location, rohi-system, rohi-time, rohi-connectivity ──► rohi-service
//!                                             (each implements the traits for its own types)
//! rohi-net, rohi-identity, rohi-location ──► rohi-config
//!                                             (each owns its config sections and their overrides)
//! rohi-time ──► rohi-hal                      (monotonic Clock, Backoff)
//! rohi-time-ntp ──► rohi-time, rohi-net, rohi-hal, rohi-config, rohi-service
//!                                             (a source: DNS + UDP samples, owns the `time` section;
//!                                              rohi-time never depends on it)
//! rohi-connectivity ──► rohi-connectivity-proto, rohi-hal, rohi-identity, rohi-location,
//!                       rohi-net, rohi-time, rohi-config, rohi-service
//!                                             (snapshots, location, signing, NetworkHandle, UtcClock)
//!
//! rohi-control-ble ──► rohi-control ──► rohi-control-proto
//! rohi-control-ble ──► rohi-net               (BleTransport)
//! rohi-control ──► rohi-control-proto, rohi-service   (nothing else: it knows no module)
//! ```
//!
//! Forbidden: a portable crate depending on an `*-esp` crate; `rohi-net*`
//! depending on `rohi-control*`; `rohi-hal` depending on `rohi-net`; a platform
//! crate depending on the firmware; any crate of the workspace except products
//! depending on `rohi-sdk` (it sits above all of them).
//!
//! ## Where does new code go?
//!
//! | You are adding... | Put it in | Notes |
//! |-------------------|-----------|-------|
//! | A new sensor driver | `rohi-hal` (`sensors/`) | [Add a sensor](https://rohi.akagi.dev/guides/add-sensor.html); a new board only needs wiring in `rohi-hal-esp` |
//! | Pins or bus setup for a board | `rohi-hal-esp` (`board/`) | [Add a board](https://rohi.akagi.dev/guides/add-board.html) |
//! | A new persistent configuration section | The crate of the feature that interprets it | [Add a ConfigSection](https://rohi.akagi.dev/guides/add-config-section.html); never in `rohi-config` |
//! | Flash or storage backend | `rohi-config-esp` (or a new `*-esp` crate) | Implement `ConfigStorage` |
//! | An absolute-time source (RTC, external time) | A new portable crate implementing `rohi_time::TimeSource` | Consumers keep using `TimeService`; see [`rohi-time`](../rohi_time/index.html) |
//! | DNS/TCP/UDP behaviour, TLS/HTTP/uplink protocols | A new portable crate over `NetworkHandle` / the `embedded-nal-async` traits | Never use `embassy-net` directly; see [Use IP networking](https://rohi.akagi.dev/guides/use-network.html) |
//! | Another IP stack or radio | A platform crate implementing `IpBackend` / `WifiBackend` | Vendor types stay inside |
//! | A service (status, config, actions) of a runtime | The crate that owns the runtime; register it in the firmware registries | [Add a service](https://rohi.akagi.dev/guides/add-service.html); no protocol or CLI change |
//! | A control operation that is none of config, status or action | `rohi-control-proto` (schema), `rohi-control` (dispatch, `ControlBackend`) | [Add a control operation or an action](https://rohi.akagi.dev/guides/add-control-operation.html) |
//! | Product resource sizes and task wiring | The firmware (`resources.rs`, `tasks/`) | The product owns the RAM budget, see [`memory`] |
//! | Anything that needs `esp-hal` / `esp-radio` | A `*-esp` platform crate | Keep the portable API free of vendor types |
//!
//! # Further reading
//!
//! Exact Rust APIs are in the generated API documentation of each crate
//! (`cargo doc --no-deps`, published on docs.rs for released crates). The
//! [book](https://rohi.akagi.dev/) keeps the product view, the guides and the
//! workflows. Resource limits are in [`memory`], targets and features in
//! [`portability`] and persistent formats in [`versioning`] and `rohi-config`.

pub mod architecture;
pub mod cancellation;
pub mod memory;
pub mod ownership;
pub mod portability;
pub mod versioning;

#[cfg(feature = "config")]
#[cfg_attr(docsrs, doc(cfg(feature = "config")))]
pub use rohi_config as config;
#[cfg(feature = "connectivity")]
#[cfg_attr(docsrs, doc(cfg(feature = "connectivity")))]
pub use rohi_connectivity as connectivity;
#[cfg(feature = "control")]
#[cfg_attr(docsrs, doc(cfg(feature = "control")))]
pub use rohi_control as control;
#[cfg(feature = "ble")]
#[cfg_attr(docsrs, doc(cfg(feature = "ble")))]
pub use rohi_control_ble as ble;
#[cfg(feature = "tcp")]
#[cfg_attr(docsrs, doc(cfg(feature = "tcp")))]
pub use rohi_control_tcp as tcp;
#[cfg(feature = "hal")]
#[cfg_attr(docsrs, doc(cfg(feature = "hal")))]
pub use rohi_hal as hal;
#[cfg(feature = "identity")]
#[cfg_attr(docsrs, doc(cfg(feature = "identity")))]
pub use rohi_identity as identity;
#[cfg(feature = "location")]
#[cfg_attr(docsrs, doc(cfg(feature = "location")))]
pub use rohi_location as location;

#[cfg(feature = "crash")]
#[cfg_attr(docsrs, doc(cfg(feature = "crash")))]
pub use rohi_crash as crash;
#[cfg(feature = "logging")]
#[cfg_attr(docsrs, doc(cfg(feature = "logging")))]
pub use rohi_logging as logging;
#[cfg(feature = "net")]
#[cfg_attr(docsrs, doc(cfg(feature = "net")))]
pub use rohi_net as net;
#[cfg(feature = "service")]
#[cfg_attr(docsrs, doc(cfg(feature = "service")))]
pub use rohi_service as service;
#[cfg(feature = "sim")]
#[cfg_attr(docsrs, doc(cfg(feature = "sim")))]
pub use rohi_sim as sim;
#[cfg(feature = "system")]
#[cfg_attr(docsrs, doc(cfg(feature = "system")))]
pub use rohi_system as system;
#[cfg(feature = "time")]
#[cfg_attr(docsrs, doc(cfg(feature = "time")))]
pub use rohi_time as time;
#[cfg(feature = "ntp")]
#[cfg_attr(docsrs, doc(cfg(feature = "ntp")))]
pub use rohi_time_ntp as ntp;

/// Host backends of the portable areas, available with the `host` feature: the
/// network of an application that runs as a process on `std`, see
/// [`rohi_net_std`].
#[cfg(feature = "host")]
#[cfg_attr(docsrs, doc(cfg(feature = "host")))]
pub mod host {
    pub use rohi_net_std::*;
}

/// ESP32 backends of the portable areas, available with the `esp` feature.
///
/// The chip is selected by the `esp32c3` or `esp32c6` feature of this crate;
/// exactly one must be enabled by a final firmware build, see
/// [`portability`].
#[cfg(feature = "esp")]
#[cfg_attr(docsrs, doc(cfg(feature = "esp")))]
pub mod esp {
    /// Flash backend of the configuration store, see [`rohi_config_esp`].
    pub use rohi_config_esp as config;
    /// Panic handler body, reset-retained crash slot and watchdog fallback, see
    /// [`rohi_crash_esp`].
    pub use rohi_crash_esp as crash;
    /// Boards, buses and clock of the HAL, see [`rohi_hal_esp`].
    pub use rohi_hal_esp as hal;
    /// ESP backend of the network, see [`rohi_net_esp`].
    pub use rohi_net_esp as net;
}
