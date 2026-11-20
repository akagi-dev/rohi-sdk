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

//! Batches: the records the logging task writes in one go.

use core::fmt::{self, Write};

use heapless::Vec;

#[cfg(feature = "lz4")]
use super::lz4;
use super::text::write_line;
use crate::{Compression, LogRecord};

/// Most text one batch holds, in bytes. A batch is compressed on its own, so
/// this also bounds the buffers of the writer and of the reader.
pub const MAX_BATCH_BYTES: usize = 1024;
/// Most bytes an encoded batch occupies: the text, and the block framing of LZ4.
#[cfg(feature = "lz4")]
pub const MAX_CHUNK_LEN: usize = MAX_BATCH_BYTES + lz4::BLOCK_OVERHEAD;
/// Most bytes an encoded batch occupies: the text itself.
#[cfg(not(feature = "lz4"))]
pub const MAX_CHUNK_LEN: usize = MAX_BATCH_BYTES;

/// Where [`Batch::encode`] compresses into: a [`MAX_CHUNK_LEN`] buffer with the `lz4`
/// feature. Zero-sized without it: plain text is written from the batch itself.
pub struct Scratch(#[cfg(feature = "lz4")] [u8; MAX_CHUNK_LEN]);

/// An empty [`Scratch`].
#[cfg(feature = "lz4")]
pub const fn scratch() -> Scratch {
    Scratch([0u8; MAX_CHUNK_LEN])
}

/// An empty [`Scratch`].
#[cfg(not(feature = "lz4"))]
pub const fn scratch() -> Scratch {
    Scratch()
}

/// A batch could not be encoded.
#[cfg(feature = "lz4")]
pub type EncodeError = lz4::Lz4Error;
/// A batch could not be encoded: never, without the `lz4` feature.
#[cfg(not(feature = "lz4"))]
pub type EncodeError = core::convert::Infallible;

/// The record does not fit the batch; encode the batch and start another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BatchFull;

struct Bounded<'a>(&'a mut Vec<u8, MAX_BATCH_BYTES>);

impl Write for Bounded<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0
            .extend_from_slice(text.as_bytes())
            .map_err(|_| fmt::Error)
    }
}

/// Records collected for one write: their text, and what the segment header
/// needs to know about them.
#[derive(Clone, Debug, Default)]
pub struct Batch {
    text: Vec<u8, MAX_BATCH_BYTES>,
    first_sequence: u64,
    last_sequence: u64,
    records: u32,
}

impl Batch {
    /// An empty batch.
    pub const fn new() -> Self {
        Self {
            text: Vec::new(),
            first_sequence: 0,
            last_sequence: 0,
            records: 0,
        }
    }

    /// Whether the batch holds no record.
    pub fn is_empty(&self) -> bool {
        self.records == 0
    }

    /// Number of records.
    pub fn records(&self) -> u32 {
        self.records
    }

    /// Bytes of text.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Sequence of the first record; 0 when empty.
    pub fn first_sequence(&self) -> u64 {
        self.first_sequence
    }

    /// Sequence of the last record; 0 when empty.
    pub fn last_sequence(&self) -> u64 {
        self.last_sequence
    }

    /// The text of the records, one line each.
    pub fn text(&self) -> &[u8] {
        &self.text
    }

    /// Appends `record` (its sequence must be assigned).
    ///
    /// # Errors
    ///
    /// [`BatchFull`] when the line does not fit; the batch is unchanged.
    pub fn push(&mut self, record: &LogRecord) -> Result<(), BatchFull> {
        let before = self.text.len();
        if write_line(&mut Bounded(&mut self.text), record).is_err() {
            self.text.truncate(before);
            return Err(BatchFull);
        }
        if self.records == 0 {
            self.first_sequence = record.sequence;
        }
        self.last_sequence = record.sequence;
        self.records += 1;
        Ok(())
    }

    /// Empties the batch.
    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// The batch as the bytes appended to a segment payload: one LZ4 block
    /// for [`Compression::Lz4`](crate::Compression) (compressed into
    /// `scratch`), the text itself, borrowed, for [`Compression::None`].
    ///
    /// # Errors
    ///
    /// [`EncodeError`] when the compressed block does not fit `scratch`; never
    /// without the `lz4` feature.
    pub fn encode<'a>(
        &'a self,
        codec: Compression,
        #[cfg_attr(not(feature = "lz4"), allow(unused_variables))] scratch: &'a mut Scratch,
    ) -> Result<&'a [u8], EncodeError> {
        match codec {
            Compression::None => Ok(&self.text),
            #[cfg(feature = "lz4")]
            Compression::Lz4 => {
                let len = lz4::encode_block(&self.text, &mut scratch.0)?;
                Ok(&scratch.0[..len])
            }
        }
    }
}

/// Length of [`payload_prefix`]: the LZ4 frame header, nothing without `lz4`.
#[cfg(feature = "lz4")]
pub const PREFIX_LEN: usize = lz4::FRAME_HEADER_LEN;
/// Length of [`payload_prefix`]: the LZ4 frame header, nothing without `lz4`.
#[cfg(not(feature = "lz4"))]
pub const PREFIX_LEN: usize = 0;

/// Bytes at the start of a segment payload before the first batch.
pub fn payload_prefix(codec: Compression) -> heapless::Vec<u8, PREFIX_LEN> {
    match codec {
        Compression::None => heapless::Vec::new(),
        #[cfg(feature = "lz4")]
        Compression::Lz4 => heapless::Vec::from_slice(&lz4::frame_header()).unwrap_or_default(),
    }
}

/// Bytes that close a segment payload when it is sealed.
pub const fn payload_suffix(codec: Compression) -> &'static [u8] {
    match codec {
        Compression::None => &[],
        #[cfg(feature = "lz4")]
        Compression::Lz4 => &lz4::END_MARK,
    }
}
