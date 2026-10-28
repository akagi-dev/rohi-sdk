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

use minicbor::{CborLen, Decode, Encode};

use crate::{SectionError, SectionId, codec};

/// A typed configuration section owned by one feature crate.
///
/// Implement it with `#[derive(ConfigSection)]` on a type that also derives
/// `minicbor::Encode`, `Decode` and `CborLen` with explicit `#[n(..)]` field
/// numbers. The encoded size comes from [`CborLen`]; there is no per-section
/// size attribute.
///
/// The default section ID is derived from the Rust struct name, so renaming a
/// persisted section type is a storage ABI change.
pub trait ConfigSection: Encode<()> + CborLen<()> + for<'b> Decode<'b, ()> + Sized {
    /// Stable persistent section identifier.
    const ID: SectionId;
    /// Version of this section's schema.
    const VERSION: u16;

    /// Decodes a stored payload written with `version`.
    ///
    /// The default accepts only [`VERSION`](Self::VERSION) and requires the
    /// complete payload to be consumed (trailing bytes are rejected). Override
    /// it (derive attribute `decode_versioned`) to migrate older versions of
    /// this section only.
    fn decode_versioned(version: u16, payload: &[u8]) -> Result<Self, SectionError> {
        if version != Self::VERSION {
            return Err(SectionError::UnsupportedSectionVersion {
                id: Self::ID,
                version,
            });
        }
        codec::decode_exact(payload)
    }
}
