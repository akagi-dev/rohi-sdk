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
//! Transport-neutral uplink types.

/// Configured uplink selection policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UplinkMode {
    /// Use Wi-Fi only.
    Wifi,
    /// Use Meshtastic only.
    Meshtastic,
    /// Choose the available uplink automatically.
    #[default]
    Auto,
}

/// Uplink currently carrying telemetry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ActiveUplink {
    /// No uplink is active.
    #[default]
    None,
    /// Wi-Fi is active.
    Wifi,
    /// Meshtastic is active.
    Meshtastic,
}

/// State of an uplink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum UplinkState {
    /// The uplink is disabled.
    #[default]
    Disabled,
    /// The uplink is being established.
    Connecting,
    /// The uplink is ready to deliver telemetry.
    Ready,
    /// The uplink works with reduced reliability.
    Degraded,
    /// The uplink failed.
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        assert_eq!(UplinkMode::default(), UplinkMode::Auto);
        assert_eq!(ActiveUplink::default(), ActiveUplink::None);
        assert_eq!(UplinkState::default(), UplinkState::Disabled);
    }
}
