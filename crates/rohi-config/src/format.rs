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

//! On-flash container format. All integers are little-endian and every field
//! is encoded explicitly; Rust memory layout is never persisted.
//!
//! ```text
//! slot:    SlotHeader (24 B) | section 0 | section 1 | ... | 0xFF padding
//! section: SectionHeader (16 B) | SCALE payload | zero padding to 4 bytes
//! ```

use crate::SectionId;

/// `"ROHC"` in memory order.
pub const SLOT_MAGIC: u32 = u32::from_le_bytes(*b"ROHC");
/// Written last; its presence makes a slot valid.
pub const COMMIT_MARKER: u32 = u32::from_le_bytes(*b"COMT");
pub const FORMAT_VERSION: u16 = 1;

pub const SLOT_HEADER_SIZE: usize = 24;
pub const SECTION_HEADER_SIZE: usize = 16;
/// Section payloads are padded with zeros to this boundary.
pub const SECTION_ALIGN: usize = 4;

/// Maximum number of sections in one container.
pub const MAX_SECTIONS: usize = 16;
/// Maximum SCALE payload of a single section in bytes.
pub const MAX_SECTION_PAYLOAD: usize = 1024;
/// Upper bound for one slot; larger regions leave the excess unused.
pub const MAX_SLOT_SIZE: usize = 64 * 1024;

/// Offset of the CRC field inside the slot header.
pub(crate) const CRC_OFFSET: usize = 16;
/// Offset of the commit marker inside the slot header.
pub(crate) const MARKER_OFFSET: usize = 20;

pub(crate) const fn align_up(value: usize) -> Option<usize> {
    match value.checked_add(SECTION_ALIGN - 1) {
        Some(sum) => Some(sum & !(SECTION_ALIGN - 1)),
        None => None,
    }
}

/// Slot header fields that are covered by the CRC (bytes `4..16`) plus the
/// integrity fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SlotHeader {
    pub magic: u32,
    pub format_version: u16,
    pub generation: u32,
    pub payload_length: u32,
    pub crc32: u32,
    pub commit_marker: u32,
}

impl SlotHeader {
    pub fn to_bytes(self) -> [u8; SLOT_HEADER_SIZE] {
        let mut bytes = [0u8; SLOT_HEADER_SIZE];
        bytes[0..4].copy_from_slice(&self.magic.to_le_bytes());
        bytes[4..6].copy_from_slice(&self.format_version.to_le_bytes());
        // bytes[6..8] reserved, zero
        bytes[8..12].copy_from_slice(&self.generation.to_le_bytes());
        bytes[12..16].copy_from_slice(&self.payload_length.to_le_bytes());
        bytes[16..20].copy_from_slice(&self.crc32.to_le_bytes());
        bytes[20..24].copy_from_slice(&self.commit_marker.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8; SLOT_HEADER_SIZE]) -> Self {
        Self {
            magic: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            format_version: u16::from_le_bytes([bytes[4], bytes[5]]),
            generation: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            payload_length: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
            crc32: u32::from_le_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]),
            commit_marker: u32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
        }
    }
}

/// Per-section header preceding every payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionHeader {
    pub id: SectionId,
    pub version: u16,
    /// Actual SCALE payload length (without padding).
    pub length: u32,
}

impl SectionHeader {
    pub fn to_bytes(self) -> [u8; SECTION_HEADER_SIZE] {
        let mut bytes = [0u8; SECTION_HEADER_SIZE];
        bytes[0..8].copy_from_slice(&self.id.0.to_le_bytes());
        bytes[8..10].copy_from_slice(&self.version.to_le_bytes());
        // bytes[10..12] reserved, zero
        bytes[12..16].copy_from_slice(&self.length.to_le_bytes());
        bytes
    }

    pub fn from_bytes(bytes: &[u8; SECTION_HEADER_SIZE]) -> Self {
        Self {
            id: SectionId(u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])),
            version: u16::from_le_bytes([bytes[8], bytes[9]]),
            length: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_header_layout_is_fixed() {
        let header = SlotHeader {
            magic: SLOT_MAGIC,
            format_version: 1,
            generation: 0x0403_0201,
            payload_length: 0x0807_0605,
            crc32: 0x0C0B_0A09,
            commit_marker: COMMIT_MARKER,
        };
        let bytes = header.to_bytes();
        assert_eq!(&bytes[0..4], b"ROHC");
        assert_eq!(&bytes[4..8], &[1, 0, 0, 0]);
        assert_eq!(&bytes[8..12], &[1, 2, 3, 4]);
        assert_eq!(&bytes[12..16], &[5, 6, 7, 8]);
        assert_eq!(&bytes[16..20], &[9, 10, 11, 12]);
        assert_eq!(&bytes[20..24], b"COMT");
        assert_eq!(SlotHeader::from_bytes(&bytes), header);
    }

    #[test]
    fn section_header_layout_is_fixed() {
        let header = SectionHeader {
            id: SectionId(0x0807_0605_0403_0201),
            version: 0x0A09,
            length: 0x0E0D_0C0B,
        };
        let bytes = header.to_bytes();
        assert_eq!(&bytes[0..8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(&bytes[8..12], &[9, 10, 0, 0]);
        assert_eq!(&bytes[12..16], &[11, 12, 13, 14]);
        assert_eq!(SectionHeader::from_bytes(&bytes), header);
    }

    #[test]
    fn align_up_is_checked() {
        assert_eq!(align_up(0), Some(0));
        assert_eq!(align_up(1), Some(4));
        assert_eq!(align_up(4), Some(4));
        assert_eq!(align_up(usize::MAX), None);
    }
}
