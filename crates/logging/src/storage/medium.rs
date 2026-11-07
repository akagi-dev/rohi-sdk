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

//! The erasable array of sectors a [`RingStorage`](super::RingStorage) is
//! laid out on.

use core::fmt::Debug;

/// Size of a sector, the unit of erasing, in bytes.
pub const SECTOR_SIZE: usize = 4096;

/// A region of NOR-style memory: sectors that are erased to `0xFF`, written
/// once by clearing bits and read back.
///
/// A medium hides the alignment of its hardware: [`write`](Self::write) takes
/// any offset and length inside the region, the bytes it covers are erased
/// (`0xFF`) and it never changes a byte outside them.
pub trait Medium {
    /// What the hardware reports.
    type Error: Debug + Copy + PartialEq;

    /// Number of sectors of [`SECTOR_SIZE`] bytes.
    fn sectors(&self) -> u32;

    /// Erases `count` sectors from `first` to `0xFF`.
    fn erase(&mut self, first: u32, count: u32) -> Result<(), Self::Error>;

    /// Writes `data` at `offset` bytes from the start of the region.
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error>;

    /// Reads into `buf` from `offset` bytes from the start of the region.
    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error>;
}

/// What a [`RamMedium`] refuses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RamError {
    /// The range leaves the region.
    OutOfBounds,
    /// A byte was written that is not erased, which NOR flash cannot do.
    NotErased,
    /// An injected fault, see [`RamMedium::fail_after`].
    Injected,
}

/// A medium in RAM with the rules of NOR flash: erased bytes are `0xFF`,
/// writing over a programmed byte is refused. It also counts erasures and can
/// stop writing after a number of bytes, to simulate a power loss.
#[derive(Clone, Debug)]
pub struct RamMedium<const SECTORS: usize> {
    data: [[u8; SECTOR_SIZE]; SECTORS],
    erases: [u32; SECTORS],
    budget: Option<usize>,
}

impl<const SECTORS: usize> Default for RamMedium<SECTORS> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const SECTORS: usize> RamMedium<SECTORS> {
    /// An erased medium.
    pub const fn new() -> Self {
        Self {
            data: [[0xFF; SECTOR_SIZE]; SECTORS],
            erases: [0; SECTORS],
            budget: None,
        }
    }

    /// Times each sector was erased.
    pub fn erase_counts(&self) -> &[u32; SECTORS] {
        &self.erases
    }

    /// Lets the next `bytes` bytes be written and then fails every write, with
    /// the last write cut where the budget ends: a power loss.
    pub fn fail_after(&mut self, bytes: usize) {
        self.budget = Some(bytes);
    }

    /// Stops injecting faults.
    pub fn restore_power(&mut self) {
        self.budget = None;
    }

    /// The raw content, for tests that damage it.
    pub fn raw(&mut self, offset: u32) -> &mut u8 {
        let offset = offset as usize;
        &mut self.data[offset / SECTOR_SIZE][offset % SECTOR_SIZE]
    }
}

impl<const SECTORS: usize> Medium for RamMedium<SECTORS> {
    type Error = RamError;

    fn sectors(&self) -> u32 {
        SECTORS as u32
    }

    fn erase(&mut self, first: u32, count: u32) -> Result<(), RamError> {
        let end = first.checked_add(count).ok_or(RamError::OutOfBounds)? as usize;
        if end > SECTORS {
            return Err(RamError::OutOfBounds);
        }
        for sector in first as usize..end {
            self.data[sector] = [0xFF; SECTOR_SIZE];
            self.erases[sector] += 1;
        }
        Ok(())
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), RamError> {
        let start = offset as usize;
        if start + data.len() > SECTORS * SECTOR_SIZE {
            return Err(RamError::OutOfBounds);
        }
        for (index, byte) in data.iter().enumerate() {
            if let Some(budget) = &mut self.budget {
                if *budget == 0 {
                    return Err(RamError::Injected);
                }
                *budget -= 1;
            }
            let at = start + index;
            let cell = &mut self.data[at / SECTOR_SIZE][at % SECTOR_SIZE];
            if *cell != 0xFF {
                return Err(RamError::NotErased);
            }
            *cell = *byte;
        }
        Ok(())
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), RamError> {
        let start = offset as usize;
        if start + buf.len() > SECTORS * SECTOR_SIZE {
            return Err(RamError::OutOfBounds);
        }
        for (index, byte) in buf.iter_mut().enumerate() {
            let at = start + index;
            *byte = self.data[at / SECTOR_SIZE][at % SECTOR_SIZE];
        }
        Ok(())
    }
}
