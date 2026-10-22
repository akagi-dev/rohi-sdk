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
//! GATT identifiers of the ROHI Control Service.
//!
//! One service with two characteristics:
//!
//! - RX: the central writes request frames (write, write without response);
//! - TX: the peripheral notifies response frames.
//!
//! UUIDs are 128-bit values in big-endian (canonical) byte order.

/// ROHI Control Service: `726f6869-0001-4d00-8000-636f6e74726f`.
pub const SERVICE_UUID: u128 = 0x726f6869_0001_4d00_8000_636f6e74726f;

/// RX characteristic (central -> peripheral): `726f6869-0002-4d00-8000-636f6e74726f`.
pub const RX_CHARACTERISTIC_UUID: u128 = 0x726f6869_0002_4d00_8000_636f6e74726f;

/// TX characteristic (peripheral -> central, notify): `726f6869-0003-4d00-8000-636f6e74726f`.
pub const TX_CHARACTERISTIC_UUID: u128 = 0x726f6869_0003_4d00_8000_636f6e74726f;

/// Advertised local name prefix; the full name is `"ROHI <board>"`.
pub const ADVERTISED_NAME_PREFIX: &str = "ROHI";
