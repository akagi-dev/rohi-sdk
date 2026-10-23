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

use core::fmt;

use rohi_time::SyncFailure;

/// Why one NTP exchange did not produce a sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NtpError {
    /// The network cannot carry traffic.
    NetworkUnavailable,
    /// The server name did not resolve.
    Resolve,
    /// Every UDP socket of the network stack is in use.
    NoSocket,
    /// The DNS lookup or the server did not answer in time.
    Timeout,
    /// The datagram could not be sent or received.
    Network,
    /// The response was malformed or failed protocol validation.
    InvalidResponse,
    /// The server refused to serve: kiss-o'-death, unsynchronized clock or
    /// excessive root distance.
    Rejected,
}

impl fmt::Display for NtpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NetworkUnavailable => "network unavailable",
            Self::Resolve => "NTP server name did not resolve",
            Self::NoSocket => "no UDP socket available",
            Self::Timeout => "NTP server did not answer in time",
            Self::Network => "NTP datagram exchange failed",
            Self::InvalidResponse => "invalid NTP response",
            Self::Rejected => "NTP server refused to serve",
        })
    }
}

impl core::error::Error for NtpError {}

impl From<NtpError> for SyncFailure {
    fn from(error: NtpError) -> Self {
        match error {
            NtpError::NetworkUnavailable => Self::NetworkUnavailable,
            NtpError::Resolve => Self::Resolve,
            NtpError::Timeout => Self::Timeout,
            NtpError::InvalidResponse => Self::InvalidResponse,
            NtpError::Rejected => Self::Rejected,
            NtpError::NoSocket | NtpError::Network => Self::Source,
        }
    }
}
