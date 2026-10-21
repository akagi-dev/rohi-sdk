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

use postcard::experimental::max_size::MaxSize;
use serde::{Serialize, de::DeserializeOwned};

use crate::{SectionError, SectionId};

/// An independent, typed, persistently stored configuration section.
///
/// Sections are serialized with [postcard](https://docs.rs/postcard). Implement
/// the trait with `#[derive(ConfigSection)]` rather than by hand: the derive
/// generates [`NAME`](Self::NAME), [`ID`](Self::ID) and
/// [`VERSION`](Self::VERSION) from `#[config(...)]` attributes.
///
/// Sections must be bounded: [`MaxSize`] limits the stored payload, so prefer
/// fixed arrays and bounded string/vector types over `String` and `Vec`.
pub trait ConfigSection: Serialize + DeserializeOwned + MaxSize + Sized {
    /// Logical section name (the hashed name, or the Rust type name).
    const NAME: &'static str;
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
            return Err(SectionError::UnsupportedVersion {
                id: Self::ID,
                version,
            });
        }
        crate::output::decode_all(payload)
    }
}
