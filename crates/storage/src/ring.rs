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

//! Format-independent sector placement and integrity primitives.

/// Returns a contiguous placement at `head`, wrapping to zero when necessary.
/// Returns `None` if the ring is empty, the head is outside it, or the
/// requested span is empty or larger than the ring.
pub fn placement(head: u32, count: u32, total: u32) -> Option<u32> {
    if total == 0 || head > total || count == 0 || count > total {
        return None;
    }
    Some(if count > total - head { 0 } else { head })
}

/// Returns whether two half-open sector spans overlap, without overflow.
pub fn overlaps(first: u32, count: u32, other: u32, other_count: u32) -> bool {
    count != 0
        && other_count != 0
        && u64::from(first) < u64::from(other) + u64::from(other_count)
        && u64::from(other) < u64::from(first) + u64::from(count)
}

/// Incremental CRC-32 (IEEE 802.3). This detects corruption, not authenticity.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

impl Crc32 {
    /// An empty checksum.
    pub const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    /// Adds `bytes`.
    pub fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u32::from(*byte);
            for _ in 0..8 {
                self.0 = (self.0 >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(self.0 & 1));
            }
        }
    }

    /// The checksum of everything added so far.
    pub const fn finish(self) -> u32 {
        !self.0
    }

    /// The checksum of `bytes` alone.
    pub fn of(bytes: &[u8]) -> u32 {
        let mut crc = Self::new();
        crc.update(bytes);
        crc.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placement_wraps_and_checks_bounds() {
        assert_eq!(placement(6, 2, 8), Some(6));
        assert_eq!(placement(7, 2, 8), Some(0));
        assert_eq!(placement(8, 2, 8), Some(0));
        assert_eq!(placement(u32::MAX, 2, u32::MAX), Some(0));
        assert_eq!(placement(9, 1, 8), None);
        assert_eq!(placement(0, 9, 8), None);
        assert_eq!(placement(0, 0, 8), None);
        assert!(overlaps(u32::MAX, 2, u32::MAX, 1));
        assert!(!overlaps(0, 2, 2, 1));
        assert!(!overlaps(1, 0, 0, 3));
    }

    #[test]
    fn crc_matches_standard_check_value() {
        assert_eq!(Crc32::of(b"123456789"), 0xcbf4_3926);
    }
}
