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
#![no_std]

//! A bounded, checksummed record of a panic for reset-retained memory, and the crash-loop policy around it.
//!
//! # Overview
//!
//! When firmware panics it cannot trust anything: the logging queue may be
//! full, the flash mutex may be held by the code that panicked, the executor
//! is gone. `rohi-crash` is what is safe to do in that state: copy the panic
//! (message, file, line, backtrace addresses) into a fixed [`Slot`] in memory
//! that survives a software or watchdog reset, say what to do next
//! ([`Next`]), and let the next boot find the record and write it to the
//! persistent log. The crate is chip independent: a platform crate places the
//! `Slot` in retained memory, installs the panic handler and resets the chip
//! (`rohi-crash-esp` for the ESP32-C3 and ESP32-C6).
//!
//! The crate is `no_std`, never allocates, has no dependencies and no Cargo
//! features. Nothing on the capture path panics: bounds are checked with
//! `get`, not indexing.
//!
//! # Quick Example
//!
//! The panic path and the next boot, with the slot in ordinary memory:
//!
//! ```
//! use rohi_crash::{Crash, Next, Policy, Reset, Slot};
//!
//! static SLOT: Slot = Slot::new();
//!
//! // The panic handler: build the record on the stack, store it, reset.
//! let mut crash = Crash::new(12_345);
//! crash.set_message(format_args!("index out of bounds: the len is {} but the index is {}", 3, 7));
//! crash.set_location("crates/hal/src/sensors/bmx280.rs", 212, 9);
//! crash.push_frame(0x4200_1a2c);
//! assert_eq!(SLOT.capture(&mut crash, &Policy::DEFAULT), Next::Reset);
//!
//! // The next boot, after a software reset.
//! let reset = SLOT.boot(Reset::Software);
//! assert_eq!(reset, Reset::Panic);
//! let stored = SLOT.report(reset).last.unwrap();
//! assert!(stored.pending);
//! assert_eq!(stored.crash.line(0).unwrap().to_string(), "panic: index out of bounds: the len is 3 but the index is 7");
//!
//! // Only once the log has the lines is the record released.
//! assert!(SLOT.acknowledge(stored.crash.serial()));
//! assert!(!SLOT.load().unwrap().pending);
//! ```
//!
//! # Core Concepts
//!
//! | Concept | Meaning |
//! |---------|---------|
//! | [`Crash`] | one panic as plain data of a fixed size; builds without allocating and formats as up to [`MAX_LINES`] log lines |
//! | [`Slot`] | the retained area: magic, version, CRC-32; a write empties it first and validates it last |
//! | [`Stored`] | what the slot holds: a [`Crash`] and whether the log has it yet (`pending`) |
//! | [`Policy`], [`Next`] | when panics are a crash loop (default: three within 60 s of boot) and what the handler does then (reset at once, or wait ten minutes) |
//! | [`Reset`] | why the chip started; a software or watchdog reset after a pending record is a [`Reset::Panic`] |
//! | [`Report`] | what a boot found (the reset and the last record), for the status |
//!
//! # Architecture and Integration
//!
//! ```text
//! panic ─► panic handler (rohi-crash-esp)            next boot
//!           interrupts off, watchdog armed             Slot::boot(reset reason)
//!           Crash::new / set_message / push_frame       │ Reset::Panic, pending record
//!           Slot::capture ─► retained RAM               ▼
//!           reset chip  (or cool down, see Policy)     rohi-logging persist ─► rohi-log
//!                                                       │ only after it succeeded
//!                                                       ▼
//!                                                     Slot::acknowledge
//! ```
//!
//! `rohi-system` reports the [`Report`] in the `system` status; Altruist
//! Urban writes the lines of a pending record to the persistent log with
//! `Logging::persist`, outside the log queue.
//!
//! # API Navigation
//!
//! | Item | Use it to |
//! |------|-----------|
//! | [`Slot`] | keep the record: `capture` on a panic, `report` and `acknowledge` at boot |
//! | [`Crash`], [`Line`] | build a record; read it back as text lines |
//! | [`Policy`], [`Next`] | decide between an immediate reset and a cool-down |
//! | [`Reset`], [`Report`] | describe the start |
//!
//! # Guarantees and Limitations
//!
//! - **Bounded:** the record is [`SLOT_BYTES`] bytes (240). The message keeps
//!   [`MAX_MESSAGE`] bytes, the file path its last [`MAX_FILE`] bytes, the
//!   backtrace [`MAX_FRAMES`] addresses; longer data is cut and flagged. Only the
//!   **last** panic is kept: a second panic before the log has the first
//!   replaces it and counts it in [`Crash::missed`].
//! - **Torn writes:** a reset in the middle of a write leaves the slot empty,
//!   never a mix of two records. An empty slot reads as no crash.
//! - **Retention is the platform's:** the slot survives exactly what the memory
//!   it is placed in survives. On the ESP32-C3 and ESP32-C6 that is software
//!   and watchdog resets; **a power loss, brown-out or reset-pin reset loses
//!   it**, and then the record never reaches the log. Whether the memory really
//!   survives on a given board is verified by the hardware checklist, not by
//!   this crate.
//! - **Persistent format:** magic `RCR1`, version 1, the layout of
//!   [`Crash`]'s encoding, are stored ABI of the retained memory. Retained
//!   memory does not outlive a firmware update that changes the version: the
//!   record then reads as empty.
//! - **Concurrency:** the slot has no lock. Writers must not overlap: the
//!   panic handler runs with interrupts off on a single core and never returns;
//!   the boot code that acknowledges runs before a second panic can happen or
//!   is replaced by it (then [`Slot::acknowledge`] refuses).
//! - **Targets:** host-tested, including resets cut at every word of a write;
//!   hardware verification: not verified ([support matrix](https://rohi.akagi.dev/support/overview.html)).

mod crc;
mod record;
mod reset;
mod slot;

pub use record::{Crash, Line, MAX_FILE, MAX_FRAMES, MAX_LINES, MAX_MESSAGE};
pub use reset::Reset;
pub use slot::{Next, Policy, Report, SLOT_BYTES, Slot, Stored, WORDS};

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
