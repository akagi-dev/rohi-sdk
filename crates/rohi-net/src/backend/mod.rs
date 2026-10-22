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

use crate::{NetworkInfo, WifiConfig, WifiError, WifiScanResults};

/// Platform Wi-Fi station driver driven by the [`WifiManager`](crate::WifiManager).
///
/// Implementations convert every vendor type into the ROHI types of this crate
/// before returning; no platform type may appear in this interface. Futures
/// returned by the methods must be cancel-safe: the manager drops `connect`
/// and `wait_link_lost` when a command or link event preempts them.
#[allow(async_fn_in_trait)]
pub trait WifiBackend {
    /// Scans for networks. The result is bounded; surplus observations are
    /// reduced with [`ScanCollector`](crate::ScanCollector).
    async fn scan(&mut self) -> Result<WifiScanResults, WifiError>;

    /// Associates and waits for a usable IPv4 configuration (DHCP).
    ///
    /// Returns only when the network is ready; association alone is not
    /// success.
    async fn connect(&mut self, config: &WifiConfig) -> Result<NetworkInfo, WifiError>;

    /// Leaves the network. Must be idempotent and succeed when not connected.
    async fn disconnect(&mut self) -> Result<(), WifiError>;

    /// Completes when an established link is lost; pends while it is up.
    async fn wait_link_lost(&mut self) -> WifiError;

    /// Fresh snapshot of the current connection, if connected.
    fn network_info(&mut self) -> Option<NetworkInfo>;
}
