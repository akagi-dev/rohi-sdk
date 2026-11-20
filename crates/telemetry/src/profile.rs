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
//! Packet profiles: what a signed packet contains, what is public and which
//! recipient receives an encrypted private section.
//!
//! The model is fixed and index-addressed: a [`TelemetryConfig`] holds
//! `PROFILES` profile **slots** (Altruist Urban: 2), addressed by their
//! `profile_index` (`0`, `1`, ...), all of them always present. Every slot has
//! a **public** selection and at most one **private** section (one recipient
//! and its selection). There is no name, no format choice (the format is the
//! one compiled into the firmware) and no recipient list. Both selections of
//! every slot start empty: the existence of a slot publishes and discloses
//! nothing.
//!
//! A profile says nothing about routes, endpoints or schedules. The state is
//! bounded, typed and persisted as one value ([`TelemetryConfig`]) by the
//! firmware's configuration owner; every mutation here works on a value, so
//! the owner can validate a candidate and publish it only after an atomic
//! commit. An index outside the slots is refused with
//! [`ProfileError::InvalidIndex`] and changes nothing.
//!
//! # Generations and revisions
//!
//! The config keeps one persisted, monotonic `generation`. Every successful
//! mutation increments it, and the
//! [`revision`](PacketProfile::revision) of the edited slot is the generation
//! of its last edit (`0` while the slot was never edited). A packet is stamped
//! with `(profile_index, revision)` ([`ProfileStamp`](crate::ProfileStamp)), so
//! a policy edit, or [`TelemetryConfig::clear`], always gives the slot a new
//! revision that no earlier packet carries, while the other slot's packets
//! stay valid.

use minicbor::{CborLen, Decode, Encode};

use crate::selection::{DataSelection, Selection, SelectionError};

/// The packet format compiled into the firmware.
///
/// There is exactly one, so it is never chosen by a client or stored in a
/// profile; it is reported in [`PacketInfo`](crate::PacketInfo).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PacketFormatId {
    /// The Robonomics Connectivity Protocol, see `rohi-telemetry-robonomics`.
    Robonomics,
}

/// The address of a private-section recipient: a raw 32-byte public key.
/// Whether the key is usable is for the [`PacketFormat`](crate::PacketFormat).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RecipientAddress(pub [u8; 32]);

impl<C> Encode<C> for RecipientAddress {
    fn encode<W: minicbor::encode::Write>(
        &self,
        e: &mut minicbor::Encoder<W>,
        _: &mut C,
    ) -> Result<(), minicbor::encode::Error<W::Error>> {
        e.bytes(&self.0)?.ok()
    }
}

impl<C> CborLen<C> for RecipientAddress {
    fn cbor_len(&self, ctx: &mut C) -> usize {
        (self.0.len() as u64).cbor_len(ctx) + self.0.len()
    }
}

impl<'b, C> Decode<'b, C> for RecipientAddress {
    fn decode(d: &mut minicbor::Decoder<'b>, _: &mut C) -> Result<Self, minicbor::decode::Error> {
        <[u8; 32]>::try_from(d.bytes()?)
            .map(Self)
            .map_err(|_| minicbor::decode::Error::message("an address is 32 bytes"))
    }
}

/// The private section of a profile: what its one recipient receives
/// encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct PrivateSectionPolicy {
    /// Who receives it.
    #[n(0)]
    pub recipient: RecipientAddress,
    /// What it contains; never empty.
    #[n(1)]
    pub selection: DataSelection,
}

/// One profile slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct PacketProfile {
    /// The public section; `none` until it is set.
    #[n(0)]
    pub public: DataSelection,
    /// The private section: one recipient with its selection, or none.
    #[n(1)]
    pub private: Option<PrivateSectionPolicy>,
    /// The generation of the last change of the policy, `0` if never edited.
    #[n(2)]
    pub revision: u32,
}

impl PacketProfile {
    /// A slot that discloses nothing: empty public selection, no private
    /// section.
    pub const EMPTY: Self = Self {
        public: DataSelection::NONE,
        private: None,
        revision: 0,
    };
}

/// Why a mutation was refused. A refused mutation changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// The profile index is not a slot of this build.
    InvalidIndex,
    /// The format cannot encode the selection.
    Selection(SelectionError),
    /// A private selection must not be empty: remove the private section
    /// instead (a stored config with an empty one is invalid).
    EmptyPrivateSelection,
    /// The recipient key is not usable with the format.
    InvalidRecipient,
    /// A revision is newer than the generation: the config was not written by
    /// this code.
    InconsistentGeneration,
}

/// All persisted profile slots.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct TelemetryConfig<const PROFILES: usize> {
    /// Monotonic counter, see the module documentation.
    #[n(0)]
    pub generation: u32,
    /// The slots, by `profile_index`. A stored array of another length does
    /// not decode.
    #[n(1)]
    pub profiles: [PacketProfile; PROFILES],
}

impl<const P: usize> Default for TelemetryConfig<P> {
    /// Every slot empty: nothing is packaged and nothing is disclosed.
    fn default() -> Self {
        Self::EMPTY
    }
}

/// What a format supports, as far as profile validation needs it.
pub trait FormatCapabilities {
    /// The measurements the format can encode correctly.
    fn supported(&self) -> Selection;

    /// Whether `recipient` is a key the format can encrypt to.
    fn accepts_recipient(&self, recipient: &RecipientAddress) -> bool;
}

impl<const P: usize> TelemetryConfig<P> {
    /// The config of a device without any edit: every slot empty.
    pub const EMPTY: Self = Self {
        generation: 0,
        profiles: [PacketProfile::EMPTY; P],
    };

    /// The profile at `index`; `None` outside the slots.
    pub fn get(&self, index: u8) -> Option<&PacketProfile> {
        self.profiles.get(usize::from(index))
    }

    fn slot(&mut self, index: u8) -> Result<&mut PacketProfile, ProfileError> {
        self.profiles
            .get_mut(usize::from(index))
            .ok_or(ProfileError::InvalidIndex)
    }

    fn bump(&mut self) -> u32 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }

    /// Replaces the public selection of slot `index` (never a union with the
    /// old one).
    ///
    /// # Errors
    ///
    /// [`ProfileError`]; the config is unchanged.
    pub fn set_public(
        &mut self,
        index: u8,
        selection: DataSelection,
        caps: &impl FormatCapabilities,
    ) -> Result<(), ProfileError> {
        if usize::from(index) >= P {
            return Err(ProfileError::InvalidIndex);
        }
        selection
            .measurements
            .validate(&caps.supported())
            .map_err(ProfileError::Selection)?;
        let generation = self.bump();
        let profile = self.slot(index)?;
        profile.public = selection;
        profile.revision = generation;
        Ok(())
    }

    /// Sets the private section of slot `index`: `recipient` becomes its only
    /// recipient, replacing any earlier one, with `selection`. An empty
    /// selection (`none`) removes the private section instead and does not
    /// look at `recipient`.
    ///
    /// # Errors
    ///
    /// [`ProfileError`]; the config is unchanged.
    pub fn set_private(
        &mut self,
        index: u8,
        recipient: RecipientAddress,
        selection: DataSelection,
        caps: &impl FormatCapabilities,
    ) -> Result<(), ProfileError> {
        if usize::from(index) >= P {
            return Err(ProfileError::InvalidIndex);
        }
        let private = if selection.measurements.is_none() {
            None
        } else {
            if !caps.accepts_recipient(&recipient) {
                return Err(ProfileError::InvalidRecipient);
            }
            selection
                .measurements
                .validate(&caps.supported())
                .map_err(ProfileError::Selection)?;
            Some(PrivateSectionPolicy {
                recipient,
                selection,
            })
        };
        let generation = self.bump();
        let profile = self.slot(index)?;
        profile.private = private;
        profile.revision = generation;
        Ok(())
    }

    /// Empties every slot (a reset): public and private selections are
    /// cleared and every revision becomes the new generation, so no packet
    /// built before the reset is current.
    pub fn clear(&mut self) {
        let generation = self.bump();
        self.profiles = [PacketProfile {
            revision: generation,
            ..PacketProfile::EMPTY
        }; P];
    }

    /// Checks a config that was decoded from storage: selections valid for
    /// the format, private selections non-empty with a usable recipient,
    /// revisions not newer than the generation. A config that fails is never
    /// used: it must not widen what is disclosed.
    ///
    /// # Errors
    ///
    /// The first [`ProfileError`] found.
    pub fn validate(&self, caps: &impl FormatCapabilities) -> Result<(), ProfileError> {
        let supported = caps.supported();
        for profile in &self.profiles {
            if profile.revision > self.generation {
                return Err(ProfileError::InconsistentGeneration);
            }
            profile
                .public
                .measurements
                .validate(&supported)
                .map_err(ProfileError::Selection)?;
            if let Some(section) = &profile.private {
                if section.selection.measurements.is_none() {
                    return Err(ProfileError::EmptyPrivateSelection);
                }
                if !caps.accepts_recipient(&section.recipient) {
                    return Err(ProfileError::InvalidRecipient);
                }
                section
                    .selection
                    .measurements
                    .validate(&supported)
                    .map_err(ProfileError::Selection)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
