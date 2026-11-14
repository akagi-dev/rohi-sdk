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

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use esp_hal::riscv;
use rohi_crash::{Crash, Next, Policy};

use super::retained::SLOT;
use super::watchdog;

/// How long the handler may take before the watchdog resets the chip, in
/// milliseconds.
const HANDLER_TIMEOUT_MS: u32 = 3_000;

static ENTERED: AtomicBool = AtomicBool::new(false);

/// The panic path: stores the panic in the retained crash slot, prints it to
/// the console and resets the chip. Never returns.
///
/// Call it as the whole body of the firmware's `#[panic_handler]`. It uses no
/// queue, flash, lock of the firmware, executor or allocation, and a panic
/// inside it resets the chip at once. What it does is described in the crate
/// documentation.
#[inline(always)]
pub fn on_panic(info: &PanicInfo<'_>) -> ! {
    riscv::interrupt::disable();
    if ENTERED.load(Ordering::Relaxed) {
        // The handler itself panicked: whatever it stored stays, reset.
        watchdog::arm(HANDLER_TIMEOUT_MS);
        esp_hal::system::software_reset()
    }
    ENTERED.store(true, Ordering::Relaxed);
    watchdog::arm(HANDLER_TIMEOUT_MS);

    let mut crash = Crash::new(
        esp_hal::time::Instant::now()
            .duration_since_epoch()
            .as_millis(),
    );
    crash.set_message(format_args!("{}", info.message()));
    if let Some(location) = info.location() {
        crash.set_location(location.file(), location.line(), location.column());
    }
    for frame in esp_backtrace::Backtrace::capture().frames() {
        if !crash.push_frame(frame.program_counter() as u32) {
            break;
        }
    }
    let next = SLOT.capture(&mut crash, &Policy::DEFAULT);
    announce(info, &crash, next);

    match next {
        Next::Reset => esp_hal::system::software_reset(),
        Next::Cooldown { ms } => {
            watchdog::arm(ms);
            loop {
                riscv::asm::wfi();
            }
        }
    }
}

// The record is already stored: the console is for a developer with a cable.
// The format of the block and of the addresses is that of `esp-backtrace`, so
// that the monitor still decodes them.
#[inline(never)]
#[cold]
fn announce(info: &PanicInfo<'_>, crash: &Crash, next: Next) {
    use esp_println::println;

    println!("");
    println!("====================== PANIC ======================");
    println!("{}", info);
    println!("");
    println!("Backtrace:");
    println!("");
    for frame in crash.frames() {
        println!("0x{:x}", frame);
    }
    println!("");
    match next {
        Next::Reset => println!("crash #{} stored, resetting", crash.serial()),
        Next::Cooldown { ms } => println!(
            "crash #{}, {} in a row: crash loop, waiting {} s before the reset",
            crash.serial(),
            crash.streak(),
            ms / 1_000
        ),
    }
    println!("");
}
