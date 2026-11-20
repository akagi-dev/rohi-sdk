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
//! The `ble_name` field of the `wireless` section in the generic management
//! plane.

use rohi_config::TextValue;

use super::config::BleName;

/// The advertised name as text. A name is 1 to 29 bytes of UTF-8 without
/// control characters.
impl TextValue for BleName {
    fn from_text(text: &str) -> Option<Self> {
        Self::new(text)
    }

    fn as_text(&self) -> Option<&str> {
        Some(self.as_str())
    }
}
