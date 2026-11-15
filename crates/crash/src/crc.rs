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

//! CRC-32 (IEEE 802.3, reflected) without a table: the panic path keeps its
//! footprint in flash and RAM small, and the record is a few hundred bytes.

const POLY: u32 = 0xEDB8_8320;

/// The CRC-32 of the concatenation of `parts`.
pub(crate) fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = !0u32;
    for part in parts {
        for byte in *part {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (POLY & mask);
            }
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_value_of_the_standard_is_reproduced() {
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
    }

    #[test]
    fn parts_are_the_same_as_one_slice() {
        assert_eq!(crc32(&[b"1234", b"56789"]), crc32(&[b"123456789"]));
        assert_eq!(crc32(&[]), 0);
    }
}
