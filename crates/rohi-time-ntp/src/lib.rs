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

//! NTP/SNTP [`TimeSource`](rohi_time::TimeSource) for the ROHI time service.
//!
//! [`NtpTimeSource`] resolves a configured server with the DNS of the
//! [`NetworkHandle`](rohi_net::NetworkHandle), exchanges one SNTPv4 datagram
//! over a UDP socket of the same handle (so it shares the firmware's network
//! stack instead of creating its own) and returns a
//! [`TimeSample`](rohi_time::TimeSample). It owns no clock: the sample is
//! validated and anchored by [`TimeService`](rohi_time::TimeService), and the
//! source is driven by [`Synchronizer`](rohi_time::Synchronizer), which
//! provides the background task, retry backoff and periodic resynchronization.
//!
//! The SNTP protocol itself is [`sntpc`]: it validates the response (mode,
//! version, stratum, origin timestamp, leap indicator, kiss-o'-death codes,
//! root distance, reply address). This crate maps the outcome to
//! [`SyncFailure`](rohi_time::SyncFailure) categories.
//!
//! # NTP era
//!
//! NTP timestamps count seconds since 1900 in 32 bits and wrap every 136
//! years (the next wrap is in 2036). A wire timestamp alone does not say
//! which era it belongs to, so the source takes a *pivot*, the best known
//! current time, and picks the era closest to it. The pivot is supplied by the
//! firmware, normally [`TimeService::pivot`](rohi_time::TimeService::pivot):
//! the synchronized time when there is one, otherwise the firmware build time
//! plus uptime. A pivot more than about 68 years off picks the wrong era, and
//! the result is then rejected by the plausibility window of the time service.
//!
//! # Security
//!
//! SNTP is not authenticated. The result is a convenience clock, not input
//! for security decisions.

#[cfg(test)]
extern crate std;

mod config;
mod error;
mod section;
mod source;

#[cfg(test)]
mod tests;

pub use config::{DEFAULT_SERVERS, DEFAULT_TIMEOUT_MS, MAX_SERVERS, NTP_PORT, NtpConfig};
pub use error::NtpError;
pub use section::{MAX_NAME_LEN, NtpServersConfig, ServerName, field};
pub use source::NtpTimeSource;
