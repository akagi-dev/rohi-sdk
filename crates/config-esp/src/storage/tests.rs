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
///////////////////////////////////////////////////////////////////////////////

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};
use minicbor::{CborLen, Decode, Encode};
use rohi_config::{ConfigSection, ConfigStore};

use super::*;

const FLASH_SIZE: usize = 64 * 1024;

#[derive(Debug)]
struct MockError;

impl NorFlashError for MockError {
    fn kind(&self) -> NorFlashErrorKind {
        NorFlashErrorKind::Other
    }
}

/// NOR flash with 4-byte word reads, like `esp-storage` without `bytewise-read`.
struct MockFlash {
    data: [u8; FLASH_SIZE],
}

impl MockFlash {
    fn new() -> Self {
        Self {
            data: [0xFF; FLASH_SIZE],
        }
    }
}

impl ErrorType for MockFlash {
    type Error = MockError;
}

impl ReadNorFlash for MockFlash {
    const READ_SIZE: usize = 4;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), MockError> {
        let offset = offset as usize;
        if !offset.is_multiple_of(4)
            || !bytes.len().is_multiple_of(4)
            || offset + bytes.len() > FLASH_SIZE
        {
            return Err(MockError);
        }
        bytes.copy_from_slice(&self.data[offset..offset + bytes.len()]);
        Ok(())
    }

    fn capacity(&self) -> usize {
        FLASH_SIZE
    }
}

impl MultiwriteNorFlash for MockFlash {}

impl NorFlash for MockFlash {
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = 4096;

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), MockError> {
        let offset = offset as usize;
        if !offset.is_multiple_of(4)
            || !bytes.len().is_multiple_of(4)
            || offset + bytes.len() > FLASH_SIZE
        {
            return Err(MockError);
        }
        for (dst, src) in self.data[offset..].iter_mut().zip(bytes) {
            *dst &= *src;
        }
        Ok(())
    }

    fn erase(&mut self, from: u32, to: u32) -> Result<(), MockError> {
        let (from, to) = (from as usize, to as usize);
        if from % 4096 != 0 || to % 4096 != 0 || from > to || to > FLASH_SIZE {
            return Err(MockError);
        }
        self.data[from..to].fill(0xFF);
        Ok(())
    }
}

const REGION: FlashRegion = FlashRegion::new(0x4000, 0x4000);

fn storage() -> EspConfigStorage<MockFlash> {
    let mut flash = MockFlash::new();
    flash.data.fill(0xA5);
    flash.erase(0x4000, 0x8000).unwrap();
    EspConfigStorage::new(flash, REGION).unwrap()
}

#[test]
fn region_validation() {
    let new = |region| EspConfigStorage::new(MockFlash::new(), region).map(|_| ());
    assert_eq!(
        new(FlashRegion::new(0x4000, 0)),
        Err(EspStorageError::InvalidRegion)
    );
    assert_eq!(
        new(FlashRegion::new(0x4001, 0x1000)),
        Err(EspStorageError::InvalidRegion)
    );
    assert_eq!(
        new(FlashRegion::new(0x4000, 0x1001)),
        Err(EspStorageError::InvalidRegion)
    );
    assert_eq!(
        new(FlashRegion::new(0xF000, 0x2000)),
        Err(EspStorageError::InvalidRegion)
    );
    assert_eq!(
        new(FlashRegion::new(usize::MAX - 0xFFF, 0x2000)),
        Err(EspStorageError::InvalidRegion)
    );
    assert_eq!(new(FlashRegion::new(0xF000, 0x1000)), Ok(()));
}

#[test]
fn accesses_cannot_escape_the_region() {
    let mut storage = storage();
    assert_eq!(storage.capacity(), 0x4000);
    let mut buf = [0u8; 8];
    assert_eq!(
        storage.read(0x4000 - 4, &mut buf),
        Err(EspStorageError::OutOfBounds)
    );
    assert_eq!(
        storage.read(usize::MAX, &mut buf),
        Err(EspStorageError::OutOfBounds)
    );
    assert_eq!(
        storage.write(0x4000 - 4, &[0; 8]),
        Err(EspStorageError::OutOfBounds)
    );
    assert_eq!(
        storage.erase(0x3000, 0x2000),
        Err(EspStorageError::OutOfBounds)
    );
    assert_eq!(
        storage.erase(0x4000, 0x1000),
        Err(EspStorageError::OutOfBounds)
    );
    let flash = storage.into_inner();
    assert!(flash.data[..0x4000].iter().all(|b| *b == 0xA5));
    assert!(flash.data[0x8000..0x10000].iter().all(|b| *b == 0xA5));
}

#[test]
fn alignment_is_enforced_for_write_and_erase() {
    let mut storage = storage();
    assert_eq!(
        storage.write(2, &[0; 4]),
        Err(EspStorageError::InvalidAlignment)
    );
    assert_eq!(
        storage.write(0, &[0; 3]),
        Err(EspStorageError::InvalidAlignment)
    );
    assert_eq!(
        storage.erase(4, 4096),
        Err(EspStorageError::InvalidAlignment)
    );
    assert_eq!(
        storage.erase(0, 100),
        Err(EspStorageError::InvalidAlignment)
    );
}

#[test]
fn unaligned_reads_use_the_bounce_buffer() {
    let mut storage = storage();
    let data: [u8; 16] = core::array::from_fn(|i| i as u8);
    storage.write(8, &data).unwrap();
    for start in 7..20 {
        for len in 0..12 {
            let mut buf = [0u8; 12];
            storage.read(start, &mut buf[..len]).unwrap();
            for (i, byte) in buf[..len].iter().enumerate() {
                let position = start + i;
                let expected = if (8..24).contains(&position) {
                    (position - 8) as u8
                } else {
                    0xFF
                };
                assert_eq!(*byte, expected, "start {start} len {len} i {i}");
            }
        }
    }
    // Long unaligned read crossing several bounce chunks.
    let mut big = [0u8; 200];
    storage.read(3, &mut big).unwrap();
    assert_eq!(big[5], 0);
    assert_eq!(big[20], 15);
    assert_eq!(big[21], 0xFF);
}

#[test]
fn write_and_erase_land_in_the_region() {
    let mut storage = storage();
    storage.write(0, &[1, 2, 3, 4]).unwrap();
    storage.erase(0x1000, 0x1000).unwrap();
    let flash = storage.into_inner();
    assert_eq!(&flash.data[0x4000..0x4004], &[1, 2, 3, 4]);
    assert!(flash.data[..0x4000].iter().all(|b| *b == 0xA5));
}

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(version = 1)]
struct Mock {
    #[n(0)]
    value: u32,
}

#[test]
fn config_store_runs_on_the_flash_backend() {
    let mut store = ConfigStore::open(storage()).unwrap();
    store.set(&Mock { value: 1 }).unwrap();
    store.set(&Mock { value: 70000 }).unwrap();
    let mut store = ConfigStore::open(store.into_storage()).unwrap();
    assert_eq!(store.get::<Mock>().unwrap(), Some(Mock { value: 70000 }));
    assert_eq!(store.generation(), Some(2));
    let flash = store.into_storage().into_inner();
    assert!(flash.data[..0x4000].iter().all(|b| *b == 0xA5));
    assert!(flash.data[0x8000..0x10000].iter().all(|b| *b == 0xA5));
}
