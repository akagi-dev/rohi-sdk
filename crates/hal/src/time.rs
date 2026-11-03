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
//! Monotonic time.

/// Monotonic point in time, in milliseconds since an arbitrary epoch
/// (usually device boot).
///
/// This crate never requires UTC, so valid measurements can be collected
/// before time synchronization succeeds. The caller supplies all timestamps
/// (on ESP32 `rohi_hal_esp::EmbassyClock`, through [`Clock`](crate::runtime::Clock));
/// this crate never reads a clock. Ordering and equality compare the
/// millisecond value.
///
/// ```
/// use rohi_hal::MonotonicTimestamp;
///
/// let boot_plus_1s = MonotonicTimestamp::from_millis(1_000);
/// let boot_plus_3s = MonotonicTimestamp::from_millis(3_000);
/// assert_eq!(boot_plus_3s.saturating_millis_since(boot_plus_1s), 2_000);
/// // Never underflows, even for timestamps from the future.
/// assert_eq!(boot_plus_1s.saturating_millis_since(boot_plus_3s), 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MonotonicTimestamp {
    /// Milliseconds since the epoch.
    pub millis: u64,
}

impl MonotonicTimestamp {
    /// Create from milliseconds.
    pub const fn from_millis(millis: u64) -> Self {
        Self { millis }
    }

    /// Milliseconds elapsed since `earlier`, zero if `earlier` is in the future.
    pub const fn saturating_millis_since(self, earlier: MonotonicTimestamp) -> u64 {
        self.millis.saturating_sub(earlier.millis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturating_since() {
        let a = MonotonicTimestamp::from_millis(100);
        let b = MonotonicTimestamp::from_millis(350);
        assert_eq!(b.saturating_millis_since(a), 250);
        assert_eq!(a.saturating_millis_since(b), 0);
    }
}
