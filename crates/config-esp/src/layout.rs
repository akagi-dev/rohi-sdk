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

//! Platform-level source of truth for the configuration flash layout.
//!
//! The constants are the layout the shipped Altruist Urban partition tables
//! provide; they are plain `usize`/`&str` values with no chip dependency.
//!
//! ```
//! use rohi_config_esp::layout::{PARTITION_LABEL, REGION_SIZE, SECTOR_SIZE, SLOT_SIZE};
//!
//! assert_eq!(PARTITION_LABEL, "rohi-cfg");
//! assert_eq!(SLOT_SIZE, 2 * SECTOR_SIZE);
//! assert_eq!(REGION_SIZE, 2 * SLOT_SIZE);
//! ```
//!
//! `firmwares/altruist-urban/partitions-esp32c3.csv` (4 MB flash) and
//! `partitions-esp32c6.csv` (8 MB flash) are mirrored by the constants of this
//! module; a host test of this module keeps both in sync. The tables differ in
//! the size of the `factory` partition and of `rohi-log`, which is always the
//! last partition of the flash: 256 KiB on ESP32-C3, 1 MiB on ESP32-C6.
//!
//! # Flash layout (Altruist Urban, ESP32-C3 4 MB / ESP32-C6 8 MB)
//!
//! ```text
//! 0x009000  nvs        24 KiB
//! 0x00f000  phy_init    4 KiB
//! 0x010000  rohi-cfg   16 KiB   <- configuration region
//!             ├─ slot A  8 KiB (2 sectors)
//!             └─ slot B  8 KiB (2 sectors)
//! 0x020000  factory    app: 0x3A0000 bytes on ESP32-C3, 0x6E0000 on ESP32-C6
//! ```
//!
//! ESP32-C3 (4 MB), the end of the flash:
//!
//! ```text
//! 0x3C0000  rohi-log   256 KiB (0x40000), up to 0x400000
//!             62 ring sectors, then 2 format header sectors
//! ```
//!
//! The app area ends on a 64 KiB boundary on both chips, so that two equal OTA
//! slots can be carved out of it later (#29).
//!
//! ESP32-C6 (8 MB), the end of the flash:
//!
//! ```text
//! 0x700000  rohi-log   1 MiB, up to 0x800000
//!             254 ring sectors, then 2 format header sectors
//! ```
//!
//! `cargo run` passes the table of the selected target to `espflash` (see
//! `.cargo/config.toml`); run it from the repository root. The store sizes its
//! two slots from the region: each slot is half of it rounded down to the
//! erase size, at most [`MAX_SLOT_SIZE`](rohi_config::MAX_SLOT_SIZE) bytes.
//!
//! Flash region, slot, payload and stack figures of the store are the constants
//! of this module and of [`rohi_config`] (`MAX_SLOT_SIZE`, `MAX_SECTION_PAYLOAD`).

/// Label of the data partition holding the configuration region.
pub const PARTITION_LABEL: &str = "rohi-cfg";

/// ESP flash erase sector size.
pub const SECTOR_SIZE: usize = 4096;

/// Size of one A/B slot (two erase sectors).
pub const SLOT_SIZE: usize = 2 * SECTOR_SIZE;

/// Size of the whole configuration region (slot A + slot B).
pub const REGION_SIZE: usize = 2 * SLOT_SIZE;

/// Label of the data partition that holds the persistent log.
pub const LOG_PARTITION_LABEL: &str = "rohi-log";

/// Size of the `rohi-log` partition of the ESP32-C3 in bytes: 256 KiB, 64
/// sectors. This is the partition, not the journal: the log is a ring of
/// segments of whole sectors and two format header sectors (`rohi-logging`), so
/// with 16 KiB segments it retains 15 full segments (240 KiB).
pub const LOG_REGION_SIZE_ESP32C3: usize = 64 * SECTOR_SIZE;

/// Size of the `rohi-log` partition of the ESP32-C6 in bytes: 1 MiB, 256
/// sectors.
pub const LOG_REGION_SIZE_ESP32C6: usize = 256 * SECTOR_SIZE;

/// Size of the `factory` partition of the ESP32-C3 in bytes.
pub const APP_REGION_SIZE_ESP32C3: usize = 0x3A_0000;

/// Size of the `factory` partition of the ESP32-C6 in bytes.
pub const APP_REGION_SIZE_ESP32C6: usize = 0x6E_0000;

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_number(value: &str) -> usize {
        let value = value.trim();
        match value.strip_prefix("0x") {
            Some(hex) => usize::from_str_radix(hex, 16).unwrap(),
            None => match value.strip_suffix('K') {
                Some(kib) => kib.parse::<usize>().unwrap() * 1024,
                None => value.parse().unwrap(),
            },
        }
    }

    #[test]
    fn partition_tables_match_the_layout() {
        for chip in [
            Chip {
                csv: include_str!("../../../firmwares/altruist-urban/partitions-esp32c3.csv"),
                flash_size: 4 << 20,
                app_size: APP_REGION_SIZE_ESP32C3,
                log_size: LOG_REGION_SIZE_ESP32C3,
                gap: 0,
            },
            Chip {
                csv: include_str!("../../../firmwares/altruist-urban/partitions-esp32c6.csv"),
                flash_size: 8 << 20,
                app_size: APP_REGION_SIZE_ESP32C6,
                log_size: LOG_REGION_SIZE_ESP32C6,
                gap: 0,
            },
        ] {
            assert_partition_matches_layout(&chip);
        }
    }

    struct Chip {
        csv: &'static str,
        flash_size: usize,
        app_size: usize,
        log_size: usize,
        gap: usize,
    }

    fn rows(csv: &str) -> std::vec::Vec<std::vec::Vec<&str>> {
        csv.lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .map(|line| line.split(',').map(str::trim).collect())
            .collect()
    }

    fn assert_data_partition(csv: &str, label: &str, size: usize) {
        let rows = rows(csv);
        let row = rows
            .iter()
            .find(|fields| fields[0] == label)
            .unwrap_or_else(|| panic!("{label} partition"));
        assert_eq!(row[1], "data");
        assert_eq!(parse_number(row[3]) % SECTOR_SIZE, 0);
        assert_eq!(parse_number(row[4]), size);
    }

    fn assert_partition_matches_layout(chip: &Chip) {
        let Chip {
            csv,
            flash_size,
            app_size: expected_app_size,
            log_size: expected_log_size,
            gap,
        } = *chip;
        assert_data_partition(csv, PARTITION_LABEL, REGION_SIZE);
        assert_data_partition(csv, LOG_PARTITION_LABEL, expected_log_size);

        // Partitions do not overlap and `rohi-log` ends at the end of the flash.
        let mut spans: std::vec::Vec<(usize, usize)> = rows(csv)
            .iter()
            .map(|row| (parse_number(row[3]), parse_number(row[4])))
            .collect();
        spans.sort_unstable();
        for pair in spans.windows(2) {
            assert!(pair[0].0 + pair[0].1 <= pair[1].0, "{pair:?} overlap");
        }
        let end = spans.last().map(|(offset, size)| offset + size).unwrap();
        assert_eq!(end, flash_size, "{end:#x}");

        let table = rows(csv);
        let offset_and_size = |label: &str| {
            let row = table.iter().find(|row| row[0] == label).unwrap();
            (parse_number(row[3]), parse_number(row[4]))
        };
        // `factory` stays where it was before the log existed, the log is the
        // last partition of the flash and the app ends `gap` bytes before it.
        let (app, app_size) = offset_and_size("factory");
        let (log, log_size) = offset_and_size(LOG_PARTITION_LABEL);
        assert_eq!(app, 0x20000);
        assert_eq!(log, flash_size - expected_log_size);
        assert_eq!(log_size, expected_log_size);
        assert_eq!(app_size, expected_app_size);
        assert_eq!(app + app_size + gap, log);
        // The app area ends on a 64 KiB boundary on both chips, so that two
        // equal OTA slots can later be carved out of it (#29).
        assert_eq!((app + app_size) % 0x10000, 0);
        // The app partition is aligned to 64 KiB, as the bootloader needs.
        assert_eq!(app % 0x10000, 0);
        assert_eq!(table.iter().filter(|row| row[1] == "app").count(), 1);
    }

    #[test]
    fn the_log_regions_are_whole_sectors() {
        assert_eq!(LOG_REGION_SIZE_ESP32C3 % SECTOR_SIZE, 0);
        assert_eq!(LOG_REGION_SIZE_ESP32C3, 256 * 1024);
        assert_eq!(LOG_REGION_SIZE_ESP32C3, 0x40000);
        assert_eq!(LOG_REGION_SIZE_ESP32C3 / SECTOR_SIZE, 64);
        assert_eq!(LOG_REGION_SIZE_ESP32C6 % SECTOR_SIZE, 0);
        assert_eq!(LOG_REGION_SIZE_ESP32C6, 1 << 20);
        assert_eq!(LOG_REGION_SIZE_ESP32C6 / SECTOR_SIZE, 256);
    }

    #[test]
    fn the_esp32c3_journal_holds_fifteen_whole_segments() {
        // Two sectors are the format header slots of the ring; the rest is
        // cut into 16 KiB segments of four sectors.
        let ring = LOG_REGION_SIZE_ESP32C3 - 2 * SECTOR_SIZE;
        assert_eq!(ring / (4 * SECTOR_SIZE), 15);
        assert_eq!(15 * 4 * SECTOR_SIZE, 240 * 1024);
    }

    #[test]
    fn region_holds_two_sector_aligned_slots() {
        assert_eq!(SLOT_SIZE, 8192);
        assert_eq!(REGION_SIZE, 16384);
        assert_eq!(SLOT_SIZE % SECTOR_SIZE, 0);
    }
}
