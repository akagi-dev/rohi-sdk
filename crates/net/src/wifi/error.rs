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

use core::fmt;

/// Platform-independent Wi-Fi error.
///
/// Vendor error codes are mapped into these variants by the platform backend;
/// they never become part of this API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiError {
    /// No Wi-Fi configuration is stored.
    NotConfigured,
    /// The manager is busy with another operation or its queue is full.
    Busy,
    /// The operation did not finish in time.
    Timeout,
    /// The scan could not be performed.
    ScanFailed,
    /// The configured network was not found.
    NetworkNotFound,
    /// The access point rejected the credentials.
    AuthenticationFailed,
    /// Association with the access point failed.
    AssociationFailed,
    /// The link is up but no IPv4 configuration was obtained.
    DhcpFailed,
    /// The link was lost.
    Disconnected,
    /// The configuration is invalid.
    InvalidConfig,
    /// The platform driver failed.
    DriverError,
}

impl fmt::Display for WifiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotConfigured => "Wi-Fi is not configured",
            Self::Busy => "Wi-Fi manager is busy",
            Self::Timeout => "Wi-Fi operation timed out",
            Self::ScanFailed => "Wi-Fi scan failed",
            Self::NetworkNotFound => "network not found",
            Self::AuthenticationFailed => "authentication failed",
            Self::AssociationFailed => "association failed",
            Self::DhcpFailed => "DHCP failed",
            Self::Disconnected => "Wi-Fi disconnected",
            Self::InvalidConfig => "invalid Wi-Fi configuration",
            Self::DriverError => "Wi-Fi driver error",
        })
    }
}
