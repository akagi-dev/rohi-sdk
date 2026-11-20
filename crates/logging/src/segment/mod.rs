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

//! The persistent form of the log: line text, segment headers and the batches
//! and decoders that connect them. With the `lz4` feature the payload can also be
//! one LZ4 frame.
//!
//! A **segment** is a header ([`SegmentHeader`]) followed by a payload. The
//! payload is the text of the records, one [line](text) each: plain
//! ([`Compression::None`](crate::Compression)) or, with the `lz4` feature, one
//! LZ4 frame. The logging task collects records into a [`Batch`], and appends
//! each batch to the open segment as one block (independently compressed with
//! `lz4`), so a segment can be cut by a power loss at any byte and still be read
//! up to its last complete block. Sealing a segment writes the header (and, with
//! `lz4`, the end mark).

pub mod batch;
pub mod decode;
pub mod header;
#[cfg(feature = "lz4")]
pub mod lz4;
pub mod text;

pub use batch::{
    Batch, BatchFull, EncodeError, MAX_BATCH_BYTES, MAX_CHUNK_LEN, PREFIX_LEN, Scratch,
    payload_prefix, payload_suffix, scratch,
};
pub use decode::{DecodeError, DecodeSummary, SegmentDecoder};
pub use header::{Crc32, FORMAT_VERSION, HEADER_LEN, HeaderError, SegmentHeader, file_name};

#[cfg(all(test, feature = "lz4"))]
mod lz4_tests;
#[cfg(test)]
mod segment_tests;
#[cfg(test)]
mod text_tests;
