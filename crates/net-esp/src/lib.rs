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
///////////////////////////////////////////////////////////////////////////////
#![no_std]

//! Robonomics Open Hardware Interface (ROHI): ESP32 wireless backend.
//!
//! `rohi-net-esp` implements the platform traits of [`rohi_net`] on ESP32-C3
//! and ESP32-C6: the Wi-Fi station driver ([`rohi_net::WifiBackend`]), the IP
//! data plane ([`rohi_net::IpBackend`]) and the BLE peripheral transport
//! ([`rohi_net::BleTransport`]). It sits on `esp-radio`, `embassy-net` and the
//! TrouBLE host stack and converts every vendor type and error into the
//! portable ROHI types, so no `esp-radio`, `esp-hal` or `embassy-net` type is
//! part of a public signature. Use it in a board firmware that needs Wi-Fi
//! and/or BLE; use [`rohi_net`] for everything that does not depend on the
//! chip.
//!
//! # Features and target
//!
//! The crate only has content when exactly one chip feature is enabled.
//! Everything below is therefore written in code font, not linked: these
//! items do not exist on a host build.
//!
//! | Feature | Chip | Rust target | Effect |
//! |---------|------|-------------|--------|
//! | `esp32c3` | ESP32-C3 | `riscv32imc-unknown-none-elf` | Enables the backend for this chip |
//! | `esp32c6` | ESP32-C6 | `riscv32imac-unknown-none-elf` | Enables the backend for this chip |
//! | `heap-stats` | any | any | `WirelessHeap::min_free` is `Some` (adds a small runtime overhead) |
//! | none | none | host | The crate is empty, so the workspace still builds on a host |
//!
//! Enabling both chip features is a `compile_error!`. The final firmware
//! forwards its own chip feature; the Altruist Urban firmware also enables
//! `heap-stats`. Because of the chip requirement the doctests of this crate
//! cannot run on the host and the examples are `text` blocks that point at
//! maintained firmware sources instead.
//!
//! ```text
//! cargo doc --no-deps -p rohi-net-esp --no-default-features \
//!     --features esp32c3 --target riscv32imc-unknown-none-elf
//! ```
//!
//! The Wi-Fi driver needs the `esp-rtos` scheduler to be running and the BLE
//! driver needs the same; `esp-radio` is built with `wifi`, `ble`, `coex` and
//! `esp-alloc`.
//!
//! # Quick example
//!
//! The firmware creates one `EspWireless`, then asks it once for each driver
//! and spawns the runners and the portable managers on Embassy tasks (a
//! sketch; the real, maintained code is `firmwares/altruist-urban/src/bin/main.rs`,
//! `src/net.rs` and `src/control.rs`):
//!
//! ```text
//! let heap = wireless_heap!(WIRELESS_HEAP_SIZE);                 // once, before esp_rtos::start
//! let mut wireless = EspWireless::new(peripherals.WIFI, peripherals.BT, heap);
//! esp_rtos::start(timer, software_interrupt);
//!
//! let (wifi, net_runner, ip) = wireless.wifi(NET_RESOURCES.init(NetResources::new()))?;
//! spawn(net_runner.run());                                       // IP stack task
//! spawn(WifiManager::new(wifi, Delay, &SHARED, settings).run()); // rohi_net::WifiManager
//! let net = rohi_net::NetworkHandle::new(IP.init(ip));           // DNS, TCP, UDP
//!
//! let (ble, ble_runner) = wireless.ble(BLE_CONFIG, BLE_RESOURCES.init(Default::default()))?;
//! spawn(ble_runner.run());                                       // BLE host stack task
//! ```
//!
//! # Core concepts
//!
//! * **One owner of the radio.** Wi-Fi and BLE share one radio, one PHY, the
//!   coexistence arbiter and one allocator. `EspWireless` takes the `WIFI` and
//!   `BT` peripherals and the wireless heap token, and hands out each driver
//!   once. Nothing else may initialize radio state or add heap regions for
//!   the radio.
//! * **The firmware sizes everything.** The heap (`wireless_heap!`), the IP
//!   stack (`NetResources`) and BLE (`BleResources`) are const-generic,
//!   statically allocated and chosen by the product; this crate has no
//!   hidden defaults for socket counts other than the generic defaults noted
//!   on `NetResources`.
//! * **Backends, not managers.** Reconnection, backoff, configuration and
//!   status live in [`rohi_net`] (`WifiManager`); this crate only drives the
//!   radio and the stack. A Wi-Fi or BLE failure surfaces as a typed error and
//!   never stops the firmware.
//! * **Runners.** `NetRunner` (IP stack) and `BleRunner` (BLE controller,
//!   host stack, advertising and GATT server) are futures that never finish;
//!   the firmware runs each on its own task.
//!
//! # Architecture and integration
//!
//! ```text
//!  WifiManager ───── EspWifi ──┐
//!  NetworkHandle ─── EspIp ────┼── EspWireless ── esp-radio · embassy-net · TrouBLE
//!  BleTransport ──── EspBle ───┘                  esp-rtos · one shared radio heap
//! ```
//!
//! | Item | Implements | Role |
//! |------|------------|------|
//! | `EspWifi` and its `NetRunner` | [`rohi_net::WifiBackend`] | Station driver on `esp-radio` and `embassy-net`; the runner is the background task of the IP stack |
//! | `EspIp` with `EspTcpConnection`, `EspUdpSocket` | [`rohi_net::IpBackend`], [`rohi_net::UdpSocket`] | DNS, TCP and UDP on the same `embassy-net` stack |
//! | `EspBle` and its `BleRunner` | [`rohi_net::BleTransport`] | BLE peripheral on `esp-radio` and the TrouBLE host stack |
//! | `EspBleStatus` | [`rohi_net::BleProbe`] | Copyable read-only view for the `ble` service |
//! | `EspWireless`, `wireless_heap!`, `WirelessHeapRegion` | | Shared radio runtime and heap |
//! | `WirelessHeap`, `wireless_heap()` | | Heap usage, also `WirelessHeap::stats` for [`rohi_net::WifiProbe`] |
//!
//! The Altruist Urban firmware wires these pieces in
//! `firmwares/altruist-urban/src/` (`bin/main.rs`: bring-up order; `net.rs`:
//! Wi-Fi tasks and probes; `control.rs`: BLE control task; `resources.rs`: the
//! sizes and the reasoning behind each). The portable side is documented in
//! [`rohi_net`], the control protocol over BLE in `rohi-control-ble`.
//!
//! # API navigation
//!
//! Chip-gated items, in code font because they do not exist on a host build:
//!
//! | Item | Start with |
//! |------|------------|
//! | `EspWireless` | `new`, `wifi`, `ble`, `heap` |
//! | `wireless_heap!`, `WirelessHeapRegion`, `WirelessHeap`, `wireless_heap()` | the heap token and its usage |
//! | `EspWifi`, `NetRunner` | Wi-Fi station backend |
//! | `NetResources`, `EspIp`, `EspTcpConnection`, `EspUdpSocket`, `STACK_INTERNAL_SOCKETS`, `UDP_PACKETS` | IP data plane and its static storage |
//! | `BleResources`, `EspBle`, `EspBleStatus`, `BleRunner` | BLE peripheral and its static storage |
//! | `WirelessError` | `AlreadyTaken`, `BleInit` |
//!
//! Always available: the portable types these backends use, in [`rohi_net`]
//! ([`rohi_net::WifiManager`], [`rohi_net::NetworkHandle`],
//! [`rohi_net::BleConfig`], [`rohi_net::NetError`], ...).
//!
//! # Guarantees and limitations
//!
//! **Memory policy.** Application and runtime code stays heapless. The
//! Espressif wireless stack needs dynamic allocation, so the backend uses a
//! **fixed-size** wireless heap (`esp-alloc`, no growth, internal RAM) that
//! Wi-Fi and BLE share; only the radio drivers allocate from it. Heap-backed
//! vendor results (for example the scan `Vec`) are converted into bounded
//! ROHI types inside the backend and dropped there. The heap usage is
//! reported through `WirelessHeap` and, via [`rohi_net::HeapStats`], by the
//! heap fields of the `wifi` service. With `heap-stats` the minimum free heap
//! is an estimate derived from the allocator's maximum usage.
//!
//! **Resource bounds.**
//!
//! | Bound | Value | Where |
//! |-------|-------|-------|
//! | Sockets the stack needs itself (DHCPv4, DNS) | 2 (`STACK_INTERNAL_SOCKETS`) | `SOCKETS >= TCP + UDP + 2` is checked at compile time |
//! | TCP connections / UDP sockets | const generics `TCP` / `UDP`, at most 32 each | exhausted: immediate [`rohi_net::NetError::NoSocketAvailable`] |
//! | Datagrams queued per UDP socket and direction | 2 (`UDP_PACKETS`) | |
//! | Raw scan entries converted per scan | 48, then reduced to [`rohi_net::MAX_WIFI_SCAN_RESULTS`] | |
//! | BLE chunk | at most [`rohi_net::MAX_BLE_PAYLOAD`] bytes, 20 until the MTU is negotiated | |
//! | BLE queues | `RX_DEPTH`, `TX_DEPTH` chunks | RX full: the ATT response to the write is delayed (backpressure); TX full: `send` waits |
//!
//! The numbers the Altruist Urban firmware chose are in its
//! `src/resources.rs` and in <https://rohi.akagi.dev/reference/resource-limits.html>.
//!
//! **Platform behaviour.** The Wi-Fi backend always requests WPA/WPA2
//! personal security for a configured password and open security without one;
//! WPA3-only networks are not supported. The IP stack is IPv4-only (DHCPv4 or
//! a static address, DNS A records, TCP, UDP); `embassy-net` is built with
//! `tcp`, `udp`, `dns`, `dhcpv4`, `dhcpv4-hostname` and `medium-ethernet`, and
//! features are added only together with the component that needs them (see
//! <https://rohi.akagi.dev/reference/targets-and-features.html#network-stack-features>).
//! The BLE peripheral advertises connectable and scannable with the 128-bit
//! service UUID, puts the name into the scan response and serves one central at
//! a time; it reports the security level the stack negotiated but the crate
//! contains no pairing configuration, and the support matrix records that
//! there is no BLE pairing or encryption yet.
//!
//! **Cancellation.** The `rohi-net` contracts apply: dropping a pending TCP
//! `connect` aborts the attempt and frees its slot, dropping a connection or
//! UDP socket frees its slot, `EspBle::recv` and `send` are cancel-safe.
//!
//! **Validation status.** Target builds: the crate is built and linted in CI
//! for ESP32-C3 and ESP32-C6 as part of the Altruist Urban firmware.
//! Host-tested: no, the crate needs a chip target (the portable logic it
//! drives is host-tested in [`rohi_net`]). Hardware-verified: **not yet**, the
//! support matrix lists `wifi-ip` and `ble-control` as `not-verified` for both
//! Altruist Urban variants. Scan, connect, reconnect, wrong password,
//! coexistence and 24 h stability are not measured; the only recorded data
//! point is one wireless heap measurement with Wi-Fi and BLE running (chip not
//! recorded), which led to the 128 KiB heap of the firmware. See
//! <https://rohi.akagi.dev/validation/hardware-testing.html> and
//! <https://rohi.akagi.dev/platform/wireless.html>.

#[cfg(all(feature = "esp32c3", feature = "esp32c6"))]
compile_error!(
    "select exactly one ESP target: features `esp32c3` and `esp32c6` are mutually exclusive"
);

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod ble;
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod ip;
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod wifi;
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod wireless;

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use ble::{BleResources, BleRunner, EspBle, EspBleStatus};
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use ip::{
    EspIp, EspTcpConnection, EspUdpSocket, NetResources, STACK_INTERNAL_SOCKETS, UDP_PACKETS,
};
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use wifi::{EspWifi, NetRunner};
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use wireless::{EspWireless, WirelessError, WirelessHeap, WirelessHeapRegion, wireless_heap};

/// Implementation detail of [`wireless_heap!`].
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
#[doc(hidden)]
pub mod __private {
    pub use esp_alloc;
}
