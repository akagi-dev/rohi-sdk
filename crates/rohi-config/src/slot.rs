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

//! Read side of the slot format: bounded streaming parser and validation.
//!
//! Nothing here buffers a document. Slots are walked with small reads so the
//! RAM use is independent of the slot size.

use minicbor::Decoder;

use crate::crc::Crc32;
use crate::format::{
    COMMIT_WORD, COMMIT_WORD_SIZE, FORMAT_VERSION, MAX_HEAD, MAX_SECTION_PAYLOAD, MAX_SECTIONS,
    parse_bytes_head,
};
use crate::{ConfigStorage, Corruption, SectionId};

/// Streaming copy/CRC chunk size; also the commit RAM workspace.
pub(crate) const CHUNK: usize = 64;

pub(crate) enum Fault<E> {
    Storage(E),
    Corrupt(Corruption),
}

/// `true` if generation `a` is newer than `b` under wrap-around arithmetic.
pub(crate) fn is_newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

/// Bounded reader over `pos..end` of the storage.
pub(crate) struct Reader<'s, S: ConfigStorage> {
    pub storage: &'s mut S,
    pub pos: usize,
    pub end: usize,
}

impl<'s, S: ConfigStorage> Reader<'s, S> {
    pub fn new(storage: &'s mut S, pos: usize, end: usize) -> Self {
        Self { storage, pos, end }
    }

    fn remaining(&self) -> usize {
        self.end - self.pos
    }

    /// The next few bytes (at most one CBOR head) without consuming them.
    fn peek(&mut self) -> Result<([u8; MAX_HEAD], usize), Fault<S::Error>> {
        let mut buffer = [0u8; MAX_HEAD];
        let len = self.remaining().min(MAX_HEAD);
        self.storage
            .read(self.pos, &mut buffer[..len])
            .map_err(Fault::Storage)?;
        Ok((buffer, len))
    }

    /// Decodes one scalar item or container head with `minicbor`.
    fn decode<T>(
        &mut self,
        reason: Corruption,
        f: impl FnOnce(&mut Decoder<'_>) -> Result<T, minicbor::decode::Error>,
    ) -> Result<T, Fault<S::Error>> {
        let (buffer, len) = self.peek()?;
        let mut decoder = Decoder::new(&buffer[..len]);
        let value = f(&mut decoder).map_err(|_| Fault::Corrupt(reason))?;
        self.pos += decoder.position();
        Ok(value)
    }

    /// Expects a definite-length map with `entries` entries.
    pub fn map(&mut self, entries: u64, reason: Corruption) -> Result<(), Fault<S::Error>> {
        match self.decode(reason, |d| d.map())? {
            Some(len) if len == entries => Ok(()),
            _ => Err(Fault::Corrupt(reason)),
        }
    }

    /// Expects a definite-length array and returns its length.
    pub fn array(&mut self, reason: Corruption) -> Result<u64, Fault<S::Error>> {
        self.decode(reason, |d| d.array())?
            .ok_or(Fault::Corrupt(reason))
    }

    pub fn key(&mut self, key: u8, reason: Corruption) -> Result<(), Fault<S::Error>> {
        match self.decode(reason, |d| d.u8())? {
            found if found == key => Ok(()),
            _ => Err(Fault::Corrupt(reason)),
        }
    }

    pub fn u16(&mut self, reason: Corruption) -> Result<u16, Fault<S::Error>> {
        self.decode(reason, |d| d.u16())
    }

    pub fn u32(&mut self, reason: Corruption) -> Result<u32, Fault<S::Error>> {
        self.decode(reason, |d| d.u32())
    }

    pub fn u64(&mut self, reason: Corruption) -> Result<u64, Fault<S::Error>> {
        self.decode(reason, |d| d.u64())
    }

    /// Consumes a byte string head and returns the content length, which must
    /// fit into the remaining bytes. The content itself is not consumed.
    pub fn bytes_head(&mut self, reason: Corruption) -> Result<usize, Fault<S::Error>> {
        let (buffer, len) = self.peek()?;
        let (value, size) = parse_bytes_head(&buffer[..len]).ok_or(Fault::Corrupt(reason))?;
        let value = usize::try_from(value)
            .ok()
            .filter(|value| *value <= self.remaining() - size)
            .ok_or(Fault::Corrupt(reason))?;
        self.pos += size;
        Ok(value)
    }

    pub fn skip(&mut self, len: usize) {
        self.pos += len;
    }
}

/// One stored section record, located without decoding its typed payload.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Entry {
    pub id: SectionId,
    pub version: u16,
    /// Absolute offset of the record bytes (after its byte string head).
    pub record_offset: usize,
    pub record_len: usize,
    /// Absolute offset of the typed section payload.
    pub payload_offset: usize,
    pub payload_len: usize,
}

/// Walks the `SectionRecord` byte strings of a `ConfigPayload`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Records {
    pos: usize,
    end: usize,
    left: usize,
}

impl Records {
    pub fn new(start: usize, end: usize, count: usize) -> Self {
        Self {
            pos: start,
            end,
            left: count,
        }
    }

    pub fn next<S: ConfigStorage>(
        &mut self,
        storage: &mut S,
    ) -> Result<Option<Entry>, Fault<S::Error>> {
        if self.left == 0 {
            return Ok(None);
        }
        let mut payload = Reader::new(storage, self.pos, self.end);
        let record_len = payload.bytes_head(Corruption::Payload)?;
        let record_offset = payload.pos;
        let next = record_offset + record_len;

        let mut record = Reader::new(payload.storage, record_offset, next);
        if record.array(Corruption::Record)? != 3 {
            return Err(Fault::Corrupt(Corruption::Record));
        }
        let id = SectionId(record.u64(Corruption::Record)?);
        let version = record.u16(Corruption::Record)?;
        let payload_len = record.bytes_head(Corruption::Record)?;
        let payload_offset = record.pos;
        if payload_len > MAX_SECTION_PAYLOAD || payload_offset + payload_len != next {
            return Err(Fault::Corrupt(Corruption::Record));
        }

        self.pos = next;
        self.left -= 1;
        Ok(Some(Entry {
            id,
            version,
            record_offset,
            record_len,
            payload_offset,
            payload_len,
        }))
    }
}

/// A fully validated slot.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SlotInfo {
    pub generation: u32,
    pub records: Records,
    pub sections: usize,
    pub payload_len: usize,
}

pub(crate) enum SlotState {
    /// No commit word: erased, never committed or interrupted before commit.
    Uncommitted,
    Valid(SlotInfo),
    /// Committed (or, when verifying, complete) but not a valid configuration.
    Invalid(Corruption),
}

fn parse_slot<S: ConfigStorage>(
    storage: &mut S,
    base: usize,
    slot_size: usize,
) -> Result<SlotInfo, Fault<S::Error>> {
    let area_end = base + slot_size - COMMIT_WORD_SIZE;

    // SlotEnvelope { 0: bstr(payload), 1: crc32 }
    let mut envelope = Reader::new(storage, base, area_end);
    envelope.map(2, Corruption::Envelope)?;
    envelope.key(0, Corruption::Envelope)?;
    let payload_len = envelope.bytes_head(Corruption::Envelope)?;
    let payload_offset = envelope.pos;

    let mut crc = Crc32::new();
    let mut chunk = [0u8; CHUNK];
    let mut done = 0;
    while done < payload_len {
        let n = CHUNK.min(payload_len - done);
        envelope
            .storage
            .read(payload_offset + done, &mut chunk[..n])
            .map_err(Fault::Storage)?;
        crc.update(&chunk[..n]);
        done += n;
    }
    envelope.skip(payload_len);
    envelope.key(1, Corruption::Envelope)?;
    let stored_crc = envelope.u32(Corruption::Envelope)?;
    if stored_crc != crc.finish() {
        return Err(Fault::Corrupt(Corruption::Crc));
    }

    // ConfigPayload { 0: format version, 1: generation, 2: [ bstr(record)* ] }
    let payload_end = payload_offset + payload_len;
    let mut payload = Reader::new(envelope.storage, payload_offset, payload_end);
    payload.map(3, Corruption::Payload)?;
    payload.key(0, Corruption::Payload)?;
    let format_version = payload.u16(Corruption::Payload)?;
    if format_version != FORMAT_VERSION {
        return Err(Fault::Corrupt(Corruption::UnsupportedFormatVersion(
            format_version,
        )));
    }
    payload.key(1, Corruption::Payload)?;
    let generation = payload.u32(Corruption::Payload)?;
    payload.key(2, Corruption::Payload)?;
    let count = payload.array(Corruption::Payload)?;
    let sections = usize::try_from(count)
        .ok()
        .filter(|count| *count <= MAX_SECTIONS)
        .ok_or(Fault::Corrupt(Corruption::TooManySections))?;

    let records = Records::new(payload.pos, payload_end, sections);
    let mut walk = records;
    let mut ids = [0u64; MAX_SECTIONS];
    let mut seen = 0;
    let mut last_end = payload.pos;
    while let Some(entry) = walk.next(payload.storage)? {
        if ids[..seen].contains(&entry.id.0) {
            return Err(Fault::Corrupt(Corruption::DuplicateSection(entry.id)));
        }
        ids[seen] = entry.id.0;
        seen += 1;
        last_end = entry.record_offset + entry.record_len;
    }
    if last_end != payload_end {
        return Err(Fault::Corrupt(Corruption::Payload));
    }
    Ok(SlotInfo {
        generation,
        records,
        sections,
        payload_len,
    })
}

/// Classifies the slot at `base`. With `require_commit` an uncommitted slot is
/// reported as such; without it the slot is validated as written, before its
/// commit word is programmed.
pub(crate) fn inspect<S: ConfigStorage>(
    storage: &mut S,
    base: usize,
    slot_size: usize,
    require_commit: bool,
) -> Result<SlotState, S::Error> {
    if require_commit {
        let mut word = [0u8; COMMIT_WORD_SIZE];
        storage.read(base + slot_size - COMMIT_WORD_SIZE, &mut word)?;
        if u32::from_le_bytes(word) != COMMIT_WORD {
            return Ok(SlotState::Uncommitted);
        }
    }
    match parse_slot(storage, base, slot_size) {
        Ok(info) => Ok(SlotState::Valid(info)),
        Err(Fault::Corrupt(reason)) => Ok(SlotState::Invalid(reason)),
        Err(Fault::Storage(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_order_wraps() {
        assert!(is_newer(2, 1));
        assert!(!is_newer(1, 2));
        assert!(!is_newer(7, 7));
        assert!(is_newer(0, u32::MAX));
        assert!(!is_newer(u32::MAX, 0));
        assert!(is_newer(5, u32::MAX - 5));
    }
}
