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

//! Delivery of sealed telemetry packets: per-slot route publication, an
//! optional fallback endpoint and signed-packet export.
//!
//! `rohi-connectivity` consumes **already sealed, immutable packets** built by
//! the telemetry side (`rohi-telemetry` and a packet format such as
//! `rohi-telemetry-robonomics`) and delivers them over HTTP. It never reads
//! observations, keys or protocol messages: it owns routes, schedules,
//! packet lifetime, delivery, retries, diagnostics and the read-only export of
//! sealed packets. A telemetry profile slot exists independently of any route.
//!
//! ```text
//! PacketProvider ──► Router (per-slot deadline + one outbox, shared pool)
//!   resolve/capture/build     │ primary ─► fallback
//!   is_current                ▼
//!                       PacketTransport ──► HTTP 202
//! ```
//!
//! # Routes
//!
//! The routes are **fixed slots addressed by index** (`0`, `1`, ...): each slot is
//! empty or holds one [`Route`], and deleting a route never moves another.
//! A route publishes one telemetry profile slot (`profile_index`) on its own
//! interval to a primary endpoint, with at most one fallback: another way to
//! deliver the *same* packet, with no profile, interval or outbox of its own. The
//! default is **no route**. Replacing the profile or the primary endpoint of a
//! slot is a new generation (the old queue, backoff and counters are gone);
//! the interval and the fallback change in place. The routes are one bounded
//! typed value ([`ConnectivitySection`]) edited atomically through the four
//! Actions of [`ConnectivityService`] (`set_endpoint`, `set_interval`,
//! `set_fallback`, `get_payload`); see [`config`] and [`managed`].
//!
//! # Delivery rules
//!
//! * Only HTTP `202` acknowledges a packet, from any path; the packet is then
//!   complete for the whole route.
//! * A transient failure (DNS, connect, timeout, I/O, `408`, `429`, `5xx`,
//!   anything that is not an acknowledgement) tries the next path at once and
//!   backs the failing path off; `401`/`403` also fail over but pause that
//!   path for ten minutes. `400`, `413` and other `4xx` drop the packet and
//!   do **not** offer it to other endpoints.
//! * Retries and fallbacks send **byte-identical** packets; nothing is ever
//!   signed or encrypted again. Delivery is **at-least-once**: a lost
//!   acknowledgement can duplicate a packet, so the ingress deduplicates by
//!   the immutable envelope.
//! * One capture and one sealed packet serve all routes due at the same
//!   instant with the same profile stamp ([`PacketPool`] handles, no copies).
//!   A full outbox skips the new packet and never evicts an older one.
//! * A change of the telemetry policy or of the endpoints of a slot drops the
//!   queued packets of the affected routes before anything new is sent.
//!
//! The decisions are in [`Router`] and need no network; [`Publisher`] is the
//! task that performs the I/O. `get_payload` exports a freshly built packet as
//! a standard Action stream with ordinary `Read` access.
//!
//! # Limits
//!
//! `no_std`, allocation-free. The number of route slots (`ROUTES`), the capacity of the
//! pool, the outboxes and the buffers are const parameters chosen by the firmware
//! (Altruist Urban: 2 route slots, each with at most 1 fallback) and
//! measured with `scripts/memcheck.sh`; they size the storage of [`Routes`],
//! [`ConnectivityState`], [`Router`], [`Publisher`] and the status schema of
//! [`ConnectivityService`]. A persisted section over capacity is refused, not truncated.
//! The persistent section is 1 KiB at most. The transport is plain
//! HTTP (`http://host[:port]`, 64 bytes at most).

#[cfg(test)]
extern crate std;

pub mod config;
pub mod endpoint;
pub mod http;
pub mod managed;
pub mod pool;
pub mod provider;
pub mod publisher;
pub mod router;
pub mod state;
pub mod transport;

pub use config::{
    ConnectivitySection, DEFAULT_INTERVAL_S, MAX_INTERVAL_S, MIN_INTERVAL_S, Route, RouteError,
    Routes,
};
pub use endpoint::{DEFAULT_PORT, Endpoint, EndpointUri, MAX_ENDPOINT_LEN};
pub use http::{HttpTransport, MAX_USER_AGENT_LEN};
pub use managed::{ConnectivityService, ConnectivityStore, RouteSlots, Slots, StoreError};
pub use pool::{PacketHandle, PacketPool};
pub use provider::{BuildFailure, PacketExporter, PacketProvider};
pub use publisher::Publisher;
pub use router::{Admit, Delivery, RouteCounters, RouteState, RouteView, Router, SkipReason};
pub use state::{ConnectivityState, Health};
pub use transport::{Disposition, PacketTransport, TransportAck, TransportError, classify_status};
