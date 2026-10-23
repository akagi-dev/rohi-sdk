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
use heapless::Vec;
use rohi_control_proto::control::Capability;
use rohi_control_proto::limits::MAX_CAPABILITIES;

/// Set of control capabilities a device offers.
///
/// Clients discover features through the capability list, never by guessing
/// from the firmware version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Capabilities(u8);

impl Capabilities {
    /// Sensor measurements ([`Capability::Sensors`]).
    pub const SENSORS: Self = Self(1 << 0);
    /// Typed configuration ([`Capability::Config`]).
    pub const CONFIG: Self = Self(1 << 1);
    /// Wi-Fi scan ([`Capability::WifiScan`]).
    pub const WIFI_SCAN: Self = Self(1 << 2);
    /// Wi-Fi status ([`Capability::WifiStatus`]).
    pub const WIFI_STATUS: Self = Self(1 << 3);
    /// Wi-Fi reconnect and disconnect ([`Capability::WifiControl`]).
    pub const WIFI_CONTROL: Self = Self(1 << 4);
    /// Device identity ([`Capability::Identity`]).
    pub const IDENTITY: Self = Self(1 << 5);

    /// No capability.
    pub const NONE: Self = Self(0);
    /// Every capability of this protocol version.
    pub const ALL: Self = Self(0b11_1111);

    /// Union of two sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether every capability of `other` is in the set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub(crate) fn to_proto(self) -> Vec<Capability, MAX_CAPABILITIES> {
        const MAP: [(Capabilities, Capability); 6] = [
            (Capabilities::SENSORS, Capability::Sensors),
            (Capabilities::CONFIG, Capability::Config),
            (Capabilities::WIFI_SCAN, Capability::WifiScan),
            (Capabilities::WIFI_STATUS, Capability::WifiStatus),
            (Capabilities::WIFI_CONTROL, Capability::WifiControl),
            (Capabilities::IDENTITY, Capability::Identity),
        ];
        let mut out = Vec::new();
        for (flag, capability) in MAP {
            if self.contains(flag) {
                // At most `MAP.len()` entries, below the protocol bound.
                let _ = out.push(capability);
            }
        }
        out
    }
}

impl core::ops::BitOr for Capabilities {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

const _: () = assert!(MAX_CAPABILITIES >= 6);
