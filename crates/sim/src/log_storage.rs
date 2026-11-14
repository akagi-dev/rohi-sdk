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

//! File-backed NOR flash for the persistent log.
//!
//! The ring journal of `rohi-logging` runs on it unchanged: it sees a
//! [`Medium`] with the geometry of the `rohi-log` partition of the Altruist
//! Urban (256 erase sectors of 4 KiB, 1 MiB) and the rules of NOR flash: erase sets
//! bytes to `0xFF` and a byte that is programmed cannot be written again until
//! its sector is erased. The file is the raw image of the region, nothing else.
//!
//! ```text
//! rohi-logging RingStorage -> FileLogFlash (NOR rules)
//!                                        -> .altruist-urban/device.log.flash (region image)
//! ```
//!
//! Every write and erase reaches the file at once, so a process that dies
//! leaves what a power loss would: the ring journal discards the incomplete
//! last write when it recovers at the next start.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use log::{info, warn};
use rohi_logging::storage::{Medium, SECTOR_SIZE};

use crate::storage::OpenError;

/// Erase sectors of the log region, as the `rohi-log` partition of the device.
pub const LOG_SECTORS: u32 = 256;

const REGION_SIZE: usize = LOG_SECTORS as usize * SECTOR_SIZE;

// The region of the first layout of the log, 64 sectors.
const LEGACY_REGION_SIZE: usize = 64 * SECTOR_SIZE;

/// A failed access to the log flash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFlashError {
    /// The range leaves the region.
    OutOfBounds,
    /// A byte was written that is not erased, which NOR flash cannot do.
    NotErased,
    /// The file behind the flash failed.
    File(std::io::ErrorKind),
}

struct Inner {
    image: Vec<u8>,
    file: Option<File>,
    path: Option<PathBuf>,
}

/// The persistent log flash of the simulated device. Clones share the flash:
/// the logging runtime reopens its storage on the same medium whenever the
/// backend is selected again.
#[derive(Clone)]
pub struct FileLogFlash {
    inner: Arc<Mutex<Inner>>,
}

impl FileLogFlash {
    /// A flash that lives only in memory.
    pub fn ephemeral() -> Self {
        Self::from_parts(vec![0xFF; REGION_SIZE], None, None)
    }

    /// The flash stored in `path`, created erased if it does not exist.
    ///
    /// # Errors
    ///
    /// The file cannot be created, read or locked, or is not a region image
    /// of [`LOG_SECTORS`] sectors; an image of the former 256 KiB log region is
    /// refused, not migrated. The file is locked while the flash is open,
    /// so two processes cannot share a log file.
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let fail = |what: &str, error: &dyn std::fmt::Display| {
            OpenError(format!("log storage {}: {what}: {error}", path.display()))
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
            .map_err(|_| fail("in use", &"another process uses this log file"))?;
        let length = file.metadata().map_err(|e| fail("cannot stat", &e))?.len();
        let mut image = vec![0xFF; REGION_SIZE];
        if length == 0 {
            file.write_all(&image)
                .map_err(|e| fail("cannot initialize", &e))?;
            info!("Log storage: created {}", path.display());
        } else if length == REGION_SIZE as u64 {
            file.read_exact(&mut image)
                .map_err(|e| fail("cannot read", &e))?;
            info!("Log storage: opened {}", path.display());
        } else if length == LEGACY_REGION_SIZE as u64 {
            return Err(fail(
                "a log flash image of the former 256 KiB layout",
                &format!(
                    "the log region is now {REGION_SIZE} bytes and the old history is not migrated: delete {} to start a new log",
                    path.display()
                ),
            ));
        } else {
            return Err(fail(
                "not a log flash image",
                &format!("{length} bytes, expected {REGION_SIZE}"),
            ));
        }
        Ok(Self::from_parts(image, Some(file), Some(path.to_owned())))
    }

    fn from_parts(image: Vec<u8>, file: Option<File>, path: Option<PathBuf>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner { image, file, path })),
        }
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
        if let Some(file) = &self.lock().file
            && let Err(e) = file.sync_all()
        {
            warn!("Log storage: cannot sync: {e}");
        }
    }

    /// The raw region image.
    pub fn image(&self) -> Vec<u8> {
        self.lock().image.clone()
    }
}

impl std::fmt::Debug for FileLogFlash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileLogFlash")
            .field("path", &self.path())
            .finish_non_exhaustive()
    }
}

impl Inner {
    fn persist(&mut self, offset: usize, len: usize) -> Result<(), LogFlashError> {
        let Some(file) = &mut self.file else {
            return Ok(());
        };
        file.seek(SeekFrom::Start(offset as u64))
            .and_then(|_| file.write_all(&self.image[offset..offset + len]))
            .map_err(|e| LogFlashError::File(e.kind()))
    }
}

impl Medium for FileLogFlash {
    type Error = LogFlashError;

    fn sectors(&self) -> u32 {
        LOG_SECTORS
    }

    fn erase(&mut self, first: u32, count: u32) -> Result<(), LogFlashError> {
        let end = first.checked_add(count).ok_or(LogFlashError::OutOfBounds)?;
        if end > LOG_SECTORS {
            return Err(LogFlashError::OutOfBounds);
        }
        let (start, len) = (first as usize * SECTOR_SIZE, count as usize * SECTOR_SIZE);
        let mut inner = self.lock();
        inner.image[start..start + len].fill(0xFF);
        inner.persist(start, len)
    }

    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), LogFlashError> {
        let start = offset as usize;
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= REGION_SIZE)
            .ok_or(LogFlashError::OutOfBounds)?;
        let mut inner = self.lock();
        if inner.image[start..end].iter().any(|byte| *byte != 0xFF) {
            return Err(LogFlashError::NotErased);
        }
        inner.image[start..end].copy_from_slice(data);
        inner.persist(start, data.len())
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), LogFlashError> {
        let start = offset as usize;
        let end = start
            .checked_add(buf.len())
            .filter(|end| *end <= REGION_SIZE)
            .ok_or(LogFlashError::OutOfBounds)?;
        buf.copy_from_slice(&self.lock().image[start..end]);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nor_rules_are_enforced() {
        let mut flash = FileLogFlash::ephemeral();
        flash.write(3, b"abc").unwrap();
        assert_eq!(flash.write(4, b"x"), Err(LogFlashError::NotErased));
        flash.erase(0, 1).unwrap();
        let mut bytes = [0u8; 3];
        flash.read(3, &mut bytes).unwrap();
        assert_eq!(bytes, [0xFF; 3]);
        flash.write(4, b"x").unwrap();
    }

    #[test]
    fn ranges_are_checked() {
        let mut flash = FileLogFlash::ephemeral();
        let end = REGION_SIZE as u32;
        assert_eq!(flash.write(end - 1, b"ab"), Err(LogFlashError::OutOfBounds));
        assert_eq!(flash.read(end, &mut [0]), Err(LogFlashError::OutOfBounds));
        assert_eq!(
            flash.erase(LOG_SECTORS - 1, 2),
            Err(LogFlashError::OutOfBounds)
        );
        assert_eq!(flash.erase(u32::MAX, 2), Err(LogFlashError::OutOfBounds));
        assert_eq!(
            flash.write(u32::MAX, b"ab"),
            Err(LogFlashError::OutOfBounds)
        );
    }

    #[test]
    fn the_image_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.log.flash");
        {
            let mut flash = FileLogFlash::open(&path).unwrap();
            flash.write(10, b"persisted").unwrap();
            flash.erase(1, 1).unwrap();
        }
        let mut flash = FileLogFlash::open(&path).unwrap();
        let mut bytes = [0u8; 9];
        flash.read(10, &mut bytes).unwrap();
        assert_eq!(&bytes, b"persisted");
        assert_eq!(flash.image().len(), REGION_SIZE);
    }

    #[test]
    fn one_process_owns_the_file_and_the_size_is_checked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.log.flash");
        let flash = FileLogFlash::open(&path).unwrap();
        assert!(FileLogFlash::open(&path).is_err(), "locked");
        drop(flash);
        std::fs::write(&path, b"short").unwrap();
        assert!(FileLogFlash::open(&path).is_err(), "not an image");
    }

    #[test]
    fn an_image_of_the_former_256_kib_region_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.log.flash");
        let legacy = vec![0xAB; LEGACY_REGION_SIZE];
        std::fs::write(&path, &legacy).unwrap();
        let error = FileLogFlash::open(&path).unwrap_err().to_string();
        assert!(
            error.contains("256 KiB") && error.contains("delete"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), legacy);
    }

    #[test]
    fn a_new_file_is_a_whole_erased_region() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.log.flash");
        drop(FileLogFlash::open(&path).unwrap());
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 1 << 20);
        assert!(bytes.iter().all(|byte| *byte == 0xFF));
    }
}
