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
//! Measurement selections: fixed 32-byte bitsets.
//!
//! ```text
//! 00 x 32  -> none
//! FF x 32  -> all: a dynamic sentinel, not a snapshot of today's catalog
//! otherwise -> explicit stable bits; bit i is byte[i / 8] bit (i % 8), LSB first
//! ```
//!
//! This is the same contract as the generic selection of the control plane
//! (`FieldDescriptor::with_selection`): names are parsed by the client against
//! the catalog Action, the device only ever sees these bytes.

use minicbor::decode::Error as DecodeError;
use minicbor::encode::{Error as EncodeError, Write};
use minicbor::{Decode, Decoder, Encode, Encoder};

use crate::catalog::LOCATION;
use crate::observation::{MeasurementId, SourceId};

/// Size of a selection, in bytes.
pub const SELECTION_LEN: usize = 32;

/// A set of measurement ids.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Selection([u8; SELECTION_LEN]);

impl core::fmt::Debug for Selection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_none() {
            f.write_str("Selection(none)")
        } else if self.is_all() {
            f.write_str("Selection(all)")
        } else {
            f.debug_set().entries(self.bits()).finish()
        }
    }
}

/// Why a selection is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionError {
    /// The explicit selection contains a bit that is unknown, reserved or that
    /// the packet format cannot encode.
    Unsupported(MeasurementId),
}

impl Selection {
    /// Selects nothing.
    pub const NONE: Self = Self([0; SELECTION_LEN]);
    /// Selects every available, supported measurement except `location`.
    pub const ALL: Self = Self([0xFF; SELECTION_LEN]);

    /// The selection of the wire bytes.
    pub const fn from_bytes(bytes: [u8; SELECTION_LEN]) -> Self {
        Self(bytes)
    }

    /// The selection of `bytes`, `None` unless exactly [`SELECTION_LEN`] long.
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        <[u8; SELECTION_LEN]>::try_from(bytes).ok().map(Self)
    }

    /// The wire bytes.
    pub const fn as_bytes(&self) -> &[u8; SELECTION_LEN] {
        &self.0
    }

    /// Whether nothing is selected.
    pub fn is_none(&self) -> bool {
        self.0.iter().all(|byte| *byte == 0)
    }

    /// Whether this is the dynamic `all` sentinel.
    pub fn is_all(&self) -> bool {
        self.0.iter().all(|byte| *byte == 0xFF)
    }

    /// Whether bit `id` is set; always false for `none`, and for `all` only
    /// as the raw bit (use [`resolve`](Self::resolve) first).
    pub fn contains(&self, id: MeasurementId) -> bool {
        self.0[usize::from(id.0) / 8] & (1 << (id.0 % 8)) != 0
    }

    /// This selection with bit `id` set.
    pub const fn with(mut self, id: MeasurementId) -> Self {
        self.0[id.0 as usize / 8] |= 1 << (id.0 % 8);
        self
    }

    /// This selection with bit `id` cleared.
    pub const fn without(mut self, id: MeasurementId) -> Self {
        self.0[id.0 as usize / 8] &= !(1 << (id.0 % 8));
        self
    }

    /// The set bits, ascending. Not meaningful for [`ALL`](Self::ALL).
    pub fn bits(&self) -> impl Iterator<Item = MeasurementId> + '_ {
        (0..=u8::MAX)
            .map(MeasurementId)
            .filter(|id| self.contains(*id))
    }

    /// The bits set in both.
    pub fn intersection(&self, other: &Self) -> Self {
        let mut out = [0; SELECTION_LEN];
        for (slot, (a, b)) in out.iter_mut().zip(self.0.iter().zip(&other.0)) {
            *slot = a & b;
        }
        Self(out)
    }

    /// Checks an explicit selection against the measurements `supported` by a
    /// format: `none` and `all` are always valid, an explicit selection must
    /// only contain supported bits.
    ///
    /// # Errors
    ///
    /// [`SelectionError::Unsupported`] with the lowest offending bit.
    pub fn validate(&self, supported: &Self) -> Result<(), SelectionError> {
        if self.is_none() || self.is_all() {
            return Ok(());
        }
        match self.bits().find(|id| !supported.contains(*id)) {
            Some(id) => Err(SelectionError::Unsupported(id)),
            None => Ok(()),
        }
    }

    /// The concrete set this selection stands for when a packet is built:
    /// `all` becomes everything `available` except `location` (a position is
    /// never disclosed implicitly), an explicit selection is itself.
    pub fn resolve(&self, available: &Self) -> Self {
        if self.is_all() {
            available.without(LOCATION)
        } else if self.is_none() {
            Self::NONE
        } else {
            *self
        }
    }
}

impl<C> Encode<C> for Selection {
    fn encode<W: Write>(&self, e: &mut Encoder<W>, _: &mut C) -> Result<(), EncodeError<W::Error>> {
        e.bytes(&self.0)?.ok()
    }
}

impl<C> minicbor::CborLen<C> for Selection {
    fn cbor_len(&self, ctx: &mut C) -> usize {
        (self.0.len() as u64).cbor_len(ctx) + self.0.len()
    }
}

impl<'b, C> Decode<'b, C> for Selection {
    fn decode(d: &mut Decoder<'b>, _: &mut C) -> Result<Self, DecodeError> {
        Self::from_slice(d.bytes()?).ok_or_else(|| DecodeError::message("a selection is 32 bytes"))
    }
}

/// Which sources a section draws from.
///
/// Only the device's own sources exist in v1. A broad measurement mask never
/// grants access to a source that is not local.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode, minicbor::CborLen)]
pub enum SourceSelection {
    /// Every local source.
    #[n(0)]
    Local,
    /// One local source instance.
    #[n(1)]
    Only(#[n(0)] u16),
}

impl SourceSelection {
    /// Whether `source` is selected.
    pub fn includes(&self, source: SourceId) -> bool {
        match self {
            Self::Local => true,
            Self::Only(id) => *id == source.0,
        }
    }
}

/// What a section of a packet contains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Encode, Decode, minicbor::CborLen)]
pub struct DataSelection {
    /// The sources.
    #[n(0)]
    pub sources: SourceSelection,
    /// The measurements.
    #[n(1)]
    pub measurements: Selection,
}

impl DataSelection {
    /// Nothing from any source.
    pub const NONE: Self = Self {
        sources: SourceSelection::Local,
        measurements: Selection::NONE,
    };

    /// The measurements of every local source.
    pub const fn local(measurements: Selection) -> Self {
        Self {
            sources: SourceSelection::Local,
            measurements,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{BMX280_TEMPERATURE, SDS011_PM10, all_cataloged};

    #[test]
    fn bits_are_lsb_first() {
        let selection = Selection::NONE
            .with(MeasurementId(0))
            .with(MeasurementId(9));
        assert_eq!(selection.as_bytes()[0], 0b0000_0001);
        assert_eq!(selection.as_bytes()[1], 0b0000_0010);
        assert!(selection.contains(LOCATION));
        assert!(!selection.contains(BMX280_TEMPERATURE));
        assert_eq!(selection.bits().count(), 2);
    }

    #[test]
    fn none_and_all_are_sentinels() {
        assert!(Selection::NONE.is_none() && !Selection::NONE.is_all());
        assert!(Selection::ALL.is_all() && !Selection::ALL.is_none());
        let explicit = Selection::NONE.with(SDS011_PM10);
        assert!(!explicit.is_none() && !explicit.is_all());
    }

    #[test]
    fn explicit_bits_must_be_supported() {
        let supported = all_cataloged();
        assert_eq!(Selection::NONE.validate(&supported), Ok(()));
        assert_eq!(Selection::ALL.validate(&supported), Ok(()));
        assert_eq!(
            Selection::NONE.with(SDS011_PM10).validate(&supported),
            Ok(())
        );
        // 18 is reserved, 200 is unknown.
        for bit in [18, 200] {
            assert_eq!(
                Selection::NONE
                    .with(MeasurementId(bit))
                    .validate(&supported),
                Err(SelectionError::Unsupported(MeasurementId(bit)))
            );
        }
        let narrow = Selection::NONE.with(BMX280_TEMPERATURE);
        assert_eq!(
            Selection::NONE.with(SDS011_PM10).validate(&narrow),
            Err(SelectionError::Unsupported(SDS011_PM10))
        );
    }

    #[test]
    fn all_is_dynamic_and_never_includes_location() {
        let available = Selection::NONE.with(LOCATION).with(BMX280_TEMPERATURE);
        let resolved = Selection::ALL.resolve(&available);
        assert!(resolved.contains(BMX280_TEMPERATURE));
        assert!(!resolved.contains(LOCATION));
        // A larger catalog changes what `all` means, the sentinel stays.
        let larger = available.with(SDS011_PM10);
        assert!(Selection::ALL.resolve(&larger).contains(SDS011_PM10));
        assert_eq!(Selection::NONE.resolve(&larger), Selection::NONE);
        let explicit = Selection::NONE.with(LOCATION);
        assert_eq!(explicit.resolve(&larger), explicit);
    }

    #[test]
    fn selections_round_trip_through_cbor() {
        let selection = DataSelection {
            sources: SourceSelection::Only(3),
            measurements: Selection::NONE.with(MeasurementId(4)),
        };
        let mut buf = [0u8; 64];
        let len = {
            let mut cursor = minicbor::encode::write::Cursor::new(&mut buf[..]);
            minicbor::encode(selection, &mut cursor).unwrap();
            cursor.position()
        };
        assert_eq!(minicbor::len(selection), len);
        let back: DataSelection = minicbor::decode(&buf[..len]).unwrap();
        assert_eq!(back, selection);
    }

    #[test]
    fn a_wrong_size_is_not_a_selection() {
        assert!(Selection::from_slice(&[0; 31]).is_none());
        assert!(Selection::from_slice(&[0; 33]).is_none());
        assert!(Selection::from_slice(&[0; 32]).is_some());
    }

    #[test]
    fn a_source_selection_restricts_sources() {
        assert!(SourceSelection::Local.includes(SourceId(7)));
        assert!(SourceSelection::Only(7).includes(SourceId(7)));
        assert!(!SourceSelection::Only(7).includes(SourceId(8)));
    }
}
