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

use core::fmt;

/// Identity failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityError {
    /// The device has no identity; it has to be provisioned first.
    NotProvisioned,
    /// The device already has an identity; it is never silently replaced.
    AlreadyProvisioned,
    /// The secret does not have the expected size or format.
    InvalidSecret,
    /// The bytes are not a valid Ed25519 public key.
    InvalidPublicKey,
    /// The signature does not verify.
    InvalidSignature,
}

impl fmt::Display for IdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotProvisioned => "identity not provisioned",
            Self::AlreadyProvisioned => "identity already provisioned",
            Self::InvalidSecret => "invalid identity secret",
            Self::InvalidPublicKey => "invalid public key",
            Self::InvalidSignature => "invalid signature",
        })
    }
}

impl core::error::Error for IdentityError {}
