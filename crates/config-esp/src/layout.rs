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
//! module; a host test of this module keeps both in sync. The tables differ
//! only in the size of the `factory` partition (`0x3E0000` on ESP32-C3,
//! `0x7E0000` on ESP32-C6).
//!
//! # Flash layout (Altruist Urban, ESP32-C3 4 MB / ESP32-C6 8 MB)
//!
//! ```text
//! 0x09000  nvs        24 KiB
//! 0x0f000  phy_init    4 KiB
//! 0x10000  rohi-cfg   16 KiB   <- configuration region
//!            ├─ slot A  8 KiB (2 sectors)
//!            └─ slot B  8 KiB (2 sectors)
//! 0x20000  factory    app (0x3E0000 bytes on ESP32-C3, 0x7E0000 on ESP32-C6)
//! ```
//!
//! `cargo run` passes the table of the selected target to `espflash` (see
//! `.cargo/config.toml`); run it from the repository root. The store sizes its
//! two slots from the region: each slot is half of it rounded down to the
//! erase size, at most [`MAX_SLOT_SIZE`](rohi_config::MAX_SLOT_SIZE) bytes.
//!
//! Flash region, slot, payload and stack figures of the store are listed with
//! all other limits in the
//! [resource limits](https://rohi.akagi.dev/reference/resource-limits.html#configuration-store).

/// Label of the data partition holding the configuration region.
pub const PARTITION_LABEL: &str = "rohi-cfg";

/// ESP flash erase sector size.
pub const SECTOR_SIZE: usize = 4096;

/// Size of one A/B slot (two erase sectors).
pub const SLOT_SIZE: usize = 2 * SECTOR_SIZE;

/// Size of the whole configuration region (slot A + slot B).
pub const REGION_SIZE: usize = 2 * SLOT_SIZE;

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
        for csv in [
            include_str!("../../../firmwares/altruist-urban/partitions-esp32c3.csv"),
            include_str!("../../../firmwares/altruist-urban/partitions-esp32c6.csv"),
        ] {
            assert_partition_matches_layout(csv);
        }
    }

    fn assert_partition_matches_layout(csv: &str) {
        let row: std::vec::Vec<&str> = csv
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .map(|line| line.split(',').map(str::trim).collect::<std::vec::Vec<_>>())
            .find(|fields| fields[0] == PARTITION_LABEL)
            .expect("rohi-cfg partition")
            .into_iter()
            .collect();
        assert_eq!(row[1], "data");
        assert_eq!(parse_number(row[3]) % SECTOR_SIZE, 0);
        assert_eq!(parse_number(row[4]), REGION_SIZE);
    }

    #[test]
    fn region_holds_two_sector_aligned_slots() {
        assert_eq!(SLOT_SIZE, 8192);
        assert_eq!(REGION_SIZE, 16384);
        assert_eq!(SLOT_SIZE % SECTOR_SIZE, 0);
    }
}
