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

use rohi_service::ActionAccess;

/// What an operation needs from the link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Privilege {
    /// Reads device state without changing it.
    Read,
    /// Changes configuration or runs an action that needs
    /// [`ActionAccess::Mutate`].
    Mutate,
    /// Runs an action that needs [`ActionAccess::Provision`] (for example
    /// installing a device identity or formatting the configuration storage).
    /// A link policy may demand more than for [`Mutate`](Self::Mutate). The
    /// owning service decides whether it may be repeated.
    Provision,
}

impl Privilege {
    /// The privilege the policy checks for an action that declares `access`.
    pub const fn of(access: ActionAccess) -> Self {
        match access {
            ActionAccess::Read => Self::Read,
            ActionAccess::Mutate => Self::Mutate,
            ActionAccess::Provision => Self::Provision,
        }
    }
}

/// The kind of transport a connection belongs to. TCP has an access mode, BLE is open, see
/// [`TransportAccessMode`](crate::TransportAccessMode).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Transport {
    /// Not declared: an [`AccessPolicy`](crate::AccessPolicy) treats it as
    /// restricted and never as open.
    #[default]
    Unspecified,
    /// A BLE central.
    Ble,
    /// A TCP client.
    Tcp,
}

/// Security properties of the link a request arrived on, as reported by the
/// transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct LinkSecurity {
    /// The link is encrypted.
    pub encrypted: bool,
    /// The link is encrypted with an authenticated key.
    pub authenticated: bool,
    /// Identity of the connection, different for every live connection of the
    /// device (a [`ControlHub`](crate::ControlHub) allocates it). A stream
    /// belongs to the session that invoked the action and is unknown to every
    /// other one. A transport that serves a single client may leave it 0.
    pub session: u64,
    /// The transport of the connection.
    pub transport: Transport,
}

/// Decides whether a request with the given [`Privilege`] may run on a link.
///
/// The transport only reports link properties; this is the single place
/// where they become authorization decisions. The properties are the ones a
/// trusted transport established (an encrypted BLE link, a TCP connection that completed
/// its handshake): nothing in a request can raise them. Secret-carrying actions use the
/// same mechanism.
///
/// ```
/// use rohi_control::{
///     AuthorizationPolicy, LinkSecurity, Privilege, RequireAuthenticated, RequireEncryption,
/// };
///
/// let encrypted = LinkSecurity { encrypted: true, ..Default::default() };
/// assert!(RequireEncryption.allows(Privilege::Mutate, &encrypted));
/// assert!(!RequireAuthenticated.allows(Privilege::Mutate, &encrypted));
/// // Reads are open under both.
/// assert!(RequireAuthenticated.allows(Privilege::Read, &LinkSecurity::default()));
/// ```
pub trait AuthorizationPolicy {
    /// `true` when the operation is allowed.
    fn allows(&self, privilege: Privilege, link: &LinkSecurity) -> bool;

    /// The stable reason sent with `PermissionDenied` for a request
    /// [`allows`](Self::allows) refused.
    fn denial_reason(&self, privilege: Privilege, link: &LinkSecurity) -> Option<&'static str> {
        let _ = (privilege, link);
        None
    }

    /// Whether `link` is protected well enough for an action that is marked
    /// [`secure_link`](rohi_service::ActionDescriptor::secure_link): it carries key material of
    /// a transport and is refused in every access mode on a link that is not. The default asks
    /// for an encrypted link.
    fn protects_secrets(&self, link: &LinkSecurity) -> bool {
        link.encrypted
    }
}

/// Allows everything. Used until a link security policy is configured.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpenPolicy;

impl AuthorizationPolicy for OpenPolicy {
    fn allows(&self, _privilege: Privilege, _link: &LinkSecurity) -> bool {
        true
    }

    fn protects_secrets(&self, _link: &LinkSecurity) -> bool {
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
