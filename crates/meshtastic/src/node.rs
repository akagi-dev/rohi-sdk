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
//! Meshtastic node numbers.

use core::fmt;

/// A Meshtastic node number, written `!` and eight hexadecimal digits.
///
/// ```
/// use rohi_meshtastic::NodeId;
///
/// assert_eq!(NodeId::parse("!DEADbeef"), Some(NodeId(0xdead_beef)));
/// assert_eq!(NodeId::parse("deadbeef"), None);
/// assert_eq!(std::format!("{}", NodeId(0xbeef)), "!0000beef");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The broadcast address, which is never a destination of this crate.
    pub const BROADCAST: Self = Self(u32::MAX);

    /// Parses `!xxxxxxxx`, upper or lower case hexadecimal; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let digits = text.strip_prefix('!')?;
        if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(digits, 16).ok().map(Self)
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "!{:08x}", self.0)
    }
}
