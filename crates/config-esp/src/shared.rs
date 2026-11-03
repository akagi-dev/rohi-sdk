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
//! One flash driver shared by the owners of its partitions.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_storage::nor_flash::{ErrorType, MultiwriteNorFlash, NorFlash, ReadNorFlash};

/// The flash driver and the lock that serializes access to it. Create it once
/// (a `static`), then hand a [`SharedFlash`] to every owner.
pub type FlashCell<M, F> = Mutex<M, RefCell<F>>;

/// A handle to a flash driver in a [`FlashCell`].
///
/// The ESP32 has one SPI flash and its driver is a single owner of the
/// peripheral. The configuration store and the log live in different
/// partitions of it, so each gets its own `SharedFlash` and its own region:
/// every operation takes the lock for its duration only, and flash operations
/// are blocking anyway (`esp-storage` runs them in a critical section), so no
/// owner can observe another half way through an operation.
///
/// A handle is `Copy`; it adds no state of its own and cannot name a region,
/// that is the job of [`EspConfigStorage`](crate::EspConfigStorage) and of the
/// log medium.
pub struct SharedFlash<M: RawMutex + 'static, F: 'static> {
    cell: &'static FlashCell<M, F>,
}

impl<M: RawMutex, F> Clone for SharedFlash<M, F> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M: RawMutex, F> Copy for SharedFlash<M, F> {}

impl<M: RawMutex, F> SharedFlash<M, F> {
    /// A cell holding `flash`.
    pub const fn cell(flash: F) -> FlashCell<M, F> {
        Mutex::new(RefCell::new(flash))
    }

    /// A handle to `cell`.
    pub const fn new(cell: &'static FlashCell<M, F>) -> Self {
        Self { cell }
    }
}

impl<M: RawMutex, F: ErrorType> ErrorType for SharedFlash<M, F> {
    type Error = F::Error;
}

impl<M: RawMutex, F: ReadNorFlash> ReadNorFlash for SharedFlash<M, F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.cell
            .lock(|flash| flash.borrow_mut().read(offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.cell.lock(|flash| flash.borrow().capacity())
    }
}

impl<M: RawMutex, F: NorFlash> NorFlash for SharedFlash<M, F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.cell.lock(|flash| flash.borrow_mut().erase(from, to))
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.cell
            .lock(|flash| flash.borrow_mut().write(offset, bytes))
    }
}

impl<M: RawMutex, F: MultiwriteNorFlash> MultiwriteNorFlash for SharedFlash<M, F> {}

#[cfg(test)]
mod tests {
    extern crate std;

    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use embedded_storage::nor_flash::NorFlashError;

    use super::*;

    #[derive(Debug)]
    struct Fail;

    impl NorFlashError for Fail {
        fn kind(&self) -> embedded_storage::nor_flash::NorFlashErrorKind {
            embedded_storage::nor_flash::NorFlashErrorKind::Other
        }
    }

    struct Ram(std::vec::Vec<u8>);

    impl ErrorType for Ram {
        type Error = Fail;
    }

    impl ReadNorFlash for Ram {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Fail> {
            let at = offset as usize;
            bytes.copy_from_slice(self.0.get(at..at + bytes.len()).ok_or(Fail)?);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.0.len()
        }
    }

    impl NorFlash for Ram {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = 4096;

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Fail> {
            self.0
                .get_mut(from as usize..to as usize)
                .ok_or(Fail)?
                .fill(0xFF);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Fail> {
            let at = offset as usize;
            self.0
                .get_mut(at..at + bytes.len())
                .ok_or(Fail)?
                .copy_from_slice(bytes);
            Ok(())
        }
    }

    impl MultiwriteNorFlash for Ram {}

    #[test]
    fn two_owners_use_one_flash_without_seeing_each_other() {
        static CELL: FlashCell<CriticalSectionRawMutex, Ram> =
            SharedFlash::<CriticalSectionRawMutex, Ram>::cell(Ram(std::vec::Vec::new()));
        critical_section::with(|_| ());
        CELL.lock(|flash| *flash.borrow_mut() = Ram(std::vec![0xFF; 3 * 4096]));

        let mut first = SharedFlash::new(&CELL);
        let mut second = first;
        assert_eq!(first.capacity(), 3 * 4096);
        first.erase(0, 4096).unwrap();
        first.write(0, &[1, 2, 3, 4]).unwrap();
        second.write(4096, &[5, 6, 7, 8]).unwrap();

        let mut back = [0u8; 4];
        second.read(0, &mut back).unwrap();
        assert_eq!(back, [1, 2, 3, 4]);
        first.read(4096, &mut back).unwrap();
        assert_eq!(back, [5, 6, 7, 8]);
        assert_eq!(
            <SharedFlash<CriticalSectionRawMutex, Ram> as NorFlash>::ERASE_SIZE,
            4096
        );
    }
}
