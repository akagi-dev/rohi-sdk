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
use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

use super::contract::*;
use super::*;

const SECTORS: u32 = 16;
// The partition does not start at the beginning of the flash.
const BASE: u32 = 3 * 4096;
const FLASH_SECTORS: usize = SECTORS as usize + 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    Misaligned,
    BitsSet,
    TooManyPrograms,
    OutOfBounds,
    PowerLoss,
}

impl NorFlashError for Fault {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::Misaligned => NorFlashErrorKind::NotAligned,
            _ => NorFlashErrorKind::Other,
        }
    }
}

/// A NOR flash with the rules of the ESP32 flash driver: words of 4 bytes
/// written whole, bits only cleared, a word reprogrammed at most twice.
#[derive(Clone)]
struct Emu {
    data: std::vec::Vec<u8>,
    programs: std::vec::Vec<u8>,
    budget: Option<usize>,
}

impl Emu {
    fn new() -> Self {
        Self::with_sectors(FLASH_SECTORS)
    }

    fn with_sectors(sectors: usize) -> Self {
        Self {
            data: std::vec![0xFF; sectors * 4096],
            programs: std::vec![0; sectors * 1024],
            budget: None,
        }
    }
}

impl ErrorType for Emu {
    type Error = Fault;
}

impl ReadNorFlash for Emu {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Fault> {
        let at = offset as usize;
        if !at.is_multiple_of(4) || !bytes.len().is_multiple_of(4) {
            return Err(Fault::Misaligned);
        }
        bytes.copy_from_slice(
            self.data
                .get(at..at + bytes.len())
                .ok_or(Fault::OutOfBounds)?,
        );
        Ok(())
    }

    fn capacity(&self) -> usize {
        self.data.len()
    }
}

impl NorFlash for Emu {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Fault> {
        let (from, to) = (from as usize, to as usize);
        if !from.is_multiple_of(4096) || !to.is_multiple_of(4096) {
            return Err(Fault::Misaligned);
        }
        self.data
            .get_mut(from..to)
            .ok_or(Fault::OutOfBounds)?
            .fill(0xFF);
        self.programs[from / 4..to / 4].fill(0);
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Fault> {
        let at = offset as usize;
        if !at.is_multiple_of(4) || !bytes.len().is_multiple_of(4) {
            return Err(Fault::Misaligned);
        }
        if at + bytes.len() > self.data.len() {
            return Err(Fault::OutOfBounds);
        }
        for (index, byte) in bytes.iter().enumerate() {
            if let Some(budget) = &mut self.budget {
                if *budget == 0 {
                    return Err(Fault::PowerLoss);
                }
                *budget -= 1;
            }
            let cell = &mut self.data[at + index];
            if *cell & *byte != *byte {
                return Err(Fault::BitsSet);
            }
            *cell = *byte;
        }
        for word in at / 4..(at + bytes.len()) / 4 {
            self.programs[word] += 1;
            if self.programs[word] > 2 {
                return Err(Fault::TooManyPrograms);
            }
        }
        Ok(())
    }
}

impl MultiwriteNorFlash for Emu {}

type Storage = FlashLogStorage<Emu>;

#[test]
fn erase_rejects_sector_arithmetic_overflow() {
    let mut medium = FlashMedium::new(Emu::new(), BASE, SECTORS).unwrap();
    assert_eq!(
        medium.erase(u32::MAX, 1),
        Err(FlashMediumError::OutOfBounds)
    );
    assert_eq!(
        medium.erase(0, u32::MAX),
        Err(FlashMediumError::OutOfBounds)
    );
    assert_eq!(medium.erase(SECTORS, 1), Err(FlashMediumError::OutOfBounds));
}

fn open(flash: Emu) -> Storage {
    let medium = FlashMedium::new(flash, BASE, SECTORS).unwrap();
    let mut storage = RingStorage::new(medium);
    block_on(storage.recover()).unwrap();
    storage
}

struct Nor;

impl Harness for Nor {
    type Storage = Storage;
    // The last two sectors hold the format header.
    const CAPACITY: u32 = (SECTORS - 2) * 4096;

    fn create() -> Storage {
        open(Emu::new())
    }

    fn reboot(storage: Storage) -> Storage {
        let mut flash = storage.into_medium().into_inner();
        flash.budget = None;
        open(flash)
    }
}

mod nor {
    use super::*;
    contract_tests!(Nor);
}

#[test]
fn the_region_must_fit_the_flash() {
    assert!(FlashMedium::new(Emu::new(), 0, FLASH_SECTORS as u32).is_ok());
    assert_eq!(
        FlashMedium::new(Emu::new(), 0, FLASH_SECTORS as u32 + 1).err(),
        Some(FlashMediumError::OutOfBounds)
    );
    assert!(FlashMedium::new(Emu::new(), 100, 1).is_err());
    assert!(FlashMedium::new(Emu::new(), 0, 0).is_err());
}

#[test]
fn writes_of_any_alignment_end_up_as_written() {
    let mut medium = FlashMedium::new(Emu::new(), BASE, SECTORS).unwrap();
    medium.erase(0, 2).unwrap();
    let mut expected = std::vec![0xFFu8; 8192];
    let mut at = 0usize;
    for len in [4usize, 5, 7, 8, 9, 13, 64, 4, 100, 5, 6, 11] {
        let chunk: std::vec::Vec<u8> = (0..len).map(|n| (at + n) as u8 | 0x80).collect();
        medium.write(at as u32, &chunk).unwrap();
        expected[at..at + len].copy_from_slice(&chunk);
        at += len;
    }
    let mut back = std::vec![0u8; 8192];
    medium.read(0, &mut back).unwrap();
    assert_eq!(back, expected);
    // Bytes outside the region are never touched.
    let mut flash = medium.into_inner();
    let mut before = [0u8; 8];
    flash.read(BASE - 8, &mut before).unwrap();
    assert_eq!(before, [0xFF; 8]);
    assert_eq!(
        FlashMedium::new(flash, BASE, SECTORS)
            .unwrap()
            .write(SECTORS * 4096 - 2, &[1, 2, 3]),
        Err(FlashMediumError::OutOfBounds)
    );
}

#[test]
fn a_power_loss_at_any_flash_write_loses_at_most_the_open_batch() {
    for &codec in crate::TEST_CODECS {
        for budget in (0..300).chain((300..70_000).step_by(311)) {
            // The medium is formatted before the power fails.
            let mut flash = open(Emu::new()).into_medium().into_inner();
            flash.budget = Some(budget);
            let mut storage = open(flash);
            let mut writer = Writer::new(1, codec, LIMITS);
            let _ = writer.push_many(&mut storage, 1_200);
            let _ = writer.flush(&mut storage);
            let durable = writer.durable;

            let mut storage = Nor::reboot(storage);
            let records = all_records(&mut storage);
            assert_contiguous(&records);
            if durable > 0 && !records.is_empty() {
                assert!(
                    records.last().unwrap().sequence >= durable,
                    "{codec:?} budget {budget}"
                );
            }
            let next = records.last().map_or(1, |r| r.sequence + 1);
            let mut writer = Writer::new(next, codec, LIMITS);
            writer.push_many(&mut storage, 100).unwrap();
            writer.flush(&mut storage).unwrap();
            assert_contiguous(&all_records(&mut storage));
        }
    }
}

#[test]
fn format_and_clear_stay_inside_the_region() {
    let region = BASE as usize..BASE as usize + SECTORS as usize * 4096;
    let mut flash = Emu::new();
    // The neighbours hold data of their own, also where the flash is not erased.
    for (at, byte) in flash.data.iter_mut().enumerate() {
        if !region.contains(&at) {
            *byte = 0x5A;
        }
    }
    // The region holds old, foreign bytes.
    flash.data[region.clone()].fill(0x00);
    let outside = flash.data.clone();

    let mut storage = open(flash);
    let mut writer = Writer::new(1, crate::DEFAULT_COMPRESSION, LIMITS);
    writer.push_many(&mut storage, 600).unwrap();
    writer.flush(&mut storage).unwrap();
    block_on(storage.clear()).unwrap();
    writer.push_many(&mut storage, 100).unwrap();
    writer.flush(&mut storage).unwrap();

    let flash = storage.into_medium().into_inner();
    assert_eq!(flash.data[..region.start], outside[..region.start]);
    assert_eq!(flash.data[region.end..], outside[region.end..]);
}

#[test]
fn reads_of_any_alignment_return_what_was_written() {
    let mut medium = FlashMedium::new(Emu::new(), BASE, SECTORS).unwrap();
    medium.erase(0, 1).unwrap();
    let data: std::vec::Vec<u8> = (0..4096usize).map(|n| n as u8 | 0x80).collect();
    medium.write(0, &data).unwrap();
    for at in 0..9usize {
        for len in (0..40usize).chain([4095 - at, 4096 - at]) {
            let mut back = std::vec![0u8; len];
            medium.read(at as u32, &mut back).unwrap();
            assert_eq!(back, data[at..at + len], "read of {len} at {at}");
        }
    }
}

// The ESP32-C3 upgrade: 4 MB of flash. The former `rohi-log` of 512 KiB at
// 0x380000 is gone from the table (what it held is now no partition at all); the
// new one is 128 KiB at 0x20000, where the application used to start, and the
// application image of the new table follows it from 0x40000. The new region
// holds old application code when the device is first started.
const C3_SECTORS: usize = 1024;
const C3_FIRST: u32 = 0x2_0000 / 4096;
const C3_SECTORS_OF_LOG: u32 = 32;
const C3_LOG_END: usize = (C3_FIRST + C3_SECTORS_OF_LOG) as usize * 4096;
const C3_OLD_FIRST: u32 = 0x38_0000 / 4096;
const C3_OLD_SECTORS: u32 = 128;

fn c3_flash_after_the_table_change() -> Emu {
    let mut flash = Emu::with_sectors(C3_SECTORS);
    // The application, the configuration and everything else around the log.
    flash.data[..C3_OLD_FIRST as usize * 4096].fill(0x5A);
    // The former log, formatted and written; it lies inside the new application area.
    let medium = FlashMedium::new(flash, C3_OLD_FIRST * 4096, C3_OLD_SECTORS).unwrap();
    let mut storage = RingStorage::new(medium);
    block_on(storage.recover()).unwrap();
    let mut writer = Writer::new(1, crate::DEFAULT_COMPRESSION, LIMITS);
    writer.push_many(&mut storage, 300).unwrap();
    writer.flush(&mut storage).unwrap();
    assert!(!all_records(&mut storage).is_empty());
    let mut flash = storage.into_medium().into_inner();
    flash.budget = None;
    flash
}

fn open_c3(flash: Emu) -> (Storage, Recovery) {
    let medium = FlashMedium::new(flash, C3_FIRST * 4096, C3_SECTORS_OF_LOG).unwrap();
    let mut storage = RingStorage::new(medium);
    let recovery = block_on(storage.recover()).unwrap();
    (storage, recovery)
}

// The flash below and above the region of the log.
fn outside(flash: &Emu) -> (std::vec::Vec<u8>, std::vec::Vec<u8>) {
    (
        flash.data[..C3_FIRST as usize * 4096].to_vec(),
        flash.data[C3_LOG_END..].to_vec(),
    )
}

#[test]
fn the_moved_esp32c3_region_is_formatted_on_the_first_start_and_kept_afterwards() {
    let flash = c3_flash_after_the_table_change();
    let (below, above) = outside(&flash);
    // Old application code is no header: the region is not read as a log.
    let (mut storage, recovery) = open_c3(flash);
    assert!(recovery.formatted);
    assert_eq!((recovery.segments, recovery.newest_sequence), (0, None));
    assert!(all_records(&mut storage).is_empty());
    assert_eq!(block_on(storage.stats()).unwrap().capacity_bytes, 30 * 4096);
    let mut writer = Writer::new(1, crate::DEFAULT_COMPRESSION, LIMITS);
    writer.push_many(&mut storage, 100).unwrap();
    writer.flush(&mut storage).unwrap();

    // The next start finds the header and keeps the history.
    let mut flash = storage.into_medium().into_inner();
    flash.budget = None;
    let (mut storage, recovery) = open_c3(flash);
    assert!(!recovery.formatted);
    assert_contiguous(&all_records(&mut storage));
    assert_eq!(all_records(&mut storage).len(), 100);

    // Nothing around the region was touched: not the partitions before it, not
    // the application after it (the former log is inside it and stays as it is).
    let flash = storage.into_medium().into_inner();
    assert_eq!((outside(&flash)), (below, above));
}

#[test]
fn a_format_of_the_moved_region_cut_by_a_power_loss_is_repeated_and_never_touches_the_neighbours() {
    for budget in [0usize, 1, 3, 4, 8, 12, 16, 19] {
        let mut flash = c3_flash_after_the_table_change();
        let neighbours = outside(&flash);
        // The header is the last write of a format: cut it.
        flash.budget = Some(budget);
        let medium = FlashMedium::new(flash, C3_FIRST * 4096, C3_SECTORS_OF_LOG).unwrap();
        let mut storage = RingStorage::new(medium);
        assert!(block_on(storage.recover()).is_err(), "budget {budget}");

        let mut flash = storage.into_medium().into_inner();
        flash.budget = None;
        let (mut storage, recovery) = open_c3(flash);
        assert!(recovery.formatted, "budget {budget}: no valid header yet");
        assert!(all_records(&mut storage).is_empty());
        let mut writer = Writer::new(1, Compression::None, LIMITS);
        writer.push_many(&mut storage, 50).unwrap();
        writer.flush(&mut storage).unwrap();
        let mut flash = storage.into_medium().into_inner();
        flash.budget = None;
        let (mut storage, recovery) = open_c3(flash);
        assert!(!recovery.formatted);
        assert_eq!(all_records(&mut storage).len(), 50);
        let flash = storage.into_medium().into_inner();
        assert_eq!(outside(&flash), neighbours);
    }
}

#[test]
fn the_esp32c3_region_clears_and_wraps_with_the_default_segments_inside_its_bounds() {
    let flash = c3_flash_after_the_table_change();
    let neighbours = outside(&flash);
    let (mut storage, _) = open_c3(flash);
    let limits = Limits {
        segment_bytes: 8 * 1024,
        retention_bytes: 128 * 1024,
    };
    let mut writer = Writer::new(1, Compression::None, limits);
    let mut most = 0;
    for _ in 0..300 {
        writer.push_many(&mut storage, 100).unwrap();
        let stats = block_on(storage.stats()).unwrap();
        assert!(stats.used_bytes <= 30 * 4096, "{stats:?}");
        most = most.max(stats.segments);
    }
    assert!(most >= 14 && most as usize <= MAX_SEGMENTS, "{most}");
    assert_contiguous(&all_records(&mut storage));
    block_on(storage.clear()).unwrap();
    assert!(all_records(&mut storage).is_empty());
    let (mut storage, recovery) = open_c3(storage.into_medium().into_inner());
    assert!(!recovery.formatted);
    assert!(all_records(&mut storage).is_empty());
    assert_eq!(outside(&storage.into_medium().into_inner()), neighbours);
}
