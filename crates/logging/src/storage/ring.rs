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

//! A circular journal of segments on a [`Medium`].
//!
//! ```text
//! sector:   0        1        2        3        4        5 ...
//!         ┌────────┬────────┬────────┬────────┬────────┬────────┐
//!         │ seg 7  │ seg 7  │ seg 8  │ seg 9  │ seg 9  │ (free) │   head ──►
//!         └────────┴────────┴────────┴────────┴────────┴────────┘
//! segment: [start 16 B][payload ...        ][ ... erased ... ][header 48 B]
//! ```
//!
//! A segment owns a run of whole sectors. Its first sector starts with a
//! checksummed *start record* (magic, id, size, codec) and the payload
//! follows; the [`SegmentHeader`] is programmed into the last 48 bytes when the
//! segment is sealed. New segments are allocated after the newest one,
//! wrapping to the start of the medium, and the oldest segments are erased
//! as the head reaches them or the budget is exceeded. Nothing is rewritten
//! in place, so the only damage a power loss can do is to the segment that was
//! being written, and recovery cuts it at its last complete block.
//!
//! Every start record carries the *generation* of the history, a number that
//! grows whenever the history is cleared or lost (see
//! [`StorageStats::generation`]). Clearing leaves a one-sector *marker*, a
//! start record without payload, so the generation survives a reboot while no
//! segment exists.
//!
//! The end mark of an LZ4 payload is never stored: it is appended when the
//! file is read, so a torn segment and a sealed one read the same way.

use heapless::Vec;

use super::medium::SECTOR_SIZE;
use super::{
    ChunkMeta, Limits, LogStorage, MAX_SEGMENTS, Medium, Recovery, SegmentId, SegmentInfo,
    StorageError, StorageStats,
};
use crate::Compression;
use crate::segment::lz4::{self, FRAME_HEADER_LEN};
use crate::segment::text::parse_line;
use crate::segment::{
    Crc32, HEADER_LEN, MAX_CHUNK_LEN, SegmentHeader, payload_prefix, payload_suffix,
};

const SECTOR: u32 = SECTOR_SIZE as u32;
const START_LEN: u32 = 20;
const HEADER_SLOT: u32 = 48;
const START_MAGIC: [u8; 4] = *b"RLS1";
const START_VERSION: u8 = 2;
// The codec byte of a marker.
const MARKER: u8 = 0xFF;

#[derive(Clone, Copy, Debug)]
struct Segment {
    id: u32,
    first_sector: u32,
    sectors: u32,
    codec: Compression,
    first_sequence: u64,
    last_sequence: u64,
    records: u32,
    raw_len: u32,
    stored: u32,
    crc: Crc32,
    pins: u8,
    generation: u32,
}

// What a start record says.
struct Start {
    id: u32,
    sectors: u32,
    // None for a marker.
    codec: Option<Compression>,
    generation: u32,
}

impl Segment {
    fn payload_capacity(&self) -> u32 {
        self.sectors * SECTOR - START_LEN - HEADER_SLOT
    }

    fn payload_offset(&self) -> u32 {
        self.first_sector * SECTOR + START_LEN
    }

    fn header_offset(&self) -> u32 {
        (self.first_sector + self.sectors) * SECTOR - HEADER_SLOT
    }

    fn overlaps(&self, first: u32, count: u32) -> bool {
        self.first_sector < first + count && first < self.first_sector + self.sectors
    }
}

/// [`LogStorage`] over a [`Medium`], see the module documentation.
pub struct RingStorage<M: Medium> {
    medium: M,
    segments: Vec<Segment, MAX_SEGMENTS>,
    open: bool,
    next_id: u32,
    budget_sectors: u32,
    // Sectors of the segments the limits ask for, 0 until one began.
    segment_sectors: u32,
    generation: u32,
}

type Failure<M> = StorageError<<M as Medium>::Error>;

impl<M: Medium> RingStorage<M> {
    /// A storage over `medium`; call [`recover`](LogStorage::recover) before
    /// use to find what the medium holds.
    pub fn new(medium: M) -> Self {
        let budget_sectors = medium.sectors();
        Self {
            medium,
            segments: Vec::new(),
            open: false,
            next_id: 1,
            budget_sectors,
            segment_sectors: 0,
            generation: 1,
        }
    }

    /// The medium, for tests and for the platform that owns it.
    pub fn into_medium(self) -> M {
        self.medium
    }

    /// The medium, borrowed.
    pub fn medium(&mut self) -> &mut M {
        &mut self.medium
    }

    /// The first sector of a segment, for tests that damage the medium.
    #[cfg(test)]
    pub(crate) fn sector_of(&self, id: SegmentId) -> Option<u32> {
        self.segments
            .iter()
            .find(|segment| segment.id == id)
            .map(|segment| segment.first_sector)
    }

    fn medium_error(error: M::Error) -> Failure<M> {
        StorageError::Medium(error)
    }

    fn used_sectors(&self) -> u32 {
        self.segments.iter().map(|segment| segment.sectors).sum()
    }

    fn read_into(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Failure<M>> {
        self.medium.read(offset, buf).map_err(Self::medium_error)
    }

    // Erases a sector only if something is programmed in it.
    fn erase_if_dirty(&mut self, first: u32, count: u32) -> Result<(), Failure<M>> {
        let mut piece = [0u8; 64];
        for sector in first..first + count {
            let mut dirty = false;
            let mut at = sector * SECTOR;
            let end = at + SECTOR;
            while at < end && !dirty {
                self.read_into(at, &mut piece)?;
                dirty = piece.iter().any(|byte| *byte != 0xFF);
                at += piece.len() as u32;
            }
            if dirty {
                self.medium.erase(sector, 1).map_err(Self::medium_error)?;
            }
        }
        Ok(())
    }

    // A segment is gone once its start sector is erased.
    fn drop_segment(&mut self, index: usize) -> Result<(), Failure<M>> {
        let segment = self.segments[index];
        self.medium
            .erase(segment.first_sector, 1)
            .map_err(Self::medium_error)?;
        self.segments.remove(index);
        Ok(())
    }

    fn head(&self) -> u32 {
        self.segments
            .last()
            .map_or(0, |newest| newest.first_sector + newest.sectors)
    }

    fn write_start(&mut self, segment: &Segment) -> Result<(), Failure<M>> {
        let codec = match segment.codec {
            Compression::None => 0,
            Compression::Lz4 => 1,
        };
        self.write_start_record(
            segment.first_sector,
            segment.sectors,
            codec,
            segment.id,
            segment.generation,
        )
    }

    fn write_start_record(
        &mut self,
        sector: u32,
        sectors: u32,
        codec: u8,
        id: u32,
        generation: u32,
    ) -> Result<(), Failure<M>> {
        let mut start = [0u8; START_LEN as usize];
        start[..4].copy_from_slice(&START_MAGIC);
        start[4] = START_VERSION;
        start[5] = codec;
        start[6..8].copy_from_slice(&(sectors as u16).to_le_bytes());
        start[8..12].copy_from_slice(&id.to_le_bytes());
        start[12..16].copy_from_slice(&generation.to_le_bytes());
        let crc = Crc32::of(&start[..16]);
        start[16..].copy_from_slice(&crc.to_le_bytes());
        self.medium
            .write(sector * SECTOR, &start)
            .map_err(Self::medium_error)
    }

    // Keeps the generation of an empty history where a reboot finds it.
    fn write_marker(&mut self) -> Result<(), Failure<M>> {
        self.erase_if_dirty(0, 1)?;
        self.write_start_record(0, 1, MARKER, 0, self.generation)
    }

    // The start record at the beginning of `sector`, if it holds a valid one.
    fn read_start(&mut self, sector: u32) -> Result<Option<Start>, Failure<M>> {
        let mut start = [0u8; START_LEN as usize];
        self.read_into(sector * SECTOR, &mut start)?;
        let word = |at: usize| {
            u32::from_le_bytes([start[at], start[at + 1], start[at + 2], start[at + 3]])
        };
        if start[..4] != START_MAGIC
            || start[4] != START_VERSION
            || Crc32::of(&start[..16]) != word(16)
        {
            return Ok(None);
        }
        let codec = match start[5] {
            0 => Some(Compression::None),
            1 => Some(Compression::Lz4),
            MARKER => None,
            _ => return Ok(None),
        };
        let sectors = u32::from(u16::from_le_bytes([start[6], start[7]]));
        if sectors == 0 || sector + sectors > self.medium.sectors() {
            return Ok(None);
        }
        Ok(Some(Start {
            id: word(8),
            sectors,
            codec,
            generation: word(12),
        }))
    }

    // The CRC-32 of `len` payload bytes at `offset`.
    fn payload_crc(&mut self, offset: u32, len: u32) -> Result<u32, Failure<M>> {
        let mut crc = Crc32::new();
        let mut piece = [0u8; 128];
        let mut at = 0;
        while at < len {
            let count = piece.len().min((len - at) as usize);
            self.read_into(offset + at, &mut piece[..count])?;
            crc.update(&piece[..count]);
            at += count as u32;
        }
        Ok(crc.finish())
    }

    // Reads a segment that was not sealed: how much of its payload is whole
    // and what it holds.
    fn scan_open(&mut self, segment: &mut Segment) -> Result<(), Failure<M>> {
        let base = segment.payload_offset();
        let capacity = segment.payload_capacity();
        let mut crc = Crc32::new();
        let note = |segment: &mut Segment, line: &str| {
            if let Some(record) = parse_line(line) {
                if segment.records == 0 {
                    segment.first_sequence = record.sequence;
                }
                segment.last_sequence = record.sequence;
                segment.records += 1;
            }
        };
        segment.records = 0;
        segment.raw_len = 0;
        segment.stored = 0;
        match segment.codec {
            Compression::Lz4 => {
                let mut prefix = [0u8; FRAME_HEADER_LEN];
                self.read_into(base, &mut prefix)?;
                if lz4::check_frame_header(&prefix).is_err() {
                    return Ok(());
                }
                crc.update(&prefix);
                let mut stored = FRAME_HEADER_LEN as u32;
                let mut chunk = [0u8; MAX_CHUNK_LEN];
                let mut text = [0u8; crate::segment::MAX_BATCH_BYTES];
                loop {
                    let mut size = [0u8; 4];
                    if stored + 4 > capacity {
                        break;
                    }
                    self.read_into(base + stored, &mut size)?;
                    let length = (u32::from_le_bytes(size) & 0x7FFF_FFFF) as usize;
                    if length == 0 || length + lz4::BLOCK_OVERHEAD > chunk.len() {
                        break;
                    }
                    let total = length + lz4::BLOCK_OVERHEAD;
                    if stored + total as u32 > capacity {
                        break;
                    }
                    self.read_into(base + stored, &mut chunk[..total])?;
                    let Ok(Some((block, used))) = lz4::next_block(&chunk[..total]) else {
                        break;
                    };
                    let Ok(len) = block.decode(&mut text) else {
                        break;
                    };
                    crc.update(&chunk[..used]);
                    stored += used as u32;
                    segment.raw_len += len as u32;
                    for line in text[..len].split(|byte| *byte == b'\n') {
                        if let Ok(line) = core::str::from_utf8(line) {
                            note(segment, line);
                        }
                    }
                }
                segment.stored = stored;
            }
            Compression::None => {
                let mut piece = [0u8; 128];
                let mut whole = 0u32;
                let mut seen = 0u32;
                let mut line = Vec::<u8, { crate::segment::text::MAX_LINE_LEN }>::new();
                'scan: while seen < capacity {
                    let count = piece.len().min((capacity - seen) as usize);
                    self.read_into(base + seen, &mut piece[..count])?;
                    for (index, byte) in piece[..count].iter().enumerate() {
                        match *byte {
                            b'\n' => {
                                if let Ok(text) = core::str::from_utf8(&line) {
                                    note(segment, text);
                                }
                                line.clear();
                                whole = seen + index as u32 + 1;
                            }
                            // Erased flash, or what a torn header left.
                            0xFF | 0..=8 | 11..=31 | 127 => break 'scan,
                            other => {
                                if line.push(other).is_err() {
                                    break 'scan;
                                }
                            }
                        }
                    }
                    seen += count as u32;
                }
                segment.stored = whole;
                segment.raw_len = whole;
                let mut at = 0;
                while at < whole {
                    let count = piece.len().min((whole - at) as usize);
                    self.read_into(base + at, &mut piece[..count])?;
                    crc.update(&piece[..count]);
                    at += count as u32;
                }
            }
        }
        segment.crc = crc;
        Ok(())
    }

    fn info(&self, segment: &Segment, is_open: bool) -> SegmentInfo {
        SegmentInfo {
            id: segment.id,
            codec: segment.codec,
            first_sequence: segment.first_sequence,
            last_sequence: segment.last_sequence,
            records: segment.records,
            file_len: segment.stored + payload_suffix(segment.codec).len() as u32,
            open: is_open,
            pinned: segment.pins > 0,
        }
    }
}

impl<M: Medium> LogStorage for RingStorage<M> {
    type Error = M::Error;

    async fn recover(&mut self) -> Result<Recovery, Failure<M>> {
        // Downloads that hold segments keep holding them across a recovery.
        let pinned: Vec<(u32, u8), MAX_SEGMENTS> = self
            .segments
            .iter()
            .filter(|segment| segment.pins > 0)
            .map(|segment| (segment.id, segment.pins))
            .collect();
        self.segments.clear();
        self.open = false;
        let total = self.medium.sectors();
        let mut found = Vec::<Segment, { MAX_SEGMENTS + 8 }>::new();
        let mut dropped = 0u32;
        let mut corrupted = 0u32;
        let mut marker_generation = 0u32;
        let mut lost_generation = 0u32;
        let mut sector = 0;
        while sector < total {
            let Some(start) = self.read_start(sector)? else {
                sector += 1;
                continue;
            };
            let Some(codec) = start.codec else {
                marker_generation = marker_generation.max(start.generation);
                sector += 1;
                continue;
            };
            let (id, sectors) = (start.id, start.sectors);
            let mut segment = Segment {
                id,
                first_sector: sector,
                sectors,
                codec,
                generation: start.generation,
                first_sequence: 0,
                last_sequence: 0,
                records: 0,
                raw_len: 0,
                stored: 0,
                crc: Crc32::new(),
                pins: 0,
            };
            let mut header = [0u8; HEADER_LEN];
            self.read_into(segment.header_offset(), &mut header)?;
            match SegmentHeader::decode(&header) {
                Ok(sealed)
                    if sealed.codec == codec
                        && sealed.payload_len <= segment.payload_capacity()
                        && sealed.records > 0 =>
                {
                    // A sealed segment is never rewritten, so a payload that does
                    // not match its checksum was damaged on the medium: the
                    // segment is dropped whole, not trusted in part.
                    if self.payload_crc(segment.payload_offset(), sealed.payload_len)?
                        == sealed.payload_crc
                    {
                        segment.first_sequence = sealed.first_sequence;
                        segment.last_sequence = sealed.last_sequence;
                        segment.records = sealed.records;
                        segment.raw_len = sealed.raw_len;
                        segment.stored = sealed.payload_len;
                        segment.crc = Crc32::new();
                    } else {
                        corrupted += 1;
                    }
                }
                _ => self.scan_open(&mut segment)?,
            }
            sector += sectors;
            if segment.records == 0 {
                self.medium
                    .erase(segment.first_sector, 1)
                    .map_err(Self::medium_error)?;
                dropped += 1;
                lost_generation = lost_generation.max(start.generation);
            } else if found.push(segment).is_err() {
                dropped += 1;
            }
        }
        found.sort_unstable_by_key(|segment| segment.id);
        while found.len() > MAX_SEGMENTS {
            let oldest = found.remove(0);
            self.medium
                .erase(oldest.first_sector, 1)
                .map_err(Self::medium_error)?;
            dropped += 1;
        }
        for segment in found {
            let mut segment = segment;
            if let Some((_, pins)) = pinned.iter().find(|(id, _)| *id == segment.id) {
                segment.pins = *pins;
            }
            let _ = self.segments.push(segment);
        }
        self.next_id = self
            .segments
            .last()
            .map_or(1, |newest| newest.id.wrapping_add(1));
        match self.segments.last() {
            Some(newest) => self.generation = newest.generation,
            None => {
                // History that was lost is a new one; a marker already says so
                // for a history that was cleared.
                let lost = if lost_generation > 0 {
                    lost_generation.wrapping_add(1)
                } else {
                    0
                };
                self.generation = marker_generation.max(lost).max(1);
                // Generation 1 is what a medium without a marker means.
                if marker_generation.max(1) != self.generation {
                    self.write_marker()?;
                }
            }
        }
        Ok(Recovery {
            newest_sequence: self.segments.last().map(|newest| newest.last_sequence),
            segments: self.segments.len() as u32,
            dropped,
            corrupted,
        })
    }

    async fn begin_segment(
        &mut self,
        codec: Compression,
        limits: Limits,
    ) -> Result<SegmentId, Failure<M>> {
        if self.open {
            self.seal().await?;
        }
        let total = self.medium.sectors();
        let sectors = limits.segment_bytes.div_ceil(SECTOR).max(1);
        let budget = (limits.retention_bytes / SECTOR).min(total);
        if sectors > budget || sectors > u32::from(u16::MAX) {
            return Err(StorageError::InvalidLimits);
        }
        self.budget_sectors = budget;
        self.segment_sectors = sectors;

        let head = loop {
            let mut head = self.head();
            if head + sectors > total {
                head = 0;
            }
            let crowded = self
                .segments
                .iter()
                .any(|segment| segment.overlaps(head, sectors));
            let over_budget = self.used_sectors() + sectors > budget;
            if !crowded && !over_budget && self.segments.len() < MAX_SEGMENTS {
                break head;
            }
            let Some(oldest) = self.segments.first() else {
                break head;
            };
            if oldest.pins > 0 {
                return Err(StorageError::Pinned);
            }
            self.drop_segment(0)?;
        };

        self.erase_if_dirty(head, sectors)?;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let prefix = payload_prefix(codec);
        let mut segment = Segment {
            id,
            first_sector: head,
            sectors,
            codec,
            first_sequence: 0,
            last_sequence: 0,
            records: 0,
            raw_len: 0,
            stored: prefix.len() as u32,
            crc: Crc32::new(),
            pins: 0,
            generation: self.generation,
        };
        segment.crc.update(&prefix);
        if !prefix.is_empty() {
            self.medium
                .write(segment.payload_offset(), &prefix)
                .map_err(Self::medium_error)?;
        }
        // The start record last: a segment exists only once it is complete.
        self.write_start(&segment)?;
        self.segments
            .push(segment)
            .map_err(|_| StorageError::InvalidLimits)?;
        self.open = true;
        Ok(id)
    }

    fn open_capacity(&self) -> usize {
        match (self.open, self.segments.last()) {
            (true, Some(segment)) => (segment.payload_capacity() - segment.stored) as usize,
            _ => 0,
        }
    }

    async fn append(&mut self, chunk: &[u8], meta: ChunkMeta) -> Result<(), Failure<M>> {
        if !self.open {
            return Err(StorageError::NoOpenSegment);
        }
        let Some(segment) = self.segments.last().copied() else {
            return Err(StorageError::NoOpenSegment);
        };
        if chunk.len() > (segment.payload_capacity() - segment.stored) as usize {
            return Err(StorageError::SegmentFull);
        }
        self.medium
            .write(segment.payload_offset() + segment.stored, chunk)
            .map_err(Self::medium_error)?;
        let Some(segment) = self.segments.last_mut() else {
            return Err(StorageError::NoOpenSegment);
        };
        if segment.records == 0 {
            segment.first_sequence = meta.first_sequence;
        }
        segment.last_sequence = meta.last_sequence;
        segment.records += meta.records;
        segment.raw_len += meta.raw_len;
        segment.stored += chunk.len() as u32;
        segment.crc.update(chunk);
        Ok(())
    }

    async fn seal(&mut self) -> Result<(), Failure<M>> {
        if !self.open {
            return Ok(());
        }
        self.open = false;
        let Some(segment) = self.segments.last().copied() else {
            return Ok(());
        };
        if segment.records == 0 {
            let index = self.segments.len() - 1;
            return self.drop_segment(index);
        }
        let header = SegmentHeader {
            codec: segment.codec,
            first_sequence: segment.first_sequence,
            last_sequence: segment.last_sequence,
            records: segment.records,
            payload_len: segment.stored,
            raw_len: segment.raw_len,
            payload_crc: segment.crc.finish(),
        };
        self.medium
            .write(segment.header_offset(), &header.encode())
            .map_err(Self::medium_error)
    }

    async fn segments(&mut self) -> Result<Vec<SegmentInfo, MAX_SEGMENTS>, Failure<M>> {
        let mut out = Vec::new();
        let last = self.segments.len().saturating_sub(1);
        for (index, segment) in self.segments.iter().enumerate() {
            let _ = out.push(self.info(segment, self.open && index == last));
        }
        Ok(out)
    }

    async fn read(
        &mut self,
        id: SegmentId,
        offset: u32,
        buf: &mut [u8],
    ) -> Result<usize, Failure<M>> {
        let Some(segment) = self
            .segments
            .iter()
            .find(|segment| segment.id == id)
            .copied()
        else {
            return Err(StorageError::UnknownSegment);
        };
        let suffix = payload_suffix(segment.codec);
        let mut done = 0;
        if offset < segment.stored {
            let count = buf.len().min((segment.stored - offset) as usize);
            self.read_into(segment.payload_offset() + offset, &mut buf[..count])?;
            done = count;
        }
        let virtual_at = (offset + done as u32).saturating_sub(segment.stored) as usize;
        if done < buf.len() && virtual_at < suffix.len() && offset + done as u32 >= segment.stored {
            let count = (buf.len() - done).min(suffix.len() - virtual_at);
            buf[done..done + count].copy_from_slice(&suffix[virtual_at..virtual_at + count]);
            done += count;
        }
        Ok(done)
    }

    async fn pin(&mut self, id: SegmentId) -> Result<(), Failure<M>> {
        let segment = self
            .segments
            .iter_mut()
            .find(|segment| segment.id == id)
            .ok_or(StorageError::UnknownSegment)?;
        segment.pins = segment.pins.saturating_add(1);
        Ok(())
    }

    fn unpin(&mut self, id: SegmentId) {
        if let Some(segment) = self.segments.iter_mut().find(|segment| segment.id == id) {
            segment.pins = segment.pins.saturating_sub(1);
        }
    }

    async fn clear(&mut self) -> Result<(), Failure<M>> {
        if self.segments.iter().any(|segment| segment.pins > 0) {
            return Err(StorageError::Busy);
        }
        self.open = false;
        while !self.segments.is_empty() {
            self.drop_segment(0)?;
        }
        // What was cleared is a history of its own: cursors into it must not
        // be mistaken for cursors into the next one, not even after a reboot.
        self.generation = self.generation.wrapping_add(1).max(1);
        self.write_marker()
    }

    async fn stats(&mut self) -> Result<StorageStats, Failure<M>> {
        let total = self.medium.sectors();
        let used = self.used_sectors();
        let mut capacity = self.budget_sectors.min(total);
        // At most MAX_SEGMENTS segments are retained: with small segments the
        // count, not the bytes, is the limit.
        let per_segment = self.segment_sectors;
        if per_segment > 0 {
            capacity = capacity.min((per_segment * MAX_SEGMENTS as u32).max(used));
        }
        let mut free = capacity.saturating_sub(used).min(total - used);
        if per_segment > 0 {
            let left = (MAX_SEGMENTS - self.segments.len()) as u32;
            free = free.min(left * per_segment);
        } else if self.segments.len() >= MAX_SEGMENTS {
            free = 0;
        }
        Ok(StorageStats {
            generation: self.generation,
            capacity_bytes: capacity * SECTOR,
            used_bytes: used * SECTOR,
            free_bytes: free * SECTOR,
            segments: self.segments.len() as u32,
            records: self
                .segments
                .iter()
                .map(|segment| u64::from(segment.records))
                .sum(),
            oldest_sequence: self.segments.first().map(|segment| segment.first_sequence),
            newest_sequence: self.segments.last().map(|segment| segment.last_sequence),
        })
    }
}
