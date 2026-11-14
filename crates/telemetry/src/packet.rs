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
//! The format-neutral boundary between telemetry and the code that encodes,
//! encrypts and signs packets.

use rohi_time::MonotonicTimestamp;

use crate::observation::MeasurementId;
use crate::profile::{FormatCapabilities, PacketFormatId, RecipientAddress};
use crate::section::{PreparedSections, ProfileStamp};
use crate::selection::Selection;

/// Why a packet was not produced. No variant leaves a partial or plaintext
/// fallback behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealError {
    /// Nothing the profile selects is available.
    NothingToPackage,
    /// The output buffer is too small for the packet.
    BufferTooSmall,
    /// A selected measurement cannot be encoded by the format.
    Unsupported(MeasurementId),
    /// The format needs UTC and there is no valid correlation of the monotonic
    /// clock to UTC. No timestamp is invented.
    TimeUnavailable,
    /// The device identity is not available.
    IdentityUnavailable,
    /// Secure randomness is not available.
    EntropyUnavailable,
    /// A private section could not be encrypted; the whole packet fails.
    Encryption,
    /// The packet could not be signed.
    Signing,
    /// The content does not fit the limits of the format.
    TooLarge,
}

/// Non-secret metadata of a packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacketInfo {
    /// The policy it was built under.
    pub profile: ProfileStamp,
    /// The format.
    pub format: PacketFormatId,
    /// Its length in bytes.
    pub length: usize,
    /// When the data was captured, on the monotonic clock.
    pub captured_at: MonotonicTimestamp,
}

/// An immutable, byte-exact, signed packet in a caller-provided buffer. There
/// is no mutable access to the signed bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedPacket<'a> {
    info: PacketInfo,
    bytes: &'a [u8],
}

impl<'a> SignedPacket<'a> {
    /// The packet of `bytes`; the length in `info` is set from them.
    pub fn new(mut info: PacketInfo, bytes: &'a [u8]) -> Self {
        info.length = bytes.len();
        Self { info, bytes }
    }

    /// The metadata.
    pub fn info(&self) -> &PacketInfo {
        &self.info
    }

    /// The signed bytes.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }
}

/// A packet format: encodes sections, encrypts private ones and signs the
/// whole into caller-provided memory.
///
/// Implemented outside the core (for example `rohi-telemetry-robonomics`);
/// the core never sees generated protocol types.
#[allow(async_fn_in_trait)]
pub trait PacketFormat {
    /// Which format this is.
    const ID: PacketFormatId;

    /// The measurements the format can encode correctly. Everything else is
    /// rejected when a profile is edited and never claimed as sent.
    fn supported(&self) -> Selection;

    /// Whether `recipient` is a key the format can encrypt to (exact length
    /// and cryptographic suitability).
    fn accepts_recipient(&self, recipient: &RecipientAddress) -> bool;

    /// Builds the packet. Failure never produces a partial or unencrypted
    /// packet: `out` is only meaningful on success.
    ///
    /// # Errors
    ///
    /// [`SealError`].
    async fn seal<'o, const N: usize, const R: usize>(
        &mut self,
        sections: &PreparedSections<'_, N, R>,
        out: &'o mut [u8],
    ) -> Result<SignedPacket<'o>, SealError>;
}

/// [`FormatCapabilities`] from a [`PacketFormat`]: profiles of any other
/// format have nothing supported.
#[derive(Debug)]
pub struct Capabilities<'a, F>(pub &'a F);

impl<F: PacketFormat> FormatCapabilities for Capabilities<'_, F> {
    fn supported(&self, format: PacketFormatId) -> Selection {
        if format == F::ID {
            self.0.supported()
        } else {
            Selection::NONE
        }
    }

    fn accepts_recipient(&self, format: PacketFormatId, recipient: &RecipientAddress) -> bool {
        format == F::ID && self.0.accepts_recipient(recipient)
    }
}
