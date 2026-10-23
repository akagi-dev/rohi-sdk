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

use core::cell::Cell;
use core::future::{pending, poll_fn};
use core::net::{IpAddr, Ipv4Addr, SocketAddr};
use core::pin::pin;
use core::task::Poll;
use core::time::Duration;
use std::rc::Rc;
use std::vec::Vec;

use embassy_futures::join::join;
use embassy_futures::{block_on, poll_once};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_hal_async::delay::DelayNs;
use rohi_hal::MonotonicTimestamp;
use rohi_hal::runtime::Clock;
use rohi_net::NetworkHandle;
use rohi_net::testing::FakeIp;
use rohi_time::{
    SyncFailure, SyncSchedule, Synchronizer, TimeConfig, TimeService, TimeSource, TimeSyncState,
    UnixTime, sync_once,
};

use crate::{NtpConfig, NtpError, NtpTimeSource};

const NTP_DELTA: u64 = 2_208_988_800;
/// 2026-01-01T00:00:00Z, the plausibility floor of the tests.
const FLOOR: UnixTime = UnixTime::from_secs(1_767_225_600);
/// A time of 2026 as seen by the server, in milliseconds.
const SERVER_MS: u64 = 1_780_000_000_250;
const SERVER_A: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1));
const SERVER_B: IpAddr = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 2));

type Ip = FakeIp<1, 64>;

fn ip() -> Ip {
    let ip = FakeIp::new();
    ip.set_ready(true);
    ip.add_host("a.ntp.example", SERVER_A);
    ip.add_host("b.ntp.example", SERVER_B);
    ip
}

/// Delay that never completes: the exchange is only bounded by the peer.
struct NeverDelay;

impl DelayNs for NeverDelay {
    async fn delay_ns(&mut self, _ns: u32) {
        pending::<()>().await
    }
}

/// Delay that completes at once: every wait times out.
struct InstantDelay;

impl DelayNs for InstantDelay {
    async fn delay_ns(&mut self, _ns: u32) {}
}

/// NTP timestamp (32.32 fixed point since 1900) of a Unix time in ms.
/// The seconds wrap at 2^32, exactly like on the wire.
fn wire(unix_ms: u64) -> [u8; 8] {
    let secs = ((unix_ms / 1_000 + NTP_DELTA) & 0xFFFF_FFFF) as u32;
    let fraction = (((unix_ms % 1_000) << 32) / 1_000) as u32;
    let mut out = [0; 8];
    out[..4].copy_from_slice(&secs.to_be_bytes());
    out[4..].copy_from_slice(&fraction.to_be_bytes());
    out
}

#[derive(Clone, Copy)]
struct Reply {
    server_ms: u64,
    li: u8,
    mode: u8,
    stratum: u8,
    ref_id: [u8; 4],
    /// NTP short format.
    root_delay: u32,
    root_dispersion: u32,
    origin_ok: bool,
    len: usize,
}

impl Reply {
    fn at(server_ms: u64) -> Self {
        Self {
            server_ms,
            li: 0,
            mode: 4,
            stratum: 2,
            ref_id: [192, 0, 2, 1],
            root_delay: 0x0000_0800,
            root_dispersion: 0x0000_0800,
            origin_ok: true,
            len: 48,
        }
    }

    /// The response to `request`: SNTPv4 server mode, origin timestamp echoed.
    fn packet(&self, request: &[u8]) -> Vec<u8> {
        let mut packet = std::vec![0u8; 48];
        packet[0] = (self.li << 6) | (4 << 3) | self.mode;
        packet[1] = self.stratum;
        packet[4..8].copy_from_slice(&self.root_delay.to_be_bytes());
        packet[8..12].copy_from_slice(&self.root_dispersion.to_be_bytes());
        packet[12..16].copy_from_slice(&self.ref_id);
        packet[16..24].copy_from_slice(&wire(self.server_ms.saturating_sub(10_000)));
        let origin = if self.origin_ok {
            request[40..48].try_into().unwrap()
        } else {
            [9; 8]
        };
        packet[24..32].copy_from_slice(&origin);
        packet[32..40].copy_from_slice(&wire(self.server_ms));
        packet[40..48].copy_from_slice(&wire(self.server_ms));
        packet.truncate(self.len);
        packet
    }
}

/// Fake server: answers the next request with `reply`, after `latency_ms` of
/// pivot time.
async fn serve(ip: &Ip, reply: Reply, now_ms: &Cell<u64>, latency_ms: u64) -> SocketAddr {
    poll_fn(|cx| match ip.udp_peer_received() {
        Some((to, request)) => {
            assert_eq!(request.len(), 48);
            assert_eq!(to.port(), 123);
            now_ms.set(now_ms.get() + latency_ms);
            assert!(ip.udp_peer_send(to, &reply.packet(&request)));
            Poll::Ready(to)
        }
        None => {
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await
}

const SERVERS: &[&str] = &["a.ntp.example", "b.ntp.example"];

fn exchange(reply: Reply, pivot_ms: u64) -> Result<rohi_time::TimeSample, NtpError> {
    exchange_with_latency(reply, pivot_ms, 0)
}

fn exchange_with_latency(
    reply: Reply,
    pivot_ms: u64,
    latency_ms: u64,
) -> Result<rohi_time::TimeSample, NtpError> {
    let ip = ip();
    let now_ms = Cell::new(pivot_ms);
    let mut source = NtpTimeSource::new(
        NetworkHandle::new(&ip),
        NeverDelay,
        NtpConfig::new(SERVERS),
        || UnixTime::from_millis(now_ms.get()),
    );
    let (sample, _) = block_on(join(
        source.sample(),
        serve(&ip, reply, &now_ms, latency_ms),
    ));
    assert_eq!(ip.open_udp_sockets(), 0, "the socket is closed after use");
    sample
}

#[test]
fn valid_response_becomes_a_sample() {
    let sample = exchange(Reply::at(SERVER_MS), SERVER_MS).unwrap();
    assert_eq!(sample.unix_time, UnixTime::from_millis(SERVER_MS));
    // Server root delay/2 + dispersion: 1/32 s + 1/32 s.
    let uncertainty = sample.uncertainty.unwrap();
    assert!(uncertainty > Duration::from_millis(40), "{uncertainty:?}");
    assert!(uncertainty < Duration::from_millis(100), "{uncertainty:?}");
}

#[test]
fn half_the_round_trip_is_added_and_counted_as_uncertainty() {
    let sample = exchange_with_latency(Reply::at(SERVER_MS), SERVER_MS, 200).unwrap();
    let added = sample.unix_time.as_millis() - SERVER_MS;
    assert!((90..=100).contains(&added), "{added}");
    assert!(sample.uncertainty.unwrap() >= Duration::from_millis(90));
}

#[test]
fn era_is_resolved_with_the_pivot() {
    // The NTP seconds wrap at 2036-02-07T06:28:16Z.
    let wrap_ms = (u64::from(u32::MAX) + 1 - NTP_DELTA) * 1_000;
    for (server_ms, pivot_ms) in [
        (wrap_ms - 5_000, wrap_ms - 5_000),
        (wrap_ms + 5_000, wrap_ms - 60_000),
        (wrap_ms - 5_000, wrap_ms + 60_000),
        // A device running in 2041, long after the second era began.
        (2_240_000_000_000, 2_240_000_000_000),
    ] {
        let sample = exchange(Reply::at(server_ms), pivot_ms).unwrap();
        assert_eq!(sample.unix_time, UnixTime::from_millis(server_ms));
    }
}

#[test]
fn a_pivot_more_than_half_an_era_off_picks_the_wrong_era() {
    // Documented limit: the era is chosen around the pivot. The result is
    // then far outside any plausibility window.
    let sample = exchange(
        Reply::at(SERVER_MS),
        SERVER_MS + (1 << 31) * 1_000 + 3_600_000,
    )
    .unwrap();
    assert_ne!(sample.unix_time, UnixTime::from_millis(SERVER_MS));
    let config = TimeConfig::new(FLOOR);
    assert!(sample.unix_time < config.not_before || sample.unix_time > config.not_after);
}

#[test]
fn protocol_violations_are_rejected() {
    let reply = Reply::at(SERVER_MS);
    let cases = [
        (
            "kiss-o'-death",
            Reply {
                stratum: 0,
                ref_id: *b"RATE",
                ..reply
            },
            NtpError::Rejected,
        ),
        (
            "unsynchronized",
            Reply { li: 3, ..reply },
            NtpError::Rejected,
        ),
        (
            "root distance",
            Reply {
                root_dispersion: 0x0020_0000,
                ..reply
            },
            NtpError::Rejected,
        ),
        (
            "stratum 16",
            Reply {
                stratum: 16,
                ..reply
            },
            NtpError::InvalidResponse,
        ),
        (
            "not a server",
            Reply { mode: 3, ..reply },
            NtpError::InvalidResponse,
        ),
        (
            "origin mismatch",
            Reply {
                origin_ok: false,
                ..reply
            },
            NtpError::InvalidResponse,
        ),
        (
            "truncated",
            Reply { len: 40, ..reply },
            NtpError::InvalidResponse,
        ),
    ];
    for (name, reply, expected) in cases {
        assert_eq!(exchange(reply, SERVER_MS).err(), Some(expected), "{name}");
    }
}

#[test]
fn errors_map_to_sync_failure_categories() {
    for (error, failure) in [
        (
            NtpError::NetworkUnavailable,
            SyncFailure::NetworkUnavailable,
        ),
        (NtpError::Resolve, SyncFailure::Resolve),
        (NtpError::Timeout, SyncFailure::Timeout),
        (NtpError::InvalidResponse, SyncFailure::InvalidResponse),
        (NtpError::Rejected, SyncFailure::Rejected),
        (NtpError::NoSocket, SyncFailure::Source),
        (NtpError::Network, SyncFailure::Source),
    ] {
        assert_eq!(SyncFailure::from(error), failure);
    }
}

fn source<'a>(ip: &'a Ip, delay: impl DelayNs + 'a) -> impl TimeSource<Error = NtpError> + 'a {
    NtpTimeSource::new(
        NetworkHandle::new(ip),
        delay,
        NtpConfig::new(SERVERS),
        || UnixTime::from_millis(SERVER_MS),
    )
}

#[test]
fn silent_server_times_out_and_the_next_attempt_uses_the_next_server() {
    let ip = ip();
    let mut source = source(&ip, InstantDelay);
    assert_eq!(block_on(source.sample()).err(), Some(NtpError::Timeout));
    assert_eq!(ip.udp_peer_received().unwrap().0.ip(), SERVER_A);
    assert_eq!(block_on(source.sample()).err(), Some(NtpError::Timeout));
    assert_eq!(ip.udp_peer_received().unwrap().0.ip(), SERVER_B);
    assert_eq!(block_on(source.sample()).err(), Some(NtpError::Timeout));
    assert_eq!(ip.udp_peer_received().unwrap().0.ip(), SERVER_A);
    assert_eq!(ip.open_udp_sockets(), 0);
}

#[test]
fn network_problems_are_reported_by_category() {
    let ip = ip();
    let mut source = source(&ip, NeverDelay);

    ip.set_ready(false);
    assert!(!source.is_available());
    assert_eq!(
        block_on(source.sample()).err(),
        Some(NtpError::NetworkUnavailable)
    );
    ip.set_ready(true);
    assert!(source.is_available());

    // Every UDP socket taken by someone else.
    let net = NetworkHandle::new(&ip);
    let held = (net.udp_socket().unwrap(), net.udp_socket().unwrap());
    assert_eq!(block_on(source.sample()).err(), Some(NtpError::NoSocket));
    drop(held);

    // The second server does not resolve.
    let ip2 = FakeIp::<1, 64>::new();
    ip2.set_ready(true);
    let mut unresolved = NtpTimeSource::new(
        NetworkHandle::new(&ip2),
        NeverDelay,
        NtpConfig::new(SERVERS),
        || UnixTime::from_millis(SERVER_MS),
    );
    assert_eq!(block_on(unresolved.sample()).err(), Some(NtpError::Resolve));
}

#[test]
fn losing_the_network_during_the_exchange_is_reported() {
    let ip = ip();
    let mut source = source(&ip, NeverDelay);
    let mut sample = pin!(source.sample());
    assert!(poll_once(sample.as_mut()).is_pending());
    ip.set_ready(false);
    assert_eq!(block_on(sample).err(), Some(NtpError::NetworkUnavailable));
    assert_eq!(ip.open_udp_sockets(), 0);
}

#[test]
fn cancelled_sample_releases_its_socket() {
    let ip = ip();
    let mut source = source(&ip, NeverDelay);
    {
        let mut sample = pin!(source.sample());
        assert!(poll_once(sample.as_mut()).is_pending());
        assert_eq!(ip.open_udp_sockets(), 1);
    }
    assert_eq!(ip.open_udp_sockets(), 0);
}

#[derive(Clone, Default)]
struct TestClock(Rc<Cell<u64>>);

impl Clock for TestClock {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::from_millis(self.0.get())
    }
}

/// Completes at once and advances the clock by the waited time.
#[derive(Clone)]
struct ClockDelay(TestClock);

impl DelayNs for ClockDelay {
    async fn delay_ns(&mut self, _ns: u32) {}

    async fn delay_ms(&mut self, ms: u32) {
        (self.0).0.set((self.0).0.get() + u64::from(ms));
    }
}

#[test]
fn ntp_drives_the_time_service_through_network_loss_and_recovery() {
    let ip = ip();
    let clock = TestClock::default();
    let service: TimeService<TestClock, NoopRawMutex> =
        TimeService::new(clock.clone(), TimeConfig::new(FLOOR));
    let pivot_ms = Cell::new(0);
    let ntp = NtpTimeSource::new(
        NetworkHandle::new(&ip),
        NeverDelay,
        NtpConfig::new(SERVERS),
        || service.pivot(),
    );
    let mut sync = Synchronizer::new(
        &service,
        ntp,
        ClockDelay(clock.clone()),
        SyncSchedule::DEFAULT,
    );

    // Before the first sample: unavailable, never a made-up date.
    assert_eq!(service.now(), None);

    block_on(join(
        sync.step(),
        serve(&ip, Reply::at(SERVER_MS), &pivot_ms, 0),
    ));
    assert_eq!(service.status().state, TimeSyncState::Synchronized);
    let synced = service.now().unwrap();
    assert!(synced.as_millis() >= SERVER_MS);

    // The network disappears: the clock keeps running from the anchor and
    // becomes stale after the threshold.
    ip.set_ready(false);
    let polls = service.config().stale_after.as_millis() as usize / 2_000 + 1;
    for _ in 0..polls {
        block_on(sync.step());
    }
    let status = service.status();
    assert_eq!(status.state, TimeSyncState::Stale);
    assert_eq!(status.consecutive_failures, 0);
    assert_eq!(status.last_failure, Some(SyncFailure::NetworkUnavailable));
    assert!(service.now().unwrap() > synced);

    // The network returns: it resynchronizes right away.
    ip.set_ready(true);
    let server_ms = service.now().unwrap().as_millis() + 1_500;
    block_on(join(
        sync.step(),
        serve(&ip, Reply::at(server_ms), &pivot_ms, 0),
    ));
    assert_eq!(service.status().state, TimeSyncState::Synchronized);
    assert_eq!(service.status().last_failure, None);
}

#[test]
fn an_implausible_server_time_never_replaces_the_anchor() {
    let ip = ip();
    let clock = TestClock::default();
    let service: TimeService<TestClock, NoopRawMutex> =
        TimeService::new(clock.clone(), TimeConfig::new(FLOOR));
    let now_ms = Cell::new(0);
    let mut ntp = NtpTimeSource::new(
        NetworkHandle::new(&ip),
        NeverDelay,
        NtpConfig::new(SERVERS),
        || service.pivot(),
    );

    // A well-formed answer from 1999: before the build time of the firmware.
    let (result, _) = block_on(join(
        sync_once(&service, &mut ntp),
        serve(&ip, Reply::at(920_000_000_000), &now_ms, 0),
    ));
    assert_eq!(result, Err(SyncFailure::Implausible));
    assert_eq!(service.now(), None);

    let (result, _) = block_on(join(
        sync_once(&service, &mut ntp),
        serve(&ip, Reply::at(SERVER_MS), &now_ms, 0),
    ));
    assert_eq!(result, Ok(()));
    let anchor = service.now().unwrap();

    let (result, _) = block_on(join(
        sync_once(&service, &mut ntp),
        serve(
            &ip,
            Reply {
                stratum: 0,
                ..Reply::at(SERVER_MS)
            },
            &now_ms,
            0,
        ),
    ));
    assert_eq!(result, Err(SyncFailure::Rejected));
    assert_eq!(service.now(), Some(anchor));
    assert_eq!(service.status().consecutive_failures, 1);
}
