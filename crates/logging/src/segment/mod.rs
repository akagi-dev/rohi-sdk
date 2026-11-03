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
//! The persistent form of the log: line text, LZ4 frames, segment headers and
//! the batches and decoders that connect them.
//!
//! A **segment** is a header ([`SegmentHeader`]) followed by a payload. The
//! payload is the text of the records, one [line](text) each: plain
//! ([`Compression::None`](crate::Compression)) or one LZ4 frame
//! ([`Compression::Lz4`](crate::Compression)). The logging task collects
//! records into a [`Batch`], and appends each batch to the open segment as one
//! independently compressed block, so a segment can be cut by a power loss at
//! any byte and still be read up to its last complete block. Sealing a segment
//! writes the end mark and the header.

pub mod batch;
pub mod decode;
pub mod header;
pub mod lz4;
pub mod text;

pub use batch::{Batch, BatchFull, MAX_BATCH_BYTES, MAX_CHUNK_LEN, payload_prefix, payload_suffix};
pub use decode::{DecodeError, DecodeSummary, SegmentDecoder};
pub use header::{Crc32, FORMAT_VERSION, HEADER_LEN, HeaderError, SegmentHeader, file_name};

#[cfg(test)]
mod lz4_tests;
#[cfg(test)]
mod segment_tests;
#[cfg(test)]
mod text_tests;
