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
//! Retry backoff and bounded waits.

use embassy_futures::select::{Either, select};
use embedded_hal_async::delay::DelayNs;

/// Exponential backoff: `initial`, `2 * initial`, ... capped at `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    initial_ms: u32,
    max_ms: u32,
    current_ms: u32,
}

impl Backoff {
    /// Create a backoff starting at `initial_ms` and growing up to `max_ms`.
    pub const fn new(initial_ms: u32, max_ms: u32) -> Self {
        Self {
            initial_ms,
            max_ms,
            current_ms: initial_ms,
        }
    }

    /// Delay to wait before the next attempt; the following delay grows.
    pub fn next_delay_ms(&mut self) -> u32 {
        let delay = self.current_ms;
        self.current_ms = self.current_ms.saturating_mul(2).min(self.max_ms);
        delay
    }

    /// Start over from the initial delay.
    pub fn reset(&mut self) {
        self.current_ms = self.initial_ms;
    }
}

/// An operation did not complete within its time budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimedOut;

/// Run `future`, giving up after `timeout_ms`.
pub async fn with_timeout<D: DelayNs, F: core::future::Future>(
    delay: &mut D,
    timeout_ms: u32,
    future: F,
) -> Result<F::Output, TimedOut> {
    match select(future, delay.delay_ms(timeout_ms)).await {
        Either::First(value) => Ok(value),
        Either::Second(()) => Err(TimedOut),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{NoDelay, block_on};

    #[test]
    fn backoff_grows_and_caps_then_resets() {
        let mut b = Backoff::new(100, 500);
        assert_eq!(b.next_delay_ms(), 100);
        assert_eq!(b.next_delay_ms(), 200);
        assert_eq!(b.next_delay_ms(), 400);
        assert_eq!(b.next_delay_ms(), 500);
        assert_eq!(b.next_delay_ms(), 500);
        b.reset();
        assert_eq!(b.next_delay_ms(), 100);
    }

    #[test]
    fn timeout_fires_when_future_pends() {
        let mut delay = NoDelay;
        let r = block_on(with_timeout(&mut delay, 10, core::future::pending::<()>()));
        assert_eq!(r, Err(TimedOut));
    }

    #[test]
    fn ready_future_wins() {
        let mut delay = NoDelay;
        let r = block_on(with_timeout(&mut delay, 10, async { 7 }));
        assert_eq!(r, Ok(7));
    }
}
