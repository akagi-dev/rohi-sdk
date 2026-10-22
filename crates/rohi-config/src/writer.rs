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

//! Write side: buffered, CRC-tracking writer streaming into the inactive slot.

use minicbor::encode::Write;

use crate::crc::Crc32;
use crate::slot::CHUNK;
use crate::{ConfigError, ConfigStorage};

/// Alignment of the final flash write; the slot tail is left erased (`0xFF`).
const PAD_ALIGN: usize = 4;

pub(crate) enum WriteFault<E> {
    Storage(E),
    Full,
}

/// Error type of the `minicbor` write sink; the cause is kept in the writer.
#[derive(Debug)]
pub(crate) struct WriteAbort;

pub(crate) struct SlotWriter<'a, S: ConfigStorage> {
    pub storage: &'a mut S,
    base: usize,
    limit: usize,
    flushed: usize,
    buffer: [u8; CHUNK],
    buffered: usize,
    crc: Crc32,
    crc_active: bool,
    fault: Option<WriteFault<S::Error>>,
}

impl<'a, S: ConfigStorage> SlotWriter<'a, S> {
    /// Writes at `base`, failing with [`WriteFault::Full`] beyond `limit` bytes.
    pub fn new(storage: &'a mut S, base: usize, limit: usize) -> Self {
        Self {
            storage,
            base,
            limit,
            flushed: 0,
            buffer: [0; CHUNK],
            buffered: 0,
            crc: Crc32::new(),
            crc_active: false,
            fault: None,
        }
    }

    pub fn position(&self) -> usize {
        self.flushed + self.buffered
    }

    /// Starts accumulating the CRC over the bytes written from now on.
    pub fn start_crc(&mut self) {
        self.crc = Crc32::new();
        self.crc_active = true;
    }

    /// Stops the CRC accumulation and returns the CRC of the covered bytes.
    pub fn finish_crc(&mut self) -> u32 {
        self.crc_active = false;
        self.crc.finish()
    }

    fn flush(&mut self) {
        if self.fault.is_some() || self.buffered == 0 {
            return;
        }
        let data = &self.buffer[..self.buffered];
        match self.storage.write(self.base + self.flushed, data) {
            Ok(()) => {
                self.flushed += self.buffered;
                self.buffered = 0;
            }
            Err(error) => self.fault = Some(WriteFault::Storage(error)),
        }
    }

    pub fn put(&mut self, mut data: &[u8]) {
        if self.fault.is_some() {
            return;
        }
        if self
            .position()
            .checked_add(data.len())
            .is_none_or(|end| end > self.limit)
        {
            self.fault = Some(WriteFault::Full);
            return;
        }
        if self.crc_active {
            self.crc.update(data);
        }
        while !data.is_empty() && self.fault.is_none() {
            let n = (CHUNK - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + n].copy_from_slice(&data[..n]);
            self.buffered += n;
            data = &data[n..];
            if self.buffered == CHUNK {
                self.flush();
            }
        }
    }

    /// Pads to the flash write granularity with erased bytes and flushes.
    pub fn finish(&mut self) {
        let missing = (PAD_ALIGN - self.position() % PAD_ALIGN) % PAD_ALIGN;
        self.crc_active = false;
        self.put(&[0xFF; PAD_ALIGN][..missing]);
        self.flush();
    }

    pub fn check(&mut self) -> Result<(), ConfigError<S::Error>> {
        match self.fault.take() {
            None => Ok(()),
            Some(WriteFault::Storage(error)) => Err(ConfigError::Storage(error)),
            Some(WriteFault::Full) => Err(ConfigError::StorageFull),
        }
    }
}

impl<S: ConfigStorage> Write for SlotWriter<'_, S> {
    type Error = WriteAbort;

    fn write_all(&mut self, buf: &[u8]) -> Result<(), WriteAbort> {
        self.put(buf);
        match self.fault {
            None => Ok(()),
            Some(_) => Err(WriteAbort),
        }
    }
}
