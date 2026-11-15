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

use core::sync::atomic::{AtomicU8, Ordering};

use rohi_crash::{Report, Reset, Slot, Stored};

use super::reset::{code, from_code, reset_reason};

/// The crash slot, in RTC fast memory. The startup code of `esp-hal` clears
/// `.rtc_fast.persistent` at a power-up and leaves it alone at every other
/// reset; `.rtc_fast.bss`, `.rtc_fast.data` and ordinary RAM do not keep it.
#[unsafe(link_section = ".rtc_fast.persistent")]
pub(super) static SLOT: Slot = Slot::new();

static RESET: AtomicU8 = AtomicU8::new(0);

/// Records why the chip started, from its reset reason and the slot, and
/// returns it. Call once, early in `main`; until then [`report`] says
/// [`Reset::Other`].
///
/// A software or watchdog reset that finds a record the log does not have yet
/// is [`Reset::Panic`]; asking again after [`acknowledge`] would say
/// `Software`, which is why the answer is kept.
pub fn init() -> Reset {
    let reset = SLOT.boot(reset_reason());
    RESET.store(code(reset), Ordering::Relaxed);
    reset
}

/// What this boot found: the reset that [`init`] recorded and the last record.
pub fn report() -> Report {
    SLOT.report(from_code(RESET.load(Ordering::Relaxed)))
}

/// The record the log does not have yet, if there is one.
pub fn pending() -> Option<Stored> {
    SLOT.load().filter(|stored| stored.pending)
}

/// Releases the record with `serial` after it was written to the log; see
/// [`Slot::acknowledge`].
pub fn acknowledge(serial: u32) -> bool {
    SLOT.acknowledge(serial)
}
