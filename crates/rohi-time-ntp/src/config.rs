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

/// UDP port of NTP servers.
pub const NTP_PORT: u16 = 123;

/// Most servers a source rotates through. Bounds the configuration.
pub const MAX_SERVERS: usize = 4;

/// Default time to wait for one DNS lookup or one SNTP exchange: 5 s.
pub const DEFAULT_TIMEOUT_MS: u32 = 5_000;

/// Public servers used when the firmware has no reason to choose others.
pub const DEFAULT_SERVERS: &[&str] = &["pool.ntp.org", "time.cloudflare.com"];

/// Servers and timeouts of an [`NtpTimeSource`](crate::NtpTimeSource).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NtpConfig<'a> {
    servers: &'a [&'a str],
    /// Time to wait for the DNS lookup and, separately, for the SNTP
    /// exchange, in milliseconds.
    pub timeout_ms: u32,
}

impl<'a> NtpConfig<'a> {
    /// Rotates through `servers` (host names), one per attempt, moving on
    /// after a failure.
    ///
    /// # Panics
    ///
    /// If `servers` is empty or has more than [`MAX_SERVERS`] entries. In a
    /// `const` context this is a build error.
    pub const fn new(servers: &'a [&'a str]) -> Self {
        assert!(
            !servers.is_empty() && servers.len() <= MAX_SERVERS,
            "an NTP configuration needs between 1 and MAX_SERVERS servers"
        );
        Self {
            servers,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }

    /// The configured servers.
    pub const fn servers(&self) -> &'a [&'a str] {
        self.servers
    }
}

impl Default for NtpConfig<'static> {
    fn default() -> Self {
        Self::new(DEFAULT_SERVERS)
    }
}
