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
use core::net::SocketAddr;
use core::time::Duration;

use embedded_hal_async::delay::DelayNs;
use embedded_nal_async::AddrType;
use rohi_hal::runtime::with_timeout;
use rohi_net::{IpBackend, NetError, NetworkHandle, UdpSocket};
use rohi_time::{TimeSample, TimeSource, TimeSourceKind, UnixTime};
use sntpc::{NtpContext, NtpResult, NtpTimestampGenerator, NtpUdpSocket};

use crate::{NTP_PORT, NtpConfig, NtpError};

/// SNTP client as a [`TimeSource`].
///
/// - `B`: the IP backend behind the [`NetworkHandle`] (DNS and UDP).
/// - `D`: delay used to bound DNS and the exchange.
/// - `P`: the pivot, a function returning the best known current time, see
///   the [crate documentation](crate#ntp-era).
///
/// Each [`sample`](TimeSource::sample) uses one server of the configuration
/// and one short-lived UDP socket, so nothing is held between samples. After
/// a failure the next sample uses the next server. The source is available
/// while the network is ready; a dead network is therefore not a failure of
/// the synchronization loop.
///
/// Cancellation: `sample` is cancel-safe; dropping it closes the socket.
pub struct NtpTimeSource<'a, B: IpBackend, D, P> {
    net: NetworkHandle<'a, B>,
    delay: D,
    config: NtpConfig<'a>,
    pivot: P,
    next_server: usize,
}

impl<'a, B, D, P> NtpTimeSource<'a, B, D, P>
where
    B: IpBackend,
    D: DelayNs,
    P: Fn() -> UnixTime,
{
    /// Source talking to the servers of `config` through `net`.
    pub fn new(net: NetworkHandle<'a, B>, delay: D, config: NtpConfig<'a>, pivot: P) -> Self {
        Self {
            net,
            delay,
            config,
            pivot,
            next_server: 0,
        }
    }

    async fn exchange(&mut self) -> Result<TimeSample, NtpError> {
        if !self.net.is_ready() {
            return Err(NtpError::NetworkUnavailable);
        }
        let host = self.config.servers()[self.next_server];
        let timeout_ms = self.config.timeout_ms;

        let address = with_timeout(
            &mut self.delay,
            timeout_ms,
            self.net.resolve(host, AddrType::IPv4),
        )
        .await
        .map_err(|_| NtpError::Timeout)?
        .map_err(|error| match error {
            NetError::NetworkUnavailable => NtpError::NetworkUnavailable,
            NetError::Timeout => NtpError::Timeout,
            _ => NtpError::Resolve,
        })?;

        let socket = self.net.udp_socket().map_err(|error| match error {
            NetError::NetworkUnavailable => NtpError::NetworkUnavailable,
            NetError::NoSocketAvailable => NtpError::NoSocket,
            _ => NtpError::Network,
        })?;
        let socket = Socket {
            socket: &socket,
            error: Cell::new(None),
        };
        let context = NtpContext::new(PivotClock::new(&self.pivot));
        let server = SocketAddr::new(address, NTP_PORT);
        let result = with_timeout(
            &mut self.delay,
            timeout_ms,
            sntpc::get_time(server, &socket, context),
        )
        .await
        .map_err(|_| NtpError::Timeout)?;

        match result {
            Ok(result) => Ok(sample_of(&result)),
            Err(error) => Err(map_error(error, socket.error.get())),
        }
    }
}

impl<B, D, P> TimeSource for NtpTimeSource<'_, B, D, P>
where
    B: IpBackend,
    D: DelayNs,
    P: Fn() -> UnixTime,
{
    type Error = NtpError;

    fn kind(&self) -> TimeSourceKind {
        TimeSourceKind::Ntp
    }

    fn is_available(&self) -> bool {
        self.net.is_ready()
    }

    async fn sample(&mut self) -> Result<TimeSample, NtpError> {
        let result = self.exchange().await;
        self.next_server = (self.next_server + 1) % self.config.servers().len();
        result
    }
}

/// The server's transmit time moved forward by half the round trip, with the
/// round trip and the server's own error bounds as uncertainty.
///
/// The local offset reported by `sntpc` is not used: the local clock is only
/// the pivot, not a time reference.
fn sample_of(result: &NtpResult) -> TimeSample {
    let micros = |short: u32| u64::from(short) * 1_000_000 / 65_536;
    let half_trip_us = result.roundtrip() / 2;
    let millis = result
        .sec()
        .saturating_mul(1_000)
        .saturating_add(u64::from(sntpc::fraction_to_milliseconds(
            result.sec_fraction(),
        )))
        .saturating_add(half_trip_us / 1_000);
    let uncertainty_us = half_trip_us
        .saturating_add(micros(result.root_delay()) / 2)
        .saturating_add(micros(result.root_dispersion()));
    TimeSample {
        unix_time: UnixTime::from_millis(millis),
        uncertainty: Some(Duration::from_micros(uncertainty_us)),
    }
}

fn map_error(error: sntpc::Error, network: Option<NetError>) -> NtpError {
    use sntpc::Error;
    match error {
        Error::Network if network == Some(NetError::NetworkUnavailable) => {
            NtpError::NetworkUnavailable
        }
        Error::Network => NtpError::Network,
        Error::UnsynchronizedClock | Error::ExcessiveRootDistance | Error::KissOfDeath(_) => {
            NtpError::Rejected
        }
        Error::AddressResolve => NtpError::Resolve,
        _ => NtpError::InvalidResponse,
    }
}

/// Timestamp generator of `sntpc`: the current pivot, in whole milliseconds.
struct PivotClock<'p, P> {
    pivot: &'p P,
    now: UnixTime,
}

impl<'p, P: Fn() -> UnixTime> PivotClock<'p, P> {
    fn new(pivot: &'p P) -> Self {
        Self {
            pivot,
            now: pivot(),
        }
    }
}

impl<P> Clone for PivotClock<'_, P> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<P> Copy for PivotClock<'_, P> {}

impl<P: Fn() -> UnixTime> NtpTimestampGenerator for PivotClock<'_, P> {
    fn init(&mut self) {
        self.now = (self.pivot)();
    }

    fn timestamp_sec(&self) -> u64 {
        self.now.as_secs()
    }

    fn timestamp_subsec_micros(&self) -> u32 {
        self.now.subsec_millis() * 1_000
    }

    /// 2^-10 s: the pivot has millisecond resolution.
    fn precision(&self) -> i8 {
        -10
    }
}

/// `sntpc` view of a ROHI UDP socket. Remembers the network error behind
/// the generic `sntpc` network error.
struct Socket<'s, U> {
    socket: &'s U,
    error: Cell<Option<NetError>>,
}

impl<U: UdpSocket> NtpUdpSocket for Socket<'_, U> {
    async fn send_to(&self, buf: &[u8], addr: SocketAddr) -> sntpc::Result<usize> {
        match self.socket.send_to(buf, addr).await {
            Ok(()) => Ok(buf.len()),
            Err(error) => {
                self.error.set(Some(error));
                Err(sntpc::Error::Network)
            }
        }
    }

    async fn recv_from(&self, buf: &mut [u8]) -> sntpc::Result<(usize, SocketAddr)> {
        match self.socket.recv_from(buf).await {
            Ok(received) => Ok(received),
            Err(error) => {
                self.error.set(Some(error));
                Err(sntpc::Error::Network)
            }
        }
    }
}
