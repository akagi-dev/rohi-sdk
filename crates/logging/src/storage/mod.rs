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

//! Where log segments live: the [`LogStorage`] contract and its backends.
//!
//! `LogStorage` owns the physical segments: opening, appending to and sealing
//! them, retention, pinning for downloads, recovery after a power loss. The
//! logging service owns the logical side: it assigns sequences, batches
//! records, decides when to rotate and reports status.
//!
//! [`RingStorage`] implements the contract over a [`Medium`], an erasable
//! array of sectors: [`RamMedium`] on the host and in tests, a flash region
//! on a device. A backend with files (an SD card) implements `LogStorage`
//! directly.

mod flash;
mod medium;
mod ring;

#[cfg(test)]
mod contract;
#[cfg(test)]
mod flash_tests;
#[cfg(test)]
mod ring_tests;

use core::fmt;

use heapless::Vec;

pub use flash::{FlashMedium, FlashMediumError};
pub use medium::{Medium, RamError, RamMedium, SECTOR_SIZE};
pub use ring::RingStorage;

use crate::Compression;

/// Most segments a storage retains, and so the most files of a download. At
/// most [`MAX_STREAM_FILES`](rohi_service::MAX_STREAM_FILES).
pub const MAX_SEGMENTS: usize = 24;

/// Identifies a segment for as long as the storage retains it. Ids grow with
/// every new segment, so a larger id is a newer segment.
pub type SegmentId = u32;

/// What a new segment is sized and budgeted by; taken from the `logging`
/// config section each time a segment begins, so a change applies to new
/// segments only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Size of the new segment, in bytes of storage.
    pub segment_bytes: u32,
    /// Budget of the whole retained history, in bytes of storage.
    pub retention_bytes: u32,
}

/// What a storage knows about one retained segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SegmentInfo {
    /// The segment.
    pub id: SegmentId,
    /// How its payload is encoded.
    pub codec: Compression,
    /// Sequence of its first record.
    pub first_sequence: u64,
    /// Sequence of its last record.
    pub last_sequence: u64,
    /// Number of records.
    pub records: u32,
    /// Size of the file a download gets: the payload and the end of its frame.
    pub file_len: u32,
    /// Whether the segment is closed. Only the newest segment can be open.
    pub open: bool,
    /// Whether a download holds it.
    pub pinned: bool,
}

/// Space and content of a storage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StorageStats {
    /// Budget of the history: the retention, bounded by the medium.
    pub capacity_bytes: u32,
    /// Storage allocated to retained segments, the open one included.
    pub used_bytes: u32,
    /// Storage that can be used before the oldest segment has to go: what is
    /// left of the budget, bounded by what is physically free.
    pub free_bytes: u32,
    /// Retained segments.
    pub segments: u32,
    /// Retained records.
    pub records: u64,
    /// Sequence of the oldest retained record.
    pub oldest_sequence: Option<u64>,
    /// Sequence of the newest retained record.
    pub newest_sequence: Option<u64>,
}

/// What [`LogStorage::recover`] found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recovery {
    /// Sequence of the newest retained record; the next one is above it.
    pub newest_sequence: Option<u64>,
    /// Segments retained.
    pub segments: u32,
    /// Segments that were damaged or empty and were dropped.
    pub dropped: u32,
}

/// Why a storage operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageError<E> {
    /// The medium failed; the storage may have lost the segment it was
    /// writing.
    Medium(E),
    /// There is no room for a new segment and the oldest one is pinned by a
    /// download, so it cannot be dropped.
    Pinned,
    /// The chunk does not fit what is left of the open segment: seal it and
    /// begin another.
    SegmentFull,
    /// There is no open segment.
    NoOpenSegment,
    /// The requested segment is not retained.
    UnknownSegment,
    /// The operation needs the segments to be unpinned.
    Busy,
    /// The limits cannot be met by this medium.
    InvalidLimits,
}

impl<E: fmt::Debug> fmt::Display for StorageError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Medium(error) => write!(f, "storage medium failed: {error:?}"),
            Self::Pinned => f.write_str("oldest segment is pinned by a download"),
            Self::SegmentFull => f.write_str("open segment is full"),
            Self::NoOpenSegment => f.write_str("no open segment"),
            Self::UnknownSegment => f.write_str("unknown segment"),
            Self::Busy => f.write_str("segments are pinned"),
            Self::InvalidLimits => f.write_str("limits do not fit the medium"),
        }
    }
}

/// The records of a chunk, for the header of its segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkMeta {
    /// Number of records in the chunk.
    pub records: u32,
    /// Sequence of its first record.
    pub first_sequence: u64,
    /// Sequence of its last record.
    pub last_sequence: u64,
    /// Bytes of text the chunk holds before compression.
    pub raw_len: u32,
}

/// The segments of the log.
///
/// A storage keeps history as a series of segments, each a file of text
/// records in one codec. At most one segment is open for appending; the others
/// are sealed and never change. Segments are retained within a budget and the
/// oldest is dropped to make room, unless a download pins it.
///
/// Every method may await the medium; none is cancel safe in the middle of a
/// write, so the logging task, the only caller, never cancels them.
#[allow(async_fn_in_trait)]
pub trait LogStorage {
    /// The error of the medium.
    type Error: fmt::Debug + Copy + PartialEq;

    /// Finds the history left on the medium after a reset: reads the sealed
    /// segments, cuts a segment that was torn by a power loss at its last
    /// complete block and drops what cannot be read. Never fails because of
    /// the content, only because of the medium.
    async fn recover(&mut self) -> Result<Recovery, StorageError<Self::Error>>;

    /// Opens a new segment, sealing the open one first and dropping the oldest
    /// segments that do not fit `limits`.
    ///
    /// # Errors
    ///
    /// [`StorageError::Pinned`] when room needs a segment a download holds,
    /// [`StorageError::InvalidLimits`] when the medium is too small.
    async fn begin_segment(
        &mut self,
        codec: Compression,
        limits: Limits,
    ) -> Result<SegmentId, StorageError<Self::Error>>;

    /// Bytes still available for [`append`](Self::append) in the open segment;
    /// 0 without one.
    fn open_capacity(&self) -> usize;

    /// Appends an encoded batch to the open segment.
    ///
    /// # Errors
    ///
    /// [`StorageError::SegmentFull`] when it does not fit,
    /// [`StorageError::NoOpenSegment`] without an open segment.
    async fn append(
        &mut self,
        chunk: &[u8],
        meta: ChunkMeta,
    ) -> Result<(), StorageError<Self::Error>>;

    /// Closes the open segment. A segment without records is dropped instead.
    async fn seal(&mut self) -> Result<(), StorageError<Self::Error>>;

    /// The retained segments, oldest first.
    async fn segments(
        &mut self,
    ) -> Result<Vec<SegmentInfo, MAX_SEGMENTS>, StorageError<Self::Error>>;

    /// Reads the file of a segment from `offset` into `buf` and returns the
    /// bytes read, 0 at its end. The file of a sealed segment never changes.
    async fn read(
        &mut self,
        id: SegmentId,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, StorageError<Self::Error>>;

    /// Keeps a segment from being dropped until it is [unpinned](Self::unpin).
    /// Pins count: each `pin` needs its `unpin`.
    async fn pin(&mut self, id: SegmentId) -> Result<(), StorageError<Self::Error>>;

    /// Releases one pin of a segment; unknown or unpinned segments are ignored.
    fn unpin(&mut self, id: SegmentId);

    /// Removes every segment.
    ///
    /// # Errors
    ///
    /// [`StorageError::Busy`] while a download pins a segment.
    async fn clear(&mut self) -> Result<(), StorageError<Self::Error>>;

    /// Space and content.
    async fn stats(&mut self) -> Result<StorageStats, StorageError<Self::Error>>;
}

/// A log storage in RAM, with the layout and the rules of NOR flash: for
/// hosts, tests and devices that keep the log only until the next reset.
pub type RamLogStorage<const SECTORS: usize> = RingStorage<RamMedium<SECTORS>>;

/// A log storage on a region of internal flash.
pub type FlashLogStorage<F> = RingStorage<FlashMedium<F>>;
