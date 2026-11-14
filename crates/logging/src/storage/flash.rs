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

/// The widest read or write word of a flash [`FlashMedium`] supports, in bytes.
const MAX_WORD: usize = 16;

/// The bytes a flash call moves at most.
///
/// Every call goes through a buffer of this size, aligned to [`MAX_WORD`]: the
/// ESP32 driver uses the caller's slice only when it is word-aligned and
/// otherwise copies through a buffer of a whole sector on the stack, which a
/// task of the firmware cannot afford.
const SCRATCH: usize = 256;

#[repr(C, align(16))]
struct Scratch([u8; SCRATCH]);

impl Scratch {
    const fn new() -> Self {
        Self([0xFF; SCRATCH])
    }
}

/// A region of a NOR flash as a [`Medium`].
///
/// The region is `sectors` sectors of [`SECTOR_SIZE`] bytes from `offset`.
/// The flash driver's erase size must divide [`SECTOR_SIZE`] and be a multiple
/// of its read size, and its read and write sizes must be at most 16 bytes: a
/// driver that reads whole words only (the ESP32 one) is served too, the words
/// at the ends of a read are read whole and the asked part is copied out.
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
            && F::ERASE_SIZE.is_multiple_of(F::READ_SIZE)
            && F::READ_SIZE <= MAX_WORD
            && F::READ_SIZE > 0
            && SCRATCH.is_multiple_of(F::READ_SIZE)
            && F::WRITE_SIZE <= MAX_WORD
            && F::WRITE_SIZE > 0
            && SCRATCH.is_multiple_of(F::WRITE_SIZE)
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
        let word = F::READ_SIZE;
        let mut scratch = Scratch::new();
        let mut at = offset as usize;
        let mut rest = buf;
        while !rest.is_empty() {
            let start = at - at % word;
            let skip = at - start;
            let take = rest.len().min(SCRATCH - skip);
            let span = (skip + take).next_multiple_of(word);
            self.flash
                .read(self.offset + start as u32, &mut scratch.0[..span])
                .map_err(Self::map)?;
            let (head, tail) = rest.split_at_mut(take);
            head.copy_from_slice(&scratch.0[skip..skip + take]);
            at += take;
            rest = tail;
        }
        Ok(())
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
        let mut scratch = Scratch::new();
        while !rest.is_empty() {
            let start = at - at % word;
            let skip = at - start;
            let take = rest.len().min(word - skip);
            if skip == 0 && take == word {
                let whole = rest.len().min(SCRATCH) / word * word;
                scratch.0[..whole].copy_from_slice(&rest[..whole]);
                self.flash
                    .write(self.offset + at as u32, &scratch.0[..whole])
                    .map_err(Self::map)?;
                at += whole;
                rest = &rest[whole..];
                continue;
            }
            // A partial word: what it holds already stays, the rest is new.
            self.read_raw(start as u32, &mut scratch.0[..word])?;
            scratch.0[skip..skip + take].copy_from_slice(&rest[..take]);
            self.flash
                .write(self.offset + start as u32, &scratch.0[..word])
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
