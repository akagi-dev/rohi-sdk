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
///////////////////////////////////////////////////////////////////////////////

//! CRC-32 (IEEE 802.3, reflected polynomial `0xEDB88320`) without lookup tables
//! larger than 64 bytes. Detects accidental corruption only; it is not
//! cryptographic authentication.

const NIBBLE_TABLE: [u32; 16] = {
    let mut table = [0u32; 16];
    let mut i = 0;
    while i < 16 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 4 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Incremental CRC-32 calculator.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Crc32 {
    pub const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    pub fn update(&mut self, data: &[u8]) {
        let mut crc = self.0;
        for &byte in data {
            crc ^= u32::from(byte);
            crc = (crc >> 4) ^ NIBBLE_TABLE[(crc & 0xF) as usize];
            crc = (crc >> 4) ^ NIBBLE_TABLE[(crc & 0xF) as usize];
        }
        self.0 = crc;
    }

    pub const fn finish(self) -> u32 {
        !self.0
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        let mut crc = Crc32::new();
        crc.update(b"123456789");
        assert_eq!(crc.finish(), 0xCBF4_3926);
    }

    #[test]
    fn empty_input() {
        assert_eq!(Crc32::new().finish(), 0);
    }

    #[test]
    fn incremental_matches_one_shot() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let mut one = Crc32::new();
        one.update(data);
        let mut two = Crc32::new();
        for chunk in data.chunks(5) {
            two.update(chunk);
        }
        assert_eq!(one.finish(), two.finish());
    }
}
