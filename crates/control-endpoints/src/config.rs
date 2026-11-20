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

//! The persisted `control` section.

use core::net::Ipv4Addr;

use minicbor::{CborLen, Decode, Encode};
use rohi_config::{Variant, VariantValue};
use rohi_control::{EndpointSettings, TransportAccessMode};
use rohi_net::Ipv4;
use rohi_net::network::Ipv4Codec;
use rohi_service::{Config, EnumDescriptor, EnumVariant};

/// Id of the enum of the `tcp_access_mode` field.
const ACCESS_MODE_ENUM: u32 = 1;

/// The enum of the `tcp_access_mode` field.
pub const ACCESS_MODES: &[EnumDescriptor] = &[EnumDescriptor::new(
    ACCESS_MODE_ENUM,
    &[
        EnumVariant::new(0, "open"),
        EnumVariant::new(1, "protected"),
        EnumVariant::new(2, "private"),
    ],
)];

/// What a TCP client may do without the pre-shared key, as a config value.
///
/// | Mode | Without the key | With the key |
/// |------|-----------------|--------------|
/// | `open` | read, mutate and provision | everything |
/// | `protected` | read only | everything |
/// | `private` | nothing | everything |
///
/// ```
/// use rohi_control::TransportAccessMode;
/// use rohi_control_endpoints::AccessMode;
///
/// assert_eq!(TransportAccessMode::from(AccessMode::Open), TransportAccessMode::Open);
/// assert_eq!(AccessMode::from(TransportAccessMode::Private), AccessMode::Private);
/// assert_eq!(AccessMode::default(), AccessMode::Protected);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(index_only)]
pub enum AccessMode {
    /// Every request is allowed without the key.
    #[n(0)]
    Open,
    /// Reads are allowed without the key; mutations and provisioning need it.
    #[n(1)]
    #[default]
    Protected,
    /// Every request needs the key.
    #[n(2)]
    Private,
}

impl VariantValue for AccessMode {
    fn variant(&self) -> u32 {
        match self {
            Self::Open => 0,
            Self::Protected => 1,
            Self::Private => 2,
        }
    }

    fn from_variant(variant: u32) -> Option<Self> {
        match variant {
            0 => Some(Self::Open),
            1 => Some(Self::Protected),
            2 => Some(Self::Private),
            _ => None,
        }
    }
}

impl From<AccessMode> for TransportAccessMode {
    fn from(mode: AccessMode) -> Self {
        match mode {
            AccessMode::Open => Self::Open,
            AccessMode::Protected => Self::Protected,
            AccessMode::Private => Self::Private,
        }
    }
}

impl From<TransportAccessMode> for AccessMode {
    fn from(mode: TransportAccessMode) -> Self {
        match mode {
            TransportAccessMode::Open => Self::Open,
            TransportAccessMode::Protected => Self::Protected,
            TransportAccessMode::Private => Self::Private,
        }
    }
}

/// The effective `control` section: the settings of the control endpoints. Only the TCP endpoint
/// has any; the BLE endpoint is open and has none.
///
/// | Field | Id | Default | Meaning |
/// |-------|----|---------|---------|
/// | `tcp_access_mode` | 1 | `protected` | what a client without the pre-shared key may do: everything (`open`), read (`protected`) or nothing (`private`) |
/// | `tcp_bind_address` | 2 | `127.0.0.1` | IPv4 address of the listener |
/// | `tcp_port` | 3 | `9300` | port of the listener, 0 for any free one |
///
/// There is no `enabled` flag, and the pre-shared key is not a field: it stays in private
/// persistent storage. Only a platform that has a TCP listener (the `std` platform) registers the
/// section and acts on it. The endpoint always starts with the defaults plus these stored values;
/// a change applies at the next start (reboot), all three together, never partly. A launch option
/// of the platform overrides the stored value for that run. A non-loopback address exposes the
/// control port to the network: the platform refuses it unless the operator explicitly allows
/// remote access.
///
/// ```
/// use rohi_control_endpoints::{AccessMode, ControlConfig};
///
/// let config = ControlConfig::default();
/// assert_eq!(config.tcp_access_mode, AccessMode::Protected);
/// assert_eq!(config.tcp_port, 9300);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Config)]
#[config(
    name = "control",
    overrides = ControlOverrides,
    version = 1,
    enums = ACCESS_MODES
)]
pub struct ControlConfig {
    /// What a TCP client without the pre-shared key may do.
    #[config(id = 1, with = Variant, enum_id = ACCESS_MODE_ENUM)]
    pub tcp_access_mode: AccessMode,
    /// IPv4 address of the TCP listener.
    #[config(id = 2, with = Ipv4Codec)]
    pub tcp_bind_address: Ipv4,
    /// Port of the TCP listener; 0 picks a free one.
    #[config(id = 3)]
    pub tcp_port: u16,
}

impl Default for ControlConfig {
    fn default() -> Self {
        Self {
            tcp_access_mode: AccessMode::Protected,
            tcp_bind_address: Ipv4::from(Ipv4Addr::LOCALHOST),
            tcp_port: 9300,
        }
    }
}

impl ControlConfig {
    /// The runtime view of the section.
    pub fn settings(&self) -> EndpointSettings {
        EndpointSettings {
            mode: self.tcp_access_mode.into(),
            address: Ipv4Addr::from(self.tcp_bind_address).octets(),
            port: self.tcp_port,
        }
    }
}

#[cfg(test)]
mod tests {
    use rohi_config::{ConfigOverrides, Override, materialize};

    use super::*;

    #[test]
    fn the_defaults_are_protected_loopback() {
        assert_eq!(ControlConfig::default().settings(), EndpointSettings::TCP);
        assert_eq!(
            ControlConfig::default().tcp_access_mode,
            AccessMode::Protected
        );
    }

    #[test]
    fn the_mode_is_stored_as_an_override() {
        let control: ControlConfig = materialize(
            &ControlOverrides::default(),
            &ControlOverrides {
                tcp_access_mode: Override::Value(AccessMode::Private),
                ..Default::default()
            },
        );
        assert_eq!(control.tcp_access_mode, AccessMode::Private);
        assert_eq!(ControlOverrides::default().is_overridden(1), Some(false));
    }

    #[test]
    fn every_mode_round_trips_through_its_variant_and_the_runtime_mode() {
        for mode in [AccessMode::Open, AccessMode::Protected, AccessMode::Private] {
            assert_eq!(AccessMode::from_variant(mode.variant()), Some(mode));
            assert_eq!(AccessMode::from(TransportAccessMode::from(mode)), mode);
        }
        assert_eq!(AccessMode::from_variant(3), None);
        // The names are those of the runtime mode.
        for (variant, mode) in ACCESS_MODES[0].variants.iter().zip([
            TransportAccessMode::Open,
            TransportAccessMode::Protected,
            TransportAccessMode::Private,
        ]) {
            assert_eq!(variant.name, mode.as_str());
        }
    }

    #[test]
    fn the_cbor_encoding_is_stable() {
        let overrides = ControlOverrides {
            tcp_access_mode: Override::Value(AccessMode::Private),
            tcp_port: Override::Value(9301),
            ..Default::default()
        };
        let mut buffer = [0u8; 32];
        let used = rohi_config::encode_to_slice(&overrides, &mut buffer).unwrap();
        assert_eq!(&buffer[..used], [0xa2, 0x01, 0x02, 0x03, 0x19, 0x24, 0x55]);
        let decoded: ControlOverrides = rohi_config::decode_exact(&buffer[..used]).unwrap();
        assert_eq!(decoded, overrides);
    }
}
