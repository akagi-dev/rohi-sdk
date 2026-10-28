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
///////////////////////////////////////////////////////////////////////////////

use core::fmt;

use crate::SectionId;

/// Coarse classification of a `minicbor` decode failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeKind {
    /// The input ended before the value was complete.
    EndOfInput,
    /// A CBOR item of an unexpected type was found.
    TypeMismatch,
    /// An enum variant is not known to this firmware.
    UnknownVariant,
    /// A required field is missing.
    MissingValue,
    /// Any other decode failure (invalid value, custom message, ...).
    Invalid,
}

/// A failed `minicbor` decode of a section payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodeFailure {
    pub kind: DecodeKind,
    /// Byte offset in the payload where decoding failed, if known.
    pub position: Option<usize>,
}

impl From<minicbor::decode::Error> for DecodeFailure {
    fn from(error: minicbor::decode::Error) -> Self {
        let kind = if error.is_end_of_input() {
            DecodeKind::EndOfInput
        } else if error.is_type_mismatch() {
            DecodeKind::TypeMismatch
        } else if error.is_unknown_variant() {
            DecodeKind::UnknownVariant
        } else if error.is_missing_value() {
            DecodeKind::MissingValue
        } else {
            DecodeKind::Invalid
        };
        Self {
            kind,
            position: error.position(),
        }
    }
}

impl fmt::Display for DecodeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let reason = match self.kind {
            DecodeKind::EndOfInput => "unexpected end of input",
            DecodeKind::TypeMismatch => "unexpected CBOR type",
            DecodeKind::UnknownVariant => "unknown variant",
            DecodeKind::MissingValue => "missing value",
            DecodeKind::Invalid => "invalid value",
        };
        match self.position {
            Some(position) => write!(f, "{reason} at byte {position}"),
            None => f.write_str(reason),
        }
    }
}

/// Failure to decode one stored section into its typed value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionError {
    /// The stored version cannot be decoded by this firmware.
    UnsupportedSectionVersion { id: SectionId, version: u16 },
    /// The payload is not a valid CBOR encoding of the requested type.
    Decode(DecodeFailure),
    /// The payload holds a valid value followed by extra bytes.
    TrailingBytes { extra: usize },
}

impl fmt::Display for SectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSectionVersion { id, version } => {
                write!(f, "unsupported version {version} of section {id}")
            }
            Self::Decode(error) => write!(f, "section payload decode failed: {error}"),
            Self::TrailingBytes { extra } => {
                write!(f, "section payload has {extra} trailing byte(s)")
            }
        }
    }
}

impl From<DecodeFailure> for SectionError {
    fn from(error: DecodeFailure) -> Self {
        Self::Decode(error)
    }
}

/// Failure to encode a value with `minicbor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// The caller's buffer is too small for the encoding.
    BufferTooSmall,
    /// The value's `Encode` implementation failed.
    Failed,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BufferTooSmall => f.write_str("buffer too small for the encoding"),
            Self::Failed => f.write_str("CBOR encoding failed"),
        }
    }
}

/// Why a committed slot is not a valid configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corruption {
    /// The outer `SlotEnvelope` is not the expected CBOR structure.
    Envelope,
    /// The CRC-32 of the configuration payload does not match.
    Crc,
    /// The `ConfigPayload` is not the expected CBOR structure.
    Payload,
    /// A `SectionRecord` is malformed or exceeds the section size limit.
    Record,
    /// Two records use the same section ID.
    DuplicateSection(SectionId),
    /// The payload lists more than `MAX_SECTIONS` sections.
    TooManySections,
    /// The container format version is not supported by this firmware.
    UnsupportedFormatVersion(u16),
}

impl fmt::Display for Corruption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Envelope => f.write_str("malformed slot envelope"),
            Self::Crc => f.write_str("CRC mismatch"),
            Self::Payload => f.write_str("malformed configuration payload"),
            Self::Record => f.write_str("malformed section record"),
            Self::DuplicateSection(id) => write!(f, "duplicate section {id}"),
            Self::TooManySections => f.write_str("too many sections"),
            Self::UnsupportedFormatVersion(version) => {
                write!(f, "unsupported format version {version}")
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError<E> {
    /// The storage backend failed.
    Storage(E),
    /// A stored section could not be decoded into the requested type.
    Section(SectionError),
    /// The storage geometry cannot hold two slots.
    InvalidStorage(&'static str),
    /// Committed configuration exists but no committed slot is valid. This is
    /// never the same as an empty configuration.
    CorruptStorage(Corruption),
    /// The encoded section exceeds the section size limit.
    SectionTooLarge {
        id: SectionId,
        len: usize,
        max: usize,
    },
    /// The resulting configuration would not fit into one slot.
    StorageFull,
    /// The configuration would exceed the maximum number of sections.
    TooManySections,
    /// The encoded size changed between the sizing and writing passes.
    EncodeMismatch { id: SectionId },
    /// The freshly written slot failed read-back verification.
    VerifyFailed,
    /// Encoding a section failed.
    Encode(EncodeError),
    /// The caller's buffer cannot hold the section payload.
    BufferTooSmall { required: usize },
}

impl<E> From<SectionError> for ConfigError<E> {
    fn from(error: SectionError) -> Self {
        Self::Section(error)
    }
}

impl<E: fmt::Debug> fmt::Display for ConfigError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "storage error: {error:?}"),
            Self::Section(error) => write!(f, "{error}"),
            Self::InvalidStorage(reason) => write!(f, "invalid storage: {reason}"),
            Self::CorruptStorage(reason) => write!(f, "corrupt configuration: {reason}"),
            Self::SectionTooLarge { id, len, max } => {
                write!(f, "section {id} is {len} bytes, maximum is {max}")
            }
            Self::StorageFull => f.write_str("configuration does not fit into a slot"),
            Self::TooManySections => f.write_str("too many configuration sections"),
            Self::EncodeMismatch { id } => {
                write!(f, "section {id} encoded size changed while writing")
            }
            Self::VerifyFailed => f.write_str("written slot failed verification"),
            Self::Encode(error) => write!(f, "{error}"),
            Self::BufferTooSmall { required } => {
                write!(f, "buffer too small, {required} bytes required")
            }
        }
    }
}
