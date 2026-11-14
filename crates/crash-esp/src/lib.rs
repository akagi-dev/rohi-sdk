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
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::todo,
        clippy::unimplemented,
        reason = "firmware must not panic: handle the error, see AGENTS.md"
    )
)]

//! Robonomics Open Hardware Interface (ROHI): the panic handler, the reset-retained crash slot and the watchdog
//! fallback of the ESP32-C3 and ESP32-C6.
//!
//! # Overview
//!
//! This is the chip side of [`rohi-crash`](rohi_crash). It places the crash
//! [`Slot`](rohi_crash::Slot) in RTC fast memory, which the chip keeps across
//! software and watchdog resets, provides the panic handler that fills it and
//! resets the chip, maps the reset reason of the chip to
//! [`Reset`](rohi_crash::Reset), and offers the boot-time calls that read and
//! release the record. Persisting the record to the log is the firmware's job
//! (`Logging::persist` of `rohi-logging`).
//!
//! The handler is **not** installed by merely linking this crate: the
//! firmware binary defines the `#[panic_handler]` and calls [`on_panic`], so
//! there is exactly one handler whatever else is linked (the binary replaces
//! `esp-backtrace`'s `panic-handler` feature, which the workspace no longer
//! enables).
//!
//! # Features and target requirements
//!
//! The chip is selected by **exactly one** Cargo feature, forwarded by the
//! final firmware. Without a chip feature the crate is empty, so it stays
//! buildable on the host together with the portable crates.
//!
//! | Feature | Chip | Rust target |
//! |---------|------|-------------|
//! | `esp32c3` | ESP32-C3 | `riscv32imc-unknown-none-elf` |
//! | `esp32c6` | ESP32-C6 | `riscv32imac-unknown-none-elf` |
//! | none (default) | any | any: empty crate |
//!
//! Enabling both is a `compile_error!`. The items below exist only with a chip
//! feature, so they are written in code font; build the docs with one:
//!
//! ```text
//! cargo doc --no-deps -p rohi-crash-esp --no-default-features --features esp32c3 --target riscv32imc-unknown-none-elf
//! ```
//!
//! # Quick Example
//!
//! The firmware binary, as `firmwares/altruist-urban` does it:
//!
//! ```text
//! #[panic_handler]
//! fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
//!     rohi_sdk::esp::crash::on_panic(info)
//! }
//!
//! // first thing in main, before the radio and the flash start:
//! let reset = rohi_sdk::esp::crash::init();           // PowerOn, Software, Panic, ...
//!
//! // once the log storage is open:
//! if let Some(stored) = rohi_sdk::esp::crash::pending() {
//!     // persist stored.crash.line(0..) with Logging::persist, then
//!     rohi_sdk::esp::crash::acknowledge(stored.crash.serial());
//! }
//! ```
//!
//! # Core Concepts
//!
//! | Item | Role |
//! |------|------|
//! | `on_panic` | the panic path: interrupts off, watchdog armed, record stored, console message, reset |
//! | `init`, `report`, `pending`, `acknowledge` | the boot side: why the chip started, what the slot holds, release the record |
//! | `reset_reason` | the chip's reset reason as a portable [`Reset`](rohi_crash::Reset) |
//!
//! # Architecture and Integration
//!
//! ```text
//! panic!() / exception ─► #[panic_handler] (firmware) ─► on_panic
//!    riscv::interrupt::disable                           │
//!    RWDT armed (3 s): a hung handler is reset anyway    │ no queue, no mutex, no allocation
//!    Crash::new / message / location / backtrace         │ nothing async
//!    Slot::capture ─► RTC fast RAM (.rtc_fast.persistent)│
//!    console: PANIC block, backtrace addresses           │
//!    Next::Reset ───► software_reset()                   │
//!    Next::Cooldown ► RWDT armed for the cool-down, wfi  ▼
//! ```
//!
//! # API Navigation
//!
//! | Item | Use it to |
//! |------|-----------|
//! | `on_panic` | be the body of the firmware's `#[panic_handler]` |
//! | `init` | learn why the chip started, once, early |
//! | `report`, `pending`, `acknowledge` | the status, the record to log, the release after logging |
//! | `reset_reason` | map the chip's reset reason |
//!
//! # Guarantees and Limitations
//!
//! - **No dependencies on the running system:** the handler does not use the
//!   log queue, the flash, a mutex of the firmware, the executor or the
//!   allocator, and does not panic itself (a panic inside it resets the chip
//!   at once). It uses the console (`esp-println`) after the record is stored.
//! - **Watchdog fallback:** the RTC watchdog is armed for 3 s before anything
//!   else runs, so a handler that hangs (a console that never drains, a lock
//!   held by the code that panicked) still ends in a system reset with the
//!   record already stored. In a crash loop the same watchdog is armed for the
//!   cool-down of [`Policy`](rohi_crash::Policy) (ten minutes by default) and the CPU waits.
//! - **Reset, not reboot:** the handler resets by `software_reset`, never by the
//!   deferred `system.reboot` path, which needs the executor.
//! - **Retention:** RTC fast memory (the slot is placed in `.rtc_fast.persistent`)
//!   is kept by the chip across software and watchdog resets and cleared at a
//!   power-up by the startup code. **A power loss, a brown-out reset or the
//!   reset pin (`EN`) lose a record that the log does not have yet.** Whether the
//!   second-stage bootloader of a given image leaves the memory alone on both
//!   chips is covered by the hardware checklist, not verified here.
//! - **Uptime:** the stamp of a crash is `esp_hal::time::Instant` since boot, the
//!   same epoch as the log's monotonic clock, in milliseconds.
//! - **Backtrace:** it follows frame pointers, which the workspace enables for
//!   both targets (`.cargo/config.toml`); at most
//!   [`MAX_FRAMES`](rohi_crash::MAX_FRAMES) addresses are kept and an
//!   exception shows the faulting address in the message (`mepc`), not in the
//!   backtrace.
//! - **Targets:** target-built for both chips (CI builds the firmware); hardware
//!   verification: not verified ([support matrix](https://rohi.akagi.dev/support/overview.html)).

#[cfg(all(feature = "esp32c3", feature = "esp32c6"))]
compile_error!(
    "rohi-crash-esp: the features `esp32c3` and `esp32c6` are mutually exclusive, enable exactly one"
);

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
mod chip;

#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
pub use chip::*;
