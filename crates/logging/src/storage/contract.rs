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
//! The behavioural contract every [`LogStorage`] backend must pass.
//!
//! Each backend instantiates [`Harness`] and runs [`contract_tests!`] over
//! it, so internal flash, the RAM medium and later the SD card are held to the
//! same behaviour.

use std::vec::Vec;

use embassy_futures::block_on;
use rohi_time::UnixTime;

use super::*;
use crate::segment::{Batch, MAX_CHUNK_LEN, SegmentDecoder, payload_suffix};
use crate::{LogLevel, LogRecord};

/// How a backend is created and restarted for the contract.
pub(crate) trait Harness {
    type Storage: LogStorage;
    /// Total storage the backend offers, in bytes.
    const CAPACITY: u32;
    /// A new, empty storage.
    fn create() -> Self::Storage;
    /// A power cycle: the same medium, a new storage that has recovered.
    fn reboot(storage: Self::Storage) -> Self::Storage;
}

pub(crate) fn record(sequence: u64) -> LogRecord {
    let mut record = LogRecord::new(
        LogLevel::Info,
        "wifi",
        format_args!(
            "connected to access point {sequence} rssi -{}",
            40 + sequence % 30
        ),
        sequence * 10,
        sequence
            .is_multiple_of(3)
            .then(|| UnixTime::from_millis(1_800_000_000_000 + sequence)),
    );
    record.sequence = sequence;
    record
}

pub(crate) const LIMITS: Limits = Limits {
    segment_bytes: 4096,
    retention_bytes: 4096 * 6,
};

/// Plays the logging task: batches records and rotates segments.
pub(crate) struct Writer {
    pub(crate) next: u64,
    pub(crate) codec: Compression,
    pub(crate) limits: Limits,
    batch: Batch,
    /// Highest sequence an `append` accepted.
    pub(crate) durable: u64,
}

impl Writer {
    pub(crate) fn new(next: u64, codec: Compression, limits: Limits) -> Self {
        Self {
            next,
            codec,
            limits,
            batch: Batch::new(),
            durable: next.saturating_sub(1),
        }
    }

    pub(crate) fn push<S: LogStorage>(
        &mut self,
        storage: &mut S,
    ) -> Result<(), StorageError<S::Error>> {
        let record = record(self.next);
        if self.batch.push(&record).is_err() {
            self.flush(storage)?;
            self.batch.push(&record).unwrap();
        }
        self.next += 1;
        Ok(())
    }

    pub(crate) fn push_many<S: LogStorage>(
        &mut self,
        storage: &mut S,
        count: u64,
    ) -> Result<(), StorageError<S::Error>> {
        for _ in 0..count {
            self.push(storage)?;
        }
        Ok(())
    }

    pub(crate) fn flush<S: LogStorage>(
        &mut self,
        storage: &mut S,
    ) -> Result<(), StorageError<S::Error>> {
        if self.batch.is_empty() {
            return Ok(());
        }
        let mut chunk = [0u8; MAX_CHUNK_LEN];
        let len = self.batch.encode(self.codec, &mut chunk).unwrap();
        if storage.open_capacity() < len {
            block_on(storage.seal())?;
            block_on(storage.begin_segment(self.codec, self.limits))?;
        }
        let meta = ChunkMeta {
            records: self.batch.records(),
            first_sequence: self.batch.first_sequence(),
            last_sequence: self.batch.last_sequence(),
            raw_len: self.batch.len() as u32,
        };
        block_on(storage.append(&chunk[..len], meta))?;
        self.durable = meta.last_sequence;
        self.batch.clear();
        Ok(())
    }
}

/// The whole file of a segment, read in pieces of `piece` bytes.
pub(crate) fn read_file<S: LogStorage>(
    storage: &mut S,
    info: &SegmentInfo,
    piece: usize,
) -> Vec<u8> {
    let mut file = Vec::new();
    let mut buf = std::vec![0u8; piece];
    loop {
        let n = block_on(storage.read(info.id, file.len() as u32, &mut buf)).unwrap();
        if n == 0 {
            break;
        }
        file.extend_from_slice(&buf[..n]);
    }
    assert_eq!(file.len() as u32, info.file_len);
    file
}

/// Every record of the storage, oldest first, each segment decoded alone.
pub(crate) fn all_records<S: LogStorage>(storage: &mut S) -> Vec<LogRecord> {
    let mut all = Vec::new();
    for info in block_on(storage.segments()).unwrap() {
        let file = read_file(storage, &info, 100);
        let mut decoder = SegmentDecoder::new(info.codec);
        let mut records = Vec::new();
        decoder
            .push(&file, &mut |record| records.push(record))
            .unwrap();
        assert!(
            decoder.is_complete(),
            "segment {} is not a whole file",
            info.id
        );
        assert_eq!(records.len() as u32, info.records);
        assert_eq!(
            records.first().map(|r| r.sequence),
            Some(info.first_sequence)
        );
        assert_eq!(records.last().map(|r| r.sequence), Some(info.last_sequence));
        all.extend(records);
    }
    all
}

pub(crate) fn assert_contiguous(records: &[LogRecord]) {
    for pair in records.windows(2) {
        assert_eq!(pair[1].sequence, pair[0].sequence + 1, "gap in the history");
    }
}

pub(crate) fn appended_records_read_back<H: Harness>() {
    let mut storage = H::create();
    for codec in [Compression::None, Compression::Lz4] {
        block_on(storage.clear()).unwrap();
        let mut writer = Writer::new(1, codec, LIMITS);
        writer.push_many(&mut storage, 60).unwrap();
        writer.flush(&mut storage).unwrap();
        block_on(storage.seal()).unwrap();
        let records = all_records(&mut storage);
        assert_eq!(
            records,
            (1..=60).map(record).collect::<Vec<_>>(),
            "{codec:?}"
        );
        let infos = block_on(storage.segments()).unwrap();
        assert_eq!(infos.len(), 1);
        assert!(!infos[0].open);
        assert_eq!(infos[0].codec, codec);
    }
}

pub(crate) fn reading_past_the_end_returns_nothing<H: Harness>() {
    let mut storage = H::create();
    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 10).unwrap();
    writer.flush(&mut storage).unwrap();
    block_on(storage.seal()).unwrap();
    let info = block_on(storage.segments()).unwrap()[0];
    let mut buf = [0u8; 16];
    assert_eq!(
        block_on(storage.read(info.id, info.file_len, &mut buf)).unwrap(),
        0
    );
    assert_eq!(
        block_on(storage.read(info.id, info.file_len + 99, &mut buf)).unwrap(),
        0
    );
    assert_eq!(
        block_on(storage.read(info.id + 100, 0, &mut buf)),
        Err(StorageError::UnknownSegment)
    );
    // The file ends with the end mark of the frame.
    let file = read_file(&mut storage, &info, 7);
    assert!(file.ends_with(payload_suffix(Compression::Lz4)));
    // A read straddling the stored bytes and the end mark.
    let mut tail = [0u8; 8];
    let n = block_on(storage.read(info.id, info.file_len - 6, &mut tail)).unwrap();
    assert_eq!(&tail[..n], &file[file.len() - 6..]);
}

pub(crate) fn segments_rotate_and_stay_contiguous<H: Harness>() {
    let mut storage = H::create();
    let mut writer = Writer::new(
        1,
        Compression::Lz4,
        Limits {
            segment_bytes: 4096,
            retention_bytes: H::CAPACITY,
        },
    );
    writer.push_many(&mut storage, 400).unwrap();
    writer.flush(&mut storage).unwrap();
    let infos = block_on(storage.segments()).unwrap();
    assert!(infos.len() > 1, "{} segments", infos.len());
    assert!(infos.last().unwrap().open);
    assert!(infos[..infos.len() - 1].iter().all(|info| !info.open));
    for pair in infos.windows(2) {
        assert!(pair[1].id > pair[0].id);
        assert_eq!(pair[1].first_sequence, pair[0].last_sequence + 1);
    }
    let records = all_records(&mut storage);
    assert_eq!(records.len(), 400);
    assert_contiguous(&records);
}

pub(crate) fn retention_drops_the_oldest_segments<H: Harness>() {
    let mut storage = H::create();
    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    let mut oldest = 0;
    for _ in 0..40 {
        writer.push_many(&mut storage, 100).unwrap();
        let stats = block_on(storage.stats()).unwrap();
        assert!(stats.used_bytes <= stats.capacity_bytes, "{stats:?}");
        assert_eq!(stats.capacity_bytes, LIMITS.retention_bytes);
        assert!(stats.segments <= 6);
        let first = stats.oldest_sequence.unwrap();
        assert!(first >= oldest, "history moved backwards");
        oldest = first;
    }
    writer.flush(&mut storage).unwrap();
    let records = all_records(&mut storage);
    assert_contiguous(&records);
    assert_eq!(records.last().unwrap().sequence, 4_000);
    assert!(records[0].sequence > 1, "nothing was dropped");
    let stats = block_on(storage.stats()).unwrap();
    assert_eq!(stats.records, records.len() as u64);
    assert_eq!(stats.newest_sequence, Some(4_000));
}

pub(crate) fn a_pinned_segment_is_kept_and_blocks_eviction<H: Harness>() {
    let mut storage = H::create();
    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 300).unwrap();
    writer.flush(&mut storage).unwrap();
    let oldest = block_on(storage.segments()).unwrap()[0];
    assert!(!oldest.open, "a download pins sealed segments");
    block_on(storage.pin(oldest.id)).unwrap();
    let before = read_file(&mut storage, &oldest, 128);

    let mut refused = false;
    for _ in 0..200 {
        match writer.push_many(&mut storage, 100) {
            Ok(()) => {}
            Err(StorageError::Pinned) => {
                refused = true;
                break;
            }
            Err(other) => panic!("{other:?}"),
        }
    }
    assert!(refused, "eviction ignored the pin");
    let held = block_on(storage.segments()).unwrap()[0];
    assert_eq!((held.id, held.pinned), (oldest.id, true));
    assert_eq!(read_file(&mut storage, &held, 64), before);

    storage.unpin(oldest.id);
    writer.flush(&mut storage).unwrap_or(());
    // Room again: the oldest is dropped.
    block_on(storage.seal()).unwrap();
    block_on(storage.begin_segment(Compression::Lz4, LIMITS)).unwrap();
    let after = block_on(storage.segments()).unwrap();
    assert_ne!(after[0].id, oldest.id);
}

pub(crate) fn pins_count<H: Harness>() {
    let mut storage = H::create();
    let mut writer = Writer::new(1, Compression::None, LIMITS);
    writer.push_many(&mut storage, 20).unwrap();
    writer.flush(&mut storage).unwrap();
    block_on(storage.seal()).unwrap();
    let id = block_on(storage.segments()).unwrap()[0].id;
    block_on(storage.pin(id)).unwrap();
    block_on(storage.pin(id)).unwrap();
    storage.unpin(id);
    assert_eq!(block_on(storage.clear()), Err(StorageError::Busy));
    storage.unpin(id);
    storage.unpin(id);
    assert_eq!(block_on(storage.clear()), Ok(()));
    assert_eq!(block_on(storage.pin(id)), Err(StorageError::UnknownSegment));
}

pub(crate) fn clear_removes_everything_and_the_storage_keeps_working<H: Harness>() {
    let mut storage = H::create();
    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 300).unwrap();
    writer.flush(&mut storage).unwrap();
    block_on(storage.clear()).unwrap();
    let stats = block_on(storage.stats()).unwrap();
    assert_eq!((stats.segments, stats.records, stats.used_bytes), (0, 0, 0));
    assert_eq!(stats.free_bytes, stats.capacity_bytes);
    assert_eq!((stats.oldest_sequence, stats.newest_sequence), (None, None));
    assert_eq!(storage.open_capacity(), 0);
    assert!(block_on(storage.segments()).unwrap().is_empty());

    let mut writer = Writer::new(301, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 50).unwrap();
    writer.flush(&mut storage).unwrap();
    let records = all_records(&mut storage);
    assert_eq!(records.first().unwrap().sequence, 301);
    assert_eq!(records.len(), 50);

    // Nothing survives a restart either.
    block_on(storage.clear()).unwrap();
    let mut storage = H::reboot(storage);
    assert!(block_on(storage.segments()).unwrap().is_empty());
}

pub(crate) fn stats_report_space<H: Harness>() {
    let mut storage = H::create();
    let initial = block_on(storage.stats()).unwrap();
    assert_eq!(initial.used_bytes, 0);
    assert_eq!(initial.segments, 0);
    assert!(initial.capacity_bytes > 0 && initial.capacity_bytes <= H::CAPACITY);
    assert_eq!(initial.free_bytes, initial.capacity_bytes);

    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 5).unwrap();
    writer.flush(&mut storage).unwrap();
    let stats = block_on(storage.stats()).unwrap();
    assert_eq!(stats.capacity_bytes, LIMITS.retention_bytes);
    assert_eq!(stats.used_bytes, 4096);
    assert_eq!(stats.free_bytes, stats.capacity_bytes - 4096);
    assert_eq!((stats.segments, stats.records), (1, 5));
    assert_eq!(
        (stats.oldest_sequence, stats.newest_sequence),
        (Some(1), Some(5))
    );

    // Filled, the free space is zero and that is not an error: the next
    // segment replaces the oldest.
    writer.push_many(&mut storage, 2_000).unwrap();
    writer.flush(&mut storage).unwrap();
    let full = block_on(storage.stats()).unwrap();
    assert!(full.used_bytes <= full.capacity_bytes);
    assert!(full.free_bytes <= full.capacity_bytes - full.used_bytes);
    assert_eq!(full.free_bytes, full.capacity_bytes - full.used_bytes);
}

pub(crate) fn every_segment_keeps_its_own_codec<H: Harness>() {
    let mut storage = H::create();
    let mut writer = Writer::new(1, Compression::None, LIMITS);
    writer.push_many(&mut storage, 30).unwrap();
    writer.flush(&mut storage).unwrap();
    // A codec change applies to the next segment only.
    block_on(storage.seal()).unwrap();
    writer.codec = Compression::Lz4;
    block_on(storage.begin_segment(Compression::Lz4, LIMITS)).unwrap();
    writer.push_many(&mut storage, 30).unwrap();
    writer.flush(&mut storage).unwrap();
    let infos = block_on(storage.segments()).unwrap();
    assert_eq!(
        infos.iter().map(|i| i.codec).collect::<Vec<_>>(),
        [Compression::None, Compression::Lz4]
    );
    let records = all_records(&mut storage);
    assert_eq!(records.len(), 60);
    assert_contiguous(&records);
}

pub(crate) fn empty_and_oversized_appends_are_handled<H: Harness>() {
    let mut storage = H::create();
    let meta = ChunkMeta {
        records: 1,
        first_sequence: 1,
        last_sequence: 1,
        raw_len: 1,
    };
    assert_eq!(
        block_on(storage.append(b"x", meta)),
        Err(StorageError::NoOpenSegment)
    );
    assert_eq!(block_on(storage.seal()), Ok(()));

    // A segment without records is not kept.
    block_on(storage.begin_segment(Compression::Lz4, LIMITS)).unwrap();
    assert!(storage.open_capacity() > 0);
    block_on(storage.seal()).unwrap();
    assert!(block_on(storage.segments()).unwrap().is_empty());

    block_on(storage.begin_segment(Compression::None, LIMITS)).unwrap();
    let room = storage.open_capacity();
    let too_big = std::vec![b'a'; room + 1];
    assert_eq!(
        block_on(storage.append(&too_big, meta)),
        Err(StorageError::SegmentFull)
    );
    assert_eq!(storage.open_capacity(), room);

    // Limits the medium cannot meet.
    assert_eq!(
        block_on(storage.begin_segment(
            Compression::Lz4,
            Limits {
                segment_bytes: 4096,
                retention_bytes: 100
            }
        )),
        Err(StorageError::InvalidLimits)
    );
    assert_eq!(
        block_on(storage.begin_segment(
            Compression::Lz4,
            Limits {
                segment_bytes: H::CAPACITY * 2,
                retention_bytes: H::CAPACITY * 2
            }
        )),
        Err(StorageError::InvalidLimits)
    );
}

pub(crate) fn the_history_survives_a_restart<H: Harness>() {
    let mut storage = H::create();
    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 500).unwrap();
    writer.flush(&mut storage).unwrap();
    let before = all_records(&mut storage);
    let stats = block_on(storage.stats()).unwrap();

    let mut storage = H::reboot(storage);
    // The open segment was not sealed: recovery keeps what was written.
    let after = all_records(&mut storage);
    assert_eq!(after, before);
    let recovered = block_on(storage.stats()).unwrap();
    assert_eq!(recovered.records, stats.records);
    assert_eq!(recovered.newest_sequence, Some(500));
    assert!(
        block_on(storage.segments())
            .unwrap()
            .iter()
            .all(|info| !info.open)
    );

    // Writing goes on in a new segment, and the sequence goes on.
    let mut writer = Writer::new(501, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 50).unwrap();
    writer.flush(&mut storage).unwrap();
    let records = all_records(&mut storage);
    assert_contiguous(&records);
    assert_eq!(records.last().unwrap().sequence, 550);

    // A restart with sealed segments and a second restart agree.
    block_on(storage.seal()).unwrap();
    let mut again = H::reboot(storage);
    assert_eq!(all_records(&mut again), records);
}

/// Instantiates the contract for a backend.
macro_rules! contract_tests {
    ($harness:ty) => {
        contract_tests!(@each $harness,
            appended_records_read_back,
            reading_past_the_end_returns_nothing,
            segments_rotate_and_stay_contiguous,
            retention_drops_the_oldest_segments,
            a_pinned_segment_is_kept_and_blocks_eviction,
            pins_count,
            clear_removes_everything_and_the_storage_keeps_working,
            stats_report_space,
            every_segment_keeps_its_own_codec,
            empty_and_oversized_appends_are_handled,
            the_history_survives_a_restart
        );
    };
    (@each $harness:ty, $($name:ident),+) => {
        $(
            #[test]
            fn $name() {
                $crate::storage::contract::$name::<$harness>();
            }
        )+
    };
}
pub(crate) use contract_tests;
