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

use crate::sensors::noise::AudioSource;

/// Samples converted per read. Bounds the stack buffer and the time between
/// two awaits.
const SAMPLES_PER_READ: usize = 64;

/// I2S microphone error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Error {
    /// The DMA transfer failed; it is restarted automatically.
    Dma(DmaError),
    /// The DMA transfer could not be restarted.
    Stopped,
}

/// DMA backed I2S microphone, one 32-bit slot per sample.
pub struct I2sMicrophone {
    transfer: Option<I2sRxDmaTransfer<'static, Async, DmaRxStreamBuf>>,
}

impl I2sMicrophone {
    /// Start receiving into `buffer`.
    pub fn new(rx: I2sRx<'static, Async>, buffer: DmaRxStreamBuf) -> Result<Self, Error> {
        match rx.read(buffer) {
            Ok(transfer) => Ok(Self {
                transfer: Some(transfer),
            }),
            Err(_) => Err(Error::Stopped),
        }
    }

    fn restart(&mut self) {
        if let Some(transfer) = self.transfer.take() {
            let (rx, buffer) = transfer.stop();
            self.transfer = rx.read(buffer).ok();
        }
    }
}

impl AudioSource for I2sMicrophone {
    type Error = Error;

    async fn read(&mut self, out: &mut [i32]) -> Result<usize, Error> {
        let wanted = out.len().min(SAMPLES_PER_READ);
        let mut bytes = [0u8; SAMPLES_PER_READ * 4];

        loop {
            let transfer = self.transfer.as_mut().ok_or(Error::Stopped)?;
            if let Err(e) = transfer.wait_for_available_async().await {
                self.restart();
                return Err(Error::Dma(e));
            }
            let available = transfer.available_bytes() & !3;
            if available == 0 {
                continue;
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
