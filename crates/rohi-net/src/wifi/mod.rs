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

//! Wi-Fi control plane: station configuration, scan, status, reconnect and
//! disconnect.
//!
//! [`WifiManager`] is the single owner of a platform [`WifiBackend`] and
//! reconciles the radio towards the latest desired [`NetworkConfig`](crate::NetworkConfig). Other tasks
//! use the copyable [`WifiHandle`]. Nothing here carries IP traffic; that is
//! the job of the [IP data plane](crate::ip).

mod backend;
mod backoff;
mod config;
mod error;
mod manager;
mod network;
mod scan;
mod state;

pub use backend::WifiBackend;
pub use backoff::Backoff;
pub use config::{WifiPassword, WifiSsid};
pub use error::WifiError;
pub use manager::{
    LinkMetrics, ManagerConfig, STATUS_RECEIVERS, WifiHandle, WifiManager, WifiShared,
};
pub use network::NetworkInfo;
pub use scan::{ScanCollector, WifiNetwork, WifiScanResults, WifiSecurity};
pub use state::{WifiState, WifiStatus};
