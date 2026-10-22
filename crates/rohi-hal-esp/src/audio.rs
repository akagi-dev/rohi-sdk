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
//! I2S microphone audio source for ESP32.
//!
//! Receives mono 32-bit Philips frames over DMA into a fixed ring buffer and
//! converts them to signed 24-bit samples. Samples are handed to the level
//! meter right away; nothing is stored.

use esp_hal::Async;
use esp_hal::dma::{DmaError, DmaRxStreamBuf};
use esp_hal::i2s::master::{I2sRx, I2sRxDmaTransfer};

use rohi_hal::sensors::noise::AudioSource;

/// Samples converted per read. Bounds the stack buffer and the time between
/// two awaits.
const SAMPLES_PER_READ: usize = 64;

/// I2S microphone error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    /// The DMA transfer failed; the next read starts a fresh one.
    Dma(DmaError),
    /// The DMA transfer could not be started.
    Stopped,
}

enum State {
    /// Not receiving. The ring buffer is only drained while a read is in
    /// progress, and an undrained ring overflows with a DMA descriptor error,
    /// so the transfer is only running while it is being read.
    Idle(I2sRx<'static, Async>, DmaRxStreamBuf),
    Running(I2sRxDmaTransfer<'static, Async, DmaRxStreamBuf>),
    Failed,
}

/// DMA backed I2S microphone, one 32-bit slot per sample.
pub struct I2sMicrophone {
    state: State,
}

impl I2sMicrophone {
    /// Prepare to receive into `buffer`. The transfer starts on the first
    /// read, so the ring is drained as soon as it starts to fill.
    pub fn new(rx: I2sRx<'static, Async>, buffer: DmaRxStreamBuf) -> Result<Self, Error> {
        Ok(Self {
            state: State::Idle(rx, buffer),
        })
    }

    fn start(&mut self) -> Result<&mut I2sRxDmaTransfer<'static, Async, DmaRxStreamBuf>, Error> {
        if matches!(self.state, State::Idle(..)) {
            let State::Idle(rx, buffer) = core::mem::replace(&mut self.state, State::Failed) else {
                return Err(Error::Stopped);
            };
            match rx.read(buffer) {
                Ok(transfer) => self.state = State::Running(transfer),
                Err((_, rx, buffer)) => {
                    self.state = State::Idle(rx, buffer);
                    return Err(Error::Stopped);
                }
            }
        }
        match &mut self.state {
            State::Running(transfer) => Ok(transfer),
            _ => Err(Error::Stopped),
        }
    }

    fn stop(&mut self) {
        if let State::Running(transfer) = core::mem::replace(&mut self.state, State::Failed) {
            let (rx, buffer) = transfer.stop();
            self.state = State::Idle(rx, buffer);
        }
    }
}

impl AudioSource for I2sMicrophone {
    type Error = Error;

    async fn read(&mut self, out: &mut [i32]) -> Result<usize, Error> {
        let wanted = out.len().min(SAMPLES_PER_READ);
        let mut bytes = [0u8; SAMPLES_PER_READ * 4];

        loop {
            let transfer = self.start()?;
            // Drain the backlog before waiting: completed chunks coalesce into
            // a single pending interrupt, so waiting first would pop one chunk
            // per chunk received and never catch up after a scheduling delay.
            let mut available = transfer.available_bytes() & !3;
            if available == 0 {
                if let Err(e) = transfer.wait_for_available_async().await {
                    self.stop();
                    return Err(Error::Dma(e));
                }
                available = transfer.available_bytes() & !3;
                if available == 0 {
                    continue;
                }
            }
            let n = transfer.pop(&mut bytes[..available.min(wanted * 4)]) / 4;
            for (sample, word) in out.iter_mut().zip(bytes.as_chunks::<4>().0).take(n) {
                // 24-bit data is left-aligned in the 32-bit slot.
                *sample = i32::from_le_bytes(*word) >> 8;
            }
            return Ok(n);
        }
    }
}
