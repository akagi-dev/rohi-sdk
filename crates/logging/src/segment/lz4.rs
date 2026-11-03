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
//! LZ4 for log segments: the block format, the frame format around it and the
//! xxHash32 checksum both use.
//!
//! The compressor is a small greedy one (a 2 KiB hash table on the stack) so
//! it fits a task of a microcontroller; its output is plain LZ4 that any
//! decoder reads. Frames are written one block at a time with independent
//! blocks and a checksum per block, so a segment is a valid `.lz4` file after
//! every flush except for the missing end mark.

use core::fmt;

/// Longest input of one block, in bytes: the block maximum the frame header
/// announces (64 KiB).
pub const MAX_BLOCK_INPUT: usize = 64 * 1024;
/// Size of the frame header: magic, flags, block descriptor, header checksum.
pub const FRAME_HEADER_LEN: usize = 7;
/// Size of the end mark closing a frame.
pub const END_MARK_LEN: usize = 4;
/// Overhead of one block: its size and its checksum.
pub const BLOCK_OVERHEAD: usize = 8;

const MAGIC: [u8; 4] = [0x04, 0x22, 0x4D, 0x18];
// Version 01, independent blocks, block checksums, no content size or checksum.
const FLG: u8 = 0b0111_0000;
// Maximum block size 64 KiB.
const BD: u8 = 0b0100_0000;
const UNCOMPRESSED: u32 = 0x8000_0000;

const MIN_MATCH: usize = 4;
const LAST_LITERALS: usize = 5;
const MF_LIMIT: usize = 12;
const HASH_BITS: u32 = 10;

/// Why LZ4 data could not be produced or read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lz4Error {
    /// The output buffer is too small.
    OutputTooSmall,
    /// The input of a block is longer than [`MAX_BLOCK_INPUT`].
    InputTooLarge,
    /// Not an LZ4 frame, or an unsupported frame descriptor.
    BadHeader,
    /// A block is cut short or its checksum does not match.
    CorruptBlock,
}

impl fmt::Display for Lz4Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutputTooSmall => "output buffer too small",
            Self::InputTooLarge => "block input too large",
            Self::BadHeader => "not a supported LZ4 frame",
            Self::CorruptBlock => "corrupt LZ4 block",
        })
    }
}

const PRIME1: u32 = 0x9E37_79B1;
const PRIME2: u32 = 0x85EB_CA77;
const PRIME3: u32 = 0xC2B2_AE3D;
const PRIME4: u32 = 0x27D4_EB2F;
const PRIME5: u32 = 0x1656_67B1;

fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The xxHash32 of `data` with seed 0, the checksum of LZ4 frames.
pub fn xxh32(data: &[u8]) -> u32 {
    let mut rest = data;
    let mut hash = if data.len() >= 16 {
        let mut lanes = [
            PRIME1.wrapping_add(PRIME2),
            PRIME2,
            0,
            0u32.wrapping_sub(PRIME1),
        ];
        while rest.len() >= 16 {
            for (index, lane) in lanes.iter_mut().enumerate() {
                let chunk = &rest[index * 4..];
                *lane = lane
                    .wrapping_add(le32(chunk).wrapping_mul(PRIME2))
                    .rotate_left(13)
                    .wrapping_mul(PRIME1);
            }
            rest = &rest[16..];
        }
        lanes[0]
            .rotate_left(1)
            .wrapping_add(lanes[1].rotate_left(7))
            .wrapping_add(lanes[2].rotate_left(12))
            .wrapping_add(lanes[3].rotate_left(18))
    } else {
        PRIME5
    };
    hash = hash.wrapping_add(data.len() as u32);
    while rest.len() >= 4 {
        hash = hash
            .wrapping_add(le32(rest).wrapping_mul(PRIME3))
            .rotate_left(17)
            .wrapping_mul(PRIME4);
        rest = &rest[4..];
    }
    for byte in rest {
        hash = hash
            .wrapping_add(u32::from(*byte).wrapping_mul(PRIME5))
            .rotate_left(11)
            .wrapping_mul(PRIME1);
    }
    hash ^= hash >> 15;
    hash = hash.wrapping_mul(PRIME2);
    hash ^= hash >> 13;
    hash = hash.wrapping_mul(PRIME3);
    hash ^ (hash >> 16)
}

struct Sink<'a> {
    out: &'a mut [u8],
    len: usize,
}

impl Sink<'_> {
    fn put(&mut self, byte: u8) -> Result<(), Lz4Error> {
        *self.out.get_mut(self.len).ok_or(Lz4Error::OutputTooSmall)? = byte;
        self.len += 1;
        Ok(())
    }

    fn put_all(&mut self, bytes: &[u8]) -> Result<(), Lz4Error> {
        let end = self.len + bytes.len();
        self.out
            .get_mut(self.len..end)
            .ok_or(Lz4Error::OutputTooSmall)?
            .copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }

    fn put_length(&mut self, mut extra: usize) -> Result<(), Lz4Error> {
        while extra >= 255 {
            self.put(255)?;
            extra -= 255;
        }
        self.put(extra as u8)
    }
}

fn hash(sequence: u32) -> usize {
    (sequence.wrapping_mul(PRIME1) >> (32 - HASH_BITS)) as usize
}

fn emit(
    sink: &mut Sink<'_>,
    literals: &[u8],
    matched: Option<(usize, usize)>,
) -> Result<(), Lz4Error> {
    let literal_nibble = literals.len().min(15) as u8;
    let match_nibble = matched.map_or(0, |(_, length)| (length - MIN_MATCH).min(15) as u8);
    sink.put(literal_nibble << 4 | match_nibble)?;
    if literals.len() >= 15 {
        sink.put_length(literals.len() - 15)?;
    }
    sink.put_all(literals)?;
    if let Some((offset, length)) = matched {
        sink.put_all(&(offset as u16).to_le_bytes())?;
        if length - MIN_MATCH >= 15 {
            sink.put_length(length - MIN_MATCH - 15)?;
        }
    }
    Ok(())
}

/// Compresses `input` into one LZ4 block and returns its length.
///
/// # Errors
///
/// [`Lz4Error::InputTooLarge`] above [`MAX_BLOCK_INPUT`],
/// [`Lz4Error::OutputTooSmall`] when the result does not fit `out`.
pub fn compress_block(input: &[u8], out: &mut [u8]) -> Result<usize, Lz4Error> {
    if input.len() > MAX_BLOCK_INPUT {
        return Err(Lz4Error::InputTooLarge);
    }
    let mut sink = Sink { out, len: 0 };
    // Positions are stored plus one, so zero means "empty"; blocks are
    // short enough for a position to fit 16 bits.
    let mut table = [0u16; 1 << HASH_BITS];
    let positions_fit = input.len() < usize::from(u16::MAX);
    let mut anchor = 0;
    let mut at = 0;
    while positions_fit && at + MF_LIMIT <= input.len() {
        let sequence = le32(&input[at..]);
        let slot = hash(sequence);
        let candidate = usize::from(table[slot]);
        table[slot] = (at + 1) as u16;
        if candidate != 0 {
            let candidate = candidate - 1;
            if le32(&input[candidate..]) == sequence {
                let longest = input.len() - LAST_LITERALS - at;
                let mut length = MIN_MATCH;
                while length < longest && input[candidate + length] == input[at + length] {
                    length += 1;
                }
                emit(
                    &mut sink,
                    &input[anchor..at],
                    Some((at - candidate, length)),
                )?;
                at += length;
                anchor = at;
                continue;
            }
        }
        at += 1;
    }
    emit(&mut sink, &input[anchor..], None)?;
    Ok(sink.len)
}

/// Decompresses one LZ4 block into `out` and returns the decoded length.
///
/// # Errors
///
/// [`Lz4Error::CorruptBlock`] for malformed data,
/// [`Lz4Error::OutputTooSmall`] when the result does not fit `out`.
pub fn decompress_block(input: &[u8], out: &mut [u8]) -> Result<usize, Lz4Error> {
    let mut read = 0;
    let mut written = 0;
    let length = |read: &mut usize, mut base: usize| -> Result<usize, Lz4Error> {
        if base == 15 {
            loop {
                let byte = *input.get(*read).ok_or(Lz4Error::CorruptBlock)?;
                *read += 1;
                base += usize::from(byte);
                if byte != 255 {
                    break;
                }
            }
        }
        Ok(base)
    };
    while read < input.len() {
        let token = input[read];
        read += 1;
        let literals = length(&mut read, usize::from(token >> 4))?;
        let source = input
            .get(read..read + literals)
            .ok_or(Lz4Error::CorruptBlock)?;
        out.get_mut(written..written + literals)
            .ok_or(Lz4Error::OutputTooSmall)?
            .copy_from_slice(source);
        read += literals;
        written += literals;
        if read == input.len() {
            return Ok(written);
        }
        let offset = input.get(read..read + 2).ok_or(Lz4Error::CorruptBlock)?;
        let offset = usize::from(u16::from_le_bytes([offset[0], offset[1]]));
        read += 2;
        let matched = length(&mut read, usize::from(token & 15))? + MIN_MATCH;
        if offset == 0 || offset > written {
            return Err(Lz4Error::CorruptBlock);
        }
        if written + matched > out.len() {
            return Err(Lz4Error::OutputTooSmall);
        }
        for _ in 0..matched {
            out[written] = out[written - offset];
            written += 1;
        }
    }
    Err(Lz4Error::CorruptBlock)
}

/// The header of a frame as this module writes it.
pub fn frame_header() -> [u8; FRAME_HEADER_LEN] {
    let checksum = (xxh32(&[FLG, BD]) >> 8) as u8;
    [MAGIC[0], MAGIC[1], MAGIC[2], MAGIC[3], FLG, BD, checksum]
}

/// Whether `header` is a frame header this module can read: its own flags.
pub fn check_frame_header(header: &[u8]) -> Result<(), Lz4Error> {
    if header.len() >= FRAME_HEADER_LEN && header[..FRAME_HEADER_LEN] == frame_header() {
        Ok(())
    } else {
        Err(Lz4Error::BadHeader)
    }
}

/// The end mark that closes a frame.
pub const END_MARK: [u8; END_MARK_LEN] = [0; END_MARK_LEN];

/// Writes `raw` as one frame block (compressed, or stored when that is not
/// smaller) with its checksum and returns the number of bytes written.
///
/// # Errors
///
/// [`Lz4Error::InputTooLarge`] above [`MAX_BLOCK_INPUT`],
/// [`Lz4Error::OutputTooSmall`] when `out` cannot hold the block: its size is
/// at most `raw.len() + BLOCK_OVERHEAD`.
pub fn encode_block(raw: &[u8], out: &mut [u8]) -> Result<usize, Lz4Error> {
    if raw.len() > MAX_BLOCK_INPUT {
        return Err(Lz4Error::InputTooLarge);
    }
    let body = out.get_mut(4..).ok_or(Lz4Error::OutputTooSmall)?;
    let room = body.len().checked_sub(4).ok_or(Lz4Error::OutputTooSmall)?;
    let body = &mut body[..room + 4];
    let (size, stored) = match compress_block(raw, &mut body[..room]) {
        Ok(size) if size < raw.len() => (size, false),
        Ok(_) | Err(Lz4Error::OutputTooSmall) => {
            body[..room]
                .get_mut(..raw.len())
                .ok_or(Lz4Error::OutputTooSmall)?
                .copy_from_slice(raw);
            (raw.len(), true)
        }
        Err(error) => return Err(error),
    };
    let checksum = xxh32(&body[..size]);
    body[size..size + 4].copy_from_slice(&checksum.to_le_bytes());
    let header = size as u32 | if stored { UNCOMPRESSED } else { 0 };
    out[..4].copy_from_slice(&header.to_le_bytes());
    Ok(BLOCK_OVERHEAD + size)
}

/// One block of a frame, still encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block<'a> {
    /// The block data: compressed, or the plain bytes when `stored`.
    pub data: &'a [u8],
    /// Whether `data` is stored as is.
    pub stored: bool,
}

impl Block<'_> {
    /// Decodes the block into `out` and returns the decoded length.
    ///
    /// # Errors
    ///
    /// As [`decompress_block`]; [`Lz4Error::OutputTooSmall`] also for a stored
    /// block that does not fit.
    pub fn decode(&self, out: &mut [u8]) -> Result<usize, Lz4Error> {
        if self.stored {
            out.get_mut(..self.data.len())
                .ok_or(Lz4Error::OutputTooSmall)?
                .copy_from_slice(self.data);
            Ok(self.data.len())
        } else {
            decompress_block(self.data, out)
        }
    }
}

/// Reads the next block of `frame` (starting at a block, not at the header).
///
/// Returns `Ok(None)` at the end mark and the block with the number of bytes
/// it occupies otherwise.
///
/// # Errors
///
/// [`Lz4Error::CorruptBlock`] when the block is cut short or its checksum
/// does not match: the frame is only readable up to the previous block.
pub fn next_block(frame: &[u8]) -> Result<Option<(Block<'_>, usize)>, Lz4Error> {
    let header = frame.get(..4).ok_or(Lz4Error::CorruptBlock)?;
    let header = le32(header);
    if header == 0 {
        return Ok(None);
    }
    let size = (header & !UNCOMPRESSED) as usize;
    let data = frame.get(4..4 + size).ok_or(Lz4Error::CorruptBlock)?;
    let checksum = frame
        .get(4 + size..8 + size)
        .ok_or(Lz4Error::CorruptBlock)?;
    if le32(checksum) != xxh32(data) {
        return Err(Lz4Error::CorruptBlock);
    }
    let stored = header & UNCOMPRESSED != 0;
    Ok(Some((Block { data, stored }, BLOCK_OVERHEAD + size)))
}
