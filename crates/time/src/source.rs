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

use core::time::Duration;

use crate::UnixTime;

/// Kind of an absolute-time source, for diagnostics.
///
/// Consumers of the time do not normally branch on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSourceKind {
    /// Network time (NTP/SNTP).
    Ntp,
    /// Hardware real-time clock.
    Rtc,
    /// Time supplied from outside, for example through the control plane.
    External,
}

/// Why a synchronization attempt did not produce time.
///
/// A coarse category that every source maps its own errors to; it is shown in
/// diagnostics and does not carry source details.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncFailure {
    /// The network cannot carry traffic.
    NetworkUnavailable,
    /// The server name could not be resolved.
    Resolve,
    /// The source did not answer in time.
    Timeout,
    /// The answer was malformed or failed protocol validation.
    InvalidResponse,
    /// The source refused to serve (for example a Kiss-o'-Death answer or an
    /// unsynchronized server).
    Rejected,
    /// The sample was well-formed but not plausible, see
    /// [`TimeConfig`](crate::TimeConfig).
    Implausible,
    /// Any other source failure, for example a bus error of a hardware clock.
    Source,
}

/// An estimate of absolute time obtained from a [`TimeSource`].
///
/// The sample describes the instant at which [`TimeSource::sample`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeSample {
    /// The estimated current time.
    pub unix_time: UnixTime,
    /// Half-width of the estimate's error bar, when the source knows it.
    pub uncertainty: Option<Duration>,
}

/// A source of absolute time: an NTP client, a hardware RTC, a value supplied
/// from outside.
///
/// The source only produces samples. It does not own the time, does not keep
/// an offset and does not decide whether a sample is trusted: validation,
/// the anchor and the synchronization state belong to
/// [`TimeService`](crate::TimeService).
///
/// Cancellation: `sample` must be cancel-safe and must bound its own waits so
/// a dead network or bus cannot stall the synchronization loop forever.
#[allow(async_fn_in_trait)]
pub trait TimeSource {
    /// Source-specific failure, mapped to the coarse [`SyncFailure`].
    type Error: Into<SyncFailure>;

    /// What kind of source this is.
    fn kind(&self) -> TimeSourceKind;

    /// `false` while sampling cannot work at all, for example while the
    /// network is down. The synchronization loop then waits without counting a
    /// failure. Sources that are always available keep the default.
    fn is_available(&self) -> bool {
        true
    }

    /// Obtains one sample of the current absolute time.
    async fn sample(&mut self) -> Result<TimeSample, Self::Error>;
}
