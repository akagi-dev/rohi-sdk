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
/// The flash must be a [`MultiwriteNorFlash`]: the marker of flash that can
/// program a word more than once (`esp-storage`'s `FlashStorage` implements it;
/// encrypted flash would not qualify), which the store needs to clear a
/// programmed commit word before erasing its slot.
///
/// - every offset is region-relative and every access is checked against the
///   region with overflow-safe arithmetic ([`EspStorageError::OutOfBounds`]);
/// - writes must be `WRITE_SIZE` aligned and erases `ERASE_SIZE` aligned,
///   otherwise [`EspStorageError::InvalidAlignment`] is returned;
/// - reads of arbitrary offset/length are supported (the flash read
///   granularity is handled internally through a small bounce buffer);
/// - platform failures are the typed [`EspStorageError`] variants
///   `OutOfBounds`, `InvalidAlignment`, `InvalidRegion`, `ReadFailed`,
///   `WriteFailed` and `EraseFailed`.
///
/// With a chip feature (`esp32c3` or `esp32c6`), `esp::config_storage` builds
/// it on the `esp-storage` driver and the `rohi-cfg` partition. It knows
/// nothing about CBOR, sections or any feature's configuration.
///
/// # Ownership
///
/// The storage owns the flash driver; [`into_inner`](Self::into_inner) gives
/// it back. Wrap it in a [`rohi_config::ConfigStore`], which takes it by
/// value, so exactly one owner touches the region.
///
/// # Example
///
/// ```
/// # use embedded_storage::nor_flash::{
/// #     ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
/// # };
/// # #[derive(Debug)]
/// # struct Fail;
/// # impl NorFlashError for Fail {
/// #     fn kind(&self) -> NorFlashErrorKind { NorFlashErrorKind::Other }
/// # }
/// # struct RamFlash([u8; 65536]);
/// # impl ErrorType for RamFlash { type Error = Fail; }
/// # impl ReadNorFlash for RamFlash {
/// #     const READ_SIZE: usize = 4;
/// #     fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Fail> {
/// #         let start = offset as usize;
/// #         bytes.copy_from_slice(self.0.get(start..start + bytes.len()).ok_or(Fail)?);
/// #         Ok(())
/// #     }
/// #     fn capacity(&self) -> usize { self.0.len() }
/// # }
/// # impl NorFlash for RamFlash {
/// #     const WRITE_SIZE: usize = 4;
/// #     const ERASE_SIZE: usize = 4096;
/// #     fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Fail> {
/// #         let start = offset as usize;
/// #         for (cell, byte) in self.0.get_mut(start..start + bytes.len()).ok_or(Fail)?.iter_mut().zip(bytes) {
/// #             *cell &= *byte;
/// #         }
/// #         Ok(())
/// #     }
/// #     fn erase(&mut self, from: u32, to: u32) -> Result<(), Fail> {
/// #         self.0.get_mut(from as usize..to as usize).ok_or(Fail)?.fill(0xFF);
/// #         Ok(())
/// #     }
/// # }
/// # impl MultiwriteNorFlash for RamFlash {}
/// use rohi_config::ConfigStorage;
/// use rohi_config_esp::{EspConfigStorage, EspStorageError, FlashRegion};
///
/// let region = FlashRegion::new(0x1000, 0x2000);
/// let mut storage = EspConfigStorage::new(RamFlash([0xFF; 65536]), region).unwrap();
/// assert_eq!(storage.capacity(), 0x2000);
///
/// // Offsets are region-relative; erases and writes must be aligned.
/// storage.erase(0, 4096).unwrap();
/// storage.write(0, &[1, 2, 3, 4]).unwrap();
/// let mut out = [0u8; 3];
/// storage.read(1, &mut out).unwrap(); // unaligned read: bounce buffer
/// assert_eq!(out, [2, 3, 4]);
///
/// assert_eq!(storage.write(2, &[0; 4]), Err(EspStorageError::InvalidAlignment));
/// assert_eq!(storage.erase(0, 100), Err(EspStorageError::InvalidAlignment));
/// assert_eq!(storage.read(0x2000, &mut out), Err(EspStorageError::OutOfBounds));
///
/// // A region that is not erase-aligned is refused.
/// let unaligned = FlashRegion::new(0x1004, 0x2000);
/// assert!(matches!(
///     EspConfigStorage::new(RamFlash([0xFF; 65536]), unaligned),
///     Err(EspStorageError::InvalidRegion)
/// ));
/// ```
pub struct EspConfigStorage<F> {
    flash: F,
    region: FlashRegion,
}

impl<F: NorFlash> EspConfigStorage<F> {
    /// Wraps `flash`, restricted to `region`.
    ///
    /// The region must be non-empty, erase-sector aligned and inside the
    /// flash capacity.
    ///
    /// # Errors
    ///
    /// [`EspStorageError::InvalidRegion`] if the region is empty, its offset
    /// or length is not a multiple of `F::ERASE_SIZE`, its end exceeds the
    /// flash capacity, or its end does not fit in `u32`.
    ///
    /// The flash read granularity `F::READ_SIZE` must be a power of two of at
    /// most 64; other flash types fail to compile.
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

    /// The region this storage is restricted to.
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

    /// # Errors
    ///
    /// [`EspStorageError::OutOfBounds`] outside the region and
    /// [`EspStorageError::ReadFailed`] if the flash driver fails.
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

    /// # Errors
    ///
    /// [`EspStorageError::OutOfBounds`] outside the region (checked first),
    /// [`EspStorageError::InvalidAlignment`] if offset or length is not a
    /// multiple of `WRITE_SIZE`, [`EspStorageError::WriteFailed`] if the
    /// flash driver fails.
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), Self::Error> {
        let absolute = self.flash_offset(offset, data.len())?;
        if !offset.is_multiple_of(F::WRITE_SIZE) || !data.len().is_multiple_of(F::WRITE_SIZE) {
            return Err(EspStorageError::InvalidAlignment);
        }
        self.flash
            .write(absolute, data)
            .map_err(|_| EspStorageError::WriteFailed)
    }

    /// # Errors
    ///
    /// [`EspStorageError::OutOfBounds`] outside the region (checked first),
    /// [`EspStorageError::InvalidAlignment`] if offset or length is not a
    /// multiple of `ERASE_SIZE`, [`EspStorageError::EraseFailed`] if the
    /// flash driver fails.
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
