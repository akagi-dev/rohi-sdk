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

//! Format-independent sector placement primitives.

pub use crate::crc::Crc32;

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
}
