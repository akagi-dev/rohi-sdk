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

//! Transport- and platform-independent dispatcher of the Robonomics Open
//! Hardware Interface (ROHI) control protocol.
//!
//! # Overview
//!
//! [`Controller::handle`] takes one encoded protobuf request, checks it
//! against an [`AuthorizationPolicy`], serves it from the registries of a
//! [`ControlBackend`] and writes exactly one encoded response. The crate knows
//! no device module (Wi-Fi, sensors, telemetry, ...): config sections,
//! services and actions are discovered through the registries of
//! [`rohi_service`], and it knows no transport (BLE) or platform (ESP). Use it
//! to expose the registries of a firmware over any byte-oriented link; the
//! wire schema is [`rohi_control_proto`].
//!
//! # Quick Example
//!
//! A device without sections or services answering `GetInfo`, and refusing a
//! mutation on an unencrypted link:
//!
//! ```
//! use embassy_futures::block_on;
//! use rohi_control::{Controller, Device, DeviceInfo, LinkSecurity, RequireEncryption, Transport};
//! use rohi_control_proto::control::{self as pb, Request_::Request as Call, Response_::Response as Reply};
//! use rohi_control_proto::{PROTOCOL_VERSION, decode, encode};
//!
//! let info = DeviceInfo { firmware: "demo-0.1.0", board: "demo-board" };
//! let device = Device::new(info, || 0, (), (), ());
//! let controller = Controller::new(device, RequireEncryption);
//!
//! let ask = |call: Call, link: &LinkSecurity| {
//!     let request = pb::Request {
//!         protocol_version: PROTOCOL_VERSION,
//!         request_id: 7,
//!         request: Some(call),
//!     };
//!     let mut bytes = [0u8; 64];
//!     let len = encode(&request, &mut bytes).unwrap();
//!     let mut response = [0u8; 256];
//!     let used = block_on(controller.handle(link, &bytes[..len], &mut response));
//!     decode::<pb::Response>(&response[..used]).unwrap()
//! };
//!
//! // Reads are open on every link.
//! let reply = ask(Call::GetInfo(pb::GetInfoRequest {}), &LinkSecurity::new(Transport::Tcp));
//! assert_eq!(reply.request_id, 7);
//! let Some(Reply::GetInfo(info)) = reply.response else { panic!() };
//! assert_eq!(info.firmware.as_str(), "demo-0.1.0");
//!
//! // A reset of a config section is a mutation: it needs an encrypted link,
//! // and this device has no such section anyway.
//! let reset = || Call::ResetConfigSection(pb::ResetConfigSectionRequest::default());
//! let denied = ask(reset(), &LinkSecurity::new(Transport::Tcp));
//! let Some(Reply::Error(error)) = denied.response else { panic!() };
//! assert_eq!(error.code, pb::ErrorCode::PermissionDenied);
//!
//! let encrypted = LinkSecurity { encrypted: true, ..LinkSecurity::new(Transport::Tcp) };
//! let unknown = ask(reset(), &encrypted);
//! let Some(Reply::Error(error)) = unknown.response else { panic!() };
//! assert_eq!(error.code, pb::ErrorCode::NotFound);
//! ```
//!
//! # Core Concepts
//!
//! | Item | Role |
//! |------|------|
//! | [`Controller`] | decodes, authorizes, executes and encodes one request at a time |
//! | [`ControlBackend`] | what the firmware provides: [`DeviceInfo`], uptime, the config, status and action registries, device health |
//! | [`Device`] | a [`ControlBackend`] built from its parts (the usual way to make one) |
//! | [`AuthorizationPolicy`] | maps a [`Privilege`] and the [`LinkSecurity`] of the transport to allow/deny |
//! | [`ControlHub`] | the bounded mailbox between the transport frontends and the one worker that owns the [`Controller`] |
//! | [`ControlAccess`], [`AccessPolicy`] | the access mode of each transport, whether a TCP pre-shared key is provisioned and the endpoint counters; the policy that enforces them |
//!
//! ## Operations and privileges
//!
//! | Request | Served from | Privilege |
//! |---------|-------------|-----------|
//! | `GetInfo` | [`ControlBackend::device_info`] | [`Read`](Privilege::Read) |
//! | `ListConfigSections`, `GetConfigSchema`, `GetConfigSection` | [`ConfigRegistry`](rohi_service::ConfigRegistry) | `Read` |
//! | `PatchConfigSection`, `ResetConfigSection` | `ConfigRegistry` | [`Mutate`](Privilege::Mutate) |
//! | `GetServicesStatus`, `GetServiceSchema`, `GetServiceStatus` | [`StatusRegistry`](rohi_service::StatusRegistry) | `Read` |
//! | `ListActions`, `GetActionSchema` | [`ActionRegistry`](rohi_service::ActionRegistry) | `Read` |
//! | `InvokeAction` | `ActionRegistry` | the [`ActionAccess`](rohi_service::ActionAccess) the action declares: `Read`, `Mutate` or `Provision`; secret arguments are a separate property (they are only wiped). The answer has one case per [`ResultFormat`](rohi_service::ResultFormat): a `Feed` (`logging.tail`) answers with one bounded page and registers nothing; a `Stream`, or a `Records` result longer than one page, is registered for the session |
//! | `ReadStream`, `CloseStream`, `ReadResultPage`, `CloseResultPage` | `ActionRegistry` | that of the action that created the stream or the snapshot |
//!
//! [`AccessPolicy`] is the policy of a device: it reads the [`TransportAccessMode`] of the TCP
//! endpoint: `Open` allows every request, `Protected` allows reads and wants a connection that
//! completed the authenticated and encrypted handshake of `rohi-control-tcp` for the rest, and
//! `Private` wants it for every request. That handshake is a property the *transport* reports in [`LinkSecurity`], which no
//! request field can change. BLE has no mode: in the current build it is open (no pairing, no link
//! encryption, and never reported as encrypted or authenticated), so every BLE request is allowed.
//! This crate implements no encryption, no signature and no challenge.
//! The three simple policies are [`OpenPolicy`] (everything),
//! [`RequireEncryption`] (reads are open, anything else needs an encrypted
//! link) and [`RequireAuthenticated`] (anything but reads needs an encrypted
//! and authenticated link). They treat `Provision` like `Mutate`; a custom
//! policy may demand more. For `InvokeAction` the action is resolved first to
//! learn its privilege, so an unknown action is answered `NotFound` or
//! `NotSupported` even to a link the policy would otherwise deny.
//!
//! ## Ownership of streams and snapshots
//!
//! A stream, and the snapshot behind a `Records` result that does not fit one page, belongs to the
//! [`LinkSecurity::session`] that invoked the action and carries that action's privilege.
//! `ReadStream`, `CloseStream`, `ReadResultPage` and `CloseResultPage` from another session are
//! answered `NotFound`, as if it did not exist, and the policy is applied with the privilege of the
//! creating action. A page token names a snapshot and a position, nothing else: it is checked
//! against the owner, the service and the action on every request and grants no authority. A page
//! is the records that fit one response (whole records, at most the page limit); a service that
//! cannot advance, or a record that cannot fit, is an `Internal` error and never an endless
//! empty page. A result that fits its first page is released at once; a longer one is held until
//! it is closed, its service expires it or the link ends.
//! [`Controller::session_closed`] closes only the streams and snapshots of one session;
//! [`Controller::link_lost`] is the same for every session.
//! A [`ControlHub`] gives each connection a session that is unique across
//! transports and ends it with the connection (the end is recorded in the hub, so the streams
//! are closed even if the queue of the worker is full or the transport task is cancelled); a
//! caller that uses [`Controller::handle`] directly gives each connection its own `session`.
//! A transport that authenticates with a pre-shared key binds its session to the generation of
//! the key ([`ControlHub::set_epoch`]): the worker does not run a queued request of a session
//! whose key was replaced meanwhile.
//!
//! ## Errors on the wire
//!
//! Every failure is a typed [`pb::Error`](rohi_control_proto::control::Error)
//! response with the `request_id` of the request:
//!
//! | Situation | `ErrorCode` |
//! |-----------|-------------|
//! | request longer than [`MAX_REQUEST_LEN`](rohi_control_proto::limits::MAX_REQUEST_LEN) or not decodable | `InvalidArgument`, with `request_id` 0 |
//! | `protocol_version` other than [`PROTOCOL_VERSION`](rohi_control_proto::PROTOCOL_VERSION), or no request set | `NotSupported` |
//! | policy denies the privilege | `PermissionDenied` |
//! | unknown section id / service id / action id | `NotFound` |
//! | service or action exists but has no such capability (status of an action-only service, action of a status-only one) | `NotSupported` |
//! | invalid mutation (unknown or read-only field, wrong kind or range, failed validation, unparsable or too many mutations) | `InvalidArgument` (the detail of the [`FieldError`](rohi_service::FieldError) is not sent) |
//! | invalid action arguments | `InvalidArgument` |
//! | storage or service temporarily unavailable, busy | `Unavailable`, `Busy` |
//! | an action refusal | `Rejected` with the stable reason of [`ActionError::Rejected`](rohi_service::ActionError::Rejected) |
//! | unexpected failure | `Internal` |
//!
//! # Architecture and Integration
//!
//! ```text
//! transport (rohi-control-ble, ...)      one request in, one response out
//!        │  bytes + LinkSecurity
//!        ▼
//!   Controller<B, P> ──► AuthorizationPolicy::allows(privilege, link)
//!        │
//!        ▼ ControlBackend
//!   ConfigRegistry ── Section<C, Store, M>   (rohi-service, rohi-config)
//!   StatusRegistry ── services: Status
//!   ActionRegistry ── services: Action
//! ```
//!
//! The firmware builds a [`Device`] over its registries
//! (`firmwares/altruist-urban/src/control.rs`, portable) and hands a [`Controller`] to a
//! transport such as [`rohi-control-ble`](../rohi_control_ble/index.html). The
//! transport reports the [`LinkSecurity`] of the connection; it does not
//! interpret requests. The message schema, the size limits and the
//! compatibility rules are in [`rohi_control_proto`]; the registries are
//! described in [`rohi_service`].
//!
//! # API Navigation
//!
//! * [`Controller`], [`Controller::handle`] - the dispatcher.
//! * [`ControlBackend`], [`Device`], [`DeviceInfo`] - the firmware side.
//! * [`AuthorizationPolicy`], [`Privilege`], [`LinkSecurity`], [`OpenPolicy`],
//!   [`RequireEncryption`], [`RequireAuthenticated`]: authorization.
//!
//! # Guarantees and Limitations
//!
//! * **One worker, one request at a time.** With a [`ControlHub`] every transport hands its
//!   requests to the single task that runs [`ControlHub::serve`], so Config, Status, Actions
//!   and streams of all transports are serialized: a long action delays the requests of the
//!   other connections. A connection has one request in flight and a full hub answers `Busy`.
//! * **One response per request, no allocation.** The request and the response
//!   are caller-owned slices; the dispatcher keeps no state between requests
//!   except the owners of the open streams and snapshots ([`MAX_OPEN_STREAMS`]).
//!   Malformed input is answered with an error response, not a panic or a
//!   disconnect.
//! * **Bounded messages.** Requests are limited to
//!   [`MAX_REQUEST_LEN`](rohi_control_proto::limits::MAX_REQUEST_LEN) bytes,
//!   responses to what fits the response buffer the caller provides
//!   ([`MAX_RESPONSE_LEN`](rohi_control_proto::limits::MAX_RESPONSE_LEN) is
//!   enough for every message). Configuration and status are never paged: the limits of
//!   [`rohi_service`] equal those of the protocol, so a full registry fits. The records of an
//!   action are paged by the control plane, not by the action.
//! * **Response overflow.** If the response does not fit the buffer, an
//!   `Internal` error is sent instead; [`Controller::handle`] returns `0` only
//!   if not even that fits.
//! * **Secrets.** Secret values of a patch or an action invocation are wiped
//!   from the decoded request and the converted values once the operation has
//!   finished. The raw request bytes belong to the caller, which should wipe
//!   them if they may hold secrets. Secret config values are never read back
//!   (see [`rohi_service::config`]).
//! * **Cancellation.** [`Controller::handle`] is an `async fn` without its own
//!   state; dropping its future abandons the request. What a registry already
//!   committed (a stored patch, a started action) is not rolled back, and
//!   the atomicity of a patch is the one of [`rohi_service::config`].
//! * **Authorization is only as strong as the link.** The policy trusts the
//!   [`LinkSecurity`] the transport reports; the transport is responsible for
//!   it being true.
//! * **Not here.** Pairing, encryption (the transports request and report them), framing and fragmentation (see
//!   [`rohi-control-ble`](../rohi_control_ble/index.html)), and any device
//!   logic.

#[cfg(test)]
extern crate std;

mod access;
mod backend;
mod controller;
mod diag;
mod hub;
mod mapping;
mod policy;

#[cfg(test)]
mod hub_tests;
#[cfg(test)]
mod tests;

pub use access::{
    AccessPolicy, Availability, ControlAccess, EndpointSettings, EndpointState, EndpointStats,
    TransportAccessMode,
};
pub use backend::{ControlBackend, Device, DeviceInfo};
pub use controller::{Controller, MAX_OPEN_STREAMS};
pub use hub::{Busy, Connection, ControlHub, MAX_SESSIONS, Reservation, Response, SessionId};
pub use policy::{
    AuthorizationPolicy, LinkSecurity, OpenPolicy, Privilege, RequireAuthenticated,
    RequireEncryption, Transport,
};
