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

/// Bounded exponential reconnect backoff: doubles from `initial_ms` up to
/// `max_ms` and is reset once a link proved stable (see
/// [`WifiManager`](crate::WifiManager)).
///
/// This is the Wi-Fi manager's own type; it is unrelated to the backoff of
/// the sensor runtime in `rohi-hal`.
///
/// ```
/// use rohi_net::Backoff;
///
/// let mut backoff = Backoff::new(1_000, 5_000);
/// assert_eq!(backoff.next_delay_ms(), 1_000);
/// assert_eq!(backoff.next_delay_ms(), 2_000);
/// assert_eq!(backoff.next_delay_ms(), 4_000);
/// assert_eq!(backoff.next_delay_ms(), 5_000); // capped
/// backoff.reset();
/// assert_eq!(backoff.next_delay_ms(), 1_000);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Backoff {
    initial_ms: u32,
    max_ms: u32,
    current_ms: u32,
}

impl Backoff {
    /// A backoff starting at `initial_ms` and capped at `max_ms`. A zero
    /// `initial_ms` becomes 1 and a `max_ms` below `initial_ms` becomes
    /// `initial_ms`, so the delay is never zero and never shrinks.
    pub const fn new(initial_ms: u32, max_ms: u32) -> Self {
        let initial_ms = if initial_ms == 0 { 1 } else { initial_ms };
        let max_ms = if max_ms < initial_ms {
            initial_ms
        } else {
            max_ms
        };
        Self {
            initial_ms,
            max_ms,
            current_ms: initial_ms,
        }
    }

    /// Delay before the next attempt; the following delay doubles, saturating
    /// at the maximum.
    pub fn next_delay_ms(&mut self) -> u32 {
        let delay = self.current_ms;
        self.current_ms = self.current_ms.saturating_mul(2).min(self.max_ms);
        delay
    }

    /// Starts again from the initial delay.
    pub fn reset(&mut self) {
        self.current_ms = self.initial_ms;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_up_to_max_and_resets() {
        let mut backoff = Backoff::new(1_000, 5_000);
        assert_eq!(backoff.next_delay_ms(), 1_000);
        assert_eq!(backoff.next_delay_ms(), 2_000);
        assert_eq!(backoff.next_delay_ms(), 4_000);
        assert_eq!(backoff.next_delay_ms(), 5_000);
        assert_eq!(backoff.next_delay_ms(), 5_000);
        backoff.reset();
        assert_eq!(backoff.next_delay_ms(), 1_000);
    }

    #[test]
    fn degenerate_parameters_stay_bounded() {
        let mut backoff = Backoff::new(0, 0);
        assert_eq!(backoff.next_delay_ms(), 1);
        let mut backoff = Backoff::new(u32::MAX, 1);
        assert_eq!(backoff.next_delay_ms(), u32::MAX);
        assert_eq!(backoff.next_delay_ms(), u32::MAX);
    }
}
