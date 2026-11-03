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

use crate::WifiError;

/// Platform-independent Wi-Fi runtime state, published by the
/// [`WifiManager`](crate::WifiManager).
///
/// ```text
/// NotConfigured --configure----> Connecting --success--> Connected
/// Connecting ----failure-------> Error --backoff elapsed--> Connecting
/// Connected -----link lost-----> Disconnected --backoff elapsed--> Connecting
/// configured ----disconnect----> Idle --reconnect or new config--> Connecting
/// ```
///
/// `Scanning` is shown while a scan runs and the link is not connected;
/// `Idle` after a `disconnect` request. See
/// [`WifiService`](crate::WifiService) for how the states read as service
/// states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WifiState {
    /// No network is configured (no SSID, or the configuration was rejected as
    /// invalid, which sets [`WifiStatus::error`]); nothing is attempted.
    #[default]
    NotConfigured,
    /// Configured but deliberately not connected (after a disconnect request).
    Idle,
    /// A scan is running.
    Scanning,
    /// Associating and acquiring an IPv4 configuration.
    Connecting,
    /// Associated and the IPv4 configuration is usable (network ready).
    Connected,
    /// The link is down; a reconnect is scheduled.
    Disconnected,
    /// The last connection attempt failed; a retry is scheduled.
    Error,
}

/// Published state together with the error of the last failed attempt.
///
/// ```
/// use rohi_net::{WifiError, WifiState, WifiStatus};
///
/// let status = WifiStatus::failed(WifiState::Error, WifiError::AuthenticationFailed);
/// assert!(!status.is_network_ready());
/// assert_eq!(status.error, Some(WifiError::AuthenticationFailed));
///
/// assert!(WifiStatus::new(WifiState::Connected).is_network_ready());
/// assert_eq!(WifiStatus::default().state, WifiState::NotConfigured);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct WifiStatus {
    /// The state of the link.
    pub state: WifiState,
    /// Set in [`WifiState::Error`] and [`WifiState::Disconnected`], and in
    /// [`WifiState::NotConfigured`] when a configuration was rejected.
    pub error: Option<WifiError>,
}

impl WifiStatus {
    /// A status without an error.
    pub const fn new(state: WifiState) -> Self {
        Self { state, error: None }
    }

    /// A status with the error of the failed attempt or lost link.
    pub const fn failed(state: WifiState, error: WifiError) -> Self {
        Self {
            state,
            error: Some(error),
        }
    }

    /// `true` once the network can be used (association and DHCP complete).
    pub const fn is_network_ready(&self) -> bool {
        matches!(self.state, WifiState::Connected)
    }
}
