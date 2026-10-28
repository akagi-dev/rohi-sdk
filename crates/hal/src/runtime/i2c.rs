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
//! I2C bus wrapper bounding every transaction in time.

use embedded_hal::i2c::{Error as I2cError, ErrorKind, ErrorType, Operation};
use embedded_hal_async::delay::DelayNs;
use embedded_hal_async::i2c::I2c;

use super::backoff::with_timeout;

/// Error of [`TimeoutI2c`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeoutError<E> {
    /// The underlying bus failed.
    Bus(E),
    /// The transaction did not finish within the time budget.
    TimedOut,
}

impl<E: I2cError> I2cError for TimeoutError<E> {
    fn kind(&self) -> ErrorKind {
        match self {
            TimeoutError::Bus(e) => e.kind(),
            TimeoutError::TimedOut => ErrorKind::Other,
        }
    }
}

/// Fails I2C transactions that take longer than a time budget, so a stuck bus
/// can never block a sensor task forever.
pub struct TimeoutI2c<I2C, D> {
    inner: I2C,
    delay: D,
    timeout_ms: u32,
}

impl<I2C, D> TimeoutI2c<I2C, D> {
    /// Wrap `inner`, allowing each transaction `timeout_ms`.
    pub const fn new(inner: I2C, delay: D, timeout_ms: u32) -> Self {
        Self {
            inner,
            delay,
            timeout_ms,
        }
    }
}

impl<I2C: I2c, D: DelayNs> ErrorType for TimeoutI2c<I2C, D> {
    type Error = TimeoutError<I2C::Error>;
}

impl<I2C: I2c, D: DelayNs> I2c for TimeoutI2c<I2C, D> {
    async fn transaction(
        &mut self,
        address: u8,
        operations: &mut [Operation<'_>],
    ) -> Result<(), Self::Error> {
        match with_timeout(
            &mut self.delay,
            self.timeout_ms,
            self.inner.transaction(address, operations),
        )
        .await
        {
            Ok(result) => result.map_err(TimeoutError::Bus),
            Err(_) => Err(TimeoutError::TimedOut),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{MockI2c, NoDelay, block_on};

    struct StuckBus;

    impl ErrorType for StuckBus {
        type Error = ErrorKind;
    }

    impl I2c for StuckBus {
        async fn transaction(&mut self, _: u8, _: &mut [Operation<'_>]) -> Result<(), ErrorKind> {
            core::future::pending().await
        }
    }

    #[test]
    fn stuck_bus_times_out() {
        let mut bus = TimeoutI2c::new(StuckBus, NoDelay, 100);
        let mut buf = [0u8; 1];
        let err = block_on(bus.read(0x10, &mut buf)).unwrap_err();
        assert_eq!(err, TimeoutError::TimedOut);
        assert_eq!(err.kind(), ErrorKind::Other);
    }

    #[test]
    fn healthy_bus_passes_through_and_errors_are_kept() {
        let mut mock = MockI2c::default();
        mock.push_read(&[7]);
        mock.push_error(MockI2c::nack());
        let mut bus = TimeoutI2c::new(mock, NoDelay, 100);
        let mut buf = [0u8; 1];
        block_on(bus.read(0x10, &mut buf)).unwrap();
        assert_eq!(buf, [7]);
        let err = block_on(bus.read(0x10, &mut buf)).unwrap_err();
        assert!(matches!(err.kind(), ErrorKind::NoAcknowledge(_)));
    }
}
