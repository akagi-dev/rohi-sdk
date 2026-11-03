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
//! Async and cancellation.
//!
//! ROHI code is `async` on Embassy. Cancellation (dropping a future before it
//! completes) is normal behaviour in async Rust: `select`, timeouts and task
//! shutdown all do it. SDK code must therefore never leave a service in an
//! undocumented, partially mutated state when a future is dropped.
//!
//! # Execution model
//!
//! - One Embassy executor (thread mode, `esp-rtos` time base). Tasks are
//!   cooperative: nothing runs concurrently with another task between two
//!   `.await` points of the same executor. Radio callbacks never run
//!   application logic; they only copy into bounded queues.
//! - Every stateful service has one owner task
//!   ([`ownership`](crate::ownership)); callers talk to it through bounded
//!   channels, so a caller's future being dropped must not stall or corrupt the
//!   owner.
//! - A future is dropped *between* two `.await` points. Anything that must be
//!   atomic must not contain an `.await`.
//!
//! # Primitives and their cancellation behaviour
//!
//! | Primitive | Use | When the waiting future is dropped |
//! |-----------|-----|-------------------------------------|
//! | Bounded `Channel` (`send`/`receive`) | commands, request/response, queues | `receive` takes an item only when it returns it; `send` queues the item entirely or not at all |
//! | `Signal` / `Watch` (latest value) | status, completion, desired state | the value stays available; waiting again returns it |
//! | `Mutex` serialising callers | one request in flight | the guard is released on drop |
//! | Sequence-numbered responses | request/response with one owner | a stale response is discarded; it never reaches a later caller |
//! | Timer / `select` | timeouts, preemption | the other branch is dropped: it must be cancel-safe |
//!
//! # Rules for SDK code
//!
//! 1. **Document it.** Every public async operation that changes durable
//!    state, changes desired runtime state, owns a peripheral transaction or
//!    uses a request/response channel documents its non-trivial cancellation
//!    semantics, using one of:
//!
//!    - *cancellation-safe*;
//!    - *cancel before commit leaves state unchanged*;
//!    - *cancel after commit leaves the operation logically completed*;
//!    - *cancel may abort the transient operation and the caller must retry*.
//!
//!    Operations that are clearly cancellation-safe by construction (a plain
//!    bounded `receive`) need only a short note.
//!
//! 2. <a id="durable-mutation-rule"></a>**No cancellable gap after a durable
//!    commit.** For operations that commit durable state: there must not be a
//!    cancellable gap between the commit and the publication of the
//!    corresponding desired runtime state. Do the publication synchronously,
//!    in the same step as the commit, in the task that owns the store. See
//!    [Durable mutation](#durable-mutation).
//!
//! 3. **Peripheral transactions are not cancel-safe.** A driver future that
//!    owns a bus or radio transaction may be cut short by a drop; document it,
//!    and run such futures to completion in their own task. Do not put them in
//!    a `select` unless the driver documents that this is safe
//!    (`WifiBackend::connect` and `wait_link_lost` are, because the manager
//!    drops them on purpose).
//!
//! 4. **State changes after the last `.await`.** Do not take or clear a flag
//!    before an `.await` that can be dropped; clear it after the await
//!    completes. The sensor runner follows this for its warm-up flag.
//!
//! 5. **Prefer state publication over queued commands** for persistent desired
//!    state; see [State versus command](crate::ownership#state-versus-command).
//!
//! 6. **Release resources in `Drop`.** Slots, locks and pool entries are owned
//!    by values that release them on drop (a TCP connection slot, a mutex
//!    guard), never by code after an `.await`.
//!
//! # Durable mutation
//!
//! The Wi-Fi configuration is the first concrete application of rule 2:
//!
//! ```text
//! validate ─► ConfigHandle::set(WifiConfig)         ┐ request is queued: from here the owner task completes it
//!                                                   │
//!            config owner task:  commit to flash ─► publish desired Wi-Fi config (synchronous) ─► respond
//!                                   no .await between commit and publish
//! ```
//!
//! - Cancelled **before the request was queued**: nothing changed.
//! - Cancelled **after**: the owner still commits *and* publishes, so the
//!   durable state and the runtime agree; the caller only misses the result.
//!
//! The same logic applies to the other sections that have a runtime consumer
//! (the shared location, the telemetry and NTP tasks, see
//! [`ownership`](crate::ownership)), to the device identity (the owner task
//! updates the signing identity in the commit step) and at boot (stored
//! configuration is published once after opening the store, also after a
//! factory reset). A section that needs a restart (`ble`) is published only at
//! boot. The `SectionStore` contract of
//! [`rohi-service`](../../rohi_service/index.html) states the same rule for every
//! section: `store` and `remove` hand the validated effective value to the
//! runtime in the commit step.
//!
//! # What each public async API guarantees
//!
//! | API | Cancellation semantics |
//! |-----|------------------------|
//! | `WifiHandle::set_wifi_config` / `set_network_config` | synchronous; nothing to cancel |
//! | `WifiHandle::scan` | dropping abandons the request. A scan already handed to the manager still runs; other callers get `Busy` until it finishes; its result is discarded and never delivered to a later caller. Capacity is not lost |
//! | `WifiHandle::reconnect` / `disconnect` | synchronous commands; `Busy` when the bounded queue is full |
//! | `WifiBackend::connect` / `wait_link_lost` | must be cancel-safe: the manager drops them when a command or configuration change preempts them |
//! | `NetworkHandle` DNS, `connect`, read, write | cancel-safe; dropping `connect` frees the slot; a dropped write sends nothing that was not reported |
//! | `UdpSocket::recv_from` / `send_to` | cancel-safe: a dropped `recv_from` loses nothing, the datagram goes to the next call; a dropped `send_to` may or may not have queued the datagram (UDP gives no delivery guarantee); dropping the socket releases its slot |
//! | `TimeSource::sample` (`NtpTimeSource`) | must be cancel-safe and bound its own waits; dropping the NTP sample closes its socket |
//! | `MeasurementHandle::snapshot` / `try_snapshot` | cancellation-safe: reads only; a stale answer is discarded; request slot and call lock are released |
//! | `MeasurementService::accept` (owner side) | cancel-safe; usable in a `select` next to the event queue |
//! | `ConfigHandle::set` / `remove` | before the request is queued: unchanged; after: logically completed (the owner commits and publishes) |
//! | `StorageMaintenance::format_storage` / `factory_reset` | as `ConfigHandle::set`: they use the same request path, so once queued the owner completes them |
//! | `IdentityHandle::store` / `erase` | as `ConfigHandle::set` / `remove`: the owner task updates the signer in the commit step, so the stored identity and the signer cannot diverge |
//! | `ConfigHandle::get` / `status` | read only; abandoned requests are answered and discarded (numbered responses) |
//! | `BleTransport::recv` / `send` | cancel-safe: a chunk is taken or queued entirely or not at all |
//! | `ControlBle::step` | dropping abandons the request being handled (no response is sent for it); no partial state, a new first fragment starts a new request; committed operations stay committed |
//! | `EventBus::receive` | cancel-safe |
//! | `ConnectivityService::run` | safe at any point; queued frames are lost, which is the contract of the in-memory outbox |
//! | `EnvelopeTransport::send` | safe at any point; the frame is not modified, so it can be sent again |
//! | sensor `Runner::step` | may cut a sensor transaction short; the next step repeats the attempt; the warm-up wait is kept |
//!
//! The owners of these APIs document the same semantics on the items
//! themselves: [`rohi-net`](../../rohi_net/index.html),
//! [`rohi-hal`](../../rohi_hal/index.html),
//! [`rohi-control-ble`](../../rohi_control_ble/index.html),
//! [`rohi-time`](../../rohi_time/index.html),
//! [`rohi-time-ntp`](../../rohi_time_ntp/index.html) and
//! [`rohi-connectivity`](../../rohi_connectivity/index.html). `ConfigHandle`,
//! `IdentityHandle` and `StorageMaintenance` live in the firmware
//! (`firmwares/altruist-urban/src/config.rs` and `identity.rs`).
//!
//! # Testing cancellation
//!
//! Host tests cover the meaningful hazards (see
//! [Host testing](https://rohi.akagi.dev/validation/host-testing.html#cancellation-tests)):
//! drop a Wi-Fi scan while waiting, drop a measurement snapshot, cancel a TCP
//! connect/read/write/DNS query, cancel a control request, cancel before and
//! after a state publication, drop a sensor step during its warm-up wait. After
//! every cancellation the tests verify that the service still responds, channel
//! capacity is not permanently lost, stale requests get no later response,
//! nothing stays locked, and the next operation succeeds.
