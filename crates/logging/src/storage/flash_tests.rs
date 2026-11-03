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
        Self {
            data: std::vec![0xFF; FLASH_SECTORS * 4096],
            programs: std::vec![0; FLASH_SECTORS * 1024],
            budget: None,
        }
    }
}

impl ErrorType for Emu {
    type Error = Fault;
}

impl ReadNorFlash for Emu {
    const READ_SIZE: usize = 1;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Fault> {
        let at = offset as usize;
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

fn open(flash: Emu) -> Storage {
    let medium = FlashMedium::new(flash, BASE, SECTORS).unwrap();
    let mut storage = RingStorage::new(medium);
    block_on(storage.recover()).unwrap();
    storage
}

struct Nor;

impl Harness for Nor {
    type Storage = Storage;
    const CAPACITY: u32 = SECTORS * 4096;

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
    for codec in [Compression::Lz4, Compression::None] {
        for budget in (0..300).chain((300..70_000).step_by(311)) {
            let mut flash = Emu::new();
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
