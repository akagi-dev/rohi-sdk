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

use embedded_storage::nor_flash::{MultiwriteNorFlash, NorFlash};
use rohi_config::ConfigStorage;

use crate::{EspStorageError, FlashRegion};

/// Bounce buffer for reads that do not satisfy the flash read granularity.
const BOUNCE: usize = 64;

/// [`ConfigStorage`] over a [`FlashRegion`] of an `embedded-storage` NOR flash.
///
/// - every access is checked against the region with overflow-safe arithmetic;
/// - writes must be `WRITE_SIZE` aligned and erases `ERASE_SIZE` aligned,
///   otherwise [`EspStorageError::InvalidAlignment`] is returned;
/// - reads of arbitrary offset/length are supported (the flash read
///   granularity is handled internally).
pub struct EspConfigStorage<F> {
    flash: F,
    region: FlashRegion,
}

impl<F: NorFlash> EspConfigStorage<F> {
    /// Wraps `flash`, restricted to `region`.
    ///
    /// The region must be non-empty, erase-sector aligned and inside the
    /// flash capacity.
    pub fn new(flash: F, region: FlashRegion) -> Result<Self, EspStorageError> {
        const { assert!(F::READ_SIZE.is_power_of_two() && F::READ_SIZE <= BOUNCE) };
        let end = region
            .offset
            .checked_add(region.length)
            .ok_or(EspStorageError::InvalidRegion)?;
        if region.length == 0
            || !region.offset.is_multiple_of(F::ERASE_SIZE)
            || !region.length.is_multiple_of(F::ERASE_SIZE)
            || end > flash.capacity()
            || u32::try_from(end).is_err()
        {
            return Err(EspStorageError::InvalidRegion);
        }
        Ok(Self { flash, region })
    }

    pub fn region(&self) -> FlashRegion {
        self.region
    }

    /// Gives the flash back.
    pub fn into_inner(self) -> F {
        self.flash
    }

    fn flash_offset(&self, offset: usize, len: usize) -> Result<u32, EspStorageError> {
        let absolute = self.region.absolute(offset, len)?;
        u32::try_from(absolute).map_err(|_| EspStorageError::OutOfBounds)
    }
}

// The store clears a programmed commit word before erasing its slot, which
// needs flash that can program a word more than once.
impl<F: MultiwriteNorFlash> ConfigStorage for EspConfigStorage<F> {
    type Error = EspStorageError;

    const ERASE_SIZE: usize = F::ERASE_SIZE;
    const WRITE_SIZE: usize = F::WRITE_SIZE;

    fn capacity(&self) -> usize {
        self.region.length
    }

    fn read(&mut self, offset: usize, output: &mut [u8]) -> Result<(), Self::Error> {
        let absolute = self.flash_offset(offset, output.len())?;
        let granularity = F::READ_SIZE;
        if (absolute as usize).is_multiple_of(granularity)
            && output.len().is_multiple_of(granularity)
        {
            return self
                .flash
                .read(absolute, output)
                .map_err(|_| EspStorageError::ReadFailed);
        }

        // Read the covering aligned range through a bounce buffer.
        let mut position = absolute as usize - absolute as usize % granularity;
        let end = absolute as usize + output.len();
        let mut done = 0;
        let mut bounce = [0u8; BOUNCE];
        while position < end {
            let chunk = BOUNCE.min((end - position).next_multiple_of(granularity));
            self.flash
                .read(position as u32, &mut bounce[..chunk])
                .map_err(|_| EspStorageError::ReadFailed)?;
            let from = (absolute as usize).saturating_sub(position).min(chunk);
            let take = (chunk - from).min(output.len() - done);
            output[done..done + take].copy_from_slice(&bounce[from..from + take]);
            done += take;
            position += chunk;
        }
        Ok(())
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), Self::Error> {
        let absolute = self.flash_offset(offset, data.len())?;
        if !offset.is_multiple_of(F::WRITE_SIZE) || !data.len().is_multiple_of(F::WRITE_SIZE) {
            return Err(EspStorageError::InvalidAlignment);
        }
        self.flash
            .write(absolute, data)
            .map_err(|_| EspStorageError::WriteFailed)
    }

    fn erase(&mut self, offset: usize, len: usize) -> Result<(), Self::Error> {
        let absolute = self.flash_offset(offset, len)?;
        if !offset.is_multiple_of(F::ERASE_SIZE) || !len.is_multiple_of(F::ERASE_SIZE) {
            return Err(EspStorageError::InvalidAlignment);
        }
        let end = u32::try_from(len)
            .ok()
            .and_then(|len| absolute.checked_add(len))
            .ok_or(EspStorageError::OutOfBounds)?;
        self.flash
            .erase(absolute, end)
            .map_err(|_| EspStorageError::EraseFailed)
    }
}

#[cfg(test)]
mod tests;
