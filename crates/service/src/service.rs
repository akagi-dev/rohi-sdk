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

//! Service identity.
//!
//! A service is a logical device capability, for example `wifi` or `sensors`;
//! it is not necessarily an Embassy task. [`Service`] carries only identity.
//! What a service exposes is declared by the independent capability traits
//! [`Status`](crate::Status) and [`Action`](crate::Action), each of which a
//! service implements only when it has something to expose.
//!
//! Identity is generated from the public name with `#[derive(Service)]`:
//!
//! ```
//! use rohi_service::Service;
//!
//! #[derive(Service)]
//! pub struct WifiService;
//!
//! #[derive(Service)]
//! #[service(name = "wifi")]
//! pub struct WirelessService;
//!
//! assert_eq!(WifiService::NAME, "wifi");
//! assert_eq!(WifiService::ID, WirelessService::ID);
//! ```
//!
//! # Names and ids
//!
//! The default name is the type name without a trailing `Service`, in
//! `snake_case` (`WifiService` is `wifi`, `TimeSyncService` is `time_sync`);
//! `#[service(name = "...")]` sets it explicitly. A name matches
//! `[a-z][a-z0-9_]*` and has at most [`MAX_NAME_LEN`] bytes; the derive rejects
//! anything else at build time.
//!
//! The id is [`service_id`] of the name: the 32-bit FNV-1a hash of its UTF-8
//! bytes. It depends on nothing but the name, never on the Rust module path,
//! the crate or the platform, so portable and platform crates agree on it. The
//! algorithm and the names are wire ABI: renaming a service changes its id.
//! Two services of one firmware must not share an id; the registries check it.

use crate::MAX_NAME_LEN;

/// The identity of a service.
pub trait Service {
    /// Numeric id, the address of status and action requests:
    /// [`service_id`]`(NAME)`.
    const ID: u32;
    /// Stable public name, for example `wifi`.
    const NAME: &'static str;
}

const FNV32_OFFSET: u32 = 0x811c_9dc5;
const FNV32_PRIME: u32 = 0x0100_0193;

/// The id of the service with the given name: the 32-bit FNV-1a hash of the
/// UTF-8 bytes of `name`.
pub const fn service_id(name: &str) -> u32 {
    let bytes = name.as_bytes();
    let mut hash = FNV32_OFFSET;
    let mut index = 0;
    while index < bytes.len() {
        hash ^= bytes[index] as u32;
        hash = hash.wrapping_mul(FNV32_PRIME);
        index += 1;
    }
    hash
}

/// Whether `name` is a valid service name: `[a-z][a-z0-9_]*`, at most
/// [`MAX_NAME_LEN`] bytes.
pub const fn service_name_is_valid(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_NAME_LEN || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    let mut index = 1;
    while index < bytes.len() {
        let byte = bytes[index];
        if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_') {
            return false;
        }
        index += 1;
    }
    true
}

/// Fails the build when `name` is not a valid service name. Used by
/// `#[derive(Service)]`.
pub const fn assert_service_name(name: &str) {
    assert!(
        service_name_is_valid(name),
        "service name must match [a-z][a-z0-9_]* and fit MAX_NAME_LEN bytes"
    );
}

/// Whether no two ids of `ids` are equal.
pub const fn distinct_ids(ids: &[u32]) -> bool {
    let mut index = 0;
    while index < ids.len() {
        let mut other = 0;
        while other < index {
            if ids[other] == ids[index] {
                return false;
            }
            other += 1;
        }
        index += 1;
    }
    true
}
