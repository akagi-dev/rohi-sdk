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
//! Runtime ownership: one owner per stateful service, state versus command.
//!
//! > **Rule.** Stateful drivers and services have **one mutable runtime
//! > owner**; other tasks interact with them through bounded handles,
//! > snapshots or commands.
//!
//! There are no global mutable stores and no shared locks around drivers. The
//! owner is an Embassy task (or, for the radio, one runtime object);
//! everything else holds a small copyable handle into a bounded, statically
//! allocated channel.
//!
//! | Service | Owner | Handle | Reached through |
//! |---------|-------|--------|-----------------|
//! | Measurements | the store task owns `MeasurementStore` | `MeasurementHandle` ([`rohi-hal`](../../rohi_hal/runtime/index.html)) | bounded snapshot request/response |
//! | Configuration | the configuration task of the firmware owns `ConfigStore` | `ConfigHandle` (firmware, `src/config.rs`) | bounded, numbered request/response |
//! | Wi-Fi | the `WifiManager` task owns the Wi-Fi backend | `WifiHandle` ([`rohi-net`](../../rohi_net/wifi/index.html)) | desired-state cell, bounded commands, published status |
//! | IP stack | the platform stack (`EspIp`) behind the stack runner task | `NetworkHandle` ([`rohi-net`](../../rohi_net/ip/index.html)) | DNS, TCP and UDP through standard traits |
//! | ESP wireless resources | `EspWireless` (radio peripherals, wireless heap) | the drivers it hands out once | `wifi()`, `ble()` ([`rohi-net-esp`](../../rohi_net_esp/index.html)) |
//! | BLE peripheral | the `BleRunner` task owns the controller and host stack | `EspBle` (`BleTransport`) | bounded RX/TX queues |
//! | Device identity | the configuration task publishes the stored identity (and so the signer) in the commit step | `IdentityHandle` (firmware, `src/identity.rs`) | runtime state, writes go through `ConfigHandle` |
//! | Telemetry | the telemetry task owns the `ConnectivityService` | `ConnectivityShared` ([`rohi-connectivity`](../../rohi_connectivity/index.html)) | latest-configuration signal, status; it only reads measurement snapshots |
//!
//! ```text
//! MeasurementStore  ─ one owner task ─►  MeasurementHandle
//! ConfigStore       ─ one owner task ─►  ConfigHandle
//! WifiManager       ─ one owner      ─►  WifiHandle
//! ESP wireless resources ─ one owner / runtime ─► Wi-Fi + BLE
//! ```
//!
//! Why: a single owner makes ordering explicit (requests are served one at a
//! time), keeps flash and radio access out of timing-critical tasks, makes
//! cancellation analysable (see [`cancellation`](crate::cancellation)) and lets
//! the owner be tested on the host with a mock backend.
//!
//! # Consequences
//!
//! - Nothing outside the configuration module of the firmware
//!   (`altruist_urban::config`) touches the configuration flash; nothing
//!   outside the Wi-Fi manager task calls the Wi-Fi driver; sensor tasks never
//!   touch the `MeasurementStore`.
//! - The owner never blocks on a caller that gave up: responses to abandoned
//!   requests are discarded.
//! - Handles are cheap (`Copy`) and are the only thing passed to other crates,
//!   which keeps portable crates independent of the owner's concrete types.
//! - Failure of one owner (for example Wi-Fi) is contained: the others keep
//!   running.
//!
//! # State versus command
//!
//! Two kinds of interaction exist; mixing them in one queue causes subtle
//! bugs:
//!
//! ```text
//! desired persistent state  ──►  latest-state publication / reconciliation
//! transient requested action ──►  bounded command
//! ```
//!
//! > **Guideline.** Prefer state publication over queued commands for
//! > persistent desired state. Use commands for transient actions.
//!
//! | Interaction | Kind | Mechanism |
//! |-------------|------|-----------|
//! | Desired `WifiConfig` and `NetworkConfig` | state | `WifiHandle::set_wifi_config` / `set_network_config` (latest value wins, synchronous) |
//! | Effective `ConnectivityConfig` and NTP servers | state | latest-value signal to the running task, only the newest value matters |
//! | Latest system / Wi-Fi status | state | published `WifiStatus` (`Watch`) |
//! | Scan, reconnect, disconnect | command | bounded queue, `Busy` when full |
//! | One-shot request (snapshot, config read) | command | bounded request/response |
//!
//! **Wi-Fi configuration is the canonical example.** The persistent
//! configuration is *state*: the manager continuously reconciles the actual
//! radio state towards the latest desired value. Rapid updates A, B, C
//! converge on C, an identical update does nothing, and a new value
//! invalidates the retry state of the old one. Had it been a command in the
//! same FIFO as `Scan`, a full queue would have forced the publisher to wait
//! (a cancellable gap after the durable commit, see
//! [durable mutation](crate::cancellation#durable-mutation)), updates could
//! not be coalesced and ordering against transient commands would matter. The
//! reconciliation rules are documented with the Wi-Fi control plane,
//! [`rohi_net::wifi`](../../rohi_net/wifi/index.html).
