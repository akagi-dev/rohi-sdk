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

//! Bounded Rust bindings of the `airalab/connectivity-protocol` telemetry
//! wire format.
//!
//! # Overview
//!
//! This crate is the wire layer of ROHI telemetry: protobuf messages that fit
//! a microcontroller. The messages are generated at build time from the
//! vendored `.proto` files with `micropb`, using `heapless` containers only:
//! no allocation, every string, bytes and repeated field has a compile-time
//! bound (see [`limits`]). Messages are encoded and decoded with [`encode`]
//! and [`decode`], which work on caller-provided buffers only.
//!
//! The crate is `no_std` and has no Cargo features. It contains no business
//! logic: building messages from measurements, signing, the envelope and the
//! delivery pipeline are in
//! [`rohi-connectivity`](../rohi_connectivity/index.html), which is the
//! public wrapper to use; this crate is what that wrapper serializes with.
//!
//! The **authoritative description of the messages is the schema**, the
//! `.proto` files in
//! [`crates/telemetry-robonomics/proto/`](https://github.com/akagi-dev/rohi-sdk/tree/master/crates/telemetry-robonomics/proto)
//! (comments there define units and scaling). The generated Rust types
//! mirror it one to one and are not listed item by item here; the table in
//! [Schema map](#schema-map) says where to look.
//!
//! # Quick Example
//!
//! Build a telemetry message, encode it into a caller-provided buffer, decode
//! it again, and compute the bytes a signature covers:
//!
//! ```
//! use rohi_telemetry_robonomics::wire::core_v1::{Message, Message_, Meta};
//! use rohi_telemetry_robonomics::wire::device_v1::{Urban, UrbanSensor, UrbanSensor_};
//! use rohi_telemetry_robonomics::wire::sensor_v1::GPS;
//! use rohi_telemetry_robonomics::wire::{decode, encode, limits, signing_preimage};
//!
//! // `Meta.timestamp` is the telemetry time: Unix UTC milliseconds.
//! let mut message = Message::default();
//! message.set_metadata(Meta { node_id: 7, timestamp: 1_700_000_000_123 });
//!
//! // `Urban.public` holds at most `limits::MAX_SENSORS_PER_MESSAGE` entries.
//! let mut urban = Urban::default();
//! let gps = GPS { lat: 52.52, lon: 13.405, height_m: 34.0 };
//! urban
//!     .public
//!     .push(UrbanSensor { sensor: Some(UrbanSensor_::Sensor::Gps(gps)) })
//!     .unwrap();
//! message.payload = Some(Message_::Payload::Urban(urban));
//!
//! // Encoding needs a big enough buffer and returns the length.
//! let mut buffer = [0u8; limits::MAX_MESSAGE_LEN];
//! let len = encode(&message, &mut buffer).unwrap();
//! let decoded: Message = decode(&buffer[..len]).unwrap();
//! assert_eq!(decoded.metadata().unwrap().timestamp, 1_700_000_000_123);
//!
//! // What an Ed25519 signature of the envelope covers: sensor_id || nonce || message.
//! let (sensor_id, nonce) = ([1u8; limits::ED25519_KEY_LEN], [2u8; limits::NONCE_LEN]);
//! let mut preimage = [0u8; limits::ED25519_KEY_LEN + limits::NONCE_LEN + limits::MAX_MESSAGE_LEN];
//! let used = signing_preimage(&sensor_id, &nonce, &buffer[..len], &mut preimage).unwrap();
//! assert_eq!(used, 32 + 16 + len);
//! ```
//!
//! # Core Concepts
//!
//! * **Message**: `core.v1.Message` is the root. It has `Meta` (node id and
//!   timestamp) and one device payload, today `device.v1.Urban`.
//! * **Signed envelope**: `crypto.v1.SignedEnvelope` carries the serialized
//!   message with the sensor's public key, a nonce and a signature. At the
//!   pinned revision it has exactly four fields: `sensor_id`, `nonce`,
//!   `message` and `signature`. The signature covers
//!   `sensor_id || nonce || message`, where `message` is the exact serialized
//!   `core.v1.Message` (see [`signing_preimage`]). The telemetry timestamp is
//!   `core.v1.Meta.timestamp` (Unix UTC, milliseconds) inside `message`; the
//!   envelope has no timestamp.
//! * **Bounds**: strings, bytes and repeated fields are fixed-capacity. A
//!   message that exceeds a bound is rejected when decoding, never truncated.
//!
//! ## Schema map
//!
//! | Module | Protocol package | `.proto` file | Contents |
//! |--------|------------------|---------------|----------|
//! | [`core_v1`] | `core.v1` | `core/v1/message.proto` | `Meta`, the root `Message` |
//! | [`crypto_v1`] | `crypto.v1` | `crypto/v1/envelope.proto` | `SignedEnvelope` |
//! | [`device_v1`] | `device.v1` | `device/v1/urban.proto`, `device/v1/insight.proto` | `Urban` and `UrbanSensor` (the Altruist Urban payload), `Insight` and `InsightSensor` (not produced by this SDK) |
//! | [`sensor_v1`] | `sensor.v1` | `sensor/v1/sensor.proto`, `sensor/v1/measurement.proto` | per-sensor messages (`GPS`, `BME280`, `SDS011`, `ICS43434`, ...) and measurement types in scaled integers (`Temperature` in 0.01 °C, `Pressure` in 0.1 Pa, ...) |
//!
//! Private (encrypted) sections and envelope batches (`SignedEnvelopeBatch`,
//! `Urban.private`, `Insight.private`) are not generated and not produced by
//! this SDK: decoding skips them as unknown fields.
//!
//! # Architecture & Integration
//!
//! ```text
//!  MeasurementsSnapshot ──► rohi-connectivity ──► core.v1.Message ──encode──► bytes
//!                          (UrbanEncoder)                                       │
//!                                                      signing_preimage ◄───────┤
//!                                                             │ Signer (rohi-identity)
//!                                                             ▼
//!                           crypto.v1.SignedEnvelope ──encode──► frame ──► uplink
//! ```
//!
//! | Neighbour | Relation |
//! |-----------|----------|
//! | [`rohi-connectivity`](../rohi_connectivity/index.html) | Builds `Urban` messages from measurements, signs them and delivers the envelope; this crate is its only protobuf dependency. |
//! | [`rohi-identity`](../rohi_identity/index.html) | Provides the Ed25519 signer; this crate only defines which bytes are signed. |
//! | `airalab/robins` | The reference implementation of the protocol; `rohi-connectivity` pins interoperability vectors generated with it. |
//!
//! # API Navigation
//!
//! | Item | Role |
//! |------|------|
//! | [`encode`], [`EncodeError`] | Serialize a message into a caller buffer. |
//! | [`decode`], [`DecodeError`] | Parse a message; unknown fields are skipped. |
//! | [`signing_preimage`] | The bytes covered by `SignedEnvelope.signature`. |
//! | [`limits`] | The compile-time bounds, applied to the generated types by `build.rs`. |
//! | [`core_v1`], [`crypto_v1`], [`device_v1`], [`sensor_v1`] | The generated messages, see the [schema map](#schema-map). |
//! | [`MessageEncode`], [`MessageDecode`] | `micropb` traits re-exported for the bounds of [`encode`] and [`decode`]. |
//!
//! # Guarantees & Limitations
//!
//! * **No allocation**: [`encode`] and [`decode`] use caller buffers and
//!   `heapless` containers only.
//! * **Bounded**: see [`limits`]; a frame never exceeds
//!   [`limits::MAX_ENVELOPE_LEN`]. The unit tests check that the worst-case
//!   encoded sizes of `Message` and `SignedEnvelope` fit the limits.
//! * **Errors**: [`EncodeError::BufferTooSmall`]; [`DecodeError::Malformed`]
//!   for invalid protobuf, [`DecodeError::Capacity`] for a field over its
//!   bound.
//! * **Lenient decoding**: unknown fields (private sections, extra envelope
//!   fields) are skipped. Decoding does not verify signatures or the meaning
//!   of any value.
//! * **Wire compatibility**: field numbers are part of the wire contract and
//!   are pinned by golden-byte tests here and by interoperability vectors in
//!   `rohi-connectivity`.
//!
//! ## Pinned revision and vendored schema
//!
//! The wire format is pinned to
//! [`airalab/connectivity-protocol`](https://github.com/airalab/connectivity-protocol)
//! revision `33b4e7ec6eaa19a930b91bfb59bd3337c1ec88ac`, recorded in
//! `crates/telemetry-robonomics/UPSTREAM_REV`. The `.proto` files (and the
//! upstream license) are vendored in `crates/telemetry-robonomics/proto/`, so
//! no build downloads anything. The copy is the pinned revision except for
//! one deliberate difference: in `device/v1/urban.proto` and
//! `device/v1/insight.proto` the `private` field is declared as
//! `repeated bytes private = 2` instead of `repeated crypto.v1.Encrypted`
//! (same field number and wire type) and the import of
//! `crypto/v1/encrypted.proto` is dropped; that file is vendored but not
//! compiled. Changing the pin means replacing the vendored files, updating
//! `UPSTREAM_REV` and regenerating the golden vectors of
//! `rohi-connectivity`.
//!
//! The reference implementation of the protocol is
//! [`airalab/robins`](https://github.com/airalab/robins).
//!
//! ## Validation level
//!
//! Host-tested (`cargo test -p rohi-telemetry-robonomics`): golden bytes pin the
//! field numbers, oversized fields are rejected with
//! [`DecodeError::Capacity`] and unknown fields are skipped. Interoperability
//! with the reference implementation is checked by the vectors in
//! `rohi-connectivity`. Nothing here is hardware-specific.

use core::fmt;

pub use micropb::{MessageDecode, MessageEncode};

use micropb::{PbDecoder, PbEncoder};

/// Compile-time bounds of the telemetry frame.
///
/// The same constants are applied to the generated types by `build.rs`, so
/// the schema and the Rust constants cannot drift.
///
/// | Constant | Value |
/// |----------|-------|
/// | `ED25519_KEY_LEN` | 32 B (`SignedEnvelope.sensor_id`) |
/// | `ED25519_SIGNATURE_LEN` | 64 B |
/// | `NONCE_LEN` | 16 B (nonce produced by this SDK) |
/// | `MAX_NONCE_LEN` | 32 B (largest nonce accepted when decoding) |
/// | `MAX_SENSORS_PER_MESSAGE` | 9 |
/// | `MAX_PRIVATE_SECTIONS` | 2 |
/// | `MAX_PRIVATE_PLAINTEXT_LEN` | 128 B (encoded `EncryptedUrban`) |
/// | `NONCE_XCHACHA_LEN` | 24 B |
/// | `MAX_ENCRYPTED_LEN` | 232 B (one encoded `crypto.v1.Encrypted`) |
/// | `MAX_MESSAGE_LEN` | 780 B (worst case of the schema: 776 B) |
/// | `MAX_ENVELOPE_LEN` | 920 B |
///
/// A full Altruist Urban frame (7 measurements) is about 200 B; every private
/// section adds about 150 B.
///
/// The bounds are enforced when decoding (a longer field is
/// [`DecodeError::Capacity`]) and, for the sizes of whole messages, checked
/// against the worst case computed from the schema:
///
/// ```
/// use rohi_telemetry_robonomics::wire::core_v1::Message;
/// use rohi_telemetry_robonomics::wire::crypto_v1::SignedEnvelope;
/// use rohi_telemetry_robonomics::wire::{MessageEncode, limits};
///
/// assert_eq!(Message::MAX_SIZE, Ok(776));
/// assert!(Message::MAX_SIZE.unwrap() <= limits::MAX_MESSAGE_LEN);
/// assert!(SignedEnvelope::MAX_SIZE.unwrap() <= limits::MAX_ENVELOPE_LEN);
/// ```
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

/// Failure to encode a message. It carries no secret.
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
///
/// Nothing is allocated; the message size is computed first, so `buf` is
/// either written completely or not at all (it is not touched when it is too
/// small). A buffer of [`limits::MAX_MESSAGE_LEN`] bytes holds any
/// `core.v1.Message`, one of [`limits::MAX_ENVELOPE_LEN`] bytes any
/// `crypto.v1.SignedEnvelope`.
///
/// # Errors
///
/// [`EncodeError::BufferTooSmall`] when `buf` is shorter than the encoded
/// message.
///
/// ```
/// use rohi_telemetry_robonomics::wire::core_v1::{Message, Meta};
/// use rohi_telemetry_robonomics::wire::{EncodeError, encode};
///
/// let mut message = Message::default();
/// message.set_metadata(Meta { node_id: 7, timestamp: 1 });
///
/// let mut buffer = [0u8; 16];
/// let len = encode(&message, &mut buffer).unwrap();
/// // metadata (field 1): node_id (1) = 7, timestamp (2) = 1
/// assert_eq!(&buffer[..len], &[0x0a, 0x04, 0x08, 0x07, 0x10, 0x01]);
///
/// assert_eq!(encode(&message, &mut [0u8; 2]), Err(EncodeError::BufferTooSmall));
/// ```
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
///
/// The whole input is parsed into a fixed-capacity message; nothing is
/// allocated. The result is only syntactically checked: no signature is
/// verified and no value is validated.
///
/// # Errors
///
/// [`DecodeError::Malformed`] for bytes that are not a valid protobuf message
/// of type `M`, [`DecodeError::Capacity`] when a string, bytes or repeated
/// field exceeds its compile-time bound (see [`limits`]).
///
/// ```
/// use rohi_telemetry_robonomics::wire::crypto_v1::SignedEnvelope;
/// use rohi_telemetry_robonomics::wire::{DecodeError, decode};
///
/// // sensor_id (field 1) with 3 bytes, then an unknown field 5 that is skipped.
/// let envelope: SignedEnvelope = decode(&[0x0a, 0x03, 1, 2, 3, 0x28, 0x09]).unwrap();
/// assert_eq!(envelope.sensor_id.as_slice(), &[1, 2, 3]);
///
/// // A sensor_id over the 32-byte bound is rejected, not truncated.
/// let mut oversized = [0u8; 40];
/// oversized[0] = 0x0a;
/// oversized[1] = 33;
/// assert_eq!(decode::<SignedEnvelope>(&oversized), Err(DecodeError::Capacity));
/// ```
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
/// returns the preimage length otherwise. A buffer of `ED25519_KEY_LEN +
/// NONCE_LEN + MAX_MESSAGE_LEN` bytes (see [`limits`]) always suffices for the
/// frames this SDK produces.
///
/// The `message` bytes are signed as they are: serialize the message once and
/// sign that serialization, never a re-encoding of the decoded message.
///
/// # Errors
///
/// [`EncodeError::BufferTooSmall`] when `out` is shorter than
/// `sensor_id.len() + nonce.len() + message.len()`.
///
/// ```
/// use rohi_telemetry_robonomics::wire::signing_preimage;
///
/// let mut out = [0u8; 8];
/// let len = signing_preimage(&[1, 2], &[3], &[4, 5, 6], &mut out).unwrap();
/// assert_eq!(&out[..len], &[1, 2, 3, 4, 5, 6]);
/// ```
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
mod tests;
