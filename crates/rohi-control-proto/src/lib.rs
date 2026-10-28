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
#![no_std]

//! Bounded protobuf schema of the ROHI local control protocol.
//!
//! The wire schema lives in `proto/rohi/control/v1/control.proto`; this crate
//! contains the generated `no_std`, allocation-free message types
//! ([`control`]), the [`PROTOCOL_VERSION`] and the compile-time [`limits`].
//! Messages are encoded and decoded with [`encode`] and [`decode`], which work
//! on caller-provided buffers only.

/// Version of the control protocol, carried in every request and response.
pub const PROTOCOL_VERSION: u32 = 1;

/// Compile-time bounds of the protocol.
pub mod limits {
    include!(concat!(env!("OUT_DIR"), "/limits.rs"));
}

/// Generated message types of `rohi.control.v1`.
pub mod control {
    #[allow(
        clippy::all,
        clippy::pedantic,
        missing_docs,
        unused_qualifications,
        warnings
    )]
    mod generated {
        include!(concat!(env!("OUT_DIR"), "/control.rs"));
    }

    pub use generated::rohi_::control_::v1_::*;
}

use core::fmt;

pub use micropb::{MessageDecode, MessageEncode};

use micropb::{PbDecoder, PbEncoder};

/// Failure to encode a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// The output buffer is smaller than the encoded message.
    BufferTooSmall,
}

/// Failure to decode a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The bytes are not a valid protobuf message (truncated, bad wire type,
    /// invalid UTF-8, ...).
    Malformed,
    /// A string, bytes or repeated field exceeds its compile-time bound.
    Capacity,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("buffer too small for the encoded message")
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Malformed => "malformed protobuf message",
            Self::Capacity => "protobuf field exceeds its bound",
        })
    }
}

/// Encodes `message` into the start of `buf` and returns the encoded length.
pub fn encode<M: MessageEncode>(message: &M, buf: &mut [u8]) -> Result<usize, EncodeError> {
    let size = message.compute_size();
    let out = buf.get_mut(..size).ok_or(EncodeError::BufferTooSmall)?;
    let mut encoder = PbEncoder::new(out);
    message
        .encode(&mut encoder)
        .map_err(|()| EncodeError::BufferTooSmall)?;
    Ok(size)
}

/// Decodes a message from `bytes`. Unknown fields are skipped, so newer
/// peers stay compatible.
pub fn decode<M: MessageDecode + Default>(bytes: &[u8]) -> Result<M, DecodeError> {
    let mut message = M::default();
    let mut decoder = PbDecoder::new(bytes);
    message
        .decode(&mut decoder, bytes.len())
        .map_err(|error| match error {
            micropb::DecodeError::Capacity => DecodeError::Capacity,
            _ => DecodeError::Malformed,
        })?;
    Ok(message)
}

impl fmt::Debug for control::ConfigMutation {
    // The value can be a secret, for example a Wi-Fi password: it is never shown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use control::ConfigMutation_::Operation;
        f.debug_struct("ConfigMutation")
            .field("field_id", &self.field_id)
            .field(
                "operation",
                &match &self.operation {
                    Some(Operation::Clear(_)) => "clear",
                    Some(Operation::None(_)) => "none",
                    Some(_) => "set <redacted>",
                    None => "<none>",
                },
            )
            .finish()
    }
}

impl fmt::Debug for control::InvokeActionRequest {
    // Arguments can be secrets, for example an identity seed: never shown.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InvokeActionRequest")
            .field("service_id", &self.service_id)
            .field("action_id", &self.action_id)
            .field("args", &self.args.len())
            .finish()
    }
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
