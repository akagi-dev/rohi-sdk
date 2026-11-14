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

use std::format;
use std::string::{String, ToString};

use crate::{
    Crash, MAX_FILE, MAX_FRAMES, MAX_MESSAGE, Next, Policy, Reset, SLOT_BYTES, Slot, Stored, WORDS,
};

fn crash(uptime_ms: u64, message: &str) -> Crash {
    let mut crash = Crash::new(uptime_ms);
    crash.set_message(format_args!("{message}"));
    crash.set_location("crates/hal/src/lib.rs", 42, 7);
    crash.push_frame(0x4200_1000);
    crash.push_frame(0x4200_2000);
    crash
}

fn lines(crash: &Crash) -> std::vec::Vec<String> {
    (0..crash.line_count())
        .map(|index| crash.line(index).unwrap().to_string())
        .collect()
}

#[test]
fn a_stored_record_reads_back_whole() {
    let slot = Slot::new();
    let mut original = crash(5_000, "boom");
    original.serial = 9;
    original.missed = 2;
    original.streak = 3;
    slot.store(&Stored {
        pending: true,
        crash: original,
    });
    let stored = slot.load().unwrap();
    assert!(stored.pending);
    assert_eq!(stored.crash, original);
    assert_eq!(stored.crash.message(), "boom");
    assert_eq!(stored.crash.file(), "crates/hal/src/lib.rs");
    assert_eq!(stored.crash.line_number(), 42);
    assert_eq!(stored.crash.column(), 7);
    assert_eq!(stored.crash.frames(), [0x4200_1000, 0x4200_2000]);
}

#[test]
fn the_slot_is_240_bytes_and_a_new_one_is_empty() {
    assert_eq!(SLOT_BYTES, 240);
    assert_eq!(WORDS, 60);
    assert!(Slot::new().load().is_none());
}

#[test]
fn a_message_is_one_line_cut_at_a_character_boundary() {
    let mut crash = Crash::new(0);
    crash.set_message(format_args!("line one\nline two\t{}", "é".repeat(100)));
    assert!(crash.message_cut());
    assert!(crash.message().len() <= MAX_MESSAGE);
    assert!(crash.message().starts_with("line one line two "));
    assert!(!crash.message().chars().any(char::is_control));
    // The last character is whole.
    assert!(crash.message().ends_with('é'));

    let mut short = Crash::new(0);
    short.set_message(format_args!("short"));
    assert!(!short.message_cut());
}

#[test]
fn a_message_of_exactly_the_limit_is_not_cut() {
    let mut crash = Crash::new(0);
    crash.set_message(format_args!("{}", "a".repeat(MAX_MESSAGE)));
    assert!(!crash.message_cut());
    assert_eq!(crash.message().len(), MAX_MESSAGE);
    crash.set_message(format_args!("{}", "a".repeat(MAX_MESSAGE + 1)));
    assert!(crash.message_cut());
    assert_eq!(crash.message().len(), MAX_MESSAGE);
}

#[test]
fn a_long_path_keeps_its_end() {
    let mut crash = Crash::new(0);
    let path = "/home/user/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/esp-hal-1.2.2/src/exception_handler/mod.rs";
    crash.set_location(path, 117, 5);
    assert!(crash.file_cut());
    assert!(crash.file().len() <= MAX_FILE);
    assert!(path.ends_with(crash.file()));
    assert!(crash.file().ends_with("exception_handler/mod.rs"));

    // A multi-byte character at the cut is dropped, not split.
    let mut crash = Crash::new(0);
    crash.set_location(
        &format!("{}/é{}", "d".repeat(50), "f".repeat(MAX_FILE - 2)),
        1,
        1,
    );
    assert!(crash.file_cut());
    assert!(core::str::from_utf8(crash.file().as_bytes()).is_ok());
    assert!(crash.file().len() <= MAX_FILE);
}

#[test]
fn the_backtrace_is_bounded() {
    let mut crash = Crash::new(0);
    for index in 0..MAX_FRAMES {
        assert!(crash.push_frame(0x4200_0000 + index as u32));
    }
    assert!(!crash.push_frame(1));
    assert_eq!(crash.frames().len(), MAX_FRAMES);
    assert_eq!(crash.frames()[0], 0x4200_0000);
}

#[test]
fn the_record_becomes_log_lines() {
    let slot = Slot::new();
    let mut panic = crash(
        7_000,
        "Exception 'Store/AMO access fault (7)' mepc=0x421033be, mtval=0x00000095",
    );
    slot.capture(&mut panic, &Policy::DEFAULT);
    assert_eq!(
        lines(&panic),
        [
            "panic: Exception 'Store/AMO access fault (7)' mepc=0x421033be, mtval=0x00000095",
            "at crates/hal/src/lib.rs:42:7",
            "backtrace 42001000 42002000",
            "crash #1",
        ]
    );
    assert!(panic.line(4).is_none());
}

#[test]
fn a_record_without_a_location_or_backtrace_has_two_lines() {
    let mut bare = Crash::new(1);
    bare.set_message(format_args!("x"));
    assert_eq!(lines(&bare), ["panic: x", "crash #0"]);
}

#[test]
fn a_loop_and_lost_records_are_in_the_summary() {
    let slot = Slot::new();
    for _ in 0..3 {
        let mut panic = crash(10, "again");
        slot.capture(&mut panic, &Policy::DEFAULT);
    }
    let last = slot.load().unwrap().crash;
    assert_eq!(
        last.line(3).unwrap().to_string(),
        "crash #3, 3 in a row, 2 earlier not logged"
    );
}

#[test]
fn no_line_is_longer_than_a_log_message() {
    let mut worst = Crash::new(u64::MAX);
    worst.set_message(format_args!("{}", "é".repeat(MAX_MESSAGE)));
    worst.set_location(&"p".repeat(200), u32::MAX, u32::MAX);
    for index in 0..MAX_FRAMES {
        worst.push_frame(0xFFFF_FFF0 + index as u32);
    }
    worst.serial = u32::MAX;
    worst.streak = u8::MAX;
    worst.missed = u32::MAX;
    for line in lines(&worst) {
        assert!(line.len() <= 120, "{} bytes: {line}", line.len());
    }
}

#[test]
fn any_flipped_bit_makes_the_slot_empty() {
    let slot = Slot::new();
    let mut original = crash(5_000, "boom");
    slot.capture(&mut original, &Policy::DEFAULT);
    assert!(slot.load().is_some());
    for index in 0..WORDS {
        // The fourth header word is reserved and not covered.
        if index == 3 {
            continue;
        }
        for bit in 0..32 {
            slot.corrupt(index, 1 << bit);
            assert!(slot.load().is_none(), "word {index} bit {bit}");
            slot.corrupt(index, 1 << bit);
            assert!(slot.load().is_some(), "word {index} bit {bit} restored");
        }
    }
}

#[test]
fn memory_that_lost_power_is_not_a_record() {
    // What an SRAM holds after power-up is arbitrary: a few patterns and a
    // simple generator, none must validate.
    for fill in [
        0u32,
        !0,
        0xAAAA_AAAA,
        0x5555_5555,
        crate::slot::WORDS as u32,
    ] {
        let slot = Slot::new();
        for index in 0..WORDS {
            slot.corrupt(index, fill);
        }
        assert!(slot.load().is_none(), "fill {fill:#x}");
    }
    let mut seed = 0x1234_5678u32;
    for _ in 0..2_000 {
        let slot = Slot::new();
        for index in 0..WORDS {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            slot.corrupt(index, seed);
        }
        assert!(slot.load().is_none());
    }
}

#[test]
fn a_reset_cut_at_every_word_leaves_the_old_record_or_nothing_but_never_a_mix() {
    let old = Stored {
        pending: false,
        crash: crash(1, "old"),
    };
    let new = Stored {
        pending: true,
        crash: crash(2, "new"),
    };
    for steps in 0..=WORDS + 1 {
        let slot = Slot::new();
        slot.store(&old);
        slot.store_cut(&new, steps);
        match slot.load() {
            Some(stored) if steps == 0 => assert_eq!(stored, old),
            Some(stored) => {
                assert_eq!(steps, WORDS + 1, "valid after {steps} writes");
                assert_eq!(stored, new);
            }
            None => assert!((1..=WORDS).contains(&steps), "empty after {steps} writes"),
        }
    }
}

#[test]
fn the_count_continues_across_panics() {
    let slot = Slot::new();
    let mut first = crash(1_000, "first");
    assert_eq!(slot.capture(&mut first, &Policy::DEFAULT), Next::Reset);
    assert_eq!((first.serial(), first.streak(), first.missed()), (1, 1, 0));
    let mut second = crash(2_000, "second");
    assert_eq!(slot.capture(&mut second, &Policy::DEFAULT), Next::Reset);
    assert_eq!(
        (second.serial(), second.streak(), second.missed()),
        (2, 2, 1)
    );
    let stored = slot.load().unwrap();
    assert!(stored.pending);
    assert_eq!(stored.crash.message(), "second");
}

#[test]
fn three_rapid_panics_are_a_crash_loop() {
    let slot = Slot::new();
    let policy = Policy::DEFAULT;
    for expected in [Next::Reset, Next::Reset] {
        assert_eq!(slot.capture(&mut crash(500, "x"), &policy), expected);
    }
    assert_eq!(
        slot.capture(&mut crash(500, "x"), &policy),
        Next::Cooldown {
            ms: policy.cooldown_ms
        }
    );
    // The loop goes on while it lasts.
    assert!(matches!(
        slot.capture(&mut crash(500, "x"), &policy),
        Next::Cooldown { .. }
    ));
}

#[test]
fn a_panic_after_a_stable_run_ends_the_streak() {
    let slot = Slot::new();
    let policy = Policy::DEFAULT;
    slot.capture(&mut crash(100, "x"), &policy);
    slot.capture(&mut crash(100, "x"), &policy);
    let mut late = crash(policy.rapid_window_ms, "late");
    assert_eq!(slot.capture(&mut late, &policy), Next::Reset);
    assert_eq!(late.streak(), 1);
    let mut soon = crash(policy.rapid_window_ms - 1, "soon");
    slot.capture(&mut soon, &policy);
    assert_eq!(soon.streak(), 2);
}

#[test]
fn a_panic_with_no_retained_history_starts_at_one() {
    let slot = Slot::new();
    let mut crash = crash(u64::MAX, "late");
    assert_eq!(slot.capture(&mut crash, &Policy::DEFAULT), Next::Reset);
    assert_eq!((crash.serial(), crash.streak(), crash.missed()), (1, 1, 0));
}

#[test]
fn counters_do_not_overflow() {
    let slot = Slot::new();
    let mut last = crash(1, "max");
    last.serial = u32::MAX;
    last.streak = u8::MAX;
    last.missed = u32::MAX;
    slot.store(&Stored {
        pending: true,
        crash: last,
    });
    let mut next = crash(1, "next");
    slot.capture(&mut next, &Policy::DEFAULT);
    assert_eq!(
        (next.serial(), next.streak(), next.missed()),
        (0, u8::MAX, u32::MAX)
    );
}

#[test]
fn acknowledging_releases_only_the_record_it_names() {
    let slot = Slot::new();
    assert!(!slot.acknowledge(1));
    let mut panic = crash(10, "x");
    slot.capture(&mut panic, &Policy::DEFAULT);
    assert!(!slot.acknowledge(panic.serial() + 1));
    assert!(slot.load().unwrap().pending);
    assert!(slot.acknowledge(panic.serial()));
    let stored = slot.load().unwrap();
    assert!(!stored.pending);
    // The record stays for the status, and a second acknowledge changes nothing.
    assert_eq!(stored.crash.message(), "x");
    assert!(!slot.acknowledge(panic.serial()));
}

#[test]
fn an_acknowledged_record_is_not_counted_as_missed() {
    let slot = Slot::new();
    let mut first = crash(10, "a");
    slot.capture(&mut first, &Policy::DEFAULT);
    assert!(slot.acknowledge(first.serial()));
    let mut second = crash(10, "b");
    slot.capture(&mut second, &Policy::DEFAULT);
    assert_eq!((second.serial(), second.missed()), (2, 0));
}

#[test]
fn the_report_tells_a_panic_from_other_resets() {
    let slot = Slot::new();
    assert_eq!(slot.boot(Reset::PowerOn), Reset::PowerOn);
    assert_eq!(slot.boot(Reset::Software), Reset::Software);
    assert_eq!(slot.report(Reset::Software).crashes(), 0);

    let mut panic = crash(10, "x");
    slot.capture(&mut panic, &Policy::DEFAULT);
    for (reported, observed) in [
        (Reset::Software, Reset::Panic),
        (Reset::Watchdog, Reset::Panic),
        (Reset::PowerOn, Reset::PowerOn),
        (Reset::Brownout, Reset::Brownout),
        (Reset::DeepSleep, Reset::DeepSleep),
    ] {
        assert_eq!(slot.boot(reported), observed);
    }
    let report = slot.report(slot.boot(Reset::Software));
    assert_eq!(report.reset, Reset::Panic);
    assert_eq!((report.crashes(), report.rapid()), (1, 1));

    // After the log has it, the next software reset is just a restart.
    slot.acknowledge(panic.serial());
    assert_eq!(slot.boot(Reset::Software), Reset::Software);
}

#[test]
fn reset_names_and_faults() {
    for reset in [
        Reset::PowerOn,
        Reset::Software,
        Reset::Watchdog,
        Reset::Brownout,
        Reset::DeepSleep,
        Reset::Other,
        Reset::Panic,
    ] {
        assert!(!reset.name().is_empty());
    }
    assert!(Reset::Panic.is_fault() && Reset::Watchdog.is_fault() && Reset::Brownout.is_fault());
    assert!(!Reset::PowerOn.is_fault() && !Reset::Software.is_fault());
}

#[test]
fn a_failing_display_leaves_what_was_written() {
    struct Failing;
    impl core::fmt::Display for Failing {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("partial")?;
            Err(core::fmt::Error)
        }
    }
    let mut crash = Crash::new(0);
    crash.set_message(format_args!("{Failing}"));
    assert_eq!(crash.message(), "partial");
}
