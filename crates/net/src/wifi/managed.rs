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
//! The `wifi_*` fields of the `wireless` section in the generic management
//! plane: the field codecs of the domain types. The fields, their ids and the
//! persisted overrides are declared once, on
//! [`WirelessConfig`](crate::WirelessConfig).

use rohi_config::TextValue;

use crate::{WifiPassword, WifiSsid};

/// The SSID as text.
///
/// An SSID that is not UTF-8 cannot be expressed as text and is reported as
/// invalid; it is not shown, but it keeps working. An empty name is not a
/// network: `none` disables Wi-Fi instead.
impl TextValue for WifiSsid {
    fn from_text(text: &str) -> Option<Self> {
        Self::from_bytes(text.as_bytes()).filter(|ssid| !ssid.is_empty())
    }

    fn as_text(&self) -> Option<&str> {
        self.as_str()
    }
}

/// The passphrase as text. It is a secret: it has no text to show, and the
/// generic interface never converts it to a value.
impl TextValue for WifiPassword {
    fn from_text(text: &str) -> Option<Self> {
        Self::from_bytes(text.as_bytes())
    }

    fn as_text(&self) -> Option<&str> {
        None
    }
}
