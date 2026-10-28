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

use core::time::Duration;

/// Absolute point in time: milliseconds since the Unix epoch
/// (1970-01-01T00:00:00Z, UTC, leap seconds ignored).
///
/// This is a different type from
/// [`MonotonicTimestamp`](rohi_hal::MonotonicTimestamp) on purpose: the two
/// can never be confused or compared by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixTime {
    millis: u64,
}

impl UnixTime {
    /// From milliseconds since the Unix epoch.
    pub const fn from_millis(millis: u64) -> Self {
        Self { millis }
    }

    /// From whole seconds since the Unix epoch, saturating.
    pub const fn from_secs(secs: u64) -> Self {
        Self {
            millis: secs.saturating_mul(1_000),
        }
    }

    /// Milliseconds since the Unix epoch.
    pub const fn as_millis(self) -> u64 {
        self.millis
    }

    /// Whole seconds since the Unix epoch.
    pub const fn as_secs(self) -> u64 {
        self.millis / 1_000
    }

    /// Milliseconds past the last whole second.
    pub const fn subsec_millis(self) -> u32 {
        (self.millis % 1_000) as u32
    }

    /// `self + duration`, `None` on overflow. The duration is truncated to
    /// whole milliseconds.
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        let millis = u64::try_from(duration.as_millis()).ok()?;
        Some(Self {
            millis: self.millis.checked_add(millis)?,
        })
    }

    /// `self + millis`, saturating.
    pub const fn saturating_add_millis(self, millis: u64) -> Self {
        Self {
            millis: self.millis.saturating_add(millis),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_seconds_and_milliseconds() {
        let time = UnixTime::from_millis(1_700_000_000_250);
        assert_eq!(time.as_secs(), 1_700_000_000);
        assert_eq!(time.subsec_millis(), 250);
        assert_eq!(UnixTime::from_secs(5).as_millis(), 5_000);
    }

    #[test]
    fn arithmetic_does_not_overflow() {
        let time = UnixTime::from_millis(u64::MAX - 1);
        assert_eq!(time.checked_add(Duration::from_millis(2)), None);
        assert_eq!(time.checked_add(Duration::MAX), None);
        assert_eq!(time.saturating_add_millis(10).as_millis(), u64::MAX);
        assert_eq!(UnixTime::from_secs(u64::MAX).as_millis(), u64::MAX);
        assert_eq!(
            UnixTime::from_millis(1).checked_add(Duration::from_millis(2)),
            Some(UnixTime::from_millis(3))
        );
    }
}
