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

//! IP data plane: DNS, TCP and UDP over the platform IP stack.
//!
//! ```text
//! TLS / HTTP / uplink  ── embedded-nal-async (Dns, TcpConnect)
//!          │              embedded-io-async (Read, Write)
//!    NetworkHandle     ── readiness, typed NetError, UdpSocket
//!          │
//!     IpBackend        ── implemented by platform crates (rohi-net-esp)
//! ```
//!
//! The Wi-Fi control plane ([`crate::wifi`]) brings the link up; this module
//! only carries traffic once it is. Only the protocols a current SDK component
//! uses are offered: DNS, TCP and UDP over DHCPv4-configured IPv4.

mod backend;
mod error;
mod handle;
mod udp;

pub use backend::IpBackend;
pub use error::NetError;
pub use handle::NetworkHandle;
pub use udp::UdpSocket;

#[cfg(test)]
mod tests;
