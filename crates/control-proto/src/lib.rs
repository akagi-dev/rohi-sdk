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
#![no_std]

//! Bounded protobuf schema of the Robonomics Open Hardware Interface (ROHI)
//! local control protocol, version 1.
//!
//! # Overview
//!
//! This crate is the wire contract between a ROHI device and its clients: the
//! `rohi` command line tool, any other host software, and the firmware side
//! [`rohi-control`](../rohi_control/index.html) (which serves requests) over a
//! transport such as [`rohi-control-ble`](../rohi_control_ble/index.html). It
//! contains no device logic. What it provides:
//!
//! * the schema, `proto/rohi/control/v1/control.proto` (package
//!   `rohi.control.v1`), the single source of truth of the wire format;
//! * the generated `no_std`, allocation-free message types in [`control`],
//!   created at build time from that file by `micropb`, with every string,
//!   bytes and repeated field bounded by the compile-time [`limits`];
//! * [`PROTOCOL_VERSION`] and the buffer based [`encode`] and [`decode`]
//!   functions with their [`EncodeError`] and [`DecodeError`].
//!
//! The generated items are documented by the schema, not by rustdoc: read the
//! comments of `control.proto` for the meaning of each message and field. The
//! Rust names follow the generator (`GetConfigSectionRequest`, oneof cases
//! become enums in modules such as [`control::Request_`]).
//!
//! # Quick Example
//!
//! Build a request, encode it into a caller buffer and decode it back; the
//! `Debug` output of a mutation never shows its value because it may be a
//! secret:
//!
//! ```
//! use heapless::{String, Vec};
//! use rohi_control_proto::control::{
//!     ConfigMutation, ConfigMutation_::Operation, PatchConfigSectionRequest, Request,
//!     Request_::Request as Call,
//! };
//! use rohi_control_proto::{DecodeError, PROTOCOL_VERSION, decode, encode, limits};
//!
//! let mut mutations = Vec::new();
//! mutations
//!     .push(ConfigMutation {
//!         field_id: 2,
//!         operation: Some(Operation::StringValue(String::try_from("hunter2").unwrap())),
//!     })
//!     .ok()
//!     .unwrap();
//! let request = Request {
//!     protocol_version: PROTOCOL_VERSION,
//!     request_id: 1,
//!     request: Some(Call::PatchConfigSection(PatchConfigSectionRequest {
//!         section_id: 0x335246ad20248b26,
//!         mutations,
//!         dry_run: true,
//!     })),
//! };
//!
//! let mut buffer = [0u8; limits::MAX_REQUEST_LEN];
//! let len = encode(&request, &mut buffer).unwrap();
//! let decoded: Request = decode(&buffer[..len]).unwrap();
//! assert!(decoded == request);
//!
//! let Some(Call::PatchConfigSection(patch)) = &decoded.request else { unreachable!() };
//! assert!(!format!("{:?}", patch.mutations[0]).contains("hunter2"));
//!
//! // A buffer that is too small is an error, not a truncated message.
//! assert!(encode(&request, &mut buffer[..4]).is_err());
//!
//! // A value above its bound is a capacity error (an `ActionArg` string holds 64 bytes).
//! let mut oversized = [b'a'; 67];
//! oversized[0] = 0x32; // field 6 (string_value), length-delimited
//! oversized[1] = 65;
//! assert_eq!(
//!     decode::<rohi_control_proto::control::ActionArg>(&oversized),
//!     Err(DecodeError::Capacity)
//! );
//! ```
//!
//! # Core Concepts
//!
//! **One request, one response.** Every message is a [`control::Request`] or
//! [`control::Response`] envelope with the `protocol_version` and a
//! client-chosen `request_id` that the device echoes (0 when the request could
//! not be parsed). A oneof selects the operation; the response is either that
//! operation's response or an [`control::Error`] with an
//! [`control::ErrorCode`].
//!
//! | Plane | Requests (field numbers) |
//! |-------|--------------------------|
//! | info | `GetInfo` (10) |
//! | config | `ListConfigSections` (11), `GetConfigSchema` (12), `GetConfigSection` (13), `PatchConfigSection` (14), `ResetConfigSection` (15) |
//! | status | `GetServicesStatus` (17), `GetServiceSchema` (18), `GetServiceStatus` (19) |
//! | actions | `ListActions` (20), `GetActionSchema` (21), `InvokeAction` (22) |
//! | stream | `ReadStream` (23), `CloseStream` (24); the files of a stream come as `action_stream` (25), the answer of `InvokeAction` |
//!
//! **Schemas drive clients.** The protocol knows no device module. A device
//! publishes descriptors (`FieldDescriptor`, `ConfigFieldDescriptor`,
//! `EnumDescriptor`, `IssueDescriptor`) and a client renders and parses typed
//! values from them. Names are sent in discovery only; runtime requests
//! address sections (64-bit ids), services and actions (32-bit ids) by
//! number, and runtime enum values and issues are numbers whose names live in
//! the schema. Values are one of bool, `sint64`, `uint64`, `float` (single
//! precision), string, bytes or an enum variant number.
//!
//! **Secrets.** A config field flagged `secret` is write-only: a read reports
//! it as `redacted`. The `Debug` implementations of [`control::ConfigMutation`]
//! and [`control::InvokeActionRequest`] are written by hand to never show
//! values.
//!
//! **Time and counters.** `state_age_ms` and issue `age_ms` are `u32` and
//! saturate at the maximum; `uptime_ms` is `u64`.
//!
//! # Architecture and Integration
//!
//! ```text
//! rohi tool / client ── bytes ── transport ── bytes ── rohi-control ── rohi-service registries
//!   (reuses these types)   (rohi-control-ble)   Controller::handle      config / status / actions
//!                 └──────── rohi-control-proto: schema, limits, encode/decode ────────┘
//! ```
//!
//! This crate depends on nothing of the SDK, so host tools and firmware share
//! it. [`rohi-control`](../rohi_control/index.html) uses [`decode`],
//! [`encode`] and [`limits`] to serve requests;
//! [`rohi-service`](../rohi_service/index.html) mirrors the same limits for its
//! in-memory schemas (`rohi-control` asserts at build time that the
//! `rohi-service` constants are not larger than these limits).
//!
//! # API Navigation
//!
//! * [`control`] - generated messages of `rohi.control.v1`; oneof payloads are
//!   in `Request_` and `Response_`, nested enums in modules named after
//!   their message (for example [`control::ConfigMutation_`]).
//! * [`limits`] - the compile-time bounds, listed below.
//! * [`encode`], [`decode`], [`EncodeError`], [`DecodeError`],
//!   [`MessageEncode`], [`MessageDecode`]: the codec.
//! * [`PROTOCOL_VERSION`] - `1`.
//!
//! # Guarantees and Limitations
//!
//! * **Bounded and allocation free.** A generated message stores every
//!   bounded field inline, so its size is that of its largest content. The
//!   tests of this crate check that the worst-case request fits
//!   [`MAX_REQUEST_LEN`](limits::MAX_REQUEST_LEN) and that the largest schema,
//!   status, action and list responses fit
//!   [`MAX_RESPONSE_LEN`](limits::MAX_RESPONSE_LEN). Collections are never
//!   paged. A memory budget test also caps the in-memory size of the largest
//!   generated types (`Request`, `Response`, `FieldDescriptor`, `FieldValue`
//!   and the status, config section and patch messages): the control task
//!   builds responses on a small microcontroller stack, so growth fails CI
//!   instead of overflowing the stack on the device.
//! * **No truncation.** [`encode`] fails with [`EncodeError::BufferTooSmall`]
//!   if the message does not fit; [`decode`] fails with
//!   [`DecodeError::Capacity`] if a string, bytes or repeated field exceeds
//!   its bound and with [`DecodeError::Malformed`] for anything that is not
//!   valid protobuf (truncated, wrong wire type, invalid UTF-8).
//! * **Compatibility.** [`decode`] skips unknown fields. An unknown request
//!   case decodes with no variant set, which a device answers as unsupported.
//!   Unknown enum values are kept as their number
//!   (`ErrorCode(0x7f)`) and clients should treat them as unknown. Field
//!   ids in schemas are only added, never reused. A device whose
//!   `protocol_version` differs from the one in the request answers
//!   `NotSupported`; this version is `1`.
//! * **Breaking changes inside version 1.** The dedicated `FactoryReset` and
//!   `Reboot` requests were removed: both operations are actions of the
//!   `system` service. Their former field numbers 23 and 24 are reused by
//!   `ReadStream` and `CloseStream`; `action_stream` uses field 25.
//!   `ActionInfo.access` reuses the former `privileged` field number (4). The
//!   version stays `1`; there is no negotiation or fallback, and clients built
//!   against the previous schema are unsupported.
//! * **Not verified here.** The encoding is plain protobuf 3; this crate
//!   tests golden bytes of representative messages, not interoperability with
//!   other protobuf implementations.
//!
//! ## Limits
//!
//! | Constant | Value | Bounds |
//! |----------|-------|--------|
//! | [`limits::MAX_REQUEST_LEN`] | 768 | encoded `Request` |
//! | [`limits::MAX_RESPONSE_LEN`] | 2048 | encoded `Response` |
//! | [`limits::MAX_TEXT_LEN`] | 32 | firmware and board strings |
//! | [`limits::MAX_NAME_LEN`] | 24 | section, service, action, field, issue names |
//! | [`limits::MAX_VALUE_LEN`] | 64 | string/bytes of requests and config values |
//! | [`limits::MAX_FIELD_VALUE_LEN`] | 32 | string/bytes of status values and action results |
//! | [`limits::MAX_REASON_LEN`] | 32 | `Error.reason` |
//! | [`limits::MAX_VARIANT_NAME_LEN`] | 16 | enum variant name |
//! | [`limits::MAX_ENUMS`], [`limits::MAX_ENUM_VARIANTS`] | 1, 9 | enums per schema, variants per enum |
//! | [`limits::MAX_SCHEMA_FIELDS`], [`limits::MAX_STATUS_FIELDS`] | 20, 20 | service schema and status fields |
//! | [`limits::MAX_CONFIG_FIELDS`], [`limits::MAX_MUTATIONS`] | 16, 8 | config fields per section, mutations per patch |
//! | [`limits::MAX_CONFIG_SECTIONS`], [`limits::MAX_SERVICES`], [`limits::MAX_ACTIONS`] | 12, 12, 12 | registry entries listed |
//! | [`limits::MAX_ISSUES`], [`limits::MAX_ISSUE_DESCRIPTORS`] | 4, 12 | active issues, issue descriptors |
//! | [`limits::MAX_ACTION_ARGS`], [`limits::MAX_RECORDS`], [`limits::MAX_RECORD_FIELDS`] | 3, 6, 4 | action arguments, result records, fields per record |
//!
//! The limits are defined once, in the build script of this crate, which also
//! applies them to the generated types, so the schema and the constants
//! cannot drift.

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
///
/// The size is computed first, so nothing is written when `buf` is too small.
///
/// # Errors
///
/// [`EncodeError::BufferTooSmall`] when `buf` is shorter than the encoded
/// message.
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
///
/// # Errors
///
/// [`DecodeError::Capacity`] when a string, bytes or repeated field is longer
/// than its compile-time bound (see [`limits`]); [`DecodeError::Malformed`]
/// for any other invalid input.
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
