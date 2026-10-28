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
//! Aggregation of the SDS011 samples of one measurement cycle.
//!
//! Only valid samples are stored. The final value of each channel is the
//! rounded integer mean; when trimming is enabled and at least
//! [`TRIM_MIN_SAMPLES`] samples exist, the lowest and the highest sample are
//! discarded first. PM2.5 and PM10 are treated independently but by the same
//! rule. Values are raw sensor units (0.1 µg/m³). No heap is used.
//!
//! ```text
//! PM2.5: 10 12 11 80 9  ->  drop 9 and 80  ->  mean(10, 12, 11) = 11
//! ```

/// Capacity of the sample buffers; the sample count is limited by it.
pub const MAX_SAMPLES: usize = 16;
/// Fewest samples for which the minimum and the maximum are discarded.
pub const TRIM_MIN_SAMPLES: usize = 5;

/// Valid samples of one measurement cycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Samples {
    pm25: [u16; MAX_SAMPLES],
    pm10: [u16; MAX_SAMPLES],
    len: usize,
}

impl Samples {
    /// Create an empty set.
    pub const fn new() -> Self {
        Self {
            pm25: [0; MAX_SAMPLES],
            pm10: [0; MAX_SAMPLES],
            len: 0,
        }
    }

    /// Number of stored samples.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// No sample is stored.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Store a valid sample; `false` if the buffer is full.
    pub fn push(&mut self, pm25: u16, pm10: u16) -> bool {
        if self.len == MAX_SAMPLES {
            return false;
        }
        self.pm25[self.len] = pm25;
        self.pm10[self.len] = pm10;
        self.len += 1;
        true
    }

    /// Final `(pm25, pm10)`, `None` without samples.
    pub fn aggregate(&self, trim_min_max: bool) -> Option<(u16, u16)> {
        if self.is_empty() {
            return None;
        }
        let trim = trim_min_max && self.len >= TRIM_MIN_SAMPLES;
        Some((
            mean(&self.pm25[..self.len], trim),
            mean(&self.pm10[..self.len], trim),
        ))
    }
}

impl Default for Samples {
    fn default() -> Self {
        Self::new()
    }
}

/// Rounded mean of non-empty `values`, optionally without one minimum and one
/// maximum (the slice must have more than two values then).
fn mean(values: &[u16], trim: bool) -> u16 {
    let mut sum: u32 = 0;
    let mut min = u16::MAX;
    let mut max = u16::MIN;
    for &v in values {
        sum += u32::from(v);
        min = min.min(v);
        max = max.max(v);
    }
    let mut count = values.len() as u32;
    if trim {
        sum -= u32::from(min) + u32::from(max);
        count -= 2;
    }
    ((sum + count / 2) / count) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(pm25: &[u16], pm10: &[u16]) -> Samples {
        let mut s = Samples::new();
        for (&a, &b) in pm25.iter().zip(pm10) {
            assert!(s.push(a, b));
        }
        s
    }

    #[test]
    fn empty_set_has_no_result() {
        assert_eq!(Samples::new().aggregate(true), None);
    }

    #[test]
    fn spike_and_dip_are_trimmed() {
        let s = samples(&[10, 12, 11, 80, 9], &[20, 22, 21, 90, 19]);
        assert_eq!(s.aggregate(true), Some((11, 21)));
    }

    #[test]
    fn trimming_can_be_disabled() {
        let s = samples(&[10, 12, 11, 80, 9], &[10, 10, 10, 10, 10]);
        assert_eq!(s.aggregate(false), Some((24, 10)));
    }

    #[test]
    fn few_samples_are_not_trimmed() {
        let s = samples(&[10, 80, 9], &[1, 2, 3]);
        assert_eq!(s.aggregate(true), Some((33, 2)));
    }

    #[test]
    fn channels_are_trimmed_independently() {
        // The extremes of PM10 are in different samples than those of PM2.5.
        let s = samples(&[100, 1, 2, 3, 4], &[5, 6, 100, 7, 1]);
        assert_eq!(s.aggregate(true), Some((3, 6)));
    }

    #[test]
    fn equal_extremes_are_removed_once() {
        let s = samples(&[5, 5, 5, 5, 5, 9], &[7, 7, 7, 7, 7, 7]);
        // Drops one 5 and the 9: mean(5, 5, 5, 5) = 5.
        assert_eq!(s.aggregate(true), Some((5, 7)));
    }

    #[test]
    fn mean_is_rounded_to_nearest() {
        let s = samples(&[1, 2], &[1, 1]);
        assert_eq!(s.aggregate(false), Some((2, 1)));
        let s = samples(&[1, 1, 2], &[0, 0, 0]);
        assert_eq!(s.aggregate(false), Some((1, 0)));
    }

    #[test]
    fn extreme_values_do_not_overflow() {
        let mut s = Samples::new();
        for _ in 0..MAX_SAMPLES {
            assert!(s.push(u16::MAX, u16::MAX));
        }
        assert_eq!(s.aggregate(true), Some((u16::MAX, u16::MAX)));
    }

    #[test]
    fn buffer_is_bounded() {
        let mut s = Samples::new();
        for _ in 0..MAX_SAMPLES {
            assert!(s.push(1, 1));
        }
        assert!(!s.push(1, 1));
        assert_eq!(s.len(), MAX_SAMPLES);
    }
}
