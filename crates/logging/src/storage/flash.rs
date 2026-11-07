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

//! A [`Medium`] on a region of any multi-write NOR flash.

use embedded_storage::nor_flash::{
    ErrorType, MultiwriteNorFlash, NorFlash, NorFlashError, NorFlashErrorKind,
};

use super::medium::{Medium, SECTOR_SIZE};

/// Why a [`FlashMedium`] operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashMediumError {
    /// The range leaves the region.
    OutOfBounds,
    /// The flash driver failed with this kind of error.
    Flash(NorFlashErrorKind),
}

/// The widest write word of a flash [`FlashMedium`] supports, in bytes.
const MAX_WORD: usize = 16;

/// A region of a NOR flash as a [`Medium`].
///
/// The region is `sectors` sectors of [`SECTOR_SIZE`] bytes from `offset`.
/// The flash driver's erase size must divide [`SECTOR_SIZE`] and its write
/// size must be at most 16 bytes.
///
/// Appending a chunk rarely ends on a write-word boundary, so a write may
/// program the first and the last word of its range partly: the bytes of the
/// word that are not part of the write are programmed with what they already
/// hold, which is the reason `F` must be [`MultiwriteNorFlash`]: a word is
/// programmed again with the same bits, never with different ones. A word is
/// programmed at most twice before its sector is erased: once by the chunk
/// that ends in it, once by the chunk that continues there.
///
/// The region is not shared with anyone else: the platform hands a
/// partition of its own to the log.
pub struct FlashMedium<F> {
    flash: F,
    offset: u32,
    sectors: u32,
}

impl<F: NorFlash + MultiwriteNorFlash> FlashMedium<F> {
    /// The region of `sectors` sectors from `offset` bytes on `flash`.
    ///
    /// # Errors
    ///
    /// [`FlashMediumError::OutOfBounds`] when the region leaves the flash or
    /// is not aligned to the erase size of the driver.
    pub fn new(flash: F, offset: u32, sectors: u32) -> Result<Self, FlashMediumError> {
        let end = u64::from(offset) + u64::from(sectors) * SECTOR_SIZE as u64;
        let aligned = SECTOR_SIZE.is_multiple_of(F::ERASE_SIZE)
            && (offset as usize).is_multiple_of(F::ERASE_SIZE)
            && F::WRITE_SIZE <= MAX_WORD
            && F::WRITE_SIZE > 0
            && sectors > 0;
        if !aligned || end > flash.capacity() as u64 {
            return Err(FlashMediumError::OutOfBounds);
        }
        Ok(Self {
            flash,
            offset,
            sectors,
        })
    }

    /// The flash back.
    pub fn into_inner(self) -> F {
        self.flash
    }

    fn check(&self, offset: u32, len: usize) -> Result<(), FlashMediumError> {
        let end = u64::from(offset) + len as u64;
        if end > u64::from(self.sectors) * SECTOR_SIZE as u64 {
            Err(FlashMediumError::OutOfBounds)
        } else {
            Ok(())
        }
    }

    fn map(error: F::Error) -> FlashMediumError {
        FlashMediumError::Flash(error.kind())
    }

    fn read_raw(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), FlashMediumError> {
        self.flash
            .read(self.offset + offset, buf)
            .map_err(Self::map)
    }
}

impl<F: NorFlash + MultiwriteNorFlash> Medium for FlashMedium<F>
where
    F: ErrorType,
{
    type Error = FlashMediumError;

    fn sectors(&self) -> u32 {
        self.sectors
    }

    fn erase(&mut self, first: u32, count: u32) -> Result<(), FlashMediumError> {
        self.check(first * SECTOR_SIZE as u32, count as usize * SECTOR_SIZE)?;
        let from = self.offset + first * SECTOR_SIZE as u32;
        self.flash
            .erase(from, from + count * SECTOR_SIZE as u32)
            .map_err(Self::map)
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), FlashMediumError> {
        self.check(offset, data.len())?;
        let word = F::WRITE_SIZE;
        let mut at = offset as usize;
        let mut rest = data;
        let mut buffer = [0xFFu8; MAX_WORD];
        while !rest.is_empty() {
            let start = at - at % word;
            let skip = at - start;
            let take = rest.len().min(word - skip);
            if skip == 0 && take == word {
                // Whole words go out in one call.
                let whole = rest.len() / word * word;
                self.flash
                    .write(self.offset + at as u32, &rest[..whole])
                    .map_err(Self::map)?;
                at += whole;
                rest = &rest[whole..];
                continue;
            }
            // A partial word: what it holds already stays, the rest is new.
            self.read_raw(start as u32, &mut buffer[..word])?;
            buffer[skip..skip + take].copy_from_slice(&rest[..take]);
            self.flash
                .write(self.offset + start as u32, &buffer[..word])
                .map_err(Self::map)?;
            at += take;
            rest = &rest[take..];
        }
        Ok(())
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), FlashMediumError> {
        self.check(offset, buf.len())?;
        self.read_raw(offset, buf)
    }
}
