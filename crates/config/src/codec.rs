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

//! Strict `minicbor` helpers for typed section payloads.

use minicbor::{Decode, Decoder, Encode};

use crate::{DecodeFailure, EncodeError, SectionError};

/// Decodes `T` from the complete `payload`.
///
/// Trailing bytes after the value are rejected with
/// [`SectionError::TrailingBytes`], never reported as a decode failure.
pub fn decode_exact<T: for<'b> Decode<'b, ()>>(payload: &[u8]) -> Result<T, SectionError> {
    let mut decoder = Decoder::new(payload);
    let value = T::decode(&mut decoder, &mut ()).map_err(DecodeFailure::from)?;
    match payload.len() - decoder.position() {
        0 => Ok(value),
        extra => Err(SectionError::TrailingBytes { extra }),
    }
}

/// Encodes `value` into `buffer` without allocating, returning the length.
pub fn encode_to_slice<T: Encode<()>>(value: &T, buffer: &mut [u8]) -> Result<usize, EncodeError> {
    let capacity = buffer.len();
    let mut remaining: &mut [u8] = buffer;
    match minicbor::encode(value, &mut remaining) {
        Ok(()) => Ok(capacity - remaining.len()),
        Err(error) if error.is_write() => Err(EncodeError::BufferTooSmall),
        Err(_) => Err(EncodeError::Failed),
    }
}

#[cfg(test)]
mod tests {
    use minicbor::{CborLen, Decode, Encode};

    use super::*;
    use crate::{DecodeKind, SectionError};

    #[derive(Debug, PartialEq, Encode, Decode, CborLen)]
    struct Pair {
        #[n(0)]
        a: u8,
        #[n(1)]
        b: u16,
    }

    #[test]
    fn encodes_into_slice() {
        let mut buffer = [0u8; 16];
        let len = encode_to_slice(&Pair { a: 7, b: 300 }, &mut buffer).unwrap();
        // [7, 300]
        assert_eq!(&buffer[..len], &[0x82, 0x07, 0x19, 0x01, 0x2C]);
        assert_eq!(len, minicbor::len(Pair { a: 7, b: 300 }));
    }

    #[test]
    fn overflow_is_reported() {
        let mut buffer = [0u8; 3];
        assert_eq!(
            encode_to_slice(&Pair { a: 7, b: 300 }, &mut buffer),
            Err(EncodeError::BufferTooSmall)
        );
    }

    #[test]
    fn decode_exact_accepts_exact_payload() {
        let payload = [0x82, 0x07, 0x19, 0x01, 0x2C];
        assert_eq!(decode_exact::<Pair>(&payload), Ok(Pair { a: 7, b: 300 }));
    }

    #[test]
    fn decode_exact_reports_trailing_bytes() {
        let payload = [0x82, 0x07, 0x19, 0x01, 0x2C, 0x00, 0x00];
        assert_eq!(
            decode_exact::<Pair>(&payload),
            Err(SectionError::TrailingBytes { extra: 2 })
        );
    }

    #[test]
    fn decode_exact_reports_decode_failures() {
        let truncated = [0x82, 0x07, 0x19, 0x01];
        match decode_exact::<Pair>(&truncated) {
            Err(SectionError::Decode(failure)) => {
                assert_eq!(failure.kind, DecodeKind::EndOfInput)
            }
            other => panic!("unexpected {other:?}"),
        }
        let wrong_type = [0x60];
        match decode_exact::<Pair>(&wrong_type) {
            Err(SectionError::Decode(failure)) => {
                assert_eq!(failure.kind, DecodeKind::TypeMismatch)
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
