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

//! The integrity metadata of a segment and the names of its files.

use core::fmt::Write;

use heapless::String;

use crate::Compression;

/// Size of an encoded [`SegmentHeader`].
pub const HEADER_LEN: usize = 44;
/// The segment format version this crate writes and reads.
pub const FORMAT_VERSION: u8 = 1;

const MAGIC: [u8; 4] = *b"RLG1";

/// The CRC-32 (IEEE 802.3) of a byte stream, computed incrementally.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// An empty checksum.
    pub const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    /// Adds `bytes`.
    pub fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u32::from(*byte);
            for _ in 0..8 {
                self.0 = (self.0 >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(self.0 & 1));
            }
        }
    }

    /// The checksum of everything added so far.
    pub const fn finish(self) -> u32 {
        !self.0
    }

    /// The checksum of `bytes` alone.
    pub fn of(bytes: &[u8]) -> u32 {
        let mut crc = Self::new();
        crc.update(bytes);
        crc.finish()
    }
}

/// Why a segment header was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderError {
    /// Fewer than [`HEADER_LEN`] bytes: nothing was written, or the write was
    /// torn.
    Truncated,
    /// Not a segment header: erased or foreign flash.
    Magic,
    /// A format version this crate does not know.
    Version,
    /// The header is damaged, or the codec is unknown.
    Corrupt,
}

/// What is known about a sealed segment.
///
/// ```text
/// 0   "RLG1"            magic
/// 4   u8 version        FORMAT_VERSION
/// 5   u8 codec          0 none, 1 lz4
/// 6   u16 reserved      0
/// 8   u64 first_sequence
/// 16  u64 last_sequence
/// 24  u32 records
/// 28  u32 payload_len   bytes of the payload that follows
/// 32  u32 raw_len       bytes of text the payload holds, uncompressed
/// 36  u32 payload_crc   CRC-32 of the payload
/// 40  u32 header_crc    CRC-32 of bytes 0..40
/// ```
///
/// All integers are little endian. The payload is the text of the records:
/// plain, or one LZ4 frame, see [`super::lz4`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentHeader {
    /// Codec of the payload.
    pub codec: Compression,
    /// Sequence of the first record.
    pub first_sequence: u64,
    /// Sequence of the last record.
    pub last_sequence: u64,
    /// Number of records.
    pub records: u32,
    /// Length of the payload in bytes.
    pub payload_len: u32,
    /// Length of the text the payload holds.
    pub raw_len: u32,
    /// CRC-32 of the payload.
    pub payload_crc: u32,
}

impl SegmentHeader {
    /// The header as bytes.
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[..4].copy_from_slice(&MAGIC);
        out[4] = FORMAT_VERSION;
        out[5] = match self.codec {
            Compression::None => 0,
            Compression::Lz4 => 1,
        };
        out[8..16].copy_from_slice(&self.first_sequence.to_le_bytes());
        out[16..24].copy_from_slice(&self.last_sequence.to_le_bytes());
        out[24..28].copy_from_slice(&self.records.to_le_bytes());
        out[28..32].copy_from_slice(&self.payload_len.to_le_bytes());
        out[32..36].copy_from_slice(&self.raw_len.to_le_bytes());
        out[36..40].copy_from_slice(&self.payload_crc.to_le_bytes());
        let crc = Crc32::of(&out[..40]);
        out[40..].copy_from_slice(&crc.to_le_bytes());
        out
    }

    /// Reads a header.
    ///
    /// # Errors
    ///
    /// [`HeaderError`] tells why the bytes are not a valid header; none of
    /// them is fatal for the rest of the storage.
    pub fn decode(bytes: &[u8]) -> Result<Self, HeaderError> {
        let bytes = bytes.get(..HEADER_LEN).ok_or(HeaderError::Truncated)?;
        if bytes[..4] != MAGIC {
            return Err(HeaderError::Magic);
        }
        if bytes[4] != FORMAT_VERSION {
            return Err(HeaderError::Version);
        }
        let word = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        if Crc32::of(&bytes[..40]) != word(40) {
            return Err(HeaderError::Corrupt);
        }
        let codec = match bytes[5] {
            0 => Compression::None,
            1 => Compression::Lz4,
            _ => return Err(HeaderError::Corrupt),
        };
        let long = |at: usize| {
            let mut value = [0u8; 8];
            value.copy_from_slice(&bytes[at..at + 8]);
            u64::from_le_bytes(value)
        };
        Ok(Self {
            codec,
            first_sequence: long(8),
            last_sequence: long(16),
            records: word(24),
            payload_len: word(28),
            raw_len: word(32),
            payload_crc: word(36),
        })
    }
}

/// The longest file name: `rohi4294967295.log.lz4`.
pub const MAX_FILE_NAME_LEN: usize = 22;

/// The name of the downloaded file of segment number `number`: `rohi<N>.log.lz4`
/// for LZ4 and `rohi<N>.log` for plain text. The number is the position in the
/// download, starting at 1, so names are stable and never collide.
pub fn file_name(number: u32, codec: Compression) -> String<MAX_FILE_NAME_LEN> {
    let mut name = String::new();
    let _ = match codec {
        Compression::None => write!(name, "rohi{number}.log"),
        Compression::Lz4 => write!(name, "rohi{number}.log.lz4"),
    };
    name
}
