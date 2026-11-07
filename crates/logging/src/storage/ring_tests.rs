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

use embassy_futures::block_on;

use super::contract::*;
use super::*;

const SECTORS: usize = 16;

struct Ram;

impl contract::Harness for Ram {
    type Storage = RingStorage<RamMedium<SECTORS>>;
    const CAPACITY: u32 = (SECTORS * 4096) as u32;

    fn create() -> Self::Storage {
        let mut storage = RingStorage::new(RamMedium::new());
        block_on(storage.recover()).unwrap();
        storage
    }

    fn reboot(storage: Self::Storage) -> Self::Storage {
        let medium = storage.into_medium();
        let mut storage = RingStorage::new(medium);
        block_on(storage.recover()).unwrap();
        storage
    }
}

mod ram {
    use super::*;
    contract_tests!(Ram);
}

fn fresh() -> RingStorage<RamMedium<SECTORS>> {
    Ram::create()
}

fn restart(storage: RingStorage<RamMedium<SECTORS>>) -> RingStorage<RamMedium<SECTORS>> {
    let mut medium = storage.into_medium();
    medium.restore_power();
    let mut storage = RingStorage::new(medium);
    block_on(storage.recover()).unwrap();
    storage
}

/// Writes until the medium gives out; returns the last sequence an append
/// accepted.
fn write_until_power_loss(
    storage: &mut RingStorage<RamMedium<SECTORS>>,
    codec: Compression,
    records: u64,
) -> u64 {
    let mut writer = Writer::new(1, codec, LIMITS);
    let _ = writer.push_many(storage, records);
    let _ = writer.flush(storage);
    writer.durable
}

#[test]
fn a_power_loss_at_any_write_loses_at_most_the_open_batch() {
    let budgets = (0..400).chain((400..80_000).step_by(373));
    for codec in [Compression::Lz4, Compression::None] {
        for budget in budgets.clone() {
            let mut storage = fresh();
            storage.medium().fail_after(budget);
            let durable = write_until_power_loss(&mut storage, codec, 1_500);

            let mut storage = restart(storage);
            let records = all_records(&mut storage);
            assert_contiguous(&records);
            // Everything an append acknowledged is still there.
            if durable > 0 && !records.is_empty() {
                assert!(
                    records.last().unwrap().sequence >= durable,
                    "{codec:?} budget {budget}: lost acknowledged records"
                );
            }
            let stats = block_on(storage.stats()).unwrap();
            assert_eq!(stats.records, records.len() as u64);
            assert!(stats.used_bytes <= stats.capacity_bytes);

            // The storage is usable again.
            let next = records.last().map_or(1, |r| r.sequence + 1);
            let mut writer = Writer::new(next, codec, LIMITS);
            writer.push_many(&mut storage, 100).unwrap();
            writer.flush(&mut storage).unwrap();
            assert_contiguous(&all_records(&mut storage));
        }
    }
}

#[test]
fn a_torn_seal_is_recovered_by_reading_the_segment() {
    let mut storage = fresh();
    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 80).unwrap();
    writer.flush(&mut storage).unwrap();
    let expected = all_records(&mut storage);
    // The header write is 44 bytes: cut it half way.
    storage.medium().fail_after(20);
    assert!(block_on(storage.seal()).is_err());
    let mut storage = restart(storage);
    assert_eq!(all_records(&mut storage), expected);
    // And again: the decision is the same on every boot.
    let mut storage = restart(storage);
    assert_eq!(all_records(&mut storage), expected);
}

#[test]
fn a_damaged_header_falls_back_to_the_content() {
    let mut storage = fresh();
    let mut writer = Writer::new(1, Compression::None, LIMITS);
    writer.push_many(&mut storage, 40).unwrap();
    writer.flush(&mut storage).unwrap();
    block_on(storage.seal()).unwrap();
    let expected = all_records(&mut storage);
    // Flip a bit inside the header slot, the last 48 bytes of the sector.
    *storage.medium().raw(4096 - 20) = 0;
    let mut storage = restart(storage);
    assert_eq!(all_records(&mut storage), expected);
}

#[test]
fn a_damaged_start_record_drops_that_segment_only() {
    let mut storage = fresh();
    let mut writer = Writer::new(1, Compression::None, LIMITS);
    writer.push_many(&mut storage, 300).unwrap();
    writer.flush(&mut storage).unwrap();
    let infos = block_on(storage.segments()).unwrap();
    assert!(infos.len() >= 3);
    // The first segment starts at sector 0.
    *storage.medium().raw(9) ^= 0x01;
    let mut storage = restart(storage);
    let left = block_on(storage.segments()).unwrap();
    assert_eq!(left.len(), infos.len() - 1);
    let records = all_records(&mut storage);
    assert_contiguous(&records);
    assert_eq!(records.last().unwrap().sequence, 300);
}

#[test]
fn foreign_content_does_not_stop_the_log() {
    let mut medium = RamMedium::<SECTORS>::new();
    medium.erase(0, SECTORS as u32).unwrap();
    // Not erased and not ours: a previous life of the partition.
    let junk: std::vec::Vec<u8> = (0..4096u32)
        .map(|n| (n.wrapping_mul(2_654_435_761) >> 24) as u8 | 1)
        .collect();
    for sector in 0..SECTORS as u32 {
        medium.erase(sector, 1).unwrap();
        medium.write(sector * 4096, &junk).unwrap();
    }
    let mut storage = RingStorage::new(medium);
    let recovery = block_on(storage.recover()).unwrap();
    assert_eq!((recovery.segments, recovery.newest_sequence), (0, None));

    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 120).unwrap();
    writer.flush(&mut storage).unwrap();
    let records = all_records(&mut storage);
    assert_eq!(records.len(), 120);
    let mut storage = restart(storage);
    assert_eq!(all_records(&mut storage), records);
}

#[test]
fn segments_of_different_sizes_share_the_ring() {
    let mut storage = fresh();
    let mut next = 1;
    for (index, segment_bytes) in [4096, 8192, 4096, 12288, 4096, 8192, 4096, 4096]
        .into_iter()
        .cycle()
        .take(30)
        .enumerate()
    {
        let limits = Limits {
            segment_bytes,
            retention_bytes: 4096 * 12,
        };
        let mut writer = Writer::new(next, Compression::Lz4, limits);
        block_on(storage.begin_segment(Compression::Lz4, limits)).unwrap();
        writer
            .push_many(&mut storage, 100 + index as u64 % 7 * 30)
            .unwrap();
        writer.flush(&mut storage).unwrap();
        next = writer.next;
        let stats = block_on(storage.stats()).unwrap();
        assert!(stats.used_bytes <= 4096 * 12, "{stats:?}");
        let records = all_records(&mut storage);
        assert_contiguous(&records);
        assert_eq!(records.last().unwrap().sequence, next - 1);
    }
    let mut storage = restart(storage);
    assert_contiguous(&all_records(&mut storage));
}

#[test]
fn flash_wear_is_spread_over_the_whole_region() {
    let mut storage = fresh();
    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    for _ in 0..60 {
        writer.push_many(&mut storage, 100).unwrap();
    }
    let counts = *storage.medium().erase_counts();
    let (min, max) = (counts.iter().min().unwrap(), counts.iter().max().unwrap());
    assert!(*min > 0, "{counts:?}");
    assert!(max - min <= 2, "{counts:?}");
}

#[test]
fn at_most_the_segment_limit_is_retained() {
    let mut medium = std::boxed::Box::new(RamMedium::<64>::new());
    medium.erase(0, 64).unwrap();
    let mut storage = RingStorage::new(*medium);
    block_on(storage.recover()).unwrap();
    let limits = Limits {
        segment_bytes: 4096,
        retention_bytes: 64 * 4096,
    };
    let mut writer = Writer::new(1, Compression::None, limits);
    writer.push_many(&mut storage, 6_000).unwrap();
    let stats = block_on(storage.stats()).unwrap();
    assert!(stats.segments as usize <= MAX_SEGMENTS);
    assert_contiguous(&all_records(&mut storage));
}

#[test]
fn a_recovery_keeps_the_pins_of_downloads() {
    let mut storage = fresh();
    let mut writer = Writer::new(1, Compression::None, LIMITS);
    writer.push_many(&mut storage, 300).unwrap();
    writer.flush(&mut storage).unwrap();
    let oldest = block_on(storage.segments()).unwrap()[0];
    block_on(storage.pin(oldest.id)).unwrap();
    // The service reads the medium again after a failed write.
    block_on(storage.recover()).unwrap();
    let after = block_on(storage.segments()).unwrap();
    assert!(after.iter().any(|info| info.id == oldest.id && info.pinned));
    assert_eq!(block_on(storage.clear()), Err(StorageError::Busy));
}
