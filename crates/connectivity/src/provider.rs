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
//! The boundary to the telemetry side: what connectivity needs from whoever
//! builds sealed packets, and nothing about their content.
//!
//! Connectivity never reads observations, keys or protocol messages. It asks
//! a [`PacketProvider`] to resolve a profile to its current stamp, to capture
//! once, to build a sealed packet for a stamp into a pool buffer, and whether
//! a stamp is still the current policy.

use rohi_telemetry::ProfileStamp;

/// Why a packet could not be produced. None of these is a transport error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildFailure {
    /// The profile policy changed or was removed while building.
    ProfileChanged,
    /// Nothing the profile selects is available.
    NothingToPackage,
    /// No valid UTC time.
    TimeUnavailable,
    /// The device identity is not available.
    IdentityUnavailable,
    /// No entropy.
    EntropyUnavailable,
    /// The packet does not fit its buffer or the format limits.
    TooLarge,
    /// A source or the format failed.
    Failed,
}

impl BuildFailure {
    /// The stable reason code of a rejected Action.
    pub const fn code(self) -> &'static str {
        match self {
            Self::ProfileChanged => "profile_changed",
            Self::NothingToPackage => "nothing_to_package",
            Self::TimeUnavailable => "time_unavailable",
            Self::IdentityUnavailable => "identity_unavailable",
            Self::EntropyUnavailable => "entropy_unavailable",
            Self::TooLarge => "packet_too_large",
            Self::Failed => "packet_failed",
        }
    }
}

/// Produces sealed packets for the publisher.
///
/// A capture is taken once per publication instant and shared by the packets
/// built from it; every packet is built once per distinct profile stamp and
/// its bytes are then delivered unchanged to every route and retry.
#[allow(async_fn_in_trait)]
pub trait PacketProvider {
    /// One bounded capture of the sources.
    type Snapshot;

    /// The current stamp of `profile`; `None` if it does not exist.
    fn resolve(&self, profile: &str) -> Option<ProfileStamp>;

    /// Whether `stamp` is still the current policy of its profile.
    fn is_current(&self, stamp: &ProfileStamp) -> bool;

    /// Captures the sources once.
    ///
    /// # Errors
    ///
    /// [`BuildFailure`].
    async fn capture(&mut self) -> Result<Self::Snapshot, BuildFailure>;

    /// Builds the packet of `stamp` from `snapshot` into `out`, returning its
    /// length. Must fail with [`BuildFailure::ProfileChanged`] when `stamp` is
    /// no longer current; never returns a partial or unencrypted packet.
    ///
    /// # Errors
    ///
    /// [`BuildFailure`].
    async fn build(
        &mut self,
        snapshot: &Self::Snapshot,
        stamp: &ProfileStamp,
        out: &mut [u8],
    ) -> Result<usize, BuildFailure>;

    /// Completes when the policy of any profile changed.
    async fn changed(&self);
}

/// Builds a fresh packet on demand for export, without any route.
#[allow(async_fn_in_trait)]
pub trait PacketExporter {
    /// The current stamp of `profile`; `None` if it does not exist.
    fn resolve(&self, profile: &str) -> Option<ProfileStamp>;

    /// Captures, builds and seals a packet for `profile` into `out`.
    ///
    /// # Errors
    ///
    /// [`BuildFailure`]; nothing usable is left in `out`.
    async fn export(
        &self,
        profile: &str,
        out: &mut [u8],
    ) -> Result<(ProfileStamp, usize), BuildFailure>;
}
