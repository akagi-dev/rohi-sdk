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

//! The record format of the control stream.
//!
//! A TCP connection carries a sequence of records in each direction. A record
//! is one encoded Protobuf message, as defined by `rohi-control-proto`, behind
//! a 4-byte length:
//!
//! ```text
//! +--------------------+------------------------------+
//! | length: u32 LE     | length bytes of Protobuf     |
//! +--------------------+------------------------------+
//! ```
//!
//! A client sends `Request` records, the device answers each with exactly one
//! `Response` record, in order. The length counts the payload only. It is
//! checked against the limit of the direction before any payload byte is
//! read: [`MAX_REQUEST_LEN`](rohi_control_proto::limits::MAX_REQUEST_LEN) for
//! requests and [`MAX_RESPONSE_LEN`](rohi_control_proto::limits::MAX_RESPONSE_LEN)
//! for responses. A length of 0 is invalid.
//!
//! The functions of this module are independent of the transport: the device
//! ([`ControlTcp`](crate::ControlTcp)) and the `rohi` tool use the same header
//! code.
//!
//! ```
//! use rohi_control_tcp::frame::{HEADER_LEN, decode_header, encode_header};
//!
//! let header = encode_header(300);
//! assert_eq!(header, [44, 1, 0, 0]);
//! assert_eq!(decode_header(header, 768), Ok(300));
//! assert_eq!(HEADER_LEN, 4);
//! ```

use core::fmt;

/// Size of the record header in bytes.
pub const HEADER_LEN: usize = 4;

/// Why a record header was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// The record announces no payload.
    Empty,
    /// The record announces more payload than the limit of the direction.
    TooLarge {
        /// The announced payload length.
        len: u32,
        /// The limit it exceeds.
        max: usize,
    },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty record"),
            Self::TooLarge { len, max } => {
                write!(f, "record of {len} bytes exceeds the limit of {max} bytes")
            }
        }
    }
}

impl core::error::Error for FrameError {}

/// The header of a record with `len` payload bytes.
pub const fn encode_header(len: u32) -> [u8; HEADER_LEN] {
    len.to_le_bytes()
}

/// The payload length announced by `header`, checked against `max`.
///
/// # Errors
///
/// [`FrameError::Empty`] for a length of 0 and [`FrameError::TooLarge`] for a
/// length above `max`. The caller must not read the payload of a rejected
/// record: the stream cannot be resynchronized, so the connection ends.
pub fn decode_header(header: [u8; HEADER_LEN], max: usize) -> Result<usize, FrameError> {
    let len = u32::from_le_bytes(header);
    match usize::try_from(len) {
        Ok(0) => Err(FrameError::Empty),
        Ok(value) if value <= max => Ok(value),
        _ => Err(FrameError::TooLarge { len, max }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_length_is_little_endian() {
        assert_eq!(encode_header(0x0102_0304), [4, 3, 2, 1]);
        assert_eq!(decode_header([4, 3, 2, 1], 0x0102_0304), Ok(0x0102_0304));
    }

    #[test]
    fn the_limit_is_inclusive() {
        assert_eq!(decode_header(encode_header(768), 768), Ok(768));
        assert_eq!(
            decode_header(encode_header(769), 768),
            Err(FrameError::TooLarge { len: 769, max: 768 })
        );
    }

    #[test]
    fn an_empty_record_is_rejected() {
        assert_eq!(decode_header(encode_header(0), 768), Err(FrameError::Empty));
    }

    #[test]
    fn a_huge_length_is_rejected_without_overflow() {
        assert!(matches!(
            decode_header([0xff; 4], 768),
            Err(FrameError::TooLarge { len: u32::MAX, .. })
        ));
    }
}
