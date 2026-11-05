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

//! Networking of ROHI applications that run on a host operating system.
//!
//! # Overview
//!
//! `rohi-net-std` is the platform crate of [`rohi-net`](rohi_net) for `std`, the
//! counterpart of `rohi-net-esp`. It lets an application written against the
//! portable [`NetworkHandle`](rohi_net::NetworkHandle) run as an ordinary
//! process of Linux or macOS: [`NativeIp`] implements the
//! [`IpBackend`](rohi_net::IpBackend) contract with the system resolver and the
//! TCP and UDP sockets of the host, so the application can reach `localhost`,
//! containers and local development services. [`TcpAcceptor`] is the listening
//! side for the control transport of
//! [`rohi-control-tcp`](rohi_control_tcp).
//!
//! The crate is useful to any ROHI application under `std`, not only to
//! the Altruist Urban. It contains no application logic and no simulation:
//! simulated Wi-Fi, faults and scenarios belong to the application or to a
//! simulator crate that wraps [`NativeIp`].
//!
//! # Quick Example
//!
//! A TCP echo through the portable [`NetworkHandle`](rohi_net::NetworkHandle)
//! to a socket of the host. The futures are polled by a plain `block_on`, not
//! by an async runtime: the reactor is the crate's own, see [`runtime`].
//!
//! ```
//! use core::net::SocketAddr;
//! use std::io::{Read as _, Write as _};
//!
//! use embassy_futures::block_on;
//! use embedded_io_async::{Read, Write};
//! use embedded_nal_async::AddrType;
//! use rohi_net::NetworkHandle;
//! use rohi_net_std::NativeIp;
//!
//! // A peer that echoes five bytes.
//! let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
//! let port = listener.local_addr().unwrap().port();
//! let peer = std::thread::spawn(move || {
//!     let (mut stream, _) = listener.accept().unwrap();
//!     let mut bytes = [0u8; 5];
//!     stream.read_exact(&mut bytes).unwrap();
//!     stream.write_all(&bytes).unwrap();
//! });
//!
//! let ip = NativeIp::new();
//! let net = NetworkHandle::new(&ip);
//! block_on(async {
//!     let host = net.resolve("localhost", AddrType::IPv4).await.unwrap();
//!     let mut connection = net.connect(SocketAddr::new(host, port)).await.unwrap();
//!     connection.write_all(b"hello").await.unwrap();
//!     let mut echoed = [0u8; 5];
//!     connection.read_exact(&mut echoed).await.unwrap();
//!     assert_eq!(&echoed, b"hello");
//! });
//! peer.join().unwrap();
//! ```
//!
//! # Core Concepts
//!
//! | Concept | Meaning |
//! |---------|---------|
//! | [`NativeIp`] | The [`IpBackend`](rohi_net::IpBackend) over host sockets, with the bounded pool of the portable contract |
//! | [`TcpAcceptor`] | A listening socket yielding clients to [`ControlTcp`](rohi_control_tcp::ControlTcp) |
//! | [`runtime`] | The one reactor thread behind every host socket of the process |
//!
//! **Embassy stays the runtime.** Application tasks run on the Embassy executor.
//! Host sockets need a reactor that waits for readiness; the crate starts one
//! dedicated thread for it (`rohi-io`) at the first use. Every future of the
//! crate is polled by the caller's executor with the reactor entered
//! ([`runtime::within`]); the reactor thread only wakes it. Nothing is spawned
//! in the background on behalf of an operation, so dropping a future cancels
//! its operation and releases its resources.
//!
//! **Limits are those of the device.** [`NativeIp`] keeps the bounded pool of the
//! portable contract (2 TCP connections and 1 UDP socket by default, set by
//! const generics), so an application does not silently rely on more sockets
//! than the firmware has.
//!
//! # Architecture and Integration
//!
//! ```text
//! application / service
//!        │ NetworkHandle (rohi-net)
//!        ▼
//!   NativeIp ──► system resolver, tokio sockets ──► reactor thread `rohi-io`
//!
//! ControlTcp (rohi-control-tcp) ◄── TcpAcceptor ◄── TcpListener
//! ```
//!
//! * The Altruist Urban composes it in `firmwares/altruist-urban/src/std`.
//! * A simulator injects faults above the socket layer by wrapping
//!   [`NativeIp`] in its own [`IpBackend`](rohi_net::IpBackend), see `rohi-sim`.
//!
//! # API Navigation
//!
//! | Item | Use it to |
//! |------|-----------|
//! | [`NativeIp`] | resolve names and open TCP and UDP sockets through `NetworkHandle` |
//! | [`ip::net_error`] | map a host I/O error to a [`NetError`](rohi_net::NetError) |
//! | [`TcpAcceptor`] | accept control clients |
//! | [`runtime::within`] | run a future of a host socket under any executor |
//! | [`runtime::handle`] | reach the reactor for other host I/O |
//!
//! # Guarantees and Limitations
//!
//! * **Errors.** Host errors are mapped into the categories of
//!   [`NetError`](rohi_net::NetError): refused connects are `ConnectFailed`, resets
//!   and broken pipes `ConnectionReset`, time-outs `Timeout`, resolver failures
//!   `DnsFailed`, anything else `Backend`. There is no time-out of its own: the
//!   caller bounds an operation with a timer, as on the device.
//! * **Cancellation.** Every future is cancel safe as the contract of
//!   [`IpBackend`](rohi_net::IpBackend) requires: dropping `connect` or `resolve`
//!   leaves nothing allocated and dropping a connection or a socket releases its
//!   slot. A name lookup in flight keeps running on the thread pool of the
//!   reactor until the system resolver returns; its result is discarded.
//! * **Readiness.** [`NativeIp`] always reports the IP plane ready. It does not
//!   model a Wi-Fi link; the operating system decides whether a route exists
//!   and the operation fails if it does not.
//! * **No access control.** The listener has no access control and
//!   the caller chooses its address; the control transport has no encryption.
//! * **Platforms.** Host-tested on Linux (`cargo test -p rohi-net-std`). macOS is
//!   expected to work because only portable `tokio` sockets are used, but is
//!   not tested.

#[cfg(all(feature = "ble", target_os = "linux"))]
pub mod ble;
pub mod ip;
pub mod runtime;
pub mod tcp;

pub use ip::{NativeConnection, NativeIp, NativeUdp};
pub use tcp::{TcpAcceptor, TcpConnection};
