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
#![no_main]

//! Configuration store hardware self-test for the ESP32-C3.
//!
//! Flash it and reset (or power-cycle) the board repeatedly. Every boot
//! verifies what the previous boot persisted and logs `SELFTEST PASS` or
//! `SELFTEST FAIL`:
//!
//! - first boot on an empty region;
//! - typed section write, reboot, read (`BootCounter`);
//! - multiple sections persisted (`PatternA`, `PatternB`);
//! - replacement of `PatternA` preserves the unrelated `PatternB`;
//! - repeated write/read cycles on every boot;
//! - third boot: the newest slot is corrupted on purpose and the store must
//!   fall back to the previous generation, then repair it with the next commit.
//!
//! Use `cargo run --release -p rohi-examples --bin example-config`.

use esp_backtrace as _;
use esp_hal::clock::CpuClock;
use log::{error, info};
use minicbor::{CborLen, Decode, Encode};
use rohi_config::{ConfigOpenState, ConfigSection, ConfigStorage, ConfigStore};
use rohi_config_esp::esp::{config_storage, esp_storage::FlashStorage};
use rohi_config_esp::layout::SLOT_SIZE;

esp_bootloader_esp_idf::esp_app_desc!();

const CORRUPTION_BOOT: u32 = 3;
const REPEATED_WRITES: u32 = 32;

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(version = 1)]
struct BootCounter {
    #[n(0)]
    boots: u32,
}

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(version = 1)]
struct PatternA {
    #[n(0)]
    seed: u32,
    #[cbor(n(1), with = "minicbor::bytes")]
    data: [u8; 32],
}

#[derive(Debug, PartialEq, Encode, Decode, CborLen, ConfigSection)]
#[config(version = 1)]
struct PatternB {
    #[cbor(n(0), with = "minicbor::bytes")]
    data: [u8; 32],
}

fn pattern(seed: u32) -> [u8; 32] {
    core::array::from_fn(|i| (seed as u8).wrapping_mul(31).wrapping_add(i as u8))
}

fn pattern_a(seed: u32) -> PatternA {
    PatternA {
        seed,
        data: pattern(seed),
    }
}

fn pattern_b() -> PatternB {
    PatternB {
        data: pattern(0xB0B0),
    }
}

struct Report {
    failures: u32,
}

impl Report {
    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            info!("  ok: {}", what);
        } else {
            error!("  FAILED: {}", what);
            self.failures += 1;
        }
    }
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger(log::LevelFilter::Info);
    let peripherals = esp_hal::init(esp_hal::Config::default().with_cpu_clock(CpuClock::max()));

    let storage = config_storage(FlashStorage::new(peripherals.FLASH))
        .expect("rohi-cfg partition missing: flash with the repository partition table");
    let mut store = ConfigStore::open(storage).expect("configuration store open failed");
    let mut report = Report { failures: 0 };

    let boot = match store.get::<BootCounter>() {
        Ok(Some(previous)) => {
            info!("Previous boot: {}", previous.boots);
            report.check(
                store.get::<PatternA>().ok().flatten() == Some(pattern_a(previous.boots)),
                "PatternA persisted across reboot",
            );
            report.check(
                store.get::<PatternB>().ok().flatten() == Some(pattern_b()),
                "PatternB untouched by replacements",
            );
            report.check(store.section_count() == 3, "exactly three sections stored");
            previous.boots + 1
        }
        Ok(None) => {
            info!("First boot on an empty region");
            report.check(
                store.open_state() == ConfigOpenState::Empty,
                "region starts empty",
            );
            report.check(store.generation().is_none(), "no generation yet");
            report.check(
                store.get::<PatternA>().ok().flatten().is_none(),
                "no stale data",
            );
            1
        }
        Err(error) => {
            report.check(false, "boot counter readable");
            error!("{:?}", error);
            1
        }
    };
    info!("Boot #{}", boot);

    report.check(
        store.set(&BootCounter { boots: boot }).is_ok(),
        "write boot counter",
    );
    if store.get::<PatternB>().ok().flatten().is_none() {
        report.check(store.set(&pattern_b()).is_ok(), "write PatternB");
    }

    let mut cycles_ok = true;
    for i in 0..REPEATED_WRITES {
        let value = pattern_a(boot * 1000 + i);
        cycles_ok &=
            store.set(&value).is_ok() && store.get::<PatternA>().ok().flatten() == Some(value);
    }
    report.check(cycles_ok, "repeated write/read cycles");

    if boot == CORRUPTION_BOOT {
        let generation = store.generation().unwrap_or(0);
        let newest = store.active_slot().unwrap_or(0);
        let mut storage = store.into_storage();
        info!("Corrupting slot {} (generation {})", newest, generation);
        // Programming zeros into the CBOR payload breaks its CRC; the commit
        // word stays, so the slot is committed but invalid.
        report.check(
            storage.write(newest * SLOT_SIZE + 8, &[0; 4]).is_ok(),
            "corrupt newest slot",
        );
        store = ConfigStore::open(storage).expect("reopen after corruption");
        report.check(
            store.open_state() == ConfigOpenState::Recovered,
            "recovery is reported",
        );
        report.check(
            store.generation() == Some(generation.wrapping_sub(1)),
            "fell back to the previous generation",
        );
        report.check(
            store.get::<BootCounter>().ok().flatten() == Some(BootCounter { boots: boot }),
            "previous generation still holds the boot counter",
        );
    }

    // The final commit also repairs the corrupted slot.
    report.check(
        store.set(&pattern_a(boot)).is_ok(),
        "final write of PatternA",
    );
    report.check(
        store.get::<PatternA>().ok().flatten() == Some(pattern_a(boot)),
        "final read of PatternA",
    );
    report.check(
        store.state() == ConfigOpenState::Valid,
        "no damaged slot left after the final commit",
    );

    if report.failures == 0 {
        info!(
            "SELFTEST PASS (boot #{}, generation {:?})",
            boot,
            store.generation()
        );
    } else {
        error!(
            "SELFTEST FAIL: {} check(s) failed (boot #{})",
            report.failures, boot
        );
    }
    loop {
        core::hint::spin_loop();
    }
}
