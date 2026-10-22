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

//! Persistent format constants and the minimal CBOR head helpers.
//!
//! Everything stored in a slot is a CBOR document:
//!
//! ```text
//! slot     = SlotEnvelope | 0xFF padding | commit word (last 4 bytes of the slot)
//! envelope = { 0: bstr(config payload), 1: crc32(config payload) }
//! payload  = { 0: format version, 1: generation, 2: [ bstr(section record)* ] }
//! record   = [ section id (u64), section version (u16), bstr(typed section CBOR) ]
//! ```
//!
//! Structured fields are encoded and decoded with `minicbor`. Only the head of a
//! definite-length byte string is handled here: the byte strings can be larger
//! than the RAM workspace, so they are streamed to and from flash and their
//! length has to be known (or read) before their content.

/// Version of the container format (`ConfigPayload` key 0).
pub const FORMAT_VERSION: u16 = 1;

/// Flash commit word, written last. It is transaction metadata, not schema.
pub const COMMIT_WORD: u32 = u32::from_le_bytes(*b"COMT");

/// Commit word of an erased slot.
pub(crate) const ERASED_WORD: u32 = u32::MAX;

/// Commit word programmed to zero before a committed slot is erased.
pub(crate) const INVALIDATED_WORD: u32 = 0;

/// Size of the commit word at the end of every slot.
pub const COMMIT_WORD_SIZE: usize = 4;

/// Maximum number of sections in a configuration.
pub const MAX_SECTIONS: usize = 16;

/// Maximum size of one encoded typed section payload in bytes.
pub const MAX_SECTION_PAYLOAD: usize = 1024;

/// Upper bound of the size of a single slot.
pub const MAX_SLOT_SIZE: usize = 64 * 1024;

/// Largest head of a CBOR item (initial byte plus a 64-bit argument).
pub(crate) const MAX_HEAD: usize = 9;

/// Encoded size of the head (initial byte and argument) of an item with
/// argument `value`, which is also the size of an unsigned integer.
pub(crate) const fn head_len(value: u64) -> usize {
    match value {
        0..=23 => 1,
        24..=0xFF => 2,
        0x100..=0xFFFF => 3,
        0x1_0000..=0xFFFF_FFFF => 5,
        _ => 9,
    }
}

/// Head of a definite-length byte string of `len` bytes.
pub(crate) fn bytes_head(len: usize) -> ([u8; MAX_HEAD], usize) {
    let value = len as u64;
    let mut head = [0u8; MAX_HEAD];
    let size = head_len(value);
    match size {
        1 => head[0] = 0x40 | value as u8,
        2 => {
            head[0] = 0x58;
            head[1] = value as u8;
        }
        3 => {
            head[0] = 0x59;
            head[1..3].copy_from_slice(&(value as u16).to_be_bytes());
        }
        5 => {
            head[0] = 0x5A;
            head[1..5].copy_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            head[0] = 0x5B;
            head[1..9].copy_from_slice(&value.to_be_bytes());
        }
    }
    (head, size)
}

/// Parses the head of a definite-length byte string, returning its length and
/// the size of the head. Indefinite and non-minimal heads are rejected so that
/// a copied record is always re-encoded to the identical bytes.
pub(crate) fn parse_bytes_head(input: &[u8]) -> Option<(u64, usize)> {
    let first = *input.first()?;
    if first >> 5 != 2 {
        return None;
    }
    let (value, size) = match first & 0x1F {
        info @ 0..=23 => (u64::from(info), 1),
        24 => (u64::from(*input.get(1)?), 2),
        25 => (
            u64::from(u16::from_be_bytes(input.get(1..3)?.try_into().ok()?)),
            3,
        ),
        26 => (
            u64::from(u32::from_be_bytes(input.get(1..5)?.try_into().ok()?)),
            5,
        ),
        27 => (u64::from_be_bytes(input.get(1..9)?.try_into().ok()?), 9),
        _ => return None,
    };
    (head_len(value) == size).then_some((value, size))
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;

    #[test]
    fn head_len_boundaries() {
        assert_eq!(head_len(0), 1);
        assert_eq!(head_len(23), 1);
        assert_eq!(head_len(24), 2);
        assert_eq!(head_len(255), 2);
        assert_eq!(head_len(256), 3);
        assert_eq!(head_len(65535), 3);
        assert_eq!(head_len(65536), 5);
        assert_eq!(head_len(u64::from(u32::MAX)), 5);
        assert_eq!(head_len(u64::from(u32::MAX) + 1), 9);
    }

    #[test]
    fn bytes_head_matches_minicbor() {
        for len in [0usize, 1, 23, 24, 255, 256, 1024, 65535, 65536] {
            let content = std::vec![0u8; len];
            let mut expected = Vec::new();
            minicbor::Encoder::new(&mut expected)
                .bytes(&content)
                .unwrap();
            let (head, size) = bytes_head(len);
            assert_eq!(&expected[..size], &head[..size], "len {len}");
            assert_eq!(expected.len(), size + len);
            assert_eq!(parse_bytes_head(&head[..size]), Some((len as u64, size)));
        }
    }

    #[test]
    fn parse_rejects_invalid_heads() {
        // Not a byte string.
        assert_eq!(parse_bytes_head(&[0x60]), None);
        assert_eq!(parse_bytes_head(&[0x80]), None);
        // Indefinite length and reserved additional information.
        assert_eq!(parse_bytes_head(&[0x5F]), None);
        assert_eq!(parse_bytes_head(&[0x5C]), None);
        // Non-minimal encodings.
        assert_eq!(parse_bytes_head(&[0x58, 0x05]), None);
        assert_eq!(parse_bytes_head(&[0x59, 0x00, 0xFF]), None);
        assert_eq!(parse_bytes_head(&[0x5A, 0x00, 0x00, 0xFF, 0xFF]), None);
        // Truncated.
        assert_eq!(parse_bytes_head(&[]), None);
        assert_eq!(parse_bytes_head(&[0x59, 0x01]), None);
    }
}
