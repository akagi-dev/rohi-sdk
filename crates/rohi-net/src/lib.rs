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
#![no_std]

//! Platform-independent network (Wi-Fi) abstraction for Robonomics Open
//! Hardware.
//!
//! The public API of this crate is expressed only in ROHI types
//! ([`WifiConfig`], [`WifiState`], [`WifiNetwork`], [`NetworkInfo`],
//! [`WifiError`]) and is fully bounded: nothing here allocates. Platform
//! details (radio drivers, IP stack, radio heap) live behind the private
//! backends and never appear in a public signature.

#[cfg(test)]
extern crate std;

pub mod backend;
mod backoff;
mod config;
mod error;
mod limits;
mod manager;
mod network;
mod scan;
mod state;

pub use backend::WifiBackend;
pub use backoff::Backoff;
pub use config::{WifiConfig, WifiPassword, WifiSsid};
pub use error::WifiError;
pub use limits::{
    MAX_SSID_LEN, MAX_WIFI_PASSWORD_LEN, MAX_WIFI_SCAN_RESULTS, MIN_WIFI_PASSWORD_LEN,
    WIFI_COMMAND_QUEUE_DEPTH,
};
pub use manager::{ManagerConfig, STATUS_RECEIVERS, WifiHandle, WifiManager, WifiShared};
pub use network::NetworkInfo;
pub use scan::{ScanCollector, WifiNetwork, WifiScanResults, WifiSecurity};
pub use state::{WifiState, WifiStatus};
