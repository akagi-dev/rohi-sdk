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

//! Bounded bindings of the `airalab/connectivity-protocol` telemetry wire
//! format.
//!
//! The schema is vendored verbatim under `proto/` from the revision recorded
//! in `UPSTREAM_REV` ([`UPSTREAM_REV`]); the build never reads it from the
//! network. The generated `no_std`, allocation-free message types are
//! [`core_v1`] (`core.v1.Message`) and [`crypto_v1`]
//! (`crypto.v1.SignedEnvelope`); [`device_v1`] and [`sensor_v1`] hold the
//! payload types.
//!
//! # Wire contract
//!
//! At the pinned revision `SignedEnvelope` has exactly four fields:
//! `sensor_id`, `nonce`, `message` and `signature`. The signature covers
//! `sensor_id || nonce || message`, where `message` is the serialized
//! `core.v1.Message`. The telemetry timestamp is `core.v1.Meta.timestamp`
//! (Unix UTC, milliseconds) inside `message`; the envelope has no timestamp.
//! See [`signing_preimage`].
//!
//! Messages are encoded and decoded with [`encode`] and [`decode`], which
//! work on caller-provided buffers only.

use core::fmt;

pub use micropb::{MessageDecode, MessageEncode};

use micropb::{PbDecoder, PbEncoder};

/// Upstream revision of `airalab/connectivity-protocol` the vendored schema
/// was copied from.
pub const UPSTREAM_REV: &str = {
    let rev = include_str!("../UPSTREAM_REV").as_bytes();
    // `trim_ascii_end` is not const on every supported toolchain.
    let mut end = rev.len();
    while end > 0 && rev[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    match core::str::from_utf8(rev.split_at(end).0) {
        Ok(rev) => rev,
        Err(_) => panic!("UPSTREAM_REV is not UTF-8"),
    }
};

/// Compile-time bounds of the telemetry frame.
pub mod limits {
    include!(concat!(env!("OUT_DIR"), "/limits.rs"));
}

#[allow(
    clippy::all,
    clippy::pedantic,
    missing_docs,
    unused_qualifications,
    warnings
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/connectivity.rs"));
}

/// `core.v1`: `Meta` and the root `Message`.
pub use generated::core_::v1_ as core_v1;
/// `crypto.v1`: `SignedEnvelope`.
pub use generated::crypto_::v1_ as crypto_v1;
/// `device.v1`: `Urban`, `UrbanSensor`.
pub use generated::device_::v1_ as device_v1;
/// `sensor.v1`: per-sensor measurement messages.
pub use generated::sensor_::v1_ as sensor_v1;

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

/// Decodes a message from `bytes`. Unknown fields are skipped.
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

/// Bytes covered by `SignedEnvelope.signature`: `sensor_id || nonce ||
/// message`, with `message` the exact serialized `core.v1.Message`.
///
/// Fails with [`EncodeError::BufferTooSmall`] if `out` is too small, and
/// returns the preimage length otherwise.
pub fn signing_preimage(
    sensor_id: &[u8],
    nonce: &[u8],
    message: &[u8],
    out: &mut [u8],
) -> Result<usize, EncodeError> {
    let total = sensor_id.len() + nonce.len() + message.len();
    let out = out.get_mut(..total).ok_or(EncodeError::BufferTooSmall)?;
    let (id_out, rest) = out.split_at_mut(sensor_id.len());
    let (nonce_out, message_out) = rest.split_at_mut(nonce.len());
    id_out.copy_from_slice(sensor_id);
    nonce_out.copy_from_slice(nonce);
    message_out.copy_from_slice(message);
    Ok(total)
}

#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;
