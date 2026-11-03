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

//! Wi-Fi control plane: station configuration, scan, status, connect,
//! reconnect and disconnect.
//!
//! [`WifiManager`] is the single owner of a platform [`WifiBackend`] and
//! reconciles the radio towards the latest desired [`WifiConfig`] and
//! [`NetworkConfig`](crate::NetworkConfig). Other tasks use the copyable
//! [`WifiHandle`], a cheap handle to the manager. It does **not** carry DNS,
//! sockets, TLS or HTTP: nothing here carries IP traffic; that is the job of
//! the [IP data plane](crate::ip).
//!
//! | Type | Role |
//! |------|------|
//! | [`WifiConfig`] | Persistent CBOR section `wifi`: `ssid`, optional `password` (open network = `None`), scan and connect timeouts, reconnect backoff and link refresh |
//! | [`NetworkConfig`](crate::NetworkConfig) | Persistent CBOR section `network`: DHCP or static IPv4, host name, DHCP timeout |
//! | [`LinkConfig`] | The two together, as the manager and the backend see them |
//! | [`WifiState`] / [`WifiStatus`] | `NotConfigured`, `Idle`, `Scanning`, `Connecting`, `Connected`, `Disconnected`, `Error` (+ last [`WifiError`]) |
//! | [`NetworkInfo`] | SSID, RSSI, channel, MAC, IPv4, gateway, DNS. Never contains credentials |
//! | [`WifiNetwork`] / [`WifiScanResults`] | Bounded scan result ([`heapless::Vec`]), strongest first |
//! | [`WifiError`] | Stable typed errors (`AuthenticationFailed`, `DhcpFailed`, `Busy`, ...) |
//! | [`WifiShared`] / [`WifiHandle`] | `static` shared state and the handle other tasks use |
//! | [`WifiService`] | The `wifi` service of the management plane |
//!
//! [`WifiState::Connected`] means *network ready*: association **and** DHCP
//! have completed.
//!
//! # The `wifi` section
//!
//! [`WifiConfig`] is the effective configuration (firmware defaults plus the
//! user's overrides, [`WifiOverrides`]) with the properties `ssid`, `password`
//! (secret), `scan_timeout`, `connect_timeout`, `backoff_initial`,
//! `backoff_max` and `link_refresh` (all timings in ms). `ssid` is nullable and
//! `None` is the default: without an `ssid` Wi-Fi is disabled and the radio
//! stays idle, an `ssid` enables it. `ssid=none` disables Wi-Fi explicitly even
//! when the firmware default names a network, `ssid=null` follows the firmware
//! default again. A change of the `wifi` or `network` section reconnects.
//! The timings default to the constants of this module. The `wifi` and
//! `network` services share the link: `network` is `running` when the link is
//! up and reports `dhcp_failed` as its issue; every other link error is an
//! issue of `wifi`.
//!
//! Status fields of the `wifi` service: `ssid`, `rssi`, `channel`, `mac`,
//! `reconnects`, `link_uptime` and the radio heap (`heap_size`, `heap_free`,
//! and `heap_min_free` where the platform tracks it). Actions: `scan`,
//! `reconnect`, `disconnect`. See [`WifiService`].
//!
//! # Desired state versus commands
//!
//! The persistent configuration is **state**, the rest are **transient
//! actions**:
//!
//! ```text
//! persistent WifiConfig + NetworkConfig ──► desired link config ──► WifiManager ──► actual Wi-Fi state
//!                                      (latest value wins)      reconciles
//! Scan · Reconnect · Disconnect ──► bounded command queue ──► WifiManager
//! ```
//!
//! - [`WifiHandle::set_wifi_config`] (the whole effective section: without an
//!   SSID no network is configured, yet [`WifiHandle::wifi_settings`] keeps the
//!   timing, so the scan timeout stays effective) and
//!   [`WifiHandle::set_network_config`] publish the desired configuration.
//!   They are **synchronous**: no `.await`, no queue, never
//!   [`WifiError::Busy`]. The latest value wins; updates published before the
//!   manager ran are coalesced, so rapid updates A, B, C converge directly on C
//!   instead of causing a connect/disconnect cycle for A and B.
//! - [`scan`](WifiHandle::scan), [`reconnect`](WifiHandle::reconnect) and
//!   [`disconnect`](WifiHandle::disconnect) are commands in a bounded queue
//!   ([`WIFI_COMMAND_QUEUE_DEPTH`](crate::WIFI_COMMAND_QUEUE_DEPTH)). They
//!   return [`WifiError::Busy`] when the queue is full, and only one scan can
//!   be in flight.
//! - Who publishes: the firmware's configuration owner task publishes the
//!   committed value right after the commit and once at boot, so a cancelled
//!   caller cannot leave a stored configuration unknown to the runtime.
//!
//! # Reconciliation and reconnection
//!
//! The reconciliation rules per update are listed on [`WifiManager`]. They
//! hold in every phase (connecting, backing off, connected, idle). An update
//! published while a scan runs is applied after the scan. A new configuration
//! invalidates the retry state of the previous one: if AP A had a wrong
//! password and a 60 s backoff, AP B does not inherit the remaining wait.
//!
//! - No configuration: state [`WifiState::NotConfigured`], no retry loop.
//! - Failure or link loss: bounded exponential [`Backoff`] (1 s doubling up to
//!   60 s by default, see [`DEFAULT_BACKOFF_INITIAL_MS`] and
//!   [`DEFAULT_BACKOFF_MAX_MS`]). The backoff is reset only after the link
//!   stayed up for one info refresh interval ([`DEFAULT_LINK_REFRESH_MS`],
//!   10 s), so a flapping link keeps backing off. Failures never panic or
//!   reboot; sensors keep running.
//! - A scan never aborts a connection attempt: one requested while connecting
//!   is served after the attempt. During the backoff wait and while connected
//!   a scan is served at once; it does not restart the backoff or the refresh
//!   timer, but the next step of the manager starts only after the scan
//!   returned.
//!
//! # Secrets
//!
//! [`WifiPassword`] has a redacting `Debug`, nothing logs configuration
//! contents and [`NetworkInfo`] has no credential field. Only the backend
//! reads the secret ([`WifiPassword::expose_secret`]).

mod backend;
mod backoff;
mod config;
mod error;
mod managed;
mod manager;
mod network;
mod scan;
mod service;
mod state;

pub use backend::WifiBackend;
pub use backoff::Backoff;
pub use config::{
    BACKOFF_INITIAL_RANGE_MS, BACKOFF_MAX_RANGE_MS, CONNECT_TIMEOUT_RANGE_MS,
    DEFAULT_BACKOFF_INITIAL_MS, DEFAULT_BACKOFF_MAX_MS, DEFAULT_CONNECT_TIMEOUT_MS,
    DEFAULT_LINK_REFRESH_MS, DEFAULT_SCAN_TIMEOUT_MS, LINK_REFRESH_RANGE_MS, LinkConfig,
    SCAN_TIMEOUT_RANGE_MS, WifiConfig, WifiOverrides, WifiPassword, WifiSsid,
};
pub use error::WifiError;
pub use manager::{
    LinkMetrics, ManagerConfig, STATUS_RECEIVERS, WifiHandle, WifiManager, WifiShared,
};
pub use network::NetworkInfo;
pub use scan::{ScanCollector, WifiNetwork, WifiScanResults, WifiSecurity};
pub(crate) use service::summary_of as service_summary;
pub use service::{
    HeapStats, WifiProbe, WifiService, action, issue, scan_field, security, status_field,
};
pub use state::{WifiState, WifiStatus};
