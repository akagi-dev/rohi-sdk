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
use rohi_net::BleConnectionInfo;

/// What an operation needs from the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privilege {
    /// Reads device state without changing it.
    Read,
    /// Changes configuration or controls a subsystem.
    Mutate,
}

/// Security properties of the link a request arrived on, as reported by the
/// transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct LinkSecurity {
    /// The link is encrypted.
    pub encrypted: bool,
    /// The link is encrypted with an authenticated key.
    pub authenticated: bool,
}

impl From<BleConnectionInfo> for LinkSecurity {
    fn from(info: BleConnectionInfo) -> Self {
        Self {
            encrypted: info.encrypted,
            authenticated: info.authenticated,
        }
    }
}

/// Decides whether a request with the given [`Privilege`] may run on a link.
///
/// The transport only reports link properties; this is the single place
/// where they become authorization decisions. Identity provisioning reuses
/// the same mechanism.
pub trait AuthorizationPolicy {
    /// `true` when the operation is allowed.
    fn allows(&self, privilege: Privilege, link: &LinkSecurity) -> bool;
}

/// Allows everything. Used until a link security policy is configured.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenPolicy;

impl AuthorizationPolicy for OpenPolicy {
    fn allows(&self, _privilege: Privilege, _link: &LinkSecurity) -> bool {
        true
    }
}

/// Mutations require an encrypted link; reads are open.
#[derive(Clone, Copy, Debug, Default)]
pub struct RequireEncryption;

impl AuthorizationPolicy for RequireEncryption {
    fn allows(&self, privilege: Privilege, link: &LinkSecurity) -> bool {
        privilege == Privilege::Read || link.encrypted
    }
}

/// Mutations require an authenticated (MITM protected) link; reads are open.
#[derive(Clone, Copy, Debug, Default)]
pub struct RequireAuthenticated;

impl AuthorizationPolicy for RequireAuthenticated {
    fn allows(&self, privilege: Privilege, link: &LinkSecurity) -> bool {
        privilege == Privilege::Read || (link.encrypted && link.authenticated)
    }
}
