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
//! The `ip_*` fields of the `wireless` section in the generic management
//! plane: the field codecs of the domain types and the enum of the address
//! mode. The fields, their ids and the persisted overrides are declared once,
//! on [`WirelessConfig`](crate::WirelessConfig).

use core::fmt::Write as _;
use core::net::Ipv4Addr;
use core::str::FromStr;

use heapless::String;
use rohi_config::{Codec, TextValue, VariantValue};
use rohi_service::{EnumDescriptor, EnumVariant, FieldError, Value, ValueKind};

use super::config::{AddressMode, Hostname, Ipv4};

/// Values of the `ip_mode` field.
pub mod address_mode {
    /// The address comes from a DHCP server.
    pub const DHCP: u32 = 0;
    /// The address is configured.
    pub const STATIC: u32 = 1;
}

/// Id of the enum of the `ip_mode` field.
pub const ADDRESS_MODE_ENUM: u32 = 1;

/// The enum of the `ip_mode` config field.
pub const ADDRESS_MODES: &[EnumDescriptor] = &[EnumDescriptor::new(
    ADDRESS_MODE_ENUM,
    &[
        EnumVariant::new(address_mode::DHCP, "dhcp"),
        EnumVariant::new(address_mode::STATIC, "static"),
    ],
)];

/// The address mode as the enum [`ADDRESS_MODES`].
impl VariantValue for AddressMode {
    fn variant(&self) -> u32 {
        match self {
            Self::Dhcp => address_mode::DHCP,
            Self::Static => address_mode::STATIC,
        }
    }

    fn from_variant(variant: u32) -> Option<Self> {
        match variant {
            address_mode::DHCP => Some(Self::Dhcp),
            address_mode::STATIC => Some(Self::Static),
            _ => None,
        }
    }
}

/// An IPv4 address as dotted text.
pub struct Ipv4Codec;

impl Codec<Ipv4> for Ipv4Codec {
    const KIND: ValueKind = ValueKind::String;

    fn encode(address: &Ipv4) -> Result<Option<Value>, FieldError> {
        let mut text = String::<15>::new();
        write!(text, "{}", Ipv4Addr::from(*address)).map_err(|_| FieldError::Invalid)?;
        Value::text(&text).map(Some).ok_or(FieldError::Invalid)
    }

    fn decode(value: &Value) -> Result<Ipv4, FieldError> {
        let text = value.as_str().ok_or(FieldError::WrongKind)?;
        Ipv4Addr::from_str(text)
            .map(Ipv4::from)
            .map_err(|_| FieldError::Invalid)
    }
}

/// A DHCP host name as text.
impl TextValue for Hostname {
    fn from_text(text: &str) -> Option<Self> {
        Self::new(text)
    }

    fn as_text(&self) -> Option<&str> {
        Some(self.as_str())
    }
}
