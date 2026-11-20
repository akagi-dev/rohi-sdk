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
//! the size of the `factory` partition and in where `rohi-log` sits: right after
//! the configuration region on the ESP32-C3 (128 KiB), at the end of the flash on
//! the ESP32-C6 (1 MiB).
//!
//! # Flash layout (Altruist Urban, ESP32-C3 4 MB / ESP32-C6 8 MB)
//!
//! ```text
//! 0x009000  nvs        24 KiB
//! 0x00f000  phy_init    4 KiB
//! 0x010000  rohi-cfg   16 KiB   <- configuration region
//!             ├─ slot A  8 KiB (2 sectors)
//!             └─ slot B  8 KiB (2 sectors)
//! ```
//!
//! ESP32-C3 (4 MB): the log follows the configuration region, the app ends the flash:
//!
//! ```text
//! 0x020000  rohi-log   128 KiB (0x20000), up to 0x040000
//!             30 ring sectors (15 segments of 8 KiB), then 2 format header sectors
//! 0x040000  factory    app: 0x3C0000 bytes, up to 0x400000
//! ```
//!
//! The app area starts and ends on a 64 KiB boundary on both chips, so that two equal OTA
//! slots can be carved out of it later (#29); no A/B OTA exists yet.
//!
//! ESP32-C6 (8 MB): the app starts at 0x20000 and the log is the last partition:
//!
//! ```text
//! 0x020000  factory    app: 0x6E0000 bytes, up to 0x700000
//! 0x700000  rohi-log   1 MiB, up to 0x800000
//!             254 ring sectors, then 2 format header sectors
//! ```
//!
//! Moving or resizing `rohi-log` (as this layout did on the ESP32-C3, where the log used to be 512 KiB at 0x380000 and
//! the app 0x360000 bytes from 0x20000) discards the old archive: the first start finds no format header in the new
//! region and formats it. Nothing is migrated, and the application, configuration and NVS partitions are never
//! touched by the format. The ESP32-C3 app moves, so the device must be reflashed with the new table (the runner does
//! it).
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

/// Size of the `rohi-log` partition of the ESP32-C3 in bytes: 128 KiB, 32
/// sectors. This is the partition, not the journal: the log is a ring of
/// segments of whole sectors and two format header sectors (`rohi-logging`), so
/// with 8 KiB segments it retains 15 full segments (120 KiB).
pub const LOG_REGION_SIZE_ESP32C3: usize = 32 * SECTOR_SIZE;

/// Size of the `rohi-log` partition of the ESP32-C6 in bytes: 1 MiB, 256
/// sectors.
pub const LOG_REGION_SIZE_ESP32C6: usize = 256 * SECTOR_SIZE;

/// Size of the `factory` partition of the ESP32-C3 in bytes.
pub const APP_REGION_SIZE_ESP32C3: usize = 0x3C_0000;

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

    // (label, type, offset, size) of every row of a partition table.
    fn rows(csv: &str) -> std::vec::Vec<(&str, &str, usize, usize)> {
        csv.lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .map(|line| {
                let fields: std::vec::Vec<&str> = line.split(',').map(str::trim).collect();
                (
                    fields[0],
                    fields[1],
                    parse_number(fields[3]),
                    parse_number(fields[4]),
                )
            })
            .collect()
    }

    struct Chip {
        csv: &'static str,
        flash_size: usize,
        // `factory` and `rohi-log`: offset and size.
        app: (usize, usize),
        log: (usize, usize),
    }

    fn chips() -> [Chip; 2] {
        [
            // `rohi-log` follows the configuration region, the app area ends the flash.
            Chip {
                csv: include_str!("../../../firmwares/altruist-urban/partitions-esp32c3.csv"),
                flash_size: 4 << 20,
                app: (0x4_0000, APP_REGION_SIZE_ESP32C3),
                log: (0x2_0000, LOG_REGION_SIZE_ESP32C3),
            },
            // `rohi-log` ends the flash.
            Chip {
                csv: include_str!("../../../firmwares/altruist-urban/partitions-esp32c6.csv"),
                flash_size: 8 << 20,
                app: (0x2_0000, APP_REGION_SIZE_ESP32C6),
                log: (0x70_0000, LOG_REGION_SIZE_ESP32C6),
            },
        ]
    }

    #[test]
    fn partition_tables_match_the_layout() {
        for chip in chips() {
            let table = rows(chip.csv);
            let find = |label: &str| {
                let row = table.iter().find(|row| row.0 == label).unwrap();
                (row.1, row.2, row.3)
            };
            assert_eq!(find(PARTITION_LABEL), ("data", 0x1_0000, REGION_SIZE));
            assert_eq!(find(LOG_PARTITION_LABEL), ("data", chip.log.0, chip.log.1));
            assert_eq!(find("factory"), ("app", chip.app.0, chip.app.1));
            assert_eq!(table.iter().filter(|row| row.1 == "app").count(), 1);

            // No partition overlaps another, and the table ends exactly at the end of the flash.
            let mut spans: std::vec::Vec<(usize, usize)> =
                table.iter().map(|row| (row.2, row.3)).collect();
            spans.sort_unstable();
            for pair in spans.windows(2) {
                assert!(pair[0].0 + pair[0].1 <= pair[1].0, "{pair:?} overlap");
            }
            let (offset, size) = *spans.last().unwrap();
            assert_eq!(offset + size, chip.flash_size, "the table ends the flash");

            // Whole erase sectors for every partition, 64 KiB alignment of the app at both ends.
            for row in &table {
                assert_eq!(row.2 % SECTOR_SIZE, 0, "{}", row.0);
                assert_eq!(row.3 % SECTOR_SIZE, 0, "{}", row.0);
            }
            assert_eq!(chip.app.0 % 0x1_0000, 0);
            assert_eq!((chip.app.0 + chip.app.1) % 0x1_0000, 0);
        }
    }

    #[test]
    fn the_esp32c3_log_follows_the_configuration_and_the_app_ends_the_flash() {
        let [c3, _] = chips();
        // No gap between the log and the app, and the app is what is left of
        // the 4 MiB: 0x3C0000, as large as with the log at the end.
        assert_eq!(c3.log.0, 0x2_0000);
        assert_eq!(c3.log.0 + c3.log.1, c3.app.0);
        assert_eq!(c3.app.0 + c3.app.1, c3.flash_size);
        assert_eq!(APP_REGION_SIZE_ESP32C3, 0x3C_0000);
    }

    #[test]
    fn the_log_regions_are_whole_sectors() {
        assert_eq!(LOG_REGION_SIZE_ESP32C3, 128 * 1024);
        assert_eq!(LOG_REGION_SIZE_ESP32C3, 0x2_0000);
        assert_eq!(LOG_REGION_SIZE_ESP32C3 / SECTOR_SIZE, 32);
        assert_eq!(LOG_REGION_SIZE_ESP32C6, 1 << 20);
        assert_eq!(LOG_REGION_SIZE_ESP32C6 / SECTOR_SIZE, 256);
    }

    #[test]
    fn the_esp32c3_journal_holds_fifteen_whole_segments() {
        // Two sectors are the format header slots of the ring; the rest is
        // cut into 8 KiB segments of two sectors.
        let ring = LOG_REGION_SIZE_ESP32C3 - 2 * SECTOR_SIZE;
        assert_eq!(ring / (2 * SECTOR_SIZE), 15);
        assert_eq!(15 * 2 * SECTOR_SIZE, 120 * 1024);
    }

    #[test]
    fn region_holds_two_sector_aligned_slots() {
        assert_eq!(SLOT_SIZE, 8192);
        assert_eq!(REGION_SIZE, 16384);
        assert_eq!(SLOT_SIZE % SECTOR_SIZE, 0);
    }
}
