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

//! ESP32 wireless backends of [`rohi-net`](rohi_net).
//!
//! [`EspWireless`] owns the shared wireless runtime (radio peripherals and the
//! single wireless heap) and creates both drivers. The firmware sizes every
//! static resource: the heap ([`wireless_heap!`]), the IP stack
//! ([`NetResources`]) and BLE ([`BleResources`]):
//!
//! - Wi-Fi: [`EspWifi`] implements [`WifiBackend`](rohi_net::WifiBackend) on top
//!   of `esp-radio` and `embassy-net`, with [`EspIp`] implementing
//!   [`IpBackend`](rohi_net::IpBackend) (DNS, TCP and UDP) on the same stack;
//! - BLE: [`EspBle`] implements [`BleTransport`](rohi_net::BleTransport) on top
//!   of `esp-radio` and the TrouBLE host stack.
//!
//! Vendor types never leave this crate. The chip is selected by
//! exactly one Cargo feature, `esp32c3` or `esp32c6`, forwarded by the final
//! firmware. Without a chip feature the crate is empty, so it stays buildable
//! on a regular host.

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
