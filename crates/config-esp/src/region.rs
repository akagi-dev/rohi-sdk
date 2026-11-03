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

use crate::EspStorageError;

/// A bounded window of flash owned by the configuration store.
///
/// All offsets handed to the storage backend are relative to `offset`. The
/// type itself does not check alignment or flash capacity;
/// [`EspConfigStorage::new`](crate::EspConfigStorage::new) validates the
/// region against the actual flash.
///
/// ```
/// use rohi_config_esp::layout::REGION_SIZE;
/// use rohi_config_esp::FlashRegion;
///
/// let region = FlashRegion::new(0x10000, REGION_SIZE);
/// assert_eq!(region.offset, 0x10000);
/// assert_eq!(region.length, 16 * 1024);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashRegion {
    /// Absolute flash offset of the region start.
    pub offset: usize,
    /// Region length in bytes.
    pub length: usize,
}

impl FlashRegion {
    /// A region of `length` bytes at the absolute flash `offset` (no validation).
    pub const fn new(offset: usize, length: usize) -> Self {
        Self { offset, length }
    }

    /// Converts a region-relative range into an absolute flash offset,
    /// failing if the range is not entirely inside the region.
    ///
    /// An empty range at the very end of the region is accepted.
    ///
    /// # Errors
    ///
    /// [`EspStorageError::OutOfBounds`] if `relative + len` overflows or
    /// exceeds the region length, or if the absolute offset overflows.
    ///
    /// ```
    /// use rohi_config_esp::{EspStorageError, FlashRegion};
    ///
    /// let region = FlashRegion::new(0x1000, 0x100);
    /// assert_eq!(region.absolute(0x10, 4), Ok(0x1010));
    /// assert_eq!(region.absolute(0x100, 0), Ok(0x1100));
    /// assert_eq!(region.absolute(0xFF, 2), Err(EspStorageError::OutOfBounds));
    /// assert_eq!(region.absolute(usize::MAX, 2), Err(EspStorageError::OutOfBounds));
    /// ```
    pub fn absolute(&self, relative: usize, len: usize) -> Result<usize, EspStorageError> {
        let end = relative
            .checked_add(len)
            .ok_or(EspStorageError::OutOfBounds)?;
        if end > self.length {
            return Err(EspStorageError::OutOfBounds);
        }
        self.offset
            .checked_add(relative)
            .ok_or(EspStorageError::OutOfBounds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_must_stay_inside_the_region() {
        let region = FlashRegion::new(0x1000, 0x100);
        assert_eq!(region.absolute(0, 0x100), Ok(0x1000));
        assert_eq!(region.absolute(0x10, 4), Ok(0x1010));
        assert_eq!(region.absolute(0x100, 0), Ok(0x1100));
        assert_eq!(region.absolute(0x100, 1), Err(EspStorageError::OutOfBounds));
        assert_eq!(region.absolute(0xFF, 2), Err(EspStorageError::OutOfBounds));
        assert_eq!(
            region.absolute(usize::MAX, 2),
            Err(EspStorageError::OutOfBounds)
        );
    }

    #[test]
    fn absolute_offset_overflow_is_rejected() {
        let region = FlashRegion::new(usize::MAX, 8);
        assert_eq!(region.absolute(4, 4), Err(EspStorageError::OutOfBounds));
    }
}
