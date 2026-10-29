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

use embedded_io_async::ErrorKind;

/// Portable IP data-plane error.
///
/// Backends map every IP stack or vendor error into one of these categories;
/// raw stack errors never reach higher layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetError {
    /// The network cannot carry traffic: Wi-Fi not configured, still
    /// connecting, DHCP incomplete or link lost. Retry once the Wi-Fi status
    /// is `Connected`.
    NetworkUnavailable,
    /// Name resolution failed while the network was up (unknown name, no DNS
    /// server, invalid name, reverse lookups).
    DnsFailed,
    /// The TCP connection could not be established while the network was up
    /// (refused, unreachable, timed out by the peer).
    ConnectFailed,
    /// Every statically allocated TCP connection or UDP socket is in use.
    /// Close one and retry; nothing waits for a free slot.
    NoSocketAvailable,
    /// The established connection was reset or closed by the peer or the
    /// stack.
    ConnectionReset,
    /// A ROHI-side timeout expired.
    Timeout,
    /// The platform stack failed in an unexpected way.
    Backend,
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NetworkUnavailable => "network unavailable",
            Self::DnsFailed => "DNS lookup failed",
            Self::ConnectFailed => "connection failed",
            Self::NoSocketAvailable => "no socket available",
            Self::ConnectionReset => "connection reset",
            Self::Timeout => "network operation timed out",
            Self::Backend => "network stack error",
        })
    }
}

impl core::error::Error for NetError {}

impl embedded_io_async::Error for NetError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::NetworkUnavailable => ErrorKind::NotConnected,
            Self::DnsFailed => ErrorKind::AddrNotAvailable,
            Self::ConnectFailed => ErrorKind::ConnectionRefused,
            Self::NoSocketAvailable => ErrorKind::OutOfMemory,
            Self::ConnectionReset => ErrorKind::ConnectionReset,
            Self::Timeout => ErrorKind::TimedOut,
            Self::Backend => ErrorKind::Other,
        }
    }
}
