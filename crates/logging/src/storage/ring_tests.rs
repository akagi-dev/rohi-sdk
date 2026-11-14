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
    // The last two sectors hold the format header.
    const CAPACITY: u32 = ((SECTORS - 2) * 4096) as u32;

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
    let start = storage.sector_of(infos[0].id).unwrap() * 4096;
    *storage.medium().raw(start + 9) ^= 0x01;
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
    // The ring, not the format header; every sector was erased once by the format.
    let counts = storage.medium().erase_counts()[..SECTORS - 2].to_vec();
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
fn the_segment_limit_bounds_the_reported_capacity() {
    let mut medium = std::boxed::Box::new(RamMedium::<64>::new());
    medium.erase(0, 64).unwrap();
    let mut storage = RingStorage::new(*medium);
    block_on(storage.recover()).unwrap();
    let limits = Limits {
        segment_bytes: 4096,
        retention_bytes: 64 * 4096,
    };
    let sector = 4096;
    let mut writer = Writer::new(1, Compression::None, limits);
    let mut seen_free = std::vec::Vec::new();
    for _ in 0..60 {
        writer.push_many(&mut storage, 100).unwrap();
        let stats = block_on(storage.stats()).unwrap();
        // The byte budget is 64 sectors, the segment limit allows 24.
        let bound = MAX_SEGMENTS as u32 * sector;
        if stats.segments > 0 {
            assert_eq!(stats.capacity_bytes, bound, "{stats:?}");
            assert!(stats.used_bytes <= stats.capacity_bytes, "{stats:?}");
            assert!(
                stats.free_bytes <= (MAX_SEGMENTS as u32 - stats.segments) * sector,
                "{stats:?}"
            );
            assert_eq!(
                stats.free_bytes,
                stats.capacity_bytes - stats.used_bytes,
                "{stats:?}"
            );
        }
        seen_free.push((stats.segments, stats.free_bytes));
    }
    // The limit is really reached before the byte budget, and then nothing is
    // left, although most of the medium is.
    let full = seen_free
        .iter()
        .find(|(segments, _)| *segments as usize == MAX_SEGMENTS)
        .expect("the segment limit is reached");
    assert_eq!(full.1, 0);
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

#[test]
fn a_sealed_segment_with_a_damaged_payload_is_dropped_and_reported() {
    for codec in [Compression::None, Compression::Lz4] {
        let mut storage = fresh();
        let mut writer = Writer::new(1, codec, LIMITS);
        let count = if codec == Compression::None { 300 } else { 900 };
        writer.push_many(&mut storage, count).unwrap();
        writer.flush(&mut storage).unwrap();
        block_on(storage.seal()).unwrap();
        let before = block_on(storage.segments()).unwrap();
        assert!(before.len() >= 3);

        // A bit of the payload of the oldest segment flips; its header, with
        // the old checksum, stays valid.
        let start = storage.sector_of(before[0].id).unwrap() * 4096;
        *storage.medium().raw(start + 100) ^= 0x04;
        let mut medium = storage.into_medium();
        medium.restore_power();
        let mut storage = RingStorage::new(medium);
        let recovery = block_on(storage.recover()).unwrap();
        assert_eq!(recovery.corrupted, 1, "{codec:?}");
        assert_eq!(recovery.dropped, 1, "{codec:?}");

        let after = block_on(storage.segments()).unwrap();
        assert_eq!(after.len(), before.len() - 1);
        assert!(after.iter().all(|info| info.id != before[0].id));
        let records = all_records(&mut storage);
        assert_contiguous(&records);
        assert_eq!(records.first().unwrap().sequence, after[0].first_sequence);
        assert_eq!(records.last().unwrap().sequence, count);

        // The damaged space is reusable and the decision is stable.
        let mut storage = restart(storage);
        assert_eq!(block_on(storage.segments()).unwrap(), after);
        let recovery = block_on(storage.recover()).unwrap();
        assert_eq!((recovery.corrupted, recovery.dropped), (0, 0));
    }
}

fn generation(storage: &mut RingStorage<RamMedium<SECTORS>>) -> u32 {
    block_on(storage.stats()).unwrap().generation
}

#[test]
fn the_generation_changes_with_the_history_and_survives_reboots() {
    let mut storage = fresh();
    assert_eq!(generation(&mut storage), 1);
    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 100).unwrap();
    writer.flush(&mut storage).unwrap();
    let mut storage = restart(storage);
    assert_eq!(generation(&mut storage), 1);

    block_on(storage.clear()).unwrap();
    assert_eq!(generation(&mut storage), 2);
    // Nothing is stored, yet a reboot knows it is not the first history.
    let mut storage = restart(storage);
    assert_eq!(generation(&mut storage), 2);
    let mut storage = restart(storage);
    assert_eq!(generation(&mut storage), 2);

    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(&mut storage, 100).unwrap();
    writer.flush(&mut storage).unwrap();
    assert_eq!(generation(&mut storage), 2);
    let mut storage = restart(storage);
    assert_eq!(generation(&mut storage), 2);

    block_on(storage.clear()).unwrap();
    block_on(storage.clear()).unwrap();
    let mut storage = restart(storage);
    assert_eq!(generation(&mut storage), 4);
}

#[test]
fn a_history_lost_to_damage_gets_a_new_generation() {
    let mut storage = fresh();
    let mut writer = Writer::new(1, Compression::None, LIMITS);
    writer.push_many(&mut storage, 20).unwrap();
    writer.flush(&mut storage).unwrap();
    let only = block_on(storage.segments()).unwrap();
    assert_eq!(only.len(), 1);
    // The open segment is torn down to nothing: its first block is damaged.
    let start = storage.sector_of(only[0].id).unwrap() * 4096;
    for offset in 0..64 {
        *storage.medium().raw(start + 20 + offset) = 0x01;
    }
    let mut storage = restart(storage);
    assert!(block_on(storage.segments()).unwrap().is_empty());
    assert_eq!(generation(&mut storage), 2);
    let mut storage = restart(storage);
    assert_eq!(generation(&mut storage), 2);
}

const RING: usize = SECTORS - 2;

fn write_history(storage: &mut RingStorage<RamMedium<SECTORS>>, records: u64) {
    let mut writer = Writer::new(1, Compression::Lz4, LIMITS);
    writer.push_many(storage, records).unwrap();
    writer.flush(storage).unwrap();
}

// A medium that holds a healthy history but no format header: what a
// partition that was moved over old data looks like.
fn medium_with_old_history() -> RamMedium<SECTORS> {
    let mut storage = fresh();
    write_history(&mut storage, 600);
    assert!(block_on(storage.segments()).unwrap().len() > 1);
    let mut medium = storage.into_medium();
    for slot in RING..SECTORS {
        medium.erase(slot as u32, 1).unwrap();
    }
    medium
}

fn assert_empty_and_usable(mut storage: RingStorage<RamMedium<SECTORS>>) {
    assert!(all_records(&mut storage).is_empty());
    assert!(block_on(storage.segments()).unwrap().is_empty());
    write_history(&mut storage, 50);
    assert_contiguous(&all_records(&mut storage));
    let mut storage = restart(storage);
    assert_eq!(all_records(&mut storage).len(), 50);
}

#[test]
fn a_new_medium_is_formatted_once_and_a_healthy_history_is_kept() {
    let mut storage = fresh();
    let counts = storage.medium().erase_counts()[..RING].to_vec();
    assert!(counts.iter().all(|count| *count == 1), "{counts:?}");
    write_history(&mut storage, 100);
    let records = all_records(&mut storage);
    let erased = *storage.medium().erase_counts();

    for _ in 0..3 {
        storage = restart(storage);
        assert_eq!(*storage.medium().erase_counts(), erased, "reformatted");
        assert_eq!(all_records(&mut storage), records);
        assert_eq!(generation(&mut storage), 1);
    }
}

#[test]
fn a_medium_without_a_header_is_formatted_whatever_it_holds() {
    let storage = RingStorage::new(medium_with_old_history());
    let mut storage = storage;
    let recovery = block_on(storage.recover()).unwrap();
    assert_eq!((recovery.segments, recovery.newest_sequence), (0, None));
    assert_empty_and_usable(storage);
}

#[test]
fn a_format_cut_anywhere_is_repeated_and_old_records_never_return() {
    // Two header slots and the ring are erased, then the header is written.
    let erases = RING + 3;
    for erase_budget in 0..=erases {
        for write_budget in [None, Some(0), Some(5), Some(19)] {
            let mut medium = medium_with_old_history();
            medium.fail_erase_after(erase_budget);
            if let Some(bytes) = write_budget {
                medium.fail_after(bytes);
            }
            let mut storage = RingStorage::new(medium);
            let cut = erase_budget < erases || write_budget.is_some();
            assert_eq!(
                block_on(storage.recover()).is_err(),
                cut,
                "{erase_budget} {write_budget:?}"
            );
            // The next start finishes the format, and again after that.
            let storage = restart(storage);
            let storage = restart(storage);
            assert_empty_and_usable(storage);
        }
    }
}

#[test]
fn a_format_yields_between_sector_erases() {
    let mut storage = RingStorage::new(RamMedium::<SECTORS>::new());
    let mut recover = core::pin::pin!(storage.recover());
    let mut yields = 0;
    while embassy_futures::poll_once(recover.as_mut()).is_pending() {
        yields += 1;
    }
    assert_eq!(yields, RING);
}

#[test]
fn clear_erases_every_sector_and_starts_a_new_generation() {
    let mut storage = fresh();
    write_history(&mut storage, 600);
    let before = storage.medium().erase_counts()[..RING].to_vec();
    block_on(storage.clear()).unwrap();
    let after = storage.medium().erase_counts()[..RING].to_vec();
    for (before, after) in before.iter().zip(&after) {
        assert_eq!(*after, before + 1, "{before:?} {after:?}");
    }
    for offset in 0..RING as u32 * 4096 {
        assert_eq!(*storage.medium().raw(offset), 0xFF, "offset {offset}");
    }
    assert_eq!(generation(&mut storage), 2);
    let storage = restart(storage);
    let mut storage = {
        let mut storage = storage;
        assert_eq!(generation(&mut storage), 2);
        storage
    };
    write_history(&mut storage, 50);
    let mut storage = restart(storage);
    assert_eq!(all_records(&mut storage).len(), 50);
    assert_eq!(generation(&mut storage), 2);
}

#[test]
fn a_clear_cut_anywhere_keeps_the_history_or_removes_all_of_it() {
    // New header slot, old header slot, every sector of the ring.
    let erases = RING + 2;
    for erase_budget in 0..=erases {
        for write_budget in [None, Some(0), Some(10), Some(19)] {
            let mut storage = fresh();
            write_history(&mut storage, 600);
            let before = all_records(&mut storage);
            storage.medium().fail_erase_after(erase_budget);
            if let Some(bytes) = write_budget {
                storage.medium().fail_after(bytes);
            }
            let result = block_on(storage.clear());
            let cut = erase_budget < erases || write_budget.is_some();
            assert_eq!(result.is_err(), cut, "{erase_budget} {write_budget:?}");

            let mut storage = restart(storage);
            let records = all_records(&mut storage);
            let generation_now = generation(&mut storage);
            if records.is_empty() {
                assert_eq!(generation_now, 2, "{erase_budget} {write_budget:?}");
            } else {
                // Cut before the new generation was committed: nothing changed.
                assert_eq!(records, before, "{erase_budget} {write_budget:?}");
                assert_eq!(generation_now, 1);
                assert!(result.is_err());
            }
            // Stale segments are neither read nor brought back by a restart.
            let mut storage = restart(storage);
            assert_eq!(all_records(&mut storage), records);
            assert_eq!(generation(&mut storage), generation_now);
            if records.is_empty() {
                assert_empty_and_usable(storage);
            }
        }
    }
}

#[test]
fn a_pinned_download_blocks_a_clear_and_keeps_the_medium_intact() {
    let mut storage = fresh();
    write_history(&mut storage, 600);
    let first = block_on(storage.segments()).unwrap()[0].id;
    block_on(storage.pin(first)).unwrap();
    let erased = *storage.medium().erase_counts();
    assert_eq!(block_on(storage.clear()), Err(StorageError::Busy));
    assert_eq!(*storage.medium().erase_counts(), erased);
    assert_eq!(generation(&mut storage), 1);
    storage.unpin(first);
    block_on(storage.clear()).unwrap();
}

#[test]
fn a_medium_that_is_too_small_is_refused() {
    let mut storage = RingStorage::new(RamMedium::<3>::new());
    assert_eq!(
        block_on(storage.recover()),
        Err(StorageError::InvalidLimits)
    );
}

#[test]
fn the_esp32c3_partition_retains_fifteen_whole_segments() {
    // 64 sectors (256 KiB): two format header sectors and a ring of 62, cut
    // into 16 KiB segments of four sectors.
    let mut medium = std::boxed::Box::new(RamMedium::<64>::new());
    medium.erase(0, 64).unwrap();
    let mut storage = RingStorage::new(*medium);
    block_on(storage.recover()).unwrap();
    let limits = Limits {
        segment_bytes: 16 * 1024,
        retention_bytes: 256 * 1024,
    };
    let mut writer = Writer::new(1, Compression::None, limits);
    let mut most_sealed = 0;
    for _ in 0..400 {
        writer.push_many(&mut storage, 100).unwrap();
        let stats = block_on(storage.stats()).unwrap();
        assert!(stats.used_bytes <= 62 * 4096, "{stats:?}");
        assert_eq!(stats.capacity_bytes, 62 * 4096, "{stats:?}");
        most_sealed = most_sealed.max(stats.segments);
    }
    assert!(most_sealed >= 15, "{most_sealed}");
    assert_contiguous(&all_records(&mut storage));
}
