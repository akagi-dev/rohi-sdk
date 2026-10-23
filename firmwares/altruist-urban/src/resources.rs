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

//! Static resource budget of the Altruist Urban firmware.
//!
//! This file is the authoritative RAM and capacity budget of the product for
//! the wireless stack: platform crates (`rohi-net-esp`) provide the
//! mechanisms, the product chooses the sizes. Everything here is fixed at
//! compile time; nothing grows at runtime. Each constant documents what
//! consumes it, why it has its value, what happens when it is exhausted, and
//! what has been measured. The measurements come from hardware runs; values
//! marked *not measured* have not been validated on a device yet.

use rohi_net_esp::STACK_INTERNAL_SOCKETS;

/// Size of the wireless heap in bytes.
///
/// - Consumer: the Espressif Wi-Fi and BLE controllers (`esp-radio`), which
///   allocate from the single fixed region added by `wireless_heap!`. ROHI
///   application code never allocates from it.
/// - Why: Wi-Fi alone ran with 72 KiB; with Wi-Fi and BLE active 96 KiB left
///   only 11 KB minimum free on hardware, so 128 KiB keeps a margin.
/// - Exhausted: a radio driver allocation fails; the driver reports an error
///   (Wi-Fi: `WifiError::DriverError`, BLE: `BleState::Error`) and the rest
///   of the firmware keeps running.
/// - Measured: minimum free heap with both radios active is reported by
///   `rohi_net_esp::wireless_heap()` (`heap-stats`) and logged by the store
///   task. Current high-water mark with the final value: *not measured*.
pub const WIRELESS_HEAP_SIZE: usize = 128 * 1024;

/// Simultaneous TCP connections of the IP data plane.
///
/// - Consumer: `NetworkHandle::connect`. Each connection owns one entry of
///   the pool in `NetResources` ([`TCP_RX_BUFFER`] + [`TCP_TX_BUFFER`] bytes).
/// - Why: the roadmap needs one uplink connection at a time; one more is
///   headroom for a connection that is being replaced.
/// - Exhausted: `connect` fails immediately with `NetError::NoSocketAvailable`;
///   it never waits for a free slot.
/// - Measured: not measured (no component opens connections yet).
pub const TCP_CONNECTIONS: usize = 2;

/// Receive buffer of one TCP connection, in bytes.
///
/// - Consumer: each TCP connection, see [`TCP_CONNECTIONS`].
/// - Why: the `embassy-net` default; enough for small request/response
///   exchanges. Revisit when a TLS or HTTP layer is added.
/// - Exhausted: the peer is flow-controlled (TCP window), nothing is lost.
/// - Measured: not measured.
pub const TCP_RX_BUFFER: usize = 1024;

/// Transmit buffer of one TCP connection, in bytes.
///
/// - Consumer and why: as [`TCP_RX_BUFFER`].
/// - Exhausted: writes wait until the peer acknowledged data.
/// - Measured: not measured.
pub const TCP_TX_BUFFER: usize = 1024;

/// Socket slots of the IP stack.
///
/// - Consumer: DHCPv4 and DNS (`STACK_INTERNAL_SOCKETS`) plus every TCP
///   connection.
/// - Why: derived, so the stack can never have fewer slots than the TCP pool
///   plus its own sockets; the stack aborts when it runs out of slots, which
///   `NetResources` rules out at compile time.
/// - Exhausted: cannot happen by construction; see [`TCP_CONNECTIONS`] for
///   the user-visible limit.
pub const NETWORK_SOCKETS: usize = TCP_CONNECTIONS + STACK_INTERNAL_SOCKETS;

/// Chunks buffered from the BLE central before its write is held back.
///
/// - Consumer: the BLE RX queue of `rohi-net-esp` (one `MAX_BLE_PAYLOAD`
///   chunk per entry, about 250 B).
/// - Why: the control protocol has one request in flight; a few chunks cover
///   the fragments of a request arriving back to back.
/// - Exhausted: the ATT response to the central's write is delayed until
///   there is room (backpressure); nothing is dropped.
/// - Measured: not measured.
pub const BLE_RX_DEPTH: usize = 4;

/// Chunks buffered for BLE notification.
///
/// - Consumer: the BLE TX queue (about 250 B per entry).
/// - Why: a response is fragmented into notifications; a few entries let the
///   control task queue a fragment burst without waiting.
/// - Exhausted: `BleTransport::send` waits for room.
/// - Measured: not measured.
pub const BLE_TX_DEPTH: usize = 4;

/// Simultaneous BLE connections.
///
/// - Consumer: the TrouBLE host stack.
/// - Why: the device is a peripheral serving one control client.
/// - Exhausted: further centrals cannot connect while one is connected.
/// - Measured: n/a.
pub const BLE_CONNECTIONS: usize = 1;

/// L2CAP channels of the BLE host stack.
///
/// - Consumer: the TrouBLE host stack: signalling and ATT.
/// - Why: the minimum for a GATT peripheral with one connection.
/// - Exhausted: connection setup fails and the stack restarts advertising.
/// - Measured: n/a.
pub const BLE_L2CAP_CHANNELS: usize = 2;

/// Entries of the GATT attribute table.
///
/// - Consumer: GAP, GATT and the ROHI control service with its two
///   characteristics and descriptors.
/// - Why: the value the control service has run with since BLE was
///   introduced; it holds GAP, GATT and the ROHI service.
/// - Exhausted: building the table panics in TrouBLE when the BLE task
///   starts, so a too small value is caught on the first boot, not later.
/// - Measured: the exact number of entries in use is not measured.
pub const BLE_ATTRIBUTES: usize = 24;

/// HCI command slots of the BLE controller.
///
/// - Consumer: `ExternalController` between `esp-radio` and the host stack.
/// - Why: the value used since the BLE control plane was introduced.
/// - Exhausted: HCI commands wait for a free slot.
/// - Measured: n/a.
pub const BLE_CONTROLLER_SLOTS: usize = 4;

/// Storage of the IP stack and its TCP pool.
pub type NetResources =
    rohi_net_esp::NetResources<NETWORK_SOCKETS, TCP_CONNECTIONS, TCP_RX_BUFFER, TCP_TX_BUFFER>;

/// IP data-plane backend of the product.
pub type Ip = rohi_net_esp::EspIp<TCP_CONNECTIONS, TCP_RX_BUFFER, TCP_TX_BUFFER>;

/// Storage and bounds of the BLE peripheral.
pub type BleResources = rohi_net_esp::BleResources<
    BLE_RX_DEPTH,
    BLE_TX_DEPTH,
    BLE_CONNECTIONS,
    BLE_L2CAP_CHANNELS,
    BLE_ATTRIBUTES,
    BLE_CONTROLLER_SLOTS,
>;

/// BLE transport of the product.
pub type BleTransport = rohi_net_esp::EspBle<BLE_RX_DEPTH, BLE_TX_DEPTH>;

/// Runner of the BLE stack of the product.
pub type BleRunner = rohi_net_esp::BleRunner<
    BLE_RX_DEPTH,
    BLE_TX_DEPTH,
    BLE_CONNECTIONS,
    BLE_L2CAP_CHANNELS,
    BLE_ATTRIBUTES,
    BLE_CONTROLLER_SLOTS,
>;
