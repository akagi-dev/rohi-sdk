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
//! Device state.

/// High-level state of the device.
///
/// Intended to drive BLE status, LED state, diagnostics and the supervisor.
/// The state machine itself is not part of this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DeviceState {
    /// The device is starting up.
    #[default]
    Booting,
    /// The device has no configuration yet.
    Unconfigured,
    /// The device is establishing an uplink connection.
    Connecting,
    /// The device is synchronizing wall-clock time.
    TimeSync,
    /// The device is fully operational and online.
    Online,
    /// The device works with reduced functionality.
    Degraded,
    /// The device is being provisioned.
    Provisioning,
    /// The device firmware is being updated.
    Updating,
    /// The device failed.
    Fault,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_booting() {
        assert_eq!(DeviceState::default(), DeviceState::Booting);
    }
}
