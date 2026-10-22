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

use crate::ConfigStorage;

/// Failure of [`MemoryStorage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryError {
    OutOfBounds,
    Unaligned,
    /// A write would have to set bits that are programmed (NOR flash can only
    /// clear bits until the sector is erased).
    NotErased,
    /// A fault armed with `fail_write_after` / `fail_erase_after` fired.
    Injected,
}

/// RAM-backed flash emulation for host tests and examples.
///
/// `N` is the capacity and `ERASE` the erase granularity in bytes. Writes are
/// 4-byte granular and, like NOR programming, may only clear bits: writing
/// erased bytes or clearing more bits of programmed ones succeeds, a write
/// that would have to set a programmed bit fails with
/// [`MemoryError::NotErased`]. Erase fills sectors with `0xFF`.
///
/// Faults are deterministic. [`fail_write_after`](Self::fail_write_after)
/// lets exactly `n` more bytes be programmed (a write straddling the limit is
/// torn: its prefix is stored) and then fails every write.
/// [`fail_erase_after`](Self::fail_erase_after) behaves the same for erased
/// bytes, so interrupted erases leave a partially erased sector.
pub struct MemoryStorage<const N: usize, const ERASE: usize = 4096> {
    data: [u8; N],
    write_budget: Option<usize>,
    erase_budget: Option<usize>,
    written: usize,
    erased: usize,
}

impl<const N: usize, const ERASE: usize> MemoryStorage<N, ERASE> {
    /// A fully erased storage.
    pub const fn new() -> Self {
        assert!(ERASE.is_power_of_two() && N.is_multiple_of(ERASE));
        Self {
            data: [0xFF; N],
            write_budget: None,
            erase_budget: None,
            written: 0,
            erased: 0,
        }
    }

    /// Raw contents.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Raw contents for corrupting storage in tests.
    pub fn as_bytes_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Allow `bytes` more programmed bytes, then fail.
    pub fn fail_write_after(&mut self, bytes: usize) {
        self.write_budget = Some(bytes);
    }

    /// Allow `bytes` more erased bytes, then fail.
    pub fn fail_erase_after(&mut self, bytes: usize) {
        self.erase_budget = Some(bytes);
    }

    /// Disarm all injected faults.
    pub fn clear_faults(&mut self) {
        self.write_budget = None;
        self.erase_budget = None;
    }

    /// Total bytes programmed since creation.
    pub fn bytes_written(&self) -> usize {
        self.written
    }

    /// Total bytes erased since creation.
    pub fn bytes_erased(&self) -> usize {
        self.erased
    }
}

impl<const N: usize, const ERASE: usize> Default for MemoryStorage<N, ERASE> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize, const ERASE: usize> ConfigStorage for MemoryStorage<N, ERASE> {
    type Error = MemoryError;

    const ERASE_SIZE: usize = ERASE;
    const WRITE_SIZE: usize = 4;

    fn capacity(&self) -> usize {
        N
    }

    fn read(&mut self, offset: usize, output: &mut [u8]) -> Result<(), MemoryError> {
        let end = offset
            .checked_add(output.len())
            .filter(|end| *end <= N)
            .ok_or(MemoryError::OutOfBounds)?;
        output.copy_from_slice(&self.data[offset..end]);
        Ok(())
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), MemoryError> {
        let end = offset
            .checked_add(data.len())
            .filter(|end| *end <= N)
            .ok_or(MemoryError::OutOfBounds)?;
        if !offset.is_multiple_of(Self::WRITE_SIZE) || !data.len().is_multiple_of(Self::WRITE_SIZE)
        {
            return Err(MemoryError::Unaligned);
        }
        if self.data[offset..end]
            .iter()
            .zip(data)
            .any(|(old, new)| old & new != *new)
        {
            return Err(MemoryError::NotErased);
        }
        let allowed = self.write_budget.map_or(data.len(), |b| b.min(data.len()));
        for (old, new) in self.data[offset..offset + allowed].iter_mut().zip(data) {
            *old &= new;
        }
        self.written += allowed;
        if let Some(budget) = &mut self.write_budget {
            *budget -= allowed;
        }
        if allowed < data.len() {
            return Err(MemoryError::Injected);
        }
        Ok(())
    }

    fn erase(&mut self, offset: usize, len: usize) -> Result<(), MemoryError> {
        let end = offset
            .checked_add(len)
            .filter(|end| *end <= N)
            .ok_or(MemoryError::OutOfBounds)?;
        if !offset.is_multiple_of(ERASE) || !len.is_multiple_of(ERASE) {
            return Err(MemoryError::Unaligned);
        }
        let allowed = self.erase_budget.map_or(len, |b| b.min(len));
        self.data[offset..offset + allowed].fill(0xFF);
        self.erased += allowed;
        if let Some(budget) = &mut self.erase_budget {
            *budget -= allowed;
        }
        debug_assert!(offset + allowed <= end);
        if allowed < len {
            return Err(MemoryError::Injected);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Mem = MemoryStorage<64, 16>;

    #[test]
    fn starts_erased_and_round_trips() {
        let mut mem = Mem::new();
        let mut buf = [0u8; 4];
        mem.read(0, &mut buf).unwrap();
        assert_eq!(buf, [0xFF; 4]);
        mem.write(4, &[1, 2, 3, 4]).unwrap();
        mem.read(4, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3, 4]);
    }

    #[test]
    fn enforces_flash_semantics() {
        let mut mem = Mem::new();
        assert_eq!(mem.write(60, &[0; 8]), Err(MemoryError::OutOfBounds));
        assert_eq!(mem.write(1, &[0; 4]), Err(MemoryError::Unaligned));
        assert_eq!(mem.write(0, &[0; 3]), Err(MemoryError::Unaligned));
        assert_eq!(mem.erase(8, 16), Err(MemoryError::Unaligned));
        assert_eq!(mem.erase(0, 80), Err(MemoryError::OutOfBounds));
        mem.write(0, &[0x0F; 4]).unwrap();
        // Programming may only clear bits.
        assert_eq!(mem.write(0, &[0xF0; 4]), Err(MemoryError::NotErased));
        mem.write(0, &[0x03; 4]).unwrap();
        assert_eq!(&mem.as_bytes()[..4], &[0x03; 4]);
        mem.erase(0, 16).unwrap();
        mem.write(0, &[0; 4]).unwrap();
    }

    #[test]
    fn write_fault_tears_the_write() {
        let mut mem = Mem::new();
        mem.fail_write_after(6);
        mem.write(0, &[1; 4]).unwrap();
        assert_eq!(mem.write(4, &[2; 8]), Err(MemoryError::Injected));
        assert_eq!(
            &mem.as_bytes()[4..12],
            &[2, 2, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
        );
        assert_eq!(mem.write(16, &[3; 4]), Err(MemoryError::Injected));
        mem.clear_faults();
        mem.write(16, &[3; 4]).unwrap();
        assert_eq!(mem.bytes_written(), 10);
    }

    #[test]
    fn erase_fault_leaves_partial_erase() {
        let mut mem = Mem::new();
        mem.write(0, &[0; 4]).unwrap();
        mem.write(20, &[0; 4]).unwrap();
        mem.fail_erase_after(16);
        mem.erase(0, 16).unwrap();
        assert_eq!(mem.erase(16, 16), Err(MemoryError::Injected));
        assert_eq!(&mem.as_bytes()[20..24], &[0; 4]);
    }
}
