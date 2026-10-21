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

use core::fmt;

use postcard::Error as CodecError;

use crate::SectionId;

/// Failure to convert a stored section payload into a typed section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SectionError {
    /// The stored version cannot be decoded by this firmware.
    UnsupportedVersion { id: SectionId, version: u16 },
    /// The payload is not a valid, completely consumed postcard encoding.
    Decode(CodecError),
}

impl fmt::Display for SectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { id, version } => {
                write!(f, "unsupported version {version} of section {id}")
            }
            Self::Decode(error) => write!(f, "section payload decode failed: {error}"),
        }
    }
}

impl From<CodecError> for SectionError {
    fn from(error: CodecError) -> Self {
        Self::Decode(error)
    }
}

/// Configuration store error, generic over the storage backend error `E`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigError<E> {
    /// The storage backend failed.
    Storage(E),
    /// A stored section could not be decoded into the requested type.
    Section(SectionError),
    /// The storage geometry cannot hold two slots.
    InvalidStorage(&'static str),
    /// The active container is structurally invalid.
    Corrupt(&'static str),
    /// The encoded section exceeds `MaxSize` or the section size limit.
    SectionTooLarge {
        id: SectionId,
        len: usize,
        max: usize,
    },
    /// The configuration would not fit into one slot.
    ContainerFull,
    /// The container would exceed the maximum number of sections.
    TooManySections,
    /// The serialized size changed between the sizing and writing passes.
    EncodeMismatch { id: SectionId },
    /// The freshly written slot failed read-back verification.
    VerifyFailed,
    /// Serializing a section failed.
    Encode(postcard::Error),
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
            Self::Corrupt(reason) => write!(f, "corrupt configuration: {reason}"),
            Self::SectionTooLarge { id, len, max } => {
                write!(f, "section {id} is {len} bytes, maximum is {max}")
            }
            Self::ContainerFull => f.write_str("configuration does not fit into a slot"),
            Self::TooManySections => f.write_str("too many configuration sections"),
            Self::EncodeMismatch { id } => {
                write!(f, "section {id} encoded size changed while writing")
            }
            Self::VerifyFailed => f.write_str("written slot failed verification"),
            Self::Encode(error) => write!(f, "section serialization failed: {error}"),
            Self::BufferTooSmall { required } => {
                write!(f, "buffer too small, {required} bytes required")
            }
        }
    }
}
