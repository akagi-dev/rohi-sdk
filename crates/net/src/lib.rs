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

//! Robonomics Open Hardware Interface (ROHI): platform-independent networking.
//!
//! `rohi-net` is the portable networking layer of the SDK. It turns "which
//! Wi-Fi network should this device use, and is it connected?", "can I
//! resolve a name, open a TCP connection or send a UDP datagram?" and "how do
//! bytes reach a connected BLE central?" into three small, separately owned
//! interfaces. Everything is expressed in ROHI and standard embedded Rust
//! types; the crate is `no_std`, never allocates, has no ESP dependency and is
//! tested on a regular host. The ESP32 implementation of its backend traits is
//! the platform crate [`rohi-net-esp`](../rohi_net_esp/index.html).
//!
//! Use it when firmware needs a Wi-Fi station with automatic reconnection, a
//! typed IP data plane for higher layers (TLS, HTTP, NTP, uplink), a BLE byte
//! pipe, or the `wifi`, `network` and `ble` sections and services of the
//! management plane ([`rohi_service`], [`rohi_config`]).
//!
//! # Quick example
//!
//! A [`WifiManager`] owns the radio backend and reconciles it towards the
//! desired configuration; every other task only holds a [`WifiHandle`]. This
//! example drives the manager by hand against a fake radio. On a device the
//! backend is `EspWifi` and [`WifiManager::run`] is the body of one task, see
//! `firmwares/altruist-urban/src/net.rs`.
//!
//! ```
//! use core::future::pending;
//! use core::net::Ipv4Addr;
//!
//! use embassy_futures::block_on;
//! use embassy_sync::blocking_mutex::raw::NoopRawMutex;
//! use embedded_hal_async::delay::DelayNs;
//! use rohi_net::{
//!     LinkConfig, ManagerConfig, NetworkInfo, WifiBackend, WifiConfig, WifiError, WifiManager,
//!     WifiPassword, WifiScanResults, WifiShared, WifiSsid, WifiState,
//! };
//!
//! // A radio that always joins the network and gets 192.168.1.20.
//! struct FakeRadio;
//!
//! impl WifiBackend for FakeRadio {
//!     async fn scan(&mut self) -> Result<WifiScanResults, WifiError> {
//!         Ok(WifiScanResults::new())
//!     }
//!
//!     async fn connect(&mut self, config: &LinkConfig) -> Result<NetworkInfo, WifiError> {
//!         Ok(NetworkInfo {
//!             ssid: config.wifi.ssid.clone().ok_or(WifiError::InvalidConfig)?,
//!             rssi_dbm: -52,
//!             channel: Some(6),
//!             mac: None,
//!             ipv4: Ipv4Addr::new(192, 168, 1, 20),
//!             gateway: Some(Ipv4Addr::new(192, 168, 1, 1)),
//!             dns: None,
//!         })
//!     }
//!
//!     async fn disconnect(&mut self) -> Result<(), WifiError> {
//!         Ok(())
//!     }
//!
//!     async fn wait_link_lost(&mut self) -> WifiError {
//!         pending().await
//!     }
//!
//!     fn network_info(&mut self) -> Option<NetworkInfo> {
//!         None
//!     }
//! }
//!
//! // Timers never fire in this example.
//! struct NeverDelay;
//!
//! impl DelayNs for NeverDelay {
//!     async fn delay_ns(&mut self, _ns: u32) {
//!         pending().await
//!     }
//! }
//!
//! let shared = WifiShared::<NoopRawMutex>::new();
//! let wifi = shared.handle();
//! let mut manager = WifiManager::new(FakeRadio, NeverDelay, &shared, ManagerConfig::DEFAULT);
//! assert_eq!(wifi.status().state, WifiState::NotConfigured);
//!
//! // Publishing the desired configuration is synchronous and never fails.
//! let ssid = WifiSsid::from_bytes(b"office").unwrap();
//! let password = WifiPassword::from_bytes(b"correct horse").unwrap();
//! wifi.set_wifi_config(WifiConfig::new(ssid, Some(password)));
//!
//! // Two transitions: take the configuration, then connect.
//! block_on(async {
//!     manager.step().await;
//!     manager.step().await;
//! });
//! assert!(wifi.status().is_network_ready());
//! assert_eq!(wifi.network_info().unwrap().ipv4, Ipv4Addr::new(192, 168, 1, 20));
//! ```
//!
//! The IP data plane has its own runnable example in [`ip`], and every module
//! below has small examples of the types it defines.
//!
//! # Core concepts
//!
//! * **Three planes, three owners.** The Wi-Fi *control plane* ([`wifi`],
//!   [`WifiHandle`]) decides which network the radio uses and reports the
//!   link. The IP *data plane* ([`ip`], [`NetworkHandle`]) carries DNS, TCP
//!   and UDP once the link is usable. The BLE *transport* ([`ble`],
//!   [`BleTransport`]) is a bounded byte pipe to one central. They are
//!   independent: Wi-Fi never depends on BLE and the other way round.
//! * **Connected is not ready.** [`WifiState::Connected`] means *network
//!   ready*: association **and** the IPv4 configuration (DHCP lease or static
//!   address) are complete. The data plane has its own readiness check and
//!   fails every operation with [`NetError::NetworkUnavailable`] while it is
//!   false, so callers can tell an unusable network from a DNS or connect
//!   failure.
//! * **Desired state versus commands.** The persistent [`WifiConfig`] and
//!   [`NetworkConfig`] are *state*: the latest published value wins and
//!   updates published before the manager ran are coalesced.
//!   `scan`, `reconnect` and `disconnect` are *commands* in a bounded queue
//!   ([`WIFI_COMMAND_QUEUE_DEPTH`]) that fail with [`WifiError::Busy`] when it
//!   is full.
//! * **Backends are traits.** [`WifiBackend`], [`IpBackend`] and
//!   [`BleTransport`] are the only places where a platform plugs in; no
//!   vendor type appears in any public signature of this crate and vendor
//!   errors become [`WifiError`], [`NetError`] and [`BleError`].
//! * **Management plane.** The radio side of the device appears as three
//!   config sections and three services named `wifi`, `network` and `ble`
//!   (for example `rohi config wifi`, `rohi status network`).
//!
//! | | `wifi` | `network` | `ble` |
//! |---|---|---|---|
//! | Config | [`WifiConfig`] | [`NetworkConfig`] | [`BleSettings`] |
//! | Service | [`WifiService`] | [`NetworkService`] | [`BleService`] |
//! | Platform readings | [`WifiProbe`] | [`NetworkProbe`] | [`BleProbe`] |
//! | Actions | `scan`, `reconnect`, `disconnect` | none | none |
//!
//! Each section holds only the properties of its own target; the properties
//! are documented where they are implemented. A field a platform cannot tell
//! (heap, clock, socket counts) is not reported at all.
//!
//! # Architecture and integration
//!
//! ```text
//!  application / control plane / NTP / future uplink (TLS, HTTP)
//!         │                              │                       │
//!    WifiHandle                    NetworkHandle           BleTransport
//!    Wi-Fi control plane           IP data plane           byte pipe to a central
//!    configure · scan · status     DNS · TCP · UDP         recv · send · link info
//!    reconnect · disconnect        embedded-nal-async
//!         │                        + embedded-io-async           │
//!    WifiManager ── WifiBackend    IpBackend                     │
//!         │                              │                       │
//!         └────────────── rohi-net-esp: EspWifi · EspIp · EspBle ┘
//!                                │
//!              esp-radio · embassy-net · TrouBLE · esp-rtos · radio heap
//! ```
//!
//! | Piece | Provided by | Notes |
//! |-------|-------------|-------|
//! | [`WifiManager`], [`WifiShared`], [`WifiHandle`] | this crate | One manager task per radio; `WifiShared` lives in a `static`. |
//! | [`WifiBackend`], [`IpBackend`], [`BleTransport`] | implemented by `rohi-net-esp` | Cancel-safe futures; every error already mapped. |
//! | [`WifiService`], [`NetworkService`], [`BleService`] | this crate, generic over probes | Register them in the service registry of [`rohi_service`]. |
//! | `wifi`, `network`, `ble` sections | [`WifiConfig`], [`NetworkConfig`], [`BleSettings`] | Persisted as overrides ([`WifiOverrides`], [`NetworkOverrides`], [`BleOverrides`]) by [`rohi_config`]. |
//! | BLE control link | [`rohi-control`](../rohi_control/index.html) and [`rohi-control-ble`](../rohi_control_ble/index.html) | Consume [`BleTransport`]; this crate only moves bytes. |
//! | NTP time source | [`rohi-time-ntp`](../rohi_time_ntp/index.html) | Built on [`UdpSocket`]. |
//!
//! The Altruist Urban firmware wires these pieces in `firmwares/altruist-urban`:
//! `src/net.rs` (manager task, probes), `src/config.rs` (the configuration
//! owner publishes the effective [`WifiConfig`] and [`NetworkConfig`] right
//! after a commit and once at boot), `src/resources.rs` (sizes of every static
//! resource). The platform side is described in the
//! [`rohi-net-esp`](../rohi_net_esp/index.html) crate documentation.
//!
//! # API navigation
//!
//! | Module | Start with |
//! |--------|------------|
//! | [`wifi`] | [`WifiManager`], [`WifiHandle`], [`WifiBackend`], [`WifiConfig`], [`WifiState`], [`WifiStatus`], [`WifiError`], [`ScanCollector`], [`Backoff`], [`WifiService`] |
//! | [`network`] | [`NetworkConfig`], [`AddressMode`], [`StaticIpv4`], [`Hostname`], [`NetworkService`] |
//! | [`ip`] | [`NetworkHandle`], [`IpBackend`], [`UdpSocket`], [`NetError`], [`IpUsage`] |
//! | [`ble`] | [`BleTransport`], [`BleConfig`], [`BleGattSpec`], [`BleState`], [`BleError`], [`BleSettings`], [`BleService`] |
//! | [`limits`] | The compile-time bounds, [`MAX_SSID_LEN`] and friends |
//! | `testing` | `FakeIp`, an in-memory [`IpBackend`] (feature `testing`, see below) |
//!
//! Service ids are exposed in per-service modules re-exported with a prefix:
//! [`wifi_action`], [`wifi_issue`], [`wifi_status_field`], [`wifi_scan_field`],
//! [`wifi_security`], [`network_issue`], [`network_status_field`],
//! [`ble_issue`], [`ble_state`], [`ble_status_field`]. They are part of the
//! wire interface of the services; the status field ids are only ever added to.
//!
//! # Guarantees and limitations
//!
//! **Errors.** Public failures are typed and platform independent:
//! [`WifiError`] (link and configuration), [`NetError`] (data plane, also an
//! [`embedded_io_async::Error`]) and [`BleError`] (transport). The manager
//! never panics or reboots because of a Wi-Fi failure; it backs off and
//! retries.
//!
//! **Resource bounds.** Nothing allocates. Every buffer and queue is bounded
//! by a constant of [`limits`] or a const generic of the platform backend:
//!
//! | Limit | Value |
//! |-------|-------|
//! | SSID | 0 to [`MAX_SSID_LEN`] (32) bytes, non-empty when configured |
//! | Wi-Fi password | [`MIN_WIFI_PASSWORD_LEN`] (8) to [`MAX_WIFI_PASSWORD_LEN`] (64) bytes |
//! | Scan result | [`MAX_WIFI_SCAN_RESULTS`] (16) networks; a client of the `wifi` service gets at most 6 records |
//! | Manager command queue | [`WIFI_COMMAND_QUEUE_DEPTH`] (4) |
//! | Status receivers | [`STATUS_RECEIVERS`] (3) |
//! | DHCP host name | [`MAX_HOSTNAME_LEN`] (32) bytes |
//! | BLE name / payload | [`MAX_BLE_NAME_LEN`] (29) / [`MAX_BLE_PAYLOAD`] (244) bytes |
//!
//! TCP connections, UDP sockets and their buffers are sized by the product in
//! the platform backend and fail immediately with
//! [`NetError::NoSocketAvailable`] when exhausted. The numbers of the whole
//! SDK are collected in <https://rohi.akagi.dev/reference/resource-limits.html>.
//!
//! **Cancellation.** Publishing a configuration is synchronous, so a caller
//! cannot be cancelled between persisting and publishing. `scan`, `connect`,
//! `resolve`, `recv_from` and the BLE futures are cancel-safe; the details are
//! on each item.
//!
//! **Security.** [`WifiPassword`] has a redacting `Debug`, nothing logs
//! configuration contents and [`NetworkInfo`] has no credential field. Only
//! the platform backend reads the secret ([`WifiPassword::expose_secret`]).
//! The control API never returns the password and accepts it write-only (see
//! [`rohi-control`](../rohi_control/index.html)). The data plane has no TLS: do
//! not send secrets over [`NetworkHandle`] without a TLS layer on top. The BLE
//! link properties are reported ([`BleConnectionInfo`]), but the support matrix
//! records that there is no BLE pairing or encryption yet.
//!
//! **Out of scope.** SoftAP, TLS/HTTP(S), multiple saved networks, WPA
//! enterprise, IPv6. The ESP backend requests WPA/WPA2 personal security for
//! a configured password, so WPA3-only networks are not supported.
//!
//! **Features.** None by default. `testing` enables `rohi_net::testing`, the
//! in-memory `FakeIp` backend for host tests of code built on
//! [`NetworkHandle`]; it is not meant for firmware. This crate's own tests
//! enable it through a dev-dependency.
//!
//! **Validation status.** Host-tested: yes (unit tests, doctests and
//! `tests/network_example.rs`; see `docs/validation/host-testing.md`). Target
//! builds: the ESP backend and the Altruist Urban firmware are built in CI for
//! ESP32-C3 and ESP32-C6. Hardware-verified: **not yet**: the support matrix
//! lists `wifi-ip` as `not-verified` for both Altruist Urban variants, and
//! scan, connect/DHCP, reconnect, wrong password, DNS/TCP through
//! [`NetworkHandle`], BLE and Wi-Fi coexistence and 24 h stability are listed
//! as not yet measured. The only recorded data point is one wireless heap
//! measurement with Wi-Fi and BLE running (chip not recorded). See
//! <https://rohi.akagi.dev/validation/hardware-testing.html> and
//! <https://rohi.akagi.dev/validation/host-testing.html>. How a product uses
//! the layers: <https://rohi.akagi.dev/guides/use-network.html> and
//! <https://rohi.akagi.dev/platform/wireless.html>.

#[cfg(test)]
extern crate std;

pub mod ble;
pub mod ip;
pub mod limits;
pub mod network;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod wifi;

pub use ble::{
    BleConfig, BleConnectionInfo, BleError, BleGattSpec, BleName, BleOverrides, BleProbe,
    BleService, BleSettings, BleState, BleTransport, DEFAULT_BLE_PAYLOAD, MAX_BLE_NAME_LEN,
    MAX_BLE_PAYLOAD, STATES as BLE_STATES, issue as ble_issue, state as ble_state,
    status_field as ble_status_field,
};
pub use ip::{IpBackend, IpUsage, NetError, NetworkHandle, UdpSocket};
pub use limits::{
    MAX_SSID_LEN, MAX_WIFI_PASSWORD_LEN, MAX_WIFI_SCAN_RESULTS, MIN_WIFI_PASSWORD_LEN,
    WIFI_COMMAND_QUEUE_DEPTH,
};
pub use network::{
    ADDRESS_MODE_ENUM, ADDRESS_MODES, AddressMode, DEFAULT_DHCP_TIMEOUT_MS, DHCP_TIMEOUT_RANGE_MS,
    Hostname, Ipv4, MAX_HOSTNAME_LEN, NetworkConfig, NetworkOverrides, NetworkProbe,
    NetworkService, StaticIpv4, address_mode, issue as network_issue,
    status_field as network_status_field,
};
pub use wifi::{
    BACKOFF_INITIAL_RANGE_MS, BACKOFF_MAX_RANGE_MS, Backoff, CONNECT_TIMEOUT_RANGE_MS,
    DEFAULT_BACKOFF_INITIAL_MS, DEFAULT_BACKOFF_MAX_MS, DEFAULT_CONNECT_TIMEOUT_MS,
    DEFAULT_LINK_REFRESH_MS, DEFAULT_SCAN_TIMEOUT_MS, HeapStats, LINK_REFRESH_RANGE_MS, LinkConfig,
    LinkMetrics, ManagerConfig, NetworkInfo, SCAN_TIMEOUT_RANGE_MS, STATUS_RECEIVERS,
    ScanCollector, WifiBackend, WifiConfig, WifiError, WifiHandle, WifiManager, WifiNetwork,
    WifiOverrides, WifiPassword, WifiProbe, WifiScanResults, WifiSecurity, WifiService, WifiShared,
    WifiSsid, WifiState, WifiStatus, action as wifi_action, issue as wifi_issue,
    scan_field as wifi_scan_field, security as wifi_security, status_field as wifi_status_field,
};
