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

//! File-backed NOR flash for the configuration store.
//!
//! The real `rohi-config` store runs on it unchanged: it sees a
//! [`ConfigStorage`] with the geometry of the Altruist Urban `rohi-cfg`
//! partition (two 8 KiB slots of 4 KiB erase sectors) and NOR semantics:
//! erase sets bytes to `0xFF`, programming may only clear bits, accesses
//! are aligned and bounded. The semantics and the fault injection are those of
//! [`MemoryStorage`], which the config crate tests itself with; this module only
//! adds the file behind it. The file is the raw region image, nothing else:
//!
//! ```text
//! rohi-config store -> FileFlash -> MemoryStorage (NOR rules, faults)
//!                                -> .rohi-sim/device.flash (region image)
//! ```
//!
//! Every write and erase reaches the file at once, so a process that dies
//! leaves exactly what a power loss would: whatever was programmed so far.
//! Interrupted updates are therefore recovered by the real A/B + commit-last
//! logic of the store at the next start.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use log::{error, info, warn};
use rohi_config::{COMMIT_WORD, COMMIT_WORD_SIZE, ConfigStorage, MemoryError, MemoryStorage};

use crate::command::StorageFault;

/// Erase sector, as on the ESP.
pub const SECTOR_SIZE: usize = 4096;
/// One A/B slot.
pub const SLOT_SIZE: usize = 2 * SECTOR_SIZE;
/// The whole region: two slots.
pub const REGION_SIZE: usize = 2 * SLOT_SIZE;

/// Exit status of a simulated power loss.
pub const POWER_LOSS_EXIT: i32 = 70;

type Memory = MemoryStorage<REGION_SIZE, SECTOR_SIZE>;

/// A failed flash access.
#[derive(Debug, PartialEq, Eq)]
pub enum StorageError {
    /// The NOR rules or the injected fault stopped the access.
    Flash(MemoryError),
    /// The file behind the flash failed.
    File(std::io::ErrorKind),
    /// The simulated power is off.
    PowerLost,
}

/// What happens when an injected power loss strikes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerLoss {
    /// The process ends at once, as the device would stop.
    Exit,
    /// The flash stays dead: every later program or erase fails. For tests.
    Freeze,
}

/// The flash could not be opened.
#[derive(Debug)]
pub struct OpenError(pub String);

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OpenError {}

struct Inner {
    memory: Memory,
    file: Option<File>,
    path: Option<PathBuf>,
    power_loss: PowerLoss,
    dead: bool,
    last_commit_slot: Option<usize>,
}

/// The configuration flash of the simulated device. Clones share the flash:
/// the owner task of the config store holds one, the management interface
/// another for fault injection.
#[derive(Clone)]
pub struct FileFlash {
    inner: Arc<Mutex<Inner>>,
}

impl fmt::Debug for FileFlash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileFlash")
            .field("path", &self.path())
            .finish_non_exhaustive()
    }
}

impl FileFlash {
    /// A flash that lives only in memory.
    pub fn ephemeral() -> Self {
        Self::from_parts(Memory::new(), None, None)
    }

    /// The flash stored in `path`, created erased if it does not exist.
    ///
    /// # Errors
    ///
    /// The file cannot be created, read or locked, or is not a region image
    /// of this size. The file is locked while the flash is open, so two
    /// two processes cannot share a storage file.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let fail = |what: &str, error: &dyn fmt::Display| {
            OpenError(format!("storage {}: {what}: {error}", path.display()))
        };
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|e| fail("cannot create the directory", &e))?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| fail("cannot open", &e))?;
        file.try_lock()
            .map_err(|_| fail("in use", &"another process uses this storage file"))?;
        let length = file.metadata().map_err(|e| fail("cannot stat", &e))?.len();
        let mut memory = Memory::new();
        if length == 0 {
            file.write_all(memory.as_bytes())
                .map_err(|e| fail("cannot initialize", &e))?;
            info!("Storage: created {}", path.display());
        } else if length == REGION_SIZE as u64 {
            file.read_exact(memory.as_bytes_mut())
                .map_err(|e| fail("cannot read", &e))?;
            info!("Storage: opened {}", path.display());
        } else {
            return Err(fail(
                "not a flash image",
                &format!("{length} bytes, expected {REGION_SIZE}"),
            ));
        }
        Ok(Self::from_parts(memory, Some(file), Some(path.to_owned())))
    }

    fn from_parts(memory: Memory, file: Option<File>, path: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                memory,
                file,
                path,
                power_loss: PowerLoss::Exit,
                dead: false,
                last_commit_slot: None,
            })),
        }
    }

    /// Sets what an injected power loss does.
    pub fn set_power_loss(&self, power_loss: PowerLoss) {
        self.lock().power_loss = power_loss;
    }

    /// The file behind the flash, if any.
    pub fn path(&self) -> Option<PathBuf> {
        self.lock().path.clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Makes everything written so far durable.
    pub fn flush(&self) {
        let inner = self.lock();
        if let Some(file) = &inner.file
            && let Err(e) = file.sync_all()
        {
            warn!("Storage: cannot sync: {e}");
        }
    }

    /// The raw region image.
    pub fn image(&self) -> Vec<u8> {
        self.lock().memory.as_bytes().to_vec()
    }

    /// Injects `fault`.
    ///
    /// # Errors
    ///
    /// A corruption of a flash that holds no committed configuration.
    pub fn inject(&self, fault: StorageFault) -> Result<(), String> {
        let mut inner = self.lock();
        match fault {
            StorageFault::PowerLossProgram { bytes } => {
                inner.memory.fail_write_after(bytes);
                warn!("Storage: power loss armed after {bytes} programmed byte(s)");
                Ok(())
            }
            StorageFault::PowerLossErase { bytes } => {
                inner.memory.fail_erase_after(bytes);
                warn!("Storage: power loss armed after {bytes} erased byte(s)");
                Ok(())
            }
            StorageFault::CorruptCrc => inner.corrupt(|slot| slot * SLOT_SIZE + 16, 0x01),
            StorageFault::CorruptCommit => {
                inner.corrupt(|slot| (slot + 1) * SLOT_SIZE - COMMIT_WORD_SIZE, 0x55)
            }
        }
    }
}

impl Inner {
    fn committed_slots(&self) -> Vec<usize> {
        (0..2)
            .filter(|slot| {
                let end = (slot + 1) * SLOT_SIZE;
                let word = &self.memory.as_bytes()[end - COMMIT_WORD_SIZE..end];
                word == COMMIT_WORD.to_le_bytes()
            })
            .collect()
    }

    /// Flips `mask` in the byte `at(slot)` of the active slot.
    fn corrupt(&mut self, at: impl Fn(usize) -> usize, mask: u8) -> Result<(), String> {
        let committed = self.committed_slots();
        let slot = match committed.as_slice() {
            [] => return Err("the storage holds no committed configuration".into()),
            [only] => *only,
            _ => self.last_commit_slot.unwrap_or(0),
        };
        let offset = at(slot);
        self.memory.as_bytes_mut()[offset] ^= mask;
        self.persist(offset, 1).map_err(|e| format!("{e:?}"))?;
        warn!("Storage: corrupted byte {offset:#x} of slot {slot}; takes effect at the next start");
        Ok(())
    }

    fn persist(&mut self, offset: usize, len: usize) -> Result<(), StorageError> {
        let Some(file) = &mut self.file else {
            return Ok(());
        };
        let bytes = &self.memory.as_bytes()[offset..offset + len];
        file.seek(SeekFrom::Start(offset as u64))
            .and_then(|_| file.write_all(bytes))
            .map_err(|e| StorageError::File(e.kind()))
    }

    fn after(
        &mut self,
        offset: usize,
        len: usize,
        result: Result<(), MemoryError>,
    ) -> Result<(), StorageError> {
        // What was programmed or erased before an injected fault stays.
        self.persist(offset, len)?;
        match result {
            Err(MemoryError::Injected) => {
                error!("Storage: simulated power loss during a flash access");
                self.dead = true;
                if self.power_loss == PowerLoss::Exit {
                    if let Some(file) = &self.file {
                        let _ = file.sync_all();
                    }
                    std::process::exit(POWER_LOSS_EXIT);
                }
                Err(StorageError::PowerLost)
            }
            other => other.map_err(StorageError::Flash),
        }
    }
}

impl ConfigStorage for FileFlash {
    type Error = StorageError;

    const ERASE_SIZE: usize = SECTOR_SIZE;
    const WRITE_SIZE: usize = <Memory as ConfigStorage>::WRITE_SIZE;

    fn capacity(&self) -> usize {
        REGION_SIZE
    }

    fn read(&mut self, offset: usize, output: &mut [u8]) -> Result<(), StorageError> {
        self.lock()
            .memory
            .read(offset, output)
            .map_err(StorageError::Flash)
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), StorageError> {
        let mut inner = self.lock();
        if inner.dead {
            return Err(StorageError::PowerLost);
        }
        let result = inner.memory.write(offset, data);
        if result.is_ok()
            && offset % SLOT_SIZE == SLOT_SIZE - COMMIT_WORD_SIZE
            && data == COMMIT_WORD.to_le_bytes()
        {
            inner.last_commit_slot = Some(offset / SLOT_SIZE);
        }
        inner.after(offset, data.len(), result)
    }

    fn erase(&mut self, offset: usize, len: usize) -> Result<(), StorageError> {
        let mut inner = self.lock();
        if inner.dead {
            return Err(StorageError::PowerLost);
        }
        let result = inner.memory.erase(offset, len);
        inner.after(offset, len, result)
    }
}

#[cfg(test)]
mod tests {
    use minicbor::{CborLen, Decode, Encode};
    use rohi_config::{ConfigSection, ConfigStore};

    use super::*;

    #[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
    #[cbor(array)]
    #[config(version = 1)]
    struct Probe {
        #[n(0)]
        value: u32,
    }

    #[test]
    fn nor_rules_are_enforced() {
        let mut flash = FileFlash::ephemeral();
        flash.write(0, &[0x0F; 4]).unwrap();
        assert!(flash.write(0, &[0xF0; 4]).is_err(), "1 -> 0 only");
        assert!(flash.write(2, &[0; 4]).is_err(), "aligned");
        assert!(flash.erase(1, SECTOR_SIZE).is_err(), "erase aligned");
        flash.erase(0, SECTOR_SIZE).unwrap();
        let mut word = [0u8; 4];
        flash.read(0, &mut word).unwrap();
        assert_eq!(word, [0xFF; 4]);
    }

    #[test]
    fn the_image_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.flash");
        {
            let flash = FileFlash::open(&path).unwrap();
            let mut store = ConfigStore::format(flash).map_err(|e| e.error).unwrap();
            store.set(&Probe { value: 7 }).unwrap();
        }
        let flash = FileFlash::open(&path).unwrap();
        let mut store = ConfigStore::try_open(flash).map_err(|e| e.error).unwrap();
        assert_eq!(store.get::<Probe>().unwrap(), Some(Probe { value: 7 }));
    }

    #[test]
    fn one_storage_file_has_one_user() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.flash");
        let _first = FileFlash::open(&path).unwrap();
        let second = FileFlash::open(&path);
        assert!(second.unwrap_err().to_string().contains("in use"));
    }

    #[test]
    fn a_foreign_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.flash");
        std::fs::write(&path, b"not a flash").unwrap();
        assert!(FileFlash::open(&path).is_err());
    }

    #[test]
    fn an_interrupted_update_recovers_the_committed_generation() {
        let flash = FileFlash::ephemeral();
        flash.set_power_loss(PowerLoss::Freeze);
        let mut store = ConfigStore::format(flash.clone())
            .map_err(|e| e.error)
            .unwrap();
        store.set(&Probe { value: 1 }).unwrap();
        let generation = store.generation();

        flash
            .inject(StorageFault::PowerLossProgram { bytes: 24 })
            .unwrap();
        assert!(store.set(&Probe { value: 2 }).is_err());
        drop(store);

        // The restart: a fresh flash handle over the same bytes.
        let image = flash.image();
        let restarted = FileFlash::ephemeral();
        restarted
            .lock()
            .memory
            .as_bytes_mut()
            .copy_from_slice(&image);
        let mut store = ConfigStore::try_open(restarted)
            .map_err(|e| e.error)
            .unwrap();
        assert_eq!(store.get::<Probe>().unwrap(), Some(Probe { value: 1 }));
        assert_eq!(store.generation(), generation);
    }
}
