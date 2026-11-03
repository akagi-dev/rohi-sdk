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

/// Byte-addressable, flash-like storage region.
///
/// Offsets are relative to the start of the region. The trait knows nothing
/// about CBOR, sections, CRCs or generations. Implementations must behave
/// like NOR flash:
///
/// - `erase` sets bytes to `0xFF` and works in [`ERASE_SIZE`](Self::ERASE_SIZE)
///   units;
/// - `write` programs bytes and works in [`WRITE_SIZE`](Self::WRITE_SIZE)
///   units. Like NOR programming it may only clear bits: the store writes
///   erased bytes, except that it programs an already written commit word to
///   zero before erasing its slot, so re-programming a word with a value that
///   only clears bits must be supported;
/// - out-of-bounds or misaligned requests return an error.
///
/// The store accepts a `WRITE_SIZE` of 1, 2 or 4 and splits the region into two
/// slots of at most [`MAX_SLOT_SIZE`](crate::MAX_SLOT_SIZE) bytes each.
///
/// # Backends
///
/// - [`MemoryStorage`](crate::MemoryStorage) emulates flash for host tests
///   (erase to `0xFF`, writes may only clear bits, deterministic fault
///   injection).
/// - `EspConfigStorage` of [`rohi_config_esp`](../rohi_config_esp/index.html)
///   restricts an `embedded-storage` NOR flash to a bounded region of ESP32
///   flash. It knows nothing about CBOR, sections or any feature configuration.
pub trait ConfigStorage {
    /// The backend failure, carried as [`ConfigError::Storage`](crate::ConfigError::Storage).
    type Error;

    /// Erase granularity in bytes (must be a power of two).
    const ERASE_SIZE: usize;
    /// Write offset/length granularity in bytes (must be a power of two).
    const WRITE_SIZE: usize;

    /// Region size in bytes.
    fn capacity(&self) -> usize;

    /// Fills `output` from `offset`.
    ///
    /// # Errors
    ///
    /// Out-of-bounds requests and backend failures.
    fn read(&mut self, offset: usize, output: &mut [u8]) -> Result<(), Self::Error>;

    /// Programs `data` at `offset`; offset and length are multiples of [`WRITE_SIZE`](Self::WRITE_SIZE).
    ///
    /// # Errors
    ///
    /// Misaligned or out-of-bounds requests and backend failures.
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), Self::Error>;

    /// Erases `len` bytes at `offset` to `0xFF`; both are multiples of [`ERASE_SIZE`](Self::ERASE_SIZE).
    ///
    /// # Errors
    ///
    /// Misaligned or out-of-bounds requests and backend failures.
    fn erase(&mut self, offset: usize, len: usize) -> Result<(), Self::Error>;
}
