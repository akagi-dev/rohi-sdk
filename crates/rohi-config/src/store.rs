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

use postcard::ser_flavors::Flavor;

use crate::crc::Crc32;
use crate::format::{
    COMMIT_MARKER, CRC_OFFSET, FORMAT_VERSION, MARKER_OFFSET, MAX_SECTION_PAYLOAD, MAX_SECTIONS,
    MAX_SLOT_SIZE, SECTION_ALIGN, SECTION_HEADER_SIZE, SLOT_HEADER_SIZE, SLOT_MAGIC, SectionHeader,
    SlotHeader, align_up,
};
use crate::{ConfigError, ConfigSection, ConfigStorage, SectionId};

/// Streaming copy/CRC chunk size; also the commit RAM workspace.
const CHUNK: usize = 64;

/// Metadata of a stored section, available without decoding its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionInfo {
    pub id: SectionId,
    pub version: u16,
    /// Payload length in bytes, without padding.
    pub length: usize,
}

#[derive(Clone, Copy, Debug)]
struct Active {
    slot: usize,
    generation: u32,
    payload_len: usize,
    sections: usize,
}

#[derive(Clone, Copy)]
struct Entry {
    raw_header: [u8; SECTION_HEADER_SIZE],
    info: SectionInfo,
    /// Absolute storage offset of the payload.
    payload_offset: usize,
    /// Payload length including padding.
    padded: usize,
}

enum Fault<E> {
    Storage(E),
    Malformed(&'static str),
}

impl<E> Fault<E> {
    fn into_config(self) -> ConfigError<E> {
        match self {
            Self::Storage(error) => ConfigError::Storage(error),
            Self::Malformed(reason) => ConfigError::Corrupt(reason),
        }
    }
}

/// `true` if generation `a` is newer than `b` under wrap-around arithmetic.
fn is_newer(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) > 0
}

fn next_entry<S: ConfigStorage>(
    storage: &mut S,
    payload_base: usize,
    cursor: &mut usize,
    end: usize,
) -> Result<Option<Entry>, Fault<S::Error>> {
    if *cursor == end {
        return Ok(None);
    }
    let body = cursor
        .checked_add(SECTION_HEADER_SIZE)
        .filter(|body| *body <= end)
        .ok_or(Fault::Malformed("truncated section header"))?;
    let mut raw_header = [0u8; SECTION_HEADER_SIZE];
    storage
        .read(payload_base + *cursor, &mut raw_header)
        .map_err(Fault::Storage)?;
    let header = SectionHeader::from_bytes(&raw_header);
    let length = usize::try_from(header.length)
        .ok()
        .filter(|length| *length <= MAX_SECTION_PAYLOAD)
        .ok_or(Fault::Malformed("section payload too large"))?;
    let padded = align_up(length).ok_or(Fault::Malformed("section length overflow"))?;
    let next = body
        .checked_add(padded)
        .filter(|next| *next <= end)
        .ok_or(Fault::Malformed("section overruns container"))?;
    let entry = Entry {
        raw_header,
        info: SectionInfo {
            id: header.id,
            version: header.version,
            length,
        },
        payload_offset: payload_base + body,
        padded,
    };
    *cursor = next;
    Ok(Some(entry))
}

struct SlotInfo {
    generation: u32,
    payload_len: usize,
    sections: usize,
}

/// Fully validates the slot at `base`: header, bounds, CRC, section structure
/// and uniqueness of section IDs. `Ok(None)` means the slot is not valid.
fn validate_slot<S: ConfigStorage>(
    storage: &mut S,
    base: usize,
    slot_size: usize,
    require_marker: bool,
) -> Result<Option<SlotInfo>, S::Error> {
    let mut raw = [0u8; SLOT_HEADER_SIZE];
    storage.read(base, &mut raw)?;
    let header = SlotHeader::from_bytes(&raw);
    if header.magic != SLOT_MAGIC
        || header.format_version != FORMAT_VERSION
        || (require_marker && header.commit_marker != COMMIT_MARKER)
    {
        return Ok(None);
    }
    let payload_len = match usize::try_from(header.payload_length) {
        Ok(len) if len <= slot_size - SLOT_HEADER_SIZE && len % SECTION_ALIGN == 0 => len,
        _ => return Ok(None),
    };

    let payload_base = base + SLOT_HEADER_SIZE;
    let mut crc = Crc32::new();
    let mut chunk = [0u8; CHUNK];
    let mut done = 0;
    while done < payload_len {
        let n = CHUNK.min(payload_len - done);
        storage.read(payload_base + done, &mut chunk[..n])?;
        crc.update(&chunk[..n]);
        done += n;
    }
    crc.update(&raw[4..CRC_OFFSET]);
    if crc.finish() != header.crc32 {
        return Ok(None);
    }

    let mut ids = [0u64; MAX_SECTIONS];
    let mut sections = 0;
    let mut cursor = 0;
    loop {
        match next_entry(storage, payload_base, &mut cursor, payload_len) {
            Ok(None) => break,
            Ok(Some(entry)) => {
                if sections == MAX_SECTIONS || ids[..sections].contains(&entry.info.id.0) {
                    return Ok(None);
                }
                ids[sections] = entry.info.id.0;
                sections += 1;
            }
            Err(Fault::Malformed(_)) => return Ok(None),
            Err(Fault::Storage(error)) => return Err(error),
        }
    }
    Ok(Some(SlotInfo {
        generation: header.generation,
        payload_len,
        sections,
    }))
}

/// Buffered, CRC-tracking writer of the container payload of the inactive slot.
struct SlotWriter<'a, S: ConfigStorage> {
    storage: &'a mut S,
    base: usize,
    limit: usize,
    flushed: usize,
    buffer: [u8; CHUNK],
    buffered: usize,
    crc: Crc32,
    fault: Option<WriteFault<S::Error>>,
}

enum WriteFault<E> {
    Storage(E),
    Full,
}

impl<'a, S: ConfigStorage> SlotWriter<'a, S> {
    fn new(storage: &'a mut S, base: usize, limit: usize) -> Self {
        Self {
            storage,
            base,
            limit,
            flushed: 0,
            buffer: [0; CHUNK],
            buffered: 0,
            crc: Crc32::new(),
            fault: None,
        }
    }

    fn position(&self) -> usize {
        self.flushed + self.buffered
    }

    fn flush(&mut self) {
        if self.fault.is_some() || self.buffered == 0 {
            return;
        }
        let data = &self.buffer[..self.buffered];
        match self.storage.write(self.base + self.flushed, data) {
            Ok(()) => {
                self.crc.update(data);
                self.flushed += self.buffered;
                self.buffered = 0;
            }
            Err(error) => self.fault = Some(WriteFault::Storage(error)),
        }
    }

    fn put(&mut self, mut data: &[u8]) {
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

    fn pad(&mut self) {
        let zeros = [0u8; SECTION_ALIGN];
        let missing = (SECTION_ALIGN - self.position() % SECTION_ALIGN) % SECTION_ALIGN;
        self.put(&zeros[..missing]);
    }

    fn check(&mut self) -> Result<(), ConfigError<S::Error>> {
        match self.fault.take() {
            None => Ok(()),
            Some(WriteFault::Storage(error)) => Err(ConfigError::Storage(error)),
            Some(WriteFault::Full) => Err(ConfigError::ContainerFull),
        }
    }
}

/// postcard flavor streaming serialized bytes straight into the slot writer.
struct Sink<'w, 'a, S: ConfigStorage>(&'w mut SlotWriter<'a, S>);

impl<S: ConfigStorage> Flavor for Sink<'_, '_, S> {
    type Output = ();

    fn try_extend(&mut self, data: &[u8]) -> postcard::Result<()> {
        self.0.put(data);
        match self.0.fault {
            None => Ok(()),
            Some(_) => Err(postcard::Error::SerializeBufferFull),
        }
    }

    fn try_push(&mut self, data: u8) -> postcard::Result<()> {
        self.try_extend(&[data])
    }

    fn finalize(self) -> postcard::Result<()> {
        Ok(())
    }
}

enum Edit {
    Set {
        id: SectionId,
        version: u16,
        len: usize,
    },
    Remove(SectionId),
    Clear,
}

/// Transactional store of independent, typed configuration sections.
///
/// The region is split into two slots (A/B). Every update streams the
/// complete new container into the inactive slot, verifies it and only then
/// writes the commit marker, so after power loss at any point either the
/// previous or the new configuration is loadable. Sections with unknown IDs
/// are copied byte-for-byte on every update.
pub struct ConfigStore<S: ConfigStorage> {
    storage: S,
    slot_size: usize,
    active: Option<Active>,
    workspace: [u8; MAX_SECTION_PAYLOAD],
}

impl<S: ConfigStorage> ConfigStore<S> {
    /// Opens the store, selecting the newest valid slot. Empty or fully
    /// corrupt storage opens as an empty configuration.
    pub fn open(mut storage: S) -> Result<Self, ConfigError<S::Error>> {
        if !S::ERASE_SIZE.is_power_of_two() {
            return Err(ConfigError::InvalidStorage(
                "erase size is not a power of two",
            ));
        }
        if !matches!(S::WRITE_SIZE, 1 | 2 | 4) {
            return Err(ConfigError::InvalidStorage("write size must be 1, 2 or 4"));
        }
        let mut slot_size = (storage.capacity() / 2).min(MAX_SLOT_SIZE);
        slot_size -= slot_size % S::ERASE_SIZE;
        if slot_size < SLOT_HEADER_SIZE + SECTION_HEADER_SIZE {
            return Err(ConfigError::InvalidStorage(
                "storage too small for two slots",
            ));
        }

        let mut best: Option<Active> = None;
        for slot in 0..2 {
            let found = validate_slot(&mut storage, slot * slot_size, slot_size, true)
                .map_err(ConfigError::Storage)?;
            if let Some(info) = found {
                let candidate = Active {
                    slot,
                    generation: info.generation,
                    payload_len: info.payload_len,
                    sections: info.sections,
                };
                if best.is_none_or(|b| is_newer(candidate.generation, b.generation)) {
                    best = Some(candidate);
                }
            }
        }
        Ok(Self {
            storage,
            slot_size,
            active: best,
            workspace: [0; MAX_SECTION_PAYLOAD],
        })
    }

    /// Generation of the active container, `None` if the store is empty.
    pub fn generation(&self) -> Option<u32> {
        self.active.map(|a| a.generation)
    }

    /// Number of sections in the active container.
    pub fn section_count(&self) -> usize {
        self.active.map_or(0, |a| a.sections)
    }

    pub fn storage(&self) -> &S {
        &self.storage
    }

    pub fn into_storage(self) -> S {
        self.storage
    }

    fn payload_base(&self, active: &Active) -> usize {
        active.slot * self.slot_size + SLOT_HEADER_SIZE
    }

    fn find(&mut self, id: SectionId) -> Result<Option<Entry>, ConfigError<S::Error>> {
        let Some(active) = self.active else {
            return Ok(None);
        };
        let base = self.payload_base(&active);
        let mut cursor = 0;
        while let Some(entry) = next_entry(&mut self.storage, base, &mut cursor, active.payload_len)
            .map_err(Fault::into_config)?
        {
            if entry.info.id == id {
                return Ok(Some(entry));
            }
        }
        Ok(None)
    }

    /// Loads section `T`, or `None` if it was never stored.
    pub fn get<T: ConfigSection>(&mut self) -> Result<Option<T>, ConfigError<S::Error>> {
        let Some(entry) = self.find(T::ID)? else {
            return Ok(None);
        };
        let payload = &mut self.workspace[..entry.info.length];
        self.storage
            .read(entry.payload_offset, payload)
            .map_err(ConfigError::Storage)?;
        let value = T::decode_versioned(entry.info.version, payload)?;
        Ok(Some(value))
    }

    /// Whether the section `T` is stored (without decoding it).
    pub fn contains<T: ConfigSection>(&mut self) -> Result<bool, ConfigError<S::Error>> {
        Ok(self.find(T::ID)?.is_some())
    }

    /// Calls `f` for every stored section, known or not, without decoding.
    pub fn for_each_section(
        &mut self,
        mut f: impl FnMut(SectionInfo),
    ) -> Result<(), ConfigError<S::Error>> {
        let Some(active) = self.active else {
            return Ok(());
        };
        let base = self.payload_base(&active);
        let mut cursor = 0;
        while let Some(entry) = next_entry(&mut self.storage, base, &mut cursor, active.payload_len)
            .map_err(Fault::into_config)?
        {
            f(entry.info);
        }
        Ok(())
    }

    /// Copies the raw payload of section `id` into `output`.
    pub fn read_raw(
        &mut self,
        id: SectionId,
        output: &mut [u8],
    ) -> Result<Option<SectionInfo>, ConfigError<S::Error>> {
        let Some(entry) = self.find(id)? else {
            return Ok(None);
        };
        let length = entry.info.length;
        if output.len() < length {
            return Err(ConfigError::BufferTooSmall { required: length });
        }
        self.storage
            .read(entry.payload_offset, &mut output[..length])
            .map_err(ConfigError::Storage)?;
        Ok(Some(entry.info))
    }

    /// Stores `value`, replacing the previous section `T` and preserving all
    /// other sections.
    pub fn set<T: ConfigSection>(&mut self, value: &T) -> Result<(), ConfigError<S::Error>> {
        let len = postcard::experimental::serialized_size(value).map_err(ConfigError::Encode)?;
        let max = T::POSTCARD_MAX_SIZE.min(MAX_SECTION_PAYLOAD);
        if len > max {
            return Err(ConfigError::SectionTooLarge {
                id: T::ID,
                len,
                max,
            });
        }
        let edit = Edit::Set {
            id: T::ID,
            version: T::VERSION,
            len,
        };
        self.commit(edit, |writer| {
            value.serialize(&mut postcard::Serializer {
                output: Sink(writer),
            })
        })
    }

    /// Removes section `T`; a no-op if it is absent.
    pub fn remove<T: ConfigSection>(&mut self) -> Result<(), ConfigError<S::Error>> {
        self.remove_raw(T::ID)
    }

    /// Removes the section with the given ID, known or unknown to this firmware.
    pub fn remove_raw(&mut self, id: SectionId) -> Result<(), ConfigError<S::Error>> {
        self.commit(Edit::Remove(id), |_| Ok(()))
    }

    /// Removes every section, including unknown ones.
    pub fn clear(&mut self) -> Result<(), ConfigError<S::Error>> {
        self.commit(Edit::Clear, |_| Ok(()))
    }

    fn commit<F>(&mut self, edit: Edit, serialize: F) -> Result<(), ConfigError<S::Error>>
    where
        F: FnOnce(&mut SlotWriter<'_, S>) -> postcard::Result<()>,
    {
        let slot_size = self.slot_size;
        let limit = slot_size - SLOT_HEADER_SIZE;
        let (target, source) = match self.active {
            Some(active) => (1 - active.slot, Some(active)),
            None => (0, None),
        };
        let new_size = match edit {
            Edit::Set { len, .. } => {
                SECTION_HEADER_SIZE + align_up(len).ok_or(ConfigError::ContainerFull)?
            }
            _ => 0,
        };

        // Plan: compute the resulting container size without writing anything.
        let mut total = 0usize;
        let mut count = 0usize;
        let mut replaced = false;
        let mut removed = false;
        if let Some(active) = source {
            let base = self.payload_base(&active);
            let mut cursor = 0;
            while let Some(entry) =
                next_entry(&mut self.storage, base, &mut cursor, active.payload_len)
                    .map_err(Fault::into_config)?
            {
                let size = match edit {
                    Edit::Set { id, .. } if id == entry.info.id => {
                        replaced = true;
                        new_size
                    }
                    Edit::Remove(id) if id == entry.info.id => {
                        removed = true;
                        continue;
                    }
                    Edit::Clear => {
                        removed = true;
                        continue;
                    }
                    _ => SECTION_HEADER_SIZE + entry.padded,
                };
                total = total.checked_add(size).ok_or(ConfigError::ContainerFull)?;
                count += 1;
            }
        }
        match edit {
            Edit::Set { .. } if !replaced => {
                total = total
                    .checked_add(new_size)
                    .ok_or(ConfigError::ContainerFull)?;
                count += 1;
            }
            Edit::Remove(_) | Edit::Clear if !removed => return Ok(()),
            _ => {}
        }
        if total > limit {
            return Err(ConfigError::ContainerFull);
        }
        if count > MAX_SECTIONS {
            return Err(ConfigError::TooManySections);
        }

        // Write the complete new container into the inactive slot.
        let target_base = target * slot_size;
        self.storage
            .erase(target_base, slot_size)
            .map_err(ConfigError::Storage)?;

        let generation = source.map_or(1, |a| a.generation.wrapping_add(1));
        let mut serialize = Some(serialize);
        let mut writer = SlotWriter::new(&mut self.storage, target_base + SLOT_HEADER_SIZE, limit);

        if let Some(active) = source {
            let base = active.slot * slot_size + SLOT_HEADER_SIZE;
            let mut cursor = 0;
            while let Some(entry) =
                next_entry(writer.storage, base, &mut cursor, active.payload_len)
                    .map_err(Fault::into_config)?
            {
                match edit {
                    Edit::Set { id, version, len } if id == entry.info.id => {
                        write_section(&mut writer, id, version, len, serialize.take())?;
                    }
                    Edit::Remove(id) if id == entry.info.id => {}
                    Edit::Clear => {}
                    _ => copy_section(&mut writer, &entry)?,
                }
            }
        }
        if let Edit::Set { id, version, len } = edit
            && !replaced
        {
            write_section(&mut writer, id, version, len, serialize.take())?;
        }
        writer.pad();
        writer.flush();
        writer.check()?;

        let payload_len = writer.position();
        let mut crc = writer.crc;
        drop(writer);

        let mut header = SlotHeader {
            magic: SLOT_MAGIC,
            format_version: FORMAT_VERSION,
            generation,
            // Bounded by the slot size, which is at most `MAX_SLOT_SIZE`.
            payload_length: payload_len as u32,
            crc32: 0,
            commit_marker: u32::MAX,
        };
        crc.update(&header.to_bytes()[4..CRC_OFFSET]);
        header.crc32 = crc.finish();
        self.storage
            .write(target_base, &header.to_bytes()[..MARKER_OFFSET])
            .map_err(ConfigError::Storage)?;

        let verified = validate_slot(&mut self.storage, target_base, slot_size, false)
            .map_err(ConfigError::Storage)?;
        match verified {
            Some(info) if info.generation == generation && info.payload_len == payload_len => {}
            _ => return Err(ConfigError::VerifyFailed),
        }

        // The commit marker is written last: it makes the slot valid.
        self.storage
            .write(target_base + MARKER_OFFSET, &COMMIT_MARKER.to_le_bytes())
            .map_err(ConfigError::Storage)?;
        self.active = Some(Active {
            slot: target,
            generation,
            payload_len,
            sections: count,
        });
        Ok(())
    }
}

fn write_section<S, F>(
    writer: &mut SlotWriter<'_, S>,
    id: SectionId,
    version: u16,
    len: usize,
    serialize: Option<F>,
) -> Result<(), ConfigError<S::Error>>
where
    S: ConfigStorage,
    F: FnOnce(&mut SlotWriter<'_, S>) -> postcard::Result<()>,
{
    let header = SectionHeader {
        id,
        version,
        // Bounded by `MAX_SECTION_PAYLOAD` in `set`.
        length: len as u32,
    };
    writer.put(&header.to_bytes());
    let start = writer.position();
    if let Some(serialize) = serialize {
        let result = serialize(writer);
        writer.check()?;
        result.map_err(ConfigError::Encode)?;
    }
    if writer.position() - start != len {
        return Err(ConfigError::EncodeMismatch { id });
    }
    writer.pad();
    writer.check()
}

/// Copies a section verbatim (header, payload and padding) without decoding it.
fn copy_section<S: ConfigStorage>(
    writer: &mut SlotWriter<'_, S>,
    entry: &Entry,
) -> Result<(), ConfigError<S::Error>> {
    writer.put(&entry.raw_header);
    let mut chunk = [0u8; CHUNK];
    let mut done = 0;
    while done < entry.padded {
        let n = CHUNK.min(entry.padded - done);
        writer
            .storage
            .read(entry.payload_offset + done, &mut chunk[..n])
            .map_err(ConfigError::Storage)?;
        writer.put(&chunk[..n]);
        writer.check()?;
        done += n;
    }
    Ok(())
}
