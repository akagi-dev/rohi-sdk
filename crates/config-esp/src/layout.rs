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
//! `firmwares/altruist-urban/partitions-esp32c3.csv` and
//! `partitions-esp32c6.csv` must agree with these constants; a host test of
//! this module enforces it.

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
