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
//! The persisted `control_tcp` section.

use core::net::Ipv4Addr;

use minicbor::{CborLen, Decode, Encode};
use rohi_config::{Variant, VariantValue};
use rohi_control::{EndpointSettings, TransportAccessMode};
use rohi_net::Ipv4;
use rohi_net::network::Ipv4Codec;
use rohi_service::{Config, EnumDescriptor, EnumVariant};

/// Id of the enum of the `access_mode` field.
const ACCESS_MODE_ENUM: u32 = 1;

/// The enum of the `access_mode` field.
pub const ACCESS_MODES: &[EnumDescriptor] = &[EnumDescriptor::new(
    ACCESS_MODE_ENUM,
    &[
        EnumVariant::new(0, "open"),
        EnumVariant::new(1, "restricted"),
    ],
)];

/// What privileged requests need on an endpoint, as a config value.
///
/// ```
/// use rohi_control::TransportAccessMode;
/// use rohi_control_endpoints::AccessMode;
///
/// assert_eq!(TransportAccessMode::from(AccessMode::Open), TransportAccessMode::Open);
/// assert_eq!(AccessMode::from(TransportAccessMode::Restricted), AccessMode::Restricted);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(index_only)]
pub enum AccessMode {
    /// Every request is allowed.
    #[n(0)]
    Open,
    /// Privileged requests need proof.
    #[n(1)]
    Restricted,
}

impl VariantValue for AccessMode {
    fn variant(&self) -> u32 {
        match self {
            Self::Open => 0,
            Self::Restricted => 1,
        }
    }

    fn from_variant(variant: u32) -> Option<Self> {
        match variant {
            0 => Some(Self::Open),
            1 => Some(Self::Restricted),
            _ => None,
        }
    }
}

impl From<AccessMode> for TransportAccessMode {
    fn from(mode: AccessMode) -> Self {
        match mode {
            AccessMode::Open => Self::Open,
            AccessMode::Restricted => Self::Restricted,
        }
    }
}

impl From<TransportAccessMode> for AccessMode {
    fn from(mode: TransportAccessMode) -> Self {
        match mode {
            TransportAccessMode::Open => Self::Open,
            TransportAccessMode::Restricted => Self::Restricted,
        }
    }
}

/// The effective `control_tcp` section: the control endpoint over TCP.
///
/// | Field | Id | Default | Meaning |
/// |-------|----|---------|---------|
/// | `access_mode` | 2 | `restricted` | what a client needs: the handshake with the pre-shared key (`restricted`) or nothing (`open`) |
/// | `bind_address` | 3 | `127.0.0.1` | IPv4 address of the listener |
/// | `port` | 4 | `9300` | port of the listener, 0 for any free one |
///
/// Only the platform that has a TCP listener (the `std` platform) acts on it.
/// The endpoint always starts with the defaults plus these stored values; a change applies at the next
/// start (reboot), never partly. A
/// launch option of the platform overrides the stored value for that run. A
/// non-loopback address exposes the control port to the network: the platform
/// refuses it unless the operator explicitly allows remote access.
///
/// ```
/// use rohi_control_endpoints::ControlTcpConfig;
///
/// let config = ControlTcpConfig::default();
/// assert_eq!(config.port, 9300);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Config)]
#[config(
    name = "control_tcp",
    overrides = ControlTcpOverrides,
    version = 2,
    enums = ACCESS_MODES
)]
pub struct ControlTcpConfig {
    /// What privileged requests need.
    #[config(id = 2, with = Variant, enum_id = ACCESS_MODE_ENUM)]
    pub access_mode: AccessMode,
    /// IPv4 address of the listener.
    #[config(id = 3, with = Ipv4Codec)]
    pub bind_address: Ipv4,
    /// Port of the listener; 0 picks a free one.
    #[config(id = 4)]
    pub port: u16,
}

impl Default for ControlTcpConfig {
    fn default() -> Self {
        Self {
            access_mode: AccessMode::Restricted,
            bind_address: Ipv4::from(Ipv4Addr::LOCALHOST),
            port: 9300,
        }
    }
}

impl ControlTcpConfig {
    /// The runtime view of the section.
    pub fn settings(&self) -> EndpointSettings {
        EndpointSettings {
            mode: self.access_mode.into(),
            address: Ipv4Addr::from(self.bind_address).octets(),
            port: self.port,
        }
    }
}

#[cfg(test)]
mod tests {
    use rohi_config::{ConfigOverrides, Override, materialize};

    use super::*;

    #[test]
    fn the_defaults_are_restricted_loopback() {
        assert_eq!(
            ControlTcpConfig::default().settings(),
            EndpointSettings::TCP
        );
    }
    #[test]
    fn the_mode_is_stored_as_an_override() {
        let tcp: ControlTcpConfig = materialize(
            &ControlTcpOverrides::default(),
            &ControlTcpOverrides {
                access_mode: Override::Value(AccessMode::Open),
                ..Default::default()
            },
        );
        assert_eq!(tcp.access_mode, AccessMode::Open);
        assert_eq!(ControlTcpOverrides::default().is_overridden(2), Some(false));
    }

    #[test]
    fn the_cbor_encoding_is_stable() {
        let overrides = ControlTcpOverrides {
            access_mode: Override::Value(AccessMode::Open),
            port: Override::Value(9301),
            ..Default::default()
        };
        let mut buffer = [0u8; 32];
        let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
        assert_eq!(&buffer[..used], [0xa2, 0x02, 0x00, 0x04, 0x19, 0x24, 0x55]);
        let decoded: ControlTcpOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(decoded, overrides);
    }
}
