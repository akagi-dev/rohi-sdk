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
#![no_std]

//! NTP/SNTP [`TimeSource`] for the ROHI time service, and the persistent
//! `time` configuration section that selects the servers.
//!
//! # Overview
//!
//! [`NtpTimeSource`] resolves a configured server with the DNS of the
//! [`NetworkHandle`], exchanges one SNTPv4 datagram over a UDP socket of the
//! same handle (so it shares the firmware's network stack instead of
//! creating its own) and returns a [`TimeSample`]: the server's transmit time
//! plus half the round trip, with an uncertainty derived from the round trip
//! and the server's root delay and dispersion. It owns no clock and never owns
//! the global concept of time: the sample is validated and anchored by
//! [`TimeService`](rohi_time::TimeService), and the source is driven by
//! [`Synchronizer`](rohi_time::Synchronizer), which provides the background
//! task, retry backoff and periodic resynchronization (see [`rohi_time`]).
//!
//! The crate is `no_std`, does not allocate and has no Cargo features. The
//! SNTP protocol itself is [`sntpc`]: it validates the response (mode,
//! version, stratum, origin timestamp, leap indicator, kiss-o'-death codes,
//! root distance, reply address). This crate maps the outcome to coarse
//! [`SyncFailure`] categories, see [`NtpError`].
//!
//! # Quick Example
//!
//! One synchronization exchange against a scripted server on the in-memory
//! `FakeIp` backend of `rohi-net` (`rohi_net::testing`, feature
//! `testing`). On a device `NetworkHandle` wraps the real IP stack and the
//! delay is a hardware timer (on the host simulator it is the host IP stack); see `firmwares/altruist-urban/src/time.rs` for
//! the firmware wiring.
//!
//! ```
//! use core::future::{pending, poll_fn};
//! use core::task::Poll;
//!
//! use embassy_futures::block_on;
//! use embassy_futures::join::join;
//! use embedded_hal_async::delay::DelayNs;
//! use rohi_net::NetworkHandle;
//! use rohi_net::testing::FakeIp;
//! use rohi_time::{TimeSource, UnixTime};
//! use rohi_time_ntp::{NtpConfig, NtpTimeSource};
//!
//! // A delay that never fires: the exchange is bounded by the peer only.
//! struct NeverDelay;
//! impl DelayNs for NeverDelay {
//!     async fn delay_ns(&mut self, _ns: u32) {
//!         pending::<()>().await
//!     }
//! }
//!
//! const SERVER_TIME: u64 = 1_780_000_000; // Unix seconds
//! const NTP_DELTA: u64 = 2_208_988_800; // 1900 -> 1970
//!
//! let ip = FakeIp::<1, 64>::new();
//! ip.set_ready(true);
//! ip.add_host("ntp.example", [203, 0, 113, 1].into());
//!
//! let servers = ["ntp.example"];
//! let mut source = NtpTimeSource::new(
//!     NetworkHandle::new(&ip),
//!     NeverDelay,
//!     NtpConfig::new(&servers),
//!     // The pivot: the best known time, here the same instant as the server's.
//!     || UnixTime::from_secs(SERVER_TIME),
//! );
//!
//! // The scripted server answers the first request with a valid SNTPv4 packet.
//! let server = poll_fn(|cx| match ip.udp_peer_received() {
//!     Some((client, request)) => {
//!         let timestamp = ((SERVER_TIME + NTP_DELTA) as u32).to_be_bytes();
//!         let mut reply = [0u8; 48];
//!         reply[0] = (4 << 3) | 4; // version 4, server mode
//!         reply[1] = 2; // stratum
//!         reply[4..8].copy_from_slice(&0x0800u32.to_be_bytes()); // root delay, 1/32 s
//!         reply[8..12].copy_from_slice(&0x0800u32.to_be_bytes()); // root dispersion
//!         reply[12..16].copy_from_slice(&[192, 0, 2, 1]); // reference id
//!         reply[16..20].copy_from_slice(&timestamp); // reference timestamp
//!         reply[24..32].copy_from_slice(&request[40..48]); // origin: echoed
//!         reply[32..36].copy_from_slice(&timestamp); // receive timestamp
//!         reply[40..44].copy_from_slice(&timestamp); // transmit timestamp
//!         assert!(ip.udp_peer_send(client, &reply));
//!         Poll::Ready(())
//!     }
//!     None => {
//!         cx.waker().wake_by_ref();
//!         Poll::Pending
//!     }
//! });
//!
//! assert!(source.is_available());
//! let (sample, ()) = block_on(join(source.sample(), server));
//! let sample = sample.unwrap();
//! assert_eq!(sample.unix_time.as_secs(), SERVER_TIME);
//! assert!(sample.uncertainty.is_some());
//! ```
//!
//! # Core Concepts
//!
//! * **Servers**: [`NtpConfig`] holds 1 to [`MAX_SERVERS`] host names and the
//!   timeout of one source. In the firmware the servers come from the
//!   persistent `time` section, [`NtpServersConfig`].
//! * **Sample**: one exchange yields the server's transmit time plus half the
//!   round trip. The uncertainty is half the round trip plus half the
//!   server's root delay plus its root dispersion. The local offset that
//!   `sntpc` computes is not used: the local clock is only the *pivot*.
//! * **Rotation**: each [`sample`](rohi_time::TimeSource::sample) uses the next server of
//!   the list, after a failure and after a success.
//! * **Failure**: every failure is an [`NtpError`], mapped to a coarse
//!   [`SyncFailure`] for the service.
//!
//! ## NTP era
//!
//! NTP timestamps count seconds since 1900 in 32 bits and wrap every 2³²
//! seconds (136 years; the next wrap is in 2036). A wire timestamp alone does
//! not say which era it belongs to, so the source takes a *pivot*, the best
//! known current time, and picks the era closest to it. The pivot is supplied
//! by the firmware, normally [`TimeService::pivot`](rohi_time::TimeService::pivot):
//! the synchronized time when there is one, otherwise the **firmware build
//! time plus uptime** (not a timestamp, never recorded). A pivot more than
//! about 68 years off picks the wrong era; such a result is far outside the
//! plausibility window of the time service and is rejected.
//!
//! # Architecture & Integration
//!
//! ```text
//!  `time` config section ──► NtpServersConfig ──► NtpConfig ──► NtpTimeSource ──┐
//!  (rohi config time ...)                                          │ DNS + UDP    │ TimeSource
//!                                                                   ▼ NetworkHandle ▼
//!                                         rohi-net (IP data plane)     Synchronizer ──► TimeService
//! ```
//!
//! | Neighbour | Relation |
//! |-----------|----------|
//! | [`rohi_time`] | Defines the [`TimeSource`] trait this crate implements, the [`TimeService`](rohi_time::TimeService) that validates and anchors the samples and the [`Synchronizer`](rohi_time::Synchronizer) that drives the source. |
//! | [`rohi_net`] | [`NetworkHandle`]: DNS (IPv4 only) and the UDP socket pool; the source is available while the network is ready. |
//! | [`rohi_config`], [`rohi_service`] | [`NtpServersConfig`] is a [`rohi_service::Config`] section with persisted overrides. |
//! | Altruist Urban firmware | `altruist_urban::time` owns the service, builds an `NtpTimeSource` from the effective `time` section and restarts the synchronizer when the section changes. |
//!
//! # API Navigation
//!
//! | Module | Contents |
//! |--------|----------|
//! | [`source`] | [`NtpTimeSource`]: server rotation, timeouts, socket use, cancellation |
//! | [`config`] | [`NtpConfig`] (servers and timeout of a source), [`MAX_SERVERS`], [`DEFAULT_SERVERS`], [`DEFAULT_TIMEOUT_MS`], [`NTP_PORT`] |
//! | [`section`] | the persistent `time` config section: [`NtpServersConfig`], [`NtpServersOverrides`], [`ServerName`], [`MAX_NAME_LEN`] |
//! | [`error`] | [`NtpError`] and its mapping to [`SyncFailure`] |
//!
//! # Guarantees & Limitations
//!
//! * **Security**: SNTP is not authenticated. The result is a convenience
//!   clock, not input for security decisions. Authenticated NTP (NTS), GPS/PTP,
//!   an NTP server and manual resynchronization are out of scope (periodic
//!   automatic synchronization is authoritative).
//! * **Resources**: `NtpTimeSource` keeps no state besides a server index. One
//!   UDP socket is allocated from the `NetResources` pool of the [`rohi_net`]
//!   stack for the duration of one exchange; the Altruist Urban firmware
//!   provides `UDP_SOCKETS = 1` with `UDP_BUFFER = 128` B per direction. When
//!   no socket is free the attempt fails with [`NtpError::NoSocket`] and is
//!   retried with backoff by the [`Synchronizer`](rohi_time::Synchronizer).
//!   See [Resource
//!   limits](https://docs.rs/rohi-sdk/latest/rohi_sdk/memory/index.html).
//! * **Bounds**: at most [`MAX_SERVERS`] servers, names up to
//!   [`MAX_NAME_LEN`] bytes, DNS and the exchange each bounded by
//!   [`NtpConfig::timeout_ms`].
//! * **Cancellation**: dropping a [`sample`](rohi_time::TimeSource::sample) future closes
//!   its socket.
//! * **IPv4 only**: names are resolved to IPv4 addresses.
//! * **Persistent ABI**: the `time` section is `NtpServersConfig` version 1,
//!   section ID `0x2feb7d230f6ed989`, a CBOR map of the overrides; see
//!   [`NtpServersConfig`].
//!
//! ## Validation level
//!
//! Everything runs on the host without Internet access (`cargo test -p
//! rohi-time-ntp`), using `rohi_net::testing::FakeIp` with a scripted NTP
//! server: valid and malformed answers, kiss-o'-death, timeouts, DNS failure,
//! unreachable server, era rollover, implausible time, network loss and
//! recovery. See [Host
//! testing](https://rohi.akagi.dev/validation/host-testing.html). The firmware
//! builds for ESP32 targets in CI, but NTP synchronization over Wi-Fi is **not
//! verified on hardware**; see the [support
//! matrix](https://rohi.akagi.dev/support/matrix.html).
//!
//! [`NetworkHandle`]: rohi_net::NetworkHandle
//! [`SyncFailure`]: rohi_time::SyncFailure
//! [`TimeSample`]: rohi_time::TimeSample
//! [`TimeSource`]: rohi_time::TimeSource

#[cfg(test)]
extern crate std;

pub mod config;
pub mod error;
pub mod section;
pub mod source;

#[cfg(test)]
mod tests;

pub use config::{DEFAULT_SERVERS, DEFAULT_TIMEOUT_MS, MAX_SERVERS, NTP_PORT, NtpConfig};
pub use error::NtpError;
pub use section::{MAX_NAME_LEN, NtpServersConfig, NtpServersOverrides, ServerName};
pub use source::NtpTimeSource;
