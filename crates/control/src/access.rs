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
//! Per-transport access modes and the shared access state.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

use crate::{AuthorizationPolicy, LinkSecurity, Privilege, Transport};

/// What a transport endpoint demands for privileged requests.
///
/// | Request | `Open` | `Restricted` on TCP | `Restricted` on BLE |
/// |---------|--------|---------------------|---------------------|
/// | read | yes | yes (the handshake already proved the client) | yes |
/// | mutate | yes | an authenticated, encrypted connection (the TCP handshake with the pre-shared key) | an encrypted (paired) link |
/// | provision | yes | the same as mutate | the same as mutate |
///
/// A restricted TCP endpoint does not even read a request before its handshake succeeded, so the
/// policy never sees an unauthenticated TCP request there. An action that is marked
/// [`secure_link`](rohi_service::ActionDescriptor::secure_link) (the one that sets the pre-shared key)
/// needs [`protects_secrets`](AuthorizationPolicy::protects_secrets) on top of this in every
/// mode: an encrypted link on BLE, an authenticated and encrypted connection on TCP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportAccessMode {
    /// Everything is allowed. Deliberately insecure for privileged operations.
    Open,
    /// Privileged requests need proof, see the table above.
    Restricted,
}

impl TransportAccessMode {
    /// The value of the `access_mode` config field.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Restricted => "restricted",
        }
    }
}

/// Whether a transport endpoint serves clients.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointState {
    /// Switched off by configuration or not supported by the build.
    Disabled,
    /// Waiting for a client or serving one.
    Running,
    /// The endpoint could not start or stopped with an error.
    Failed,
}

/// What an endpoint is configured to be: the persisted `control_tcp` or
/// `control_ble` section as the runtime sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointSettings {
    /// The endpoint is switched on.
    pub enabled: bool,
    /// What privileged requests need.
    pub mode: TransportAccessMode,
    /// The IPv4 address a TCP endpoint listens on; unused for BLE.
    pub address: [u8; 4],
    /// The port a TCP endpoint listens on, 0 for any free one; unused for BLE.
    pub port: u16,
}

impl EndpointSettings {
    /// The defaults of the TCP endpoint: restricted, loopback, port 9300.
    pub const TCP: Self = Self {
        enabled: true,
        mode: TransportAccessMode::Restricted,
        address: [127, 0, 0, 1],
        port: 9300,
    };

    /// The defaults of the BLE endpoint: open.
    pub const BLE: Self = Self {
        enabled: true,
        mode: TransportAccessMode::Open,
        address: [0; 4],
        port: 0,
    };
}

/// Counters and state of one transport endpoint, for its status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointStats {
    /// Whether the endpoint serves clients.
    pub state: EndpointState,
    /// What the running endpoint was started with.
    pub effective: EndpointSettings,
    /// What the configuration asks for now; it differs from `effective` in
    /// the fields a change of which needs a restart.
    pub desired: EndpointSettings,
    /// Live connections.
    pub sessions: u32,
    /// Requests the policy allowed.
    pub accepted: u32,
    /// Requests the policy refused.
    pub denied: u32,
    /// Connections that did not authenticate (a TCP handshake that failed).
    pub auth_failures: u32,
    /// Framing, reassembly or I/O errors of the transport.
    pub errors: u32,
    /// The kind of the last transport error, 0 for none; set by the transport.
    pub last_error: u8,
}

impl EndpointStats {
    const fn new(settings: EndpointSettings) -> Self {
        Self {
            state: EndpointState::Disabled,
            effective: settings,
            desired: settings,
            sessions: 0,
            accepted: 0,
            denied: 0,
            auth_failures: 0,
            errors: 0,
            last_error: 0,
        }
    }

    /// `true` when the configuration asks for something only a restart
    /// applies: switching the endpoint on or off, another address or port.
    /// The access mode applies at once.
    pub const fn restart_required(&self) -> bool {
        self.desired.enabled != self.effective.enabled
            || self.desired.address[0] != self.effective.address[0]
            || self.desired.address[1] != self.effective.address[1]
            || self.desired.address[2] != self.effective.address[2]
            || self.desired.address[3] != self.effective.address[3]
            || self.desired.port != self.effective.port
    }
}

const TCP: usize = 0;
const BLE: usize = 1;

fn index(transport: Transport) -> Option<usize> {
    match transport {
        Transport::Tcp => Some(TCP),
        Transport::Ble => Some(BLE),
        Transport::Unspecified => None,
    }
}

struct State {
    stats: [EndpointStats; 2],
    psk_enrolled: bool,
    psk_epoch: u32,
}

/// The access state shared by the [`Controller`](crate::Controller) (through
/// [`AccessPolicy`]), the transports and the endpoint services: the mode of
/// every transport, whether a pre-shared key of the TCP endpoint is provisioned and the counters.
///
/// Defaults: TCP `Restricted`, BLE `Open`, no pre-shared key. Nothing here
/// touches storage: the owners of the configuration publish into it.
///
/// ```
/// use rohi_control::{ControlAccess, Transport, TransportAccessMode};
///
/// let access = ControlAccess::new(|_| false);
/// assert_eq!(access.mode(Transport::Tcp), TransportAccessMode::Restricted);
/// assert_eq!(access.mode(Transport::Ble), TransportAccessMode::Open);
/// access.set_mode(Transport::Ble, TransportAccessMode::Restricted);
/// assert_eq!(access.mode(Transport::Ble), TransportAccessMode::Restricted);
/// ```
pub struct ControlAccess {
    state: Mutex<CriticalSectionRawMutex, RefCell<State>>,
    entropy: fn(&mut [u8]) -> bool,
}

impl ControlAccess {
    /// The defaults, with `entropy` as the cryptographic random source of
    /// the challenges. It must return `false` rather than weak bytes.
    pub const fn new(entropy: fn(&mut [u8]) -> bool) -> Self {
        Self {
            state: Mutex::new(RefCell::new(State {
                stats: [
                    EndpointStats::new(EndpointSettings::TCP),
                    EndpointStats::new(EndpointSettings::BLE),
                ],
                psk_enrolled: false,
                psk_epoch: 0,
            })),
            entropy,
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
        self.state.lock(|state| f(&mut state.borrow_mut()))
    }

    /// The effective mode of `transport`; an unspecified transport is always
    /// restricted.
    pub fn mode(&self, transport: Transport) -> TransportAccessMode {
        match index(transport) {
            Some(index) => self.with(|state| state.stats[index].effective.mode),
            None => TransportAccessMode::Restricted,
        }
    }

    /// Applies the mode to the requests that follow.
    pub fn set_mode(&self, transport: Transport, mode: TransportAccessMode) {
        if let Some(index) = index(transport) {
            self.with(|state| {
                state.stats[index].effective.mode = mode;
                state.stats[index].desired.mode = mode;
            });
        }
    }

    /// Whether a pre-shared key for the TCP endpoint is provisioned. The key itself never
    /// enters this state: it is held by the platform and read by `rohi-control-tcp` only.
    pub fn tcp_psk_enrolled(&self) -> bool {
        self.with(|state| state.psk_enrolled)
    }

    /// The generation of the pre-shared key: it changes every time the key is set or removed, so
    /// a TCP connection that was authenticated with an older key can be told to end.
    pub fn tcp_psk_epoch(&self) -> u32 {
        self.with(|state| state.psk_epoch)
    }

    /// Records that the pre-shared key was set (`true`), replaced or removed (`false`).
    pub fn set_tcp_psk(&self, enrolled: bool) {
        self.with(|state| {
            state.psk_enrolled = enrolled;
            state.psk_epoch = state.psk_epoch.wrapping_add(1);
        });
    }

    /// Fills `dst` with unpredictable bytes from the random source the platform gave this state.
    /// `false` when it has none, which makes a TCP handshake fail closed.
    pub fn fill_random(&self, dst: &mut [u8]) -> bool {
        (self.entropy)(dst)
    }

    /// The counters of `transport`.
    pub fn stats(&self, transport: Transport) -> EndpointStats {
        match index(transport) {
            Some(index) => self.with(|state| state.stats[index]),
            None => EndpointStats::new(EndpointSettings::BLE),
        }
    }

    /// Records what `transport` was started with, which is also what it is
    /// asked for until [`set_desired`](Self::set_desired) says otherwise.
    pub fn set_effective(&self, transport: Transport, settings: EndpointSettings) {
        if let Some(index) = index(transport) {
            self.with(|state| {
                state.stats[index].effective = settings;
                state.stats[index].desired = settings;
            });
        }
    }

    /// Records the configuration of `transport` and applies its access mode to
    /// the requests that follow. The other fields apply at the next start, see
    /// [`EndpointStats::restart_required`].
    pub fn set_desired(&self, transport: Transport, settings: EndpointSettings) {
        if let Some(index) = index(transport) {
            self.with(|state| {
                state.stats[index].desired = settings;
                state.stats[index].effective.mode = settings.mode;
            });
        }
    }

    /// Records what the running endpoint of `transport` really listens on:
    /// the platform may start it from a launch option and not from the stored
    /// settings. The access mode and the stored (desired) settings are kept.
    pub fn set_running(&self, transport: Transport, enabled: bool, address: [u8; 4], port: u16) {
        if let Some(index) = index(transport) {
            self.with(|state| {
                let effective = &mut state.stats[index].effective;
                effective.enabled = enabled;
                effective.address = address;
                effective.port = port;
            });
        }
    }

    /// Records whether the endpoint of `transport` serves clients.
    pub fn set_state(&self, transport: Transport, endpoint: EndpointState) {
        if let Some(index) = index(transport) {
            self.with(|state| state.stats[index].state = endpoint);
        }
    }

    /// A connection of `transport` opened.
    pub fn session_opened(&self, transport: Transport) {
        if let Some(index) = index(transport) {
            self.with(|state| {
                state.stats[index].sessions = state.stats[index].sessions.saturating_add(1)
            });
        }
    }

    /// A connection of `transport` closed.
    pub fn session_closed(&self, transport: Transport) {
        if let Some(index) = index(transport) {
            self.with(|state| {
                state.stats[index].sessions = state.stats[index].sessions.saturating_sub(1)
            });
        }
    }

    /// The transport hit a framing or I/O error of the given kind (not 0).
    pub fn transport_error(&self, transport: Transport, kind: u8) {
        if let Some(index) = index(transport) {
            self.with(|state| {
                let stats = &mut state.stats[index];
                stats.errors = stats.errors.saturating_add(1);
                stats.last_error = kind;
            });
        }
    }
}

/// The [`AuthorizationPolicy`] of a [`ControlAccess`]: the mode of the
/// transport the request arrived on decides.
///
/// It consumes only what the transport established, never a field of the request: `encrypted`
/// and `authenticated` are set by the TCP handshake or by BLE pairing. A transport the policy
/// cannot identify is restricted and, being neither TCP nor BLE, needs an encrypted and
/// authenticated link.
///
/// ```
/// use rohi_control::{
///     AccessPolicy, AuthorizationPolicy, ControlAccess, LinkSecurity, Privilege, Transport,
/// };
///
/// let access = ControlAccess::new(|_| false);
/// let policy = AccessPolicy::new(&access);
/// let tcp = LinkSecurity { transport: Transport::Tcp, ..Default::default() };
/// let ble = LinkSecurity { transport: Transport::Ble, ..Default::default() };
/// // TCP is restricted by default, BLE is open.
/// assert!(policy.allows(Privilege::Read, &tcp));
/// assert!(!policy.allows(Privilege::Mutate, &tcp));
/// assert!(policy.allows(Privilege::Mutate, &ble));
/// // What an action that carries transport keys wants on top of that, in every mode.
/// assert!(!policy.protects_secrets(&ble));
/// let paired = LinkSecurity { encrypted: true, ..ble };
/// assert!(policy.protects_secrets(&paired));
/// let secure = LinkSecurity { encrypted: true, authenticated: true, ..tcp };
/// assert!(policy.allows(Privilege::Provision, &secure) && policy.protects_secrets(&secure));
/// assert!(!policy.protects_secrets(&LinkSecurity { encrypted: true, ..tcp }));
/// ```
#[derive(Clone, Copy)]
pub struct AccessPolicy<'a>(&'a ControlAccess);

impl<'a> AccessPolicy<'a> {
    /// The policy over `access`.
    pub const fn new(access: &'a ControlAccess) -> Self {
        Self(access)
    }

    /// The shared state.
    pub const fn access(&self) -> &'a ControlAccess {
        self.0
    }

    fn permits(&self, privilege: Privilege, link: &LinkSecurity) -> bool {
        if privilege == Privilege::Read {
            return true;
        }
        match (link.transport, self.0.mode(link.transport)) {
            (Transport::Tcp, TransportAccessMode::Open) => true,
            (Transport::Ble, TransportAccessMode::Open) => true,
            (Transport::Ble, TransportAccessMode::Restricted) => link.encrypted,
            (Transport::Tcp, TransportAccessMode::Restricted) | (Transport::Unspecified, _) => {
                link.encrypted && link.authenticated
            }
        }
    }
}

impl AuthorizationPolicy for AccessPolicy<'_> {
    fn allows(&self, privilege: Privilege, link: &LinkSecurity) -> bool {
        let allowed = self.permits(privilege, link);
        if let Some(index) = index(link.transport) {
            self.0.with(|state| {
                let stats = &mut state.stats[index];
                if allowed {
                    stats.accepted = stats.accepted.saturating_add(1);
                } else {
                    stats.denied = stats.denied.saturating_add(1);
                }
            });
        }
        allowed
    }

    fn protects_secrets(&self, link: &LinkSecurity) -> bool {
        match link.transport {
            Transport::Ble => link.encrypted,
            Transport::Tcp | Transport::Unspecified => link.encrypted && link.authenticated,
        }
    }

    fn denial_reason(&self, _privilege: Privilege, link: &LinkSecurity) -> Option<&'static str> {
        Some(match link.transport {
            Transport::Ble => "pairing_required",
            _ if !self.0.tcp_psk_enrolled() => "no_psk",
            _ => "encryption_required",
        })
    }
}

impl ControlAccess {
    /// A client of `transport` failed to authenticate (a TCP handshake that did not verify).
    pub fn auth_failed(&self, transport: Transport) {
        if let Some(index) = index(transport) {
            self.with(|state| {
                state.stats[index].auth_failures =
                    state.stats[index].auth_failures.saturating_add(1)
            });
        }
    }
}
