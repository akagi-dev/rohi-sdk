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
//! Reading segments back into records.

use heapless::Vec;

use super::batch::{MAX_BATCH_BYTES, MAX_CHUNK_LEN};
use super::lz4::{self, FRAME_HEADER_LEN};
use super::text::{MAX_LINE_LEN, parse_line};
use crate::{Compression, LogRecord};

/// The payload cannot be decoded past this point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// A block is damaged or the frame header is wrong. Records before it
    /// were delivered and are valid.
    Corrupt,
}

/// What a decoder found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeSummary {
    /// Records delivered.
    pub records: u32,
    /// Lines that were not valid records and were skipped.
    pub skipped: u32,
}

/// Turns the payload of one segment, pushed in pieces of any size, into
/// records.
///
/// The decoder keeps at most one block and one line in memory. It is
/// tolerant at the line level (a line that is not a record is counted and
/// skipped) and strict at the block level (a damaged block stops decoding, so
/// what follows a power loss or a bit flip is never guessed).
pub struct SegmentDecoder {
    codec: Compression,
    header_done: bool,
    ended: bool,
    failed: bool,
    pending: Vec<u8, { MAX_CHUNK_LEN + 4 }>,
    line: Vec<u8, MAX_LINE_LEN>,
    discarding: bool,
    summary: DecodeSummary,
}

impl SegmentDecoder {
    /// A decoder for a payload of `codec`.
    pub const fn new(codec: Compression) -> Self {
        Self {
            codec,
            header_done: false,
            ended: false,
            failed: false,
            pending: Vec::new(),
            line: Vec::new(),
            discarding: false,
            summary: DecodeSummary {
                records: 0,
                skipped: 0,
            },
        }
    }

    /// What was decoded so far.
    pub fn summary(&self) -> DecodeSummary {
        self.summary
    }

    /// Whether the payload ended properly: the end mark for LZ4, a complete
    /// last line for plain text. False for a payload that is still arriving,
    /// is torn or is damaged.
    pub fn is_complete(&self) -> bool {
        match self.codec {
            Compression::Lz4 => self.ended && !self.failed,
            Compression::None => self.line.is_empty() && !self.discarding && !self.failed,
        }
    }

    /// Feeds the next bytes of the payload; `on_record` gets every record in
    /// order.
    ///
    /// # Errors
    ///
    /// [`DecodeError::Corrupt`] when a block is damaged; the decoder then
    /// refuses further input.
    pub fn push(
        &mut self,
        mut bytes: &[u8],
        on_record: &mut impl FnMut(LogRecord),
    ) -> Result<(), DecodeError> {
        if self.failed {
            return Err(DecodeError::Corrupt);
        }
        if self.codec == Compression::None {
            self.lines(bytes, on_record);
            return Ok(());
        }
        while !bytes.is_empty() && !self.ended {
            let room = self.pending.capacity() - self.pending.len();
            if room == 0 {
                self.failed = true;
                return Err(DecodeError::Corrupt);
            }
            let take = room.min(bytes.len());
            let _ = self.pending.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            self.blocks(on_record)?;
        }
        Ok(())
    }

    fn blocks(&mut self, on_record: &mut impl FnMut(LogRecord)) -> Result<(), DecodeError> {
        if !self.header_done {
            if self.pending.len() < FRAME_HEADER_LEN {
                return Ok(());
            }
            if lz4::check_frame_header(&self.pending).is_err() {
                self.failed = true;
                return Err(DecodeError::Corrupt);
            }
            drain(&mut self.pending, FRAME_HEADER_LEN);
            self.header_done = true;
        }
        loop {
            let Some(size) = self.pending.get(..4) else {
                return Ok(());
            };
            let size = u32::from_le_bytes([size[0], size[1], size[2], size[3]]);
            if size == 0 {
                self.ended = true;
                return Ok(());
            }
            let size = (size & 0x7FFF_FFFF) as usize;
            if size > MAX_BATCH_BYTES + 64 || 4 + size + 4 > self.pending.capacity() {
                self.failed = true;
                return Err(DecodeError::Corrupt);
            }
            if self.pending.len() < 8 + size {
                return Ok(());
            }
            let mut raw = [0u8; MAX_BATCH_BYTES];
            let decoded = match lz4::next_block(&self.pending) {
                Ok(Some((block, used))) => block.decode(&mut raw).map(|len| (len, used)),
                _ => Err(lz4::Lz4Error::CorruptBlock),
            };
            let Ok((len, used)) = decoded else {
                self.failed = true;
                return Err(DecodeError::Corrupt);
            };
            drain(&mut self.pending, used);
            self.lines(&raw[..len], on_record);
        }
    }

    fn lines(&mut self, mut text: &[u8], on_record: &mut impl FnMut(LogRecord)) {
        while !text.is_empty() {
            let end = text.iter().position(|byte| *byte == b'\n');
            let part = &text[..end.unwrap_or(text.len())];
            if !self.discarding && self.line.extend_from_slice(part).is_err() {
                self.line.clear();
                self.discarding = true;
                self.summary.skipped += 1;
            }
            let Some(end) = end else { return };
            if self.discarding {
                self.discarding = false;
            } else {
                match core::str::from_utf8(&self.line).ok().and_then(parse_line) {
                    Some(record) => {
                        self.summary.records += 1;
                        on_record(record);
                    }
                    None if self.line.is_empty() => {}
                    None => self.summary.skipped += 1,
                }
            }
            self.line.clear();
            text = &text[end + 1..];
        }
    }
}

fn drain<const N: usize>(bytes: &mut Vec<u8, N>, count: usize) {
    let remaining = bytes.len() - count;
    bytes.copy_within(count.., 0);
    bytes.truncate(remaining);
}
