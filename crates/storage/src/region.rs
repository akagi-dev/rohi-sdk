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

/// A bounded flash window. Construction does not validate capacity or alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashRegion {
    /// Absolute start in bytes.
    pub offset: usize,
    /// Length in bytes.
    pub length: usize,
}

impl FlashRegion {
    /// Constructs a window without validating it against a device.
    pub const fn new(offset: usize, length: usize) -> Self {
        Self { offset, length }
    }

    /// Translates a relative range, returning `None` for overflow or an
    /// out-of-region range. An empty range at the end is accepted.
    pub fn absolute(&self, relative: usize, len: usize) -> Option<usize> {
        let end = relative.checked_add(len)?;
        if end > self.length {
            return None;
        }
        let start = self.offset.checked_add(relative)?;
        start.checked_add(len)?;
        Some(start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_ranges() {
        let region = FlashRegion::new(0x1000, 0x100);
        assert_eq!(region.absolute(0, 0x100), Some(0x1000));
        assert_eq!(region.absolute(0x100, 0), Some(0x1100));
        assert_eq!(region.absolute(0xff, 2), None);
        assert_eq!(region.absolute(usize::MAX, 2), None);
        assert_eq!(FlashRegion::new(usize::MAX - 1, 4).absolute(0, 2), None);
    }
}
