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

use core::fmt;

use minicbor::Encoder;

use crate::format::{
    COMMIT_WORD, COMMIT_WORD_SIZE, ERASED_WORD, FORMAT_VERSION, INVALIDATED_WORD,
    MAX_SECTION_PAYLOAD, MAX_SECTIONS, MAX_SLOT_SIZE, bytes_head, head_len,
};
use crate::slot::{CHUNK, Entry, Fault, SlotInfo, SlotState, inspect, is_newer};
use crate::writer::{SlotWriter, WriteAbort};
use crate::{ConfigError, ConfigSection, ConfigStorage, Corruption, EncodeError, SectionId};

/// Smallest usable slot: envelope, payload head and the commit word.
const MIN_SLOT_SIZE: usize = 64;

/// Metadata of a stored section, available without decoding its payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionInfo {
    pub id: SectionId,
    pub version: u16,
    /// Typed payload length in bytes.
    pub length: usize,
}

impl From<Entry> for SectionInfo {
    fn from(entry: Entry) -> Self {
        Self {
            id: entry.id,
            version: entry.version,
            length: entry.payload_len,
        }
    }
}

/// Health of the configuration storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigOpenState {
    /// Neither slot holds a committed configuration: a fresh (erased) device,
    /// or a first write interrupted before its commit word was programmed.
    Empty,
    /// Every committed slot validated; the newest one is active.
    Valid,
    /// One committed slot is invalid and the other, valid, slot is active.
    /// The damaged slot's generation cannot be read, so the active
    /// configuration may be older than the one that was lost. The next
    /// successful update overwrites the damaged slot.
    Recovered,
}

/// A failed [`ConfigStore::try_open`] or [`ConfigStore::format`], handing the
/// storage back so that the caller can still recover it (for example format
/// corrupted configuration).
pub struct OpenFailure<S: ConfigStorage> {
    pub storage: S,
    pub error: ConfigError<S::Error>,
}

impl<S: ConfigStorage> fmt::Debug for OpenFailure<S>
where
    S::Error: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
struct Active {
    slot: usize,
    info: SlotInfo,
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

/// Size of a `SectionRecord` array: head, ID, version and payload byte string.
fn record_len(id: SectionId, version: u16, payload_len: usize) -> usize {
    1 + head_len(id.0) + head_len(u64::from(version)) + head_len(payload_len as u64) + payload_len
}

/// Size of a record inside the `ConfigPayload`: byte string head plus record.
fn record_total(record_len: usize) -> usize {
    head_len(record_len as u64) + record_len
}

fn encoded<S, F>(writer: &mut SlotWriter<'_, S>, f: F) -> Result<(), ConfigError<S::Error>>
where
    S: ConfigStorage,
    F: FnOnce(
        &mut Encoder<&mut SlotWriter<'_, S>>,
    ) -> Result<(), minicbor::encode::Error<WriteAbort>>,
{
    let result = f(&mut Encoder::new(&mut *writer));
    // A storage fault or a full slot explains an aborted encode.
    writer.check()?;
    result.map_err(|_| ConfigError::Encode(EncodeError::Failed))
}

/// Transactional store of independent, typed configuration sections.
///
/// The region is split into two slots (A/B). Every update streams the
/// complete new CBOR document into the inactive slot, verifies it and only then
/// programs the commit word, so after power loss at any point either the
/// previous or the new configuration is loadable. Sections with unknown IDs are
/// copied byte-for-byte on every update.
pub struct ConfigStore<S: ConfigStorage> {
    storage: S,
    slot_size: usize,
    active: Option<Active>,
    open_state: ConfigOpenState,
    /// The inactive slot is committed but invalid.
    damaged: bool,
}

/// Validates the storage geometry and returns the slot size.
fn slot_size<S: ConfigStorage>(storage: &S) -> Result<usize, ConfigError<S::Error>> {
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
    if slot_size < MIN_SLOT_SIZE {
        return Err(ConfigError::InvalidStorage(
            "storage too small for two slots",
        ));
    }
    if !slot_size.is_multiple_of(COMMIT_WORD_SIZE) {
        return Err(ConfigError::InvalidStorage(
            "slot size is not a multiple of the commit word",
        ));
    }
    Ok(slot_size)
}

/// Makes the slot at `base` uncommitted by programming its commit word to
/// [`INVALIDATED_WORD`]. NOR programming only clears bits, so this works on a
/// committed or torn word and does not depend on how an erase proceeds.
fn invalidate<S: ConfigStorage>(
    storage: &mut S,
    base: usize,
    slot_size: usize,
) -> Result<(), S::Error> {
    let offset = base + slot_size - COMMIT_WORD_SIZE;
    let mut word = [0u8; COMMIT_WORD_SIZE];
    storage.read(offset, &mut word)?;
    match u32::from_le_bytes(word) {
        ERASED_WORD | INVALIDATED_WORD => Ok(()),
        _ => storage.write(offset, &INVALIDATED_WORD.to_le_bytes()),
    }
}

fn erase_slot<S: ConfigStorage>(
    storage: &mut S,
    base: usize,
    slot_size: usize,
) -> Result<(), S::Error> {
    let mut offset = 0;
    while offset < slot_size {
        storage.erase(base + offset, S::ERASE_SIZE)?;
        offset += S::ERASE_SIZE;
    }
    Ok(())
}

impl<S: ConfigStorage> ConfigStore<S> {
    /// Opens the store, selecting the newest valid committed slot.
    ///
    /// Erased storage opens as [`ConfigOpenState::Empty`]. If committed
    /// configuration exists but no committed slot validates this fails with
    /// [`ConfigError::CorruptStorage`] instead of pretending to be empty.
    ///
    /// The storage is dropped on failure; use [`try_open`](Self::try_open) to
    /// get it back, for example to [`format`](Self::format) it.
    pub fn open(storage: S) -> Result<Self, ConfigError<S::Error>> {
        Self::try_open(storage).map_err(|failure| failure.error)
    }

    /// Like [`open`](Self::open), but returns the storage with the error.
    pub fn try_open(mut storage: S) -> Result<Self, OpenFailure<S>> {
        match Self::scan(&mut storage) {
            Ok((slot_size, active, open_state)) => Ok(Self {
                storage,
                slot_size,
                active,
                open_state,
                damaged: open_state == ConfigOpenState::Recovered,
            }),
            Err(error) => Err(OpenFailure { storage, error }),
        }
    }

    fn scan(
        storage: &mut S,
    ) -> Result<(usize, Option<Active>, ConfigOpenState), ConfigError<S::Error>> {
        let slot_size = slot_size(storage)?;
        let mut best: Option<Active> = None;
        let mut damaged: Option<Corruption> = None;
        for slot in 0..2 {
            match inspect(storage, slot * slot_size, slot_size, true)
                .map_err(ConfigError::Storage)?
            {
                SlotState::Uncommitted => {}
                SlotState::Invalid(reason) => damaged = damaged.or(Some(reason)),
                SlotState::Valid(info) => {
                    if best.is_none_or(|b| is_newer(info.generation, b.info.generation)) {
                        best = Some(Active { slot, info });
                    }
                }
            }
        }
        let open_state = match (best, damaged) {
            (Some(_), None) => ConfigOpenState::Valid,
            (Some(_), Some(_)) => ConfigOpenState::Recovered,
            (None, None) => ConfigOpenState::Empty,
            (None, Some(reason)) => return Err(ConfigError::CorruptStorage(reason)),
        };
        Ok((slot_size, best, open_state))
    }

    /// Erases the whole configuration region, valid, unknown or corrupted,
    /// and returns an empty store. This is the explicit way out of
    /// [`ConfigError::CorruptStorage`]; it discards every section.
    ///
    /// Both commit words are invalidated before anything is erased, so after
    /// an interruption the storage opens as empty, or as it was if not even
    /// the first commit word was invalidated. Repeating `format` completes it.
    pub fn format(mut storage: S) -> Result<Self, OpenFailure<S>> {
        let result = slot_size(&storage).and_then(|slot_size| {
            for slot in 0..2 {
                invalidate(&mut storage, slot * slot_size, slot_size)
                    .map_err(ConfigError::Storage)?;
            }
            for slot in 0..2 {
                erase_slot(&mut storage, slot * slot_size, slot_size)
                    .map_err(ConfigError::Storage)?;
            }
            Ok(slot_size)
        });
        match result {
            Ok(slot_size) => Ok(Self {
                storage,
                slot_size,
                active: None,
                open_state: ConfigOpenState::Empty,
                damaged: false,
            }),
            Err(error) => Err(OpenFailure { storage, error }),
        }
    }

    /// How the storage looked when the store was opened (or formatted).
    pub fn open_state(&self) -> ConfigOpenState {
        self.open_state
    }

    /// Current health: like [`open_state`](Self::open_state), but a
    /// [`Recovered`](ConfigOpenState::Recovered) store becomes
    /// [`Valid`](ConfigOpenState::Valid) once an update overwrote the damaged
    /// slot, and an empty store becomes valid after its first update.
    pub fn state(&self) -> ConfigOpenState {
        match (self.active, self.damaged) {
            (None, _) => ConfigOpenState::Empty,
            (Some(_), true) => ConfigOpenState::Recovered,
            (Some(_), false) => ConfigOpenState::Valid,
        }
    }

    /// Generation of the active configuration, `None` if the store is empty.
    pub fn generation(&self) -> Option<u32> {
        self.active.map(|a| a.info.generation)
    }

    /// Slot (0 = A, 1 = B) holding the active configuration, for diagnostics.
    pub fn active_slot(&self) -> Option<usize> {
        self.active.map(|a| a.slot)
    }

    /// Number of sections in the active configuration.
    pub fn section_count(&self) -> usize {
        self.active.map_or(0, |a| a.info.sections)
    }

    pub fn storage(&self) -> &S {
        &self.storage
    }

    pub fn into_storage(self) -> S {
        self.storage
    }

    fn find(&mut self, id: SectionId) -> Result<Option<Entry>, ConfigError<S::Error>> {
        let Some(active) = self.active else {
            return Ok(None);
        };
        let mut records = active.info.records;
        while let Some(entry) = records.next(&mut self.storage).map_err(into_config)? {
            if entry.id == id {
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
        let mut workspace = [0u8; MAX_SECTION_PAYLOAD];
        let payload = &mut workspace[..entry.payload_len];
        self.storage
            .read(entry.payload_offset, payload)
            .map_err(ConfigError::Storage)?;
        let value = T::decode_versioned(entry.version, payload)?;
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
        let mut records = active.info.records;
        while let Some(entry) = records.next(&mut self.storage).map_err(into_config)? {
            f(entry.into());
        }
        Ok(())
    }

    /// Copies the raw typed payload of section `id` into `output`.
    pub fn read_raw(
        &mut self,
        id: SectionId,
        output: &mut [u8],
    ) -> Result<Option<SectionInfo>, ConfigError<S::Error>> {
        let Some(entry) = self.find(id)? else {
            return Ok(None);
        };
        let length = entry.payload_len;
        if output.len() < length {
            return Err(ConfigError::BufferTooSmall { required: length });
        }
        self.storage
            .read(entry.payload_offset, &mut output[..length])
            .map_err(ConfigError::Storage)?;
        Ok(Some(entry.into()))
    }

    /// Stores `value`, replacing the previous section `T` and preserving all
    /// other sections. The encoded size is taken from `minicbor::len`.
    pub fn set<T: ConfigSection>(&mut self, value: &T) -> Result<(), ConfigError<S::Error>> {
        let len = minicbor::len(value);
        self.check_section_size(T::ID, len)?;
        let edit = Edit::Set {
            id: T::ID,
            version: T::VERSION,
            len,
        };
        self.commit(edit, |writer| {
            minicbor::encode(value, &mut *writer).map_err(|_| EncodeError::Failed)
        })
    }

    /// Stores an already encoded section payload under `id`.
    ///
    /// The payload is stored as is; typed code is responsible for it being a
    /// valid encoding of the section type that owns `id`.
    pub fn set_raw(
        &mut self,
        id: SectionId,
        version: u16,
        payload: &[u8],
    ) -> Result<(), ConfigError<S::Error>> {
        self.check_section_size(id, payload.len())?;
        let edit = Edit::Set {
            id,
            version,
            len: payload.len(),
        };
        self.commit(edit, |writer| {
            writer.put(payload);
            Ok(())
        })
    }

    fn check_section_size(&self, id: SectionId, len: usize) -> Result<(), ConfigError<S::Error>> {
        if len > MAX_SECTION_PAYLOAD {
            return Err(ConfigError::SectionTooLarge {
                id,
                len,
                max: MAX_SECTION_PAYLOAD,
            });
        }
        Ok(())
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
    ///
    /// Low-level: runtime APIs must not expose this as a generic reset, since
    /// it also erases sections the firmware does not know about.
    pub fn clear(&mut self) -> Result<(), ConfigError<S::Error>> {
        self.commit(Edit::Clear, |_| Ok(()))
    }

    fn commit<F>(&mut self, edit: Edit, fill: F) -> Result<(), ConfigError<S::Error>>
    where
        F: FnOnce(&mut SlotWriter<'_, S>) -> Result<(), EncodeError>,
    {
        let slot_size = self.slot_size;
        let limit = slot_size - COMMIT_WORD_SIZE;
        let (target, source) = match self.active {
            Some(active) => (1 - active.slot, Some(active)),
            None => (0, None),
        };
        let new_record = match edit {
            Edit::Set { id, version, len } => record_total(record_len(id, version, len)),
            _ => 0,
        };

        // Plan: compute the exact size of the resulting document without
        // touching the flash.
        let mut records_total = 0usize;
        let mut count = 0usize;
        let mut replaced = false;
        let mut removed = false;
        if let Some(active) = source {
            let mut records = active.info.records;
            while let Some(entry) = records.next(&mut self.storage).map_err(into_config)? {
                let size = match edit {
                    Edit::Set { id, .. } if id == entry.id => {
                        replaced = true;
                        new_record
                    }
                    Edit::Remove(id) if id == entry.id => {
                        removed = true;
                        continue;
                    }
                    Edit::Clear => {
                        removed = true;
                        continue;
                    }
                    _ => record_total(entry.record_len),
                };
                records_total += size;
                count += 1;
            }
        }
        match edit {
            Edit::Set { .. } if !replaced => {
                records_total += new_record;
                count += 1;
            }
            Edit::Remove(_) | Edit::Clear if !removed => return Ok(()),
            _ => {}
        }
        if count > MAX_SECTIONS {
            return Err(ConfigError::TooManySections);
        }

        let generation = source.map_or(1, |a| a.info.generation.wrapping_add(1));
        let payload_len = 1
            + (1 + head_len(u64::from(FORMAT_VERSION)))
            + (1 + head_len(u64::from(generation)))
            + (1 + head_len(count as u64))
            + records_total;
        // The CRC is encoded last; reserve its largest encoding.
        let envelope_len = 1 + 1 + head_len(payload_len as u64) + payload_len + 1 + 5;
        if envelope_len > limit {
            return Err(ConfigError::StorageFull);
        }

        // Make the inactive slot uncommitted before erasing it: an interrupted
        // erase then never leaves a committed but half-erased slot behind,
        // whatever order the flash erases its cells in.
        let target_base = target * slot_size;
        invalidate(&mut self.storage, target_base, slot_size).map_err(ConfigError::Storage)?;
        erase_slot(&mut self.storage, target_base, slot_size).map_err(ConfigError::Storage)?;

        let mut fill = Some(fill);
        let mut writer = SlotWriter::new(&mut self.storage, target_base, limit);

        encoded(&mut writer, |e| {
            e.map(2)?.u8(0)?;
            Ok(())
        })?;
        let (head, size) = bytes_head(payload_len);
        writer.put(&head[..size]);
        writer.check()?;

        let payload_start = writer.position();
        writer.start_crc();
        encoded(&mut writer, |e| {
            e.map(3)?
                .u8(0)?
                .u16(FORMAT_VERSION)?
                .u8(1)?
                .u32(generation)?
                .u8(2)?
                .array(count as u64)?;
            Ok(())
        })?;

        if let Some(active) = source {
            let mut records = active.info.records;
            while let Some(entry) = records.next(writer.storage).map_err(into_config)? {
                match edit {
                    Edit::Set { id, version, len } if id == entry.id => {
                        write_record(&mut writer, id, version, len, fill.take())?;
                    }
                    Edit::Remove(id) if id == entry.id => {}
                    Edit::Clear => {}
                    _ => copy_record(&mut writer, &entry)?,
                }
            }
        }
        if let Edit::Set { id, version, len } = edit
            && !replaced
        {
            write_record(&mut writer, id, version, len, fill.take())?;
        }
        if writer.position() - payload_start != payload_len {
            return Err(ConfigError::VerifyFailed);
        }

        let crc = writer.finish_crc();
        encoded(&mut writer, |e| {
            e.u8(1)?.u32(crc)?;
            Ok(())
        })?;
        writer.finish();
        writer.check()?;
        drop(writer);

        // Read the whole candidate back before it can become active.
        match inspect(&mut self.storage, target_base, slot_size, false)
            .map_err(ConfigError::Storage)?
        {
            SlotState::Valid(info)
                if info.generation == generation
                    && info.payload_len == payload_len
                    && info.sections == count => {}
            _ => return Err(ConfigError::VerifyFailed),
        }

        // The commit word is written last: it makes the slot valid.
        self.storage
            .write(
                target_base + slot_size - COMMIT_WORD_SIZE,
                &COMMIT_WORD.to_le_bytes(),
            )
            .map_err(ConfigError::Storage)?;
        let info = match inspect(&mut self.storage, target_base, slot_size, true)
            .map_err(ConfigError::Storage)?
        {
            SlotState::Valid(info) => info,
            _ => return Err(ConfigError::VerifyFailed),
        };
        self.active = Some(Active { slot: target, info });
        // The damaged slot, if any, was the inactive one and is now replaced.
        self.damaged = false;
        Ok(())
    }
}

fn into_config<E>(fault: Fault<E>) -> ConfigError<E> {
    match fault {
        Fault::Storage(error) => ConfigError::Storage(error),
        Fault::Corrupt(reason) => ConfigError::CorruptStorage(reason),
    }
}

fn write_record<S, F>(
    writer: &mut SlotWriter<'_, S>,
    id: SectionId,
    version: u16,
    payload_len: usize,
    fill: Option<F>,
) -> Result<(), ConfigError<S::Error>>
where
    S: ConfigStorage,
    F: FnOnce(&mut SlotWriter<'_, S>) -> Result<(), EncodeError>,
{
    let len = record_len(id, version, payload_len);
    let (head, size) = bytes_head(len);
    writer.put(&head[..size]);
    let start = writer.position();
    encoded(writer, |e| {
        e.array(3)?.u64(id.0)?.u16(version)?;
        Ok(())
    })?;
    let (head, size) = bytes_head(payload_len);
    writer.put(&head[..size]);
    if let Some(fill) = fill {
        let result = fill(writer);
        writer.check()?;
        result.map_err(ConfigError::Encode)?;
    }
    writer.check()?;
    if writer.position() - start != len {
        return Err(ConfigError::EncodeMismatch { id });
    }
    Ok(())
}

/// Copies a record verbatim without decoding it.
fn copy_record<S: ConfigStorage>(
    writer: &mut SlotWriter<'_, S>,
    entry: &Entry,
) -> Result<(), ConfigError<S::Error>> {
    let (head, size) = bytes_head(entry.record_len);
    writer.put(&head[..size]);
    let mut chunk = [0u8; CHUNK];
    let mut done = 0;
    while done < entry.record_len {
        let n = CHUNK.min(entry.record_len - done);
        writer
            .storage
            .read(entry.record_offset + done, &mut chunk[..n])
            .map_err(ConfigError::Storage)?;
        writer.put(&chunk[..n]);
        writer.check()?;
        done += n;
    }
    Ok(())
}
