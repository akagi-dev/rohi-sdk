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

use postcard::Error as CodecError;
use serde::{Serialize, de::DeserializeOwned};

use crate::SectionError;

/// Serializes `value` into `buffer` without allocating, returning the length.
pub fn encode_to_slice<T: Serialize + ?Sized>(
    value: &T,
    buffer: &mut [u8],
) -> Result<usize, CodecError> {
    postcard::to_slice(value, buffer).map(|written| written.len())
}

/// Deserializes `T` from the complete `payload`; trailing bytes are rejected.
pub fn decode_all<T: DeserializeOwned>(payload: &[u8]) -> Result<T, SectionError> {
    let (value, rest) = postcard::take_from_bytes::<T>(payload)?;
    if rest.is_empty() {
        Ok(value)
    } else {
        Err(SectionError::Decode(CodecError::DeserializeUnexpectedEnd))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_into_slice() {
        let mut buffer = [0u8; 8];
        let len = encode_to_slice(&(7u8, 300u16), &mut buffer).unwrap();
        assert_eq!(&buffer[..len], &[7, 0xAC, 0x02]);
    }

    #[test]
    fn overflow_is_reported() {
        let mut buffer = [0u8; 1];
        assert!(encode_to_slice(&300u16, &mut buffer).is_err());
    }

    #[test]
    fn decode_all_rejects_trailing_bytes() {
        assert_eq!(decode_all::<u8>(&[7]), Ok(7));
        assert!(decode_all::<u8>(&[7, 0]).is_err());
        assert!(decode_all::<u16>(&[0x80]).is_err());
    }
}
