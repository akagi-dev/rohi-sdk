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

//! The reset-retained slot, and the panic path that fills it.

use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{Ordering, compiler_fence};

use crate::crc::crc32;
use crate::record::{Crash, PAYLOAD};
use crate::reset::Reset;

const MAGIC: u32 = u32::from_le_bytes(*b"RCR1");
const VERSION: u16 = 1;
const HEADER_WORDS: usize = 4;
/// Size of the slot in 32-bit words.
pub const WORDS: usize = HEADER_WORDS + PAYLOAD / 4;
/// Size of the slot in bytes.
pub const SLOT_BYTES: usize = WORDS * 4;

/// What the slot holds: the last panic, and whether the log has it yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stored {
    /// `true` until [`Slot::acknowledge`] says the record is in the persistent log.
    pub pending: bool,
    /// The record.
    pub crash: Crash,
}

/// When a crash loop is treated as one, and what happens then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Policy {
    /// A panic this soon after boot, in milliseconds, is *rapid*.
    pub rapid_window_ms: u64,
    /// This many rapid panics in a row are a crash loop.
    pub max_rapid: u8,
    /// The pause before the next start of a crash loop, in milliseconds.
    pub cooldown_ms: u32,
}

impl Policy {
    /// Three panics within a minute of boot are a crash loop; the device then
    /// waits ten minutes before it starts again.
    pub const DEFAULT: Self = Self {
        rapid_window_ms: 60_000,
        max_rapid: 3,
        cooldown_ms: 600_000,
    };
}

/// What the panic handler does after [`Slot::capture`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Reset the chip now.
    Reset,
    /// A crash loop: stay stopped for `ms` milliseconds, then reset. A
    /// watchdog armed for that long does the reset, so a handler that hangs is
    /// covered too.
    Cooldown {
        /// The pause in milliseconds.
        ms: u32,
    },
}

/// A fixed area of memory that survives software and watchdog resets, holding
/// the last [`Crash`].
///
/// Put one in a `static` placed in retained memory (the platform crate does
/// this). Everything the slot stores has a magic number, a version and a
/// CRC-32; whatever does not validate reads as empty, which is how memory that
/// lost power looks. A write invalidates the slot first and validates it last,
/// so a reset in the middle leaves it empty rather than half old, half new.
///
/// The methods take `&self`: the memory is accessed with volatile reads and
/// writes and no lock, so the panic path can use it whatever else is running.
/// It is **not** safe against two writers at the same time; on a single core
/// the panic handler runs with interrupts disabled and never returns.
#[repr(C, align(4))]
pub struct Slot {
    words: UnsafeCell<[u32; WORDS]>,
}

// SAFETY: every access is a volatile word read or write; the contract above
// says who may write when.
unsafe impl Sync for Slot {}

impl Slot {
    /// An empty slot.
    pub const fn new() -> Self {
        Self {
            words: UnsafeCell::new([0; WORDS]),
        }
    }

    fn word(&self, index: usize) -> *mut u32 {
        self.words.get().cast::<u32>().wrapping_add(index)
    }

    fn read(&self) -> [u32; WORDS] {
        let mut out = [0u32; WORDS];
        for (index, word) in out.iter_mut().enumerate() {
            // SAFETY: `index < WORDS`, the pointer is inside the array.
            *word = unsafe { ptr::read_volatile(self.word(index)) };
        }
        out
    }

    /// Writes `words` in the order that keeps the slot empty until the end:
    /// the magic is cleared first and written last. Stops after `steps` writes
    /// (`usize::MAX` for all of them).
    fn write(&self, words: &[u32; WORDS], steps: usize) {
        let mut done = 0;
        let mut step = |index: usize, value: u32| {
            if done < steps {
                // SAFETY: `index < WORDS`.
                unsafe { ptr::write_volatile(self.word(index), value) };
                compiler_fence(Ordering::SeqCst);
            }
            done += 1;
        };
        step(0, 0);
        for (index, value) in words.iter().enumerate().skip(1) {
            step(index, *value);
        }
        step(0, words.first().copied().unwrap_or(0));
    }

    /// The stored record, `None` when the slot is empty or does not validate.
    pub fn load(&self) -> Option<Stored> {
        let words = self.read();
        let [magic, version_len, crc, _] = *words.first_chunk::<HEADER_WORDS>()?;
        let payload_len = (version_len >> 16) as usize;
        if magic != MAGIC || version_len as u16 != VERSION || payload_len != PAYLOAD {
            return None;
        }
        let mut payload = [0u8; PAYLOAD];
        for (chunk, word) in payload
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(words.iter().skip(HEADER_WORDS))
        {
            *chunk = word.to_le_bytes();
        }
        if crc32(&[&version_len.to_le_bytes(), &payload]) != crc {
            return None;
        }
        let (pending, crash) = Crash::decode(&payload)?;
        Some(Stored { pending, crash })
    }

    /// Stores `stored`, replacing what is there.
    pub fn store(&self, stored: &Stored) {
        self.store_steps(stored, usize::MAX);
    }

    fn store_steps(&self, stored: &Stored, steps: usize) {
        let mut payload = [0u8; PAYLOAD];
        stored.crash.encode(stored.pending, &mut payload);
        let version_len = u32::from(VERSION) | ((PAYLOAD as u32) << 16);
        let mut words = [0u32; WORDS];
        for (word, header) in words.iter_mut().zip([
            MAGIC,
            version_len,
            crc32(&[&version_len.to_le_bytes(), &payload]),
            0,
        ]) {
            *word = header;
        }
        for (word, chunk) in words
            .iter_mut()
            .skip(HEADER_WORDS)
            .zip(payload.as_chunks::<4>().0)
        {
            *word = u32::from_le_bytes(*chunk);
        }
        self.write(&words, steps);
    }

    /// Empties the slot.
    pub fn clear(&self) {
        // SAFETY: word 0 is inside the array. Clearing the magic is enough.
        unsafe { ptr::write_volatile(self.word(0), 0) };
        compiler_fence(Ordering::SeqCst);
    }

    /// The panic path: numbers `crash`, stores it as pending and says what to
    /// do next.
    ///
    /// It continues the count of the previous record: `serial` goes up by one;
    /// `streak` goes up by one if the panic is within the policy's rapid window
    /// of boot and the previous one was too, otherwise it is 1; `missed` counts
    /// the previous record when it never reached the log. Allocation free; it
    /// does not panic.
    pub fn capture(&self, crash: &mut Crash, policy: &Policy) -> Next {
        let previous = self.load();
        let rapid = crash.uptime_ms < policy.rapid_window_ms;
        let (serial, streak, missed) = match &previous {
            Some(stored) => (
                stored.crash.serial.wrapping_add(1),
                if rapid {
                    stored.crash.streak.saturating_add(1)
                } else {
                    1
                },
                if stored.pending {
                    stored.crash.missed.saturating_add(1)
                } else {
                    0
                },
            ),
            None => (1, 1, 0),
        };
        crash.serial = serial;
        crash.streak = streak;
        crash.missed = missed;
        self.store(&Stored {
            pending: true,
            crash: *crash,
        });
        if streak >= policy.max_rapid {
            Next::Cooldown {
                ms: policy.cooldown_ms,
            }
        } else {
            Next::Reset
        }
    }

    /// Marks the record with `serial` as written to the log.
    ///
    /// Returns `false`, changing nothing, when the slot holds no pending record
    /// with that serial: another panic replaced it, or the memory was lost.
    pub fn acknowledge(&self, serial: u32) -> bool {
        match self.load() {
            Some(stored) if stored.pending && stored.crash.serial == serial => {
                self.store(&Stored {
                    pending: false,
                    crash: stored.crash,
                });
                true
            }
            _ => false,
        }
    }

    /// Why this boot happened, given the reset the chip reports: a software or
    /// watchdog reset that finds a pending record is [`Reset::Panic`].
    ///
    /// Ask once, at boot, before the record is acknowledged.
    pub fn boot(&self, reported: Reset) -> Reset {
        Reset::observed(reported, self.load().is_some_and(|stored| stored.pending))
    }

    /// The status of this boot: `reset` as [`boot`](Self::boot) returned it and
    /// what the slot holds now.
    pub fn report(&self, reset: Reset) -> Report {
        Report {
            reset,
            last: self.load(),
        }
    }

    #[cfg(test)]
    pub(crate) fn corrupt(&self, index: usize, mask: u32) {
        // SAFETY: tests pass `index < WORDS`.
        unsafe {
            ptr::write_volatile(
                self.word(index),
                ptr::read_volatile(self.word(index)) ^ mask,
            )
        };
    }

    #[cfg(test)]
    pub(crate) fn store_cut(&self, stored: &Stored, steps: usize) {
        self.store_steps(stored, steps);
    }
}

impl Default for Slot {
    fn default() -> Self {
        Self::new()
    }
}

/// What a boot found, for the status and the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    /// Why the chip started; a panic is told from other software resets.
    pub reset: Reset,
    /// The last panic the slot holds, if any.
    pub last: Option<Stored>,
}

impl Report {
    /// Panics since the retained memory was last cleared by a power-up.
    pub fn crashes(&self) -> u32 {
        self.last.map_or(0, |stored| stored.crash.serial)
    }

    /// Consecutive rapid panics at the last one, 0 if none.
    pub fn rapid(&self) -> u8 {
        self.last.map_or(0, |stored| stored.crash.streak)
    }
}
