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
//! recipients receive encrypted private sections.
//!
//! A profile says nothing about routes, endpoints or schedules. The state is
//! bounded, typed and persisted as one value ([`TelemetryConfig`]) by the
//! firmware's configuration owner; every mutation here works on a value, so
//! the owner can validate a candidate and publish it only after an atomic
//! commit.
//!
//! # Generations, incarnations and revisions
//!
//! The config keeps one persisted, monotonic `generation`. Every successful
//! mutation increments it. A profile's [`incarnation`](PacketProfile::incarnation)
//! is the generation at which it was created and its
//! [`revision`](PacketProfile::revision) the generation of its last edit.
//! Because the counter never decreases (also across removal and reboot), a
//! profile removed and recreated under the same text id has a different
//! incarnation, and a policy edit always has a new revision: neither can be
//! mistaken for an earlier policy.

use heapless::{String, Vec};
use minicbor::{CborLen, Decode, Encode};

use crate::selection::{DataSelection, Selection, SelectionError};

/// Maximum length of a profile id, in bytes.
pub const PROFILE_ID_LEN: usize = 24;

/// A packet format compiled into the firmware.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
#[cbor(index_only)]
pub enum PacketFormatId {
    /// The Robonomics Connectivity Protocol, see `rohi-telemetry-robonomics`.
    #[n(1)]
    Robonomics,
}

impl PacketFormatId {
    /// The number of the format on the control wire (the enum value of the
    /// Actions).
    pub const fn number(self) -> u32 {
        match self {
            Self::Robonomics => 1,
        }
    }

    /// The canonical name of the format, as the Actions show it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Robonomics => "robonomics",
        }
    }

    /// The format with canonical `name`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "robonomics" => Some(Self::Robonomics),
            _ => None,
        }
    }
}

/// The stable text id of a profile: 1 to [`PROFILE_ID_LEN`] bytes of
/// `a-z`, `0-9`, `-` and `_`.
#[derive(Clone, PartialEq, Eq, Hash, Encode, Decode, CborLen)]
#[cbor(transparent)]
pub struct PacketProfileId(
    #[n(0)]
    #[cbor(
        encode_with = "rohi_config::cbor::text::encode",
        decode_with = "rohi_config::cbor::text::decode",
        cbor_len = "rohi_config::cbor::text::cbor_len"
    )]
    String<PROFILE_ID_LEN>,
);

impl PacketProfileId {
    /// `None` unless `id` is a valid profile id.
    pub fn new(id: &str) -> Option<Self> {
        let valid = !id.is_empty()
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !valid {
            return None;
        }
        String::try_from(id).ok().map(Self)
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for PacketProfileId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        core::fmt::Debug::fmt(self.as_str(), f)
    }
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

/// One private section: what one recipient receives encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct PrivateSectionPolicy {
    /// Who receives it.
    #[n(0)]
    pub recipient: RecipientAddress,
    /// What it contains; never empty.
    #[n(1)]
    pub selection: DataSelection,
}

/// A packet profile.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct PacketProfile<const RECIPIENTS: usize> {
    /// The text id.
    #[n(0)]
    pub id: PacketProfileId,
    /// The format of the packet.
    #[n(1)]
    pub format: PacketFormatId,
    /// The public section; `none` when the profile is created.
    #[n(2)]
    pub public: DataSelection,
    /// The private sections, one per recipient.
    #[n(3)]
    #[cbor(with = "seq")]
    pub private: Vec<PrivateSectionPolicy, RECIPIENTS>,
    /// The generation at which the profile was created.
    #[n(4)]
    pub incarnation: u32,
    /// The generation of the last change of the policy.
    #[n(5)]
    pub revision: u32,
}

/// Why a mutation was refused. A refused mutation changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileError {
    /// The profile id is not valid.
    InvalidId,
    /// A profile with this id exists.
    Duplicate,
    /// No profile with this id.
    NotFound,
    /// No private section for this recipient.
    RecipientNotFound,
    /// The format cannot encode the selection or accept the recipient.
    Selection(SelectionError),
    /// A private selection must not be empty: remove the recipient instead.
    EmptyPrivateSelection,
    /// The recipient key is not usable with the format.
    InvalidRecipient,
    /// The profile table is full.
    TooManyProfiles,
    /// The profile has all its private sections.
    TooManyRecipients,
}

/// All persisted profiles.
#[derive(Clone, Debug, PartialEq, Eq, Encode, Decode, CborLen)]
pub struct TelemetryConfig<const PROFILES: usize, const RECIPIENTS: usize> {
    /// Monotonic counter, see the module documentation.
    #[n(0)]
    pub generation: u32,
    /// The profiles, in creation order.
    #[n(1)]
    #[cbor(with = "seq")]
    pub profiles: Vec<PacketProfile<RECIPIENTS>, PROFILES>,
}

impl<const P: usize, const R: usize> Default for TelemetryConfig<P, R> {
    /// No profile: nothing is packaged and nothing is disclosed.
    fn default() -> Self {
        Self {
            generation: 0,
            profiles: Vec::new(),
        }
    }
}

/// What a format supports, as far as profile validation needs it.
pub trait FormatCapabilities {
    /// The measurements the format can encode correctly.
    fn supported(&self, format: PacketFormatId) -> Selection;

    /// Whether `recipient` is a key the format can encrypt to.
    fn accepts_recipient(&self, format: PacketFormatId, recipient: &RecipientAddress) -> bool;
}

impl<const P: usize, const R: usize> TelemetryConfig<P, R> {
    /// The profile with `id`.
    pub fn get(&self, id: &str) -> Option<&PacketProfile<R>> {
        self.profiles
            .iter()
            .find(|profile| profile.id.as_str() == id)
    }

    fn index(&self, id: &str) -> Result<usize, ProfileError> {
        self.profiles
            .iter()
            .position(|profile| profile.id.as_str() == id)
            .ok_or(ProfileError::NotFound)
    }

    fn bump(&mut self) -> u32 {
        self.generation = self.generation.saturating_add(1);
        self.generation
    }

    /// Creates an empty profile: public `none`, no private section. Creating a
    /// profile discloses nothing.
    ///
    /// # Errors
    ///
    /// [`ProfileError`]; the config is unchanged.
    pub fn add_profile(&mut self, id: &str, format: PacketFormatId) -> Result<(), ProfileError> {
        let id = PacketProfileId::new(id).ok_or(ProfileError::InvalidId)?;
        if self.get(id.as_str()).is_some() {
            return Err(ProfileError::Duplicate);
        }
        if self.profiles.is_full() {
            return Err(ProfileError::TooManyProfiles);
        }
        let generation = self.bump();
        let profile = PacketProfile {
            id,
            format,
            public: DataSelection::NONE,
            private: Vec::new(),
            incarnation: generation,
            revision: generation,
        };
        self.profiles
            .push(profile)
            .map_err(|_| ProfileError::TooManyProfiles)
    }

    /// Deletes a profile. The generation advances, so the same id created
    /// again is a new incarnation.
    ///
    /// # Errors
    ///
    /// [`ProfileError::NotFound`].
    pub fn remove_profile(&mut self, id: &str) -> Result<(), ProfileError> {
        let index = self.index(id)?;
        self.profiles.remove(index);
        self.bump();
        Ok(())
    }

    /// Replaces the public selection (never a union with the old one).
    ///
    /// # Errors
    ///
    /// [`ProfileError`]; the config is unchanged.
    pub fn set_public(
        &mut self,
        id: &str,
        selection: DataSelection,
        caps: &impl FormatCapabilities,
    ) -> Result<(), ProfileError> {
        let index = self.index(id)?;
        let supported = caps.supported(self.profiles[index].format);
        selection
            .measurements
            .validate(&supported)
            .map_err(ProfileError::Selection)?;
        let generation = self.bump();
        let profile = &mut self.profiles[index];
        profile.public = selection;
        profile.revision = generation;
        Ok(())
    }

    /// Adds the private section of `recipient`, or replaces its selection
    /// wholesale.
    ///
    /// # Errors
    ///
    /// [`ProfileError`]; the config is unchanged.
    pub fn add_private(
        &mut self,
        id: &str,
        recipient: RecipientAddress,
        selection: DataSelection,
        caps: &impl FormatCapabilities,
    ) -> Result<(), ProfileError> {
        let index = self.index(id)?;
        let format = self.profiles[index].format;
        if selection.measurements.is_none() {
            return Err(ProfileError::EmptyPrivateSelection);
        }
        if !caps.accepts_recipient(format, &recipient) {
            return Err(ProfileError::InvalidRecipient);
        }
        selection
            .measurements
            .validate(&caps.supported(format))
            .map_err(ProfileError::Selection)?;
        let existing = self.profiles[index]
            .private
            .iter()
            .position(|section| section.recipient == recipient);
        if existing.is_none() && self.profiles[index].private.is_full() {
            return Err(ProfileError::TooManyRecipients);
        }
        let generation = self.bump();
        let profile = &mut self.profiles[index];
        match existing {
            Some(position) => profile.private[position].selection = selection,
            None => {
                profile
                    .private
                    .push(PrivateSectionPolicy {
                        recipient,
                        selection,
                    })
                    .map_err(|_| ProfileError::TooManyRecipients)?;
            }
        }
        profile.revision = generation;
        Ok(())
    }

    /// Removes the private section of `recipient`.
    ///
    /// # Errors
    ///
    /// [`ProfileError::NotFound`] or [`ProfileError::RecipientNotFound`].
    pub fn remove_private(
        &mut self,
        id: &str,
        recipient: &RecipientAddress,
    ) -> Result<(), ProfileError> {
        let index = self.index(id)?;
        let position = self.profiles[index]
            .private
            .iter()
            .position(|section| section.recipient == *recipient)
            .ok_or(ProfileError::RecipientNotFound)?;
        let generation = self.bump();
        let profile = &mut self.profiles[index];
        profile.private.remove(position);
        profile.revision = generation;
        Ok(())
    }

    /// Checks a config that was decoded from storage: ids valid and unique,
    /// recipients unique, selections valid for the format, private selections
    /// non-empty, revisions not newer than the generation. A config that
    /// fails is never used: it must not widen what is disclosed.
    ///
    /// # Errors
    ///
    /// The first [`ProfileError`] found.
    pub fn validate(&self, caps: &impl FormatCapabilities) -> Result<(), ProfileError> {
        for (index, profile) in self.profiles.iter().enumerate() {
            if PacketProfileId::new(profile.id.as_str()).is_none() {
                return Err(ProfileError::InvalidId);
            }
            if self.profiles[..index].iter().any(|p| p.id == profile.id) {
                return Err(ProfileError::Duplicate);
            }
            if profile.incarnation > profile.revision || profile.revision > self.generation {
                return Err(ProfileError::NotFound);
            }
            let supported = caps.supported(profile.format);
            profile
                .public
                .measurements
                .validate(&supported)
                .map_err(ProfileError::Selection)?;
            for (position, section) in profile.private.iter().enumerate() {
                if section.selection.measurements.is_none() {
                    return Err(ProfileError::EmptyPrivateSelection);
                }
                if !caps.accepts_recipient(profile.format, &section.recipient) {
                    return Err(ProfileError::InvalidRecipient);
                }
                if profile.private[..position]
                    .iter()
                    .any(|other| other.recipient == section.recipient)
                {
                    return Err(ProfileError::Duplicate);
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

/// CBOR of `heapless::Vec<T, N>` as a definite array.
mod seq {
    use heapless::Vec;
    use minicbor::decode::Error as DecodeError;
    use minicbor::encode::{Error as EncodeError, Write};
    use minicbor::{CborLen, Decode, Decoder, Encode, Encoder};

    pub fn encode<C, W: Write, T: Encode<C>, const N: usize>(
        value: &Vec<T, N>,
        e: &mut Encoder<W>,
        ctx: &mut C,
    ) -> Result<(), EncodeError<W::Error>> {
        e.array(value.len() as u64)?;
        for item in value {
            item.encode(e, ctx)?;
        }
        Ok(())
    }

    pub fn decode<'b, C, T: Decode<'b, C>, const N: usize>(
        d: &mut Decoder<'b>,
        ctx: &mut C,
    ) -> Result<Vec<T, N>, DecodeError> {
        let len = d
            .array()?
            .ok_or_else(|| DecodeError::message("indefinite arrays are not accepted"))?;
        let mut out = Vec::new();
        for _ in 0..len {
            out.push(T::decode(d, ctx)?)
                .map_err(|_| DecodeError::message("too many elements"))?;
        }
        Ok(out)
    }

    pub fn cbor_len<C, T: CborLen<C>, const N: usize>(value: &Vec<T, N>, ctx: &mut C) -> usize {
        (value.len() as u64).cbor_len(ctx)
            + value.iter().map(|item| item.cbor_len(ctx)).sum::<usize>()
    }
}

#[cfg(test)]
mod tests;
